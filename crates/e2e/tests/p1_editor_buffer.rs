//! P1, editor-buffer case (ADR 0037 §3–§4): with an ACP editor bound to one
//! environment connection, Read and Edit use the editor's unsaved buffer merged
//! with disk as their base, a second host's concurrent disk edit survives, and
//! every write commits through the real document authority. Nothing is pushed
//! to the editor.
//!
//! The editor answers the daemon's `AcpReadQuery` frames on its own framed
//! connection, exactly as the `omp acp` adapter's environment client does; the
//! adapter's `fs/read_text_file` hop is proven by
//! `crates/app/tests/acp_spine.rs`.

#![cfg(unix)]

use std::sync::{
	Arc,
	atomic::{AtomicUsize, Ordering},
};

use bytes::Bytes;
use omp_e2e::{
	Result, error,
	support::{DEFAULT_TIMEOUT, EnvHarness, Scratch, within},
};
use omp_env::{AcpRequest, EnvClient, InvocationEvent};
use omp_proto::env::v1::{AcpDocumentAnswer, InvokeTool, acp_document_answer};
use omp_tool::{CallOutcome, Registry};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// The editor on the other end of one environment connection: one buffer per
/// file name, and a count of every query it answered.
#[derive(Default)]
struct Editor {
	buffers: Mutex<Vec<(String, String)>>,
	reads:   AtomicUsize,
	writes:  AtomicUsize,
}

impl Editor {
	fn set(&self, name: &str, text: &str) {
		let mut buffers = self.buffers.lock();
		buffers.retain(|(stored, _)| stored != name);
		buffers.push((name.to_owned(), text.to_owned()));
	}

	fn buffer(&self, path: &str) -> Option<String> {
		self
			.buffers
			.lock()
			.iter()
			.find(|(name, _)| path.ends_with(&format!("/{name}")))
			.map(|(_, text)| text.clone())
	}
}

/// Binds `client` as the editor's connection and answers its document queries
/// until the connection closes.
fn serve_editor(client: EnvClient, editor: Arc<Editor>) -> Result<tokio::task::JoinHandle<()>> {
	client
		.bind_acp(Some(DEFAULT_TIMEOUT))
		.map_err(|source| error(format!("binding the editor failed: {source}")))?;
	let requests = client.acp_requests();
	Ok(tokio::spawn(async move {
		while let Ok(request) = requests.recv_async().await {
			let (request_id, query_id, invocation_id, body) = match request {
				AcpRequest::Read { request_id, query } => {
					editor.reads.fetch_add(1, Ordering::SeqCst);
					let body = editor.buffer(&query.path).map_or_else(
						|| {
							acp_document_answer::Body::Error(omp_proto::env::v1::ProtocolError {
								code:    omp_proto::env::v1::ProtocolErrorCode::PermissionDenied as i32,
								message: "no such buffer".to_owned(),
								props:   None,
							})
						},
						acp_document_answer::Body::Content,
					);
					(request_id, query.query_id, query.invocation_id, body)
				},
				AcpRequest::Write { request_id, query } => {
					editor.writes.fetch_add(1, Ordering::SeqCst);
					let body = acp_document_answer::Body::Content(query.content);
					(request_id, query.query_id, query.invocation_id, body)
				},
			};
			let answer = AcpDocumentAnswer { query_id, invocation_id, body: Some(body) };
			if client
				.answer_acp_document(request_id, answer)
				.await
				.is_err()
			{
				break;
			}
		}
	}))
}

/// Runs one built-in tool to its verdict: `Ok(payload)` or `Err(fault)`.
async fn invoke(
	client: &EnvClient,
	invocation_id: &str,
	name: &str,
	rev: &str,
	args: Value,
) -> Result<std::result::Result<Value, Value>> {
	let mut invocation = within(
		"opening a tool invocation",
		DEFAULT_TIMEOUT,
		client.invoke(InvokeTool {
			invocation_id: invocation_id.to_owned(),
			name: name.to_owned(),
			rev: rev.to_owned(),
			..InvokeTool::default()
		}),
	)
	.await??;
	match within("tool acceptance", DEFAULT_TIMEOUT, invocation.next_event()).await?? {
		Some(InvocationEvent::Accepted(_)) => {},
		other => return Err(error(format!("expected acceptance, got {other:?}"))),
	}
	within(
		"committing tool arguments",
		DEFAULT_TIMEOUT,
		invocation.commit_args(
			Bytes::from(serde_json::to_vec(&args)?),
			Bytes::from_static(b"p1-editor-buffer-token"),
			1000,
			None,
		),
	)
	.await??;
	loop {
		match within("tool verdict", DEFAULT_TIMEOUT, invocation.next_event()).await?? {
			Some(InvocationEvent::Verdict(verdict)) => {
				return match serde_json::from_slice::<CallOutcome<Value, Value>>(&verdict.json)? {
					CallOutcome::Ok(payload) => Ok(Ok(payload)),
					CallOutcome::Faulted(fault) => Ok(Err(fault)),
					other => Err(error(format!("{name} did not settle: {other:?}"))),
				};
			},
			Some(InvocationEvent::Update(_)) => {},
			other => return Err(error(format!("unexpected {name} event {other:?}"))),
		}
	}
}

async fn read(client: &EnvClient, id: &str, path: &str) -> Result<String> {
	let payload = invoke(client, id, "read", "2", json!({"path": path}))
		.await?
		.map_err(|fault| error(format!("read failed: {fault}")))?;
	Ok(payload.to_string())
}

async fn edit(
	client: &EnvClient,
	id: &str,
	path: &str,
	seen: &str,
	op: &str,
) -> Result<std::result::Result<Value, Value>> {
	let tag = omp_edit::store::file_hash(seen);
	invoke(client, id, "edit", "hl.1", json!({"input": format!("[{path}#{tag}]\n{op}")})).await
}

const ORIGINAL: &str = "left=0\nmid=0\nright=0\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p1_editor_buffer_merges_user_host_and_agent_edits_through_the_authority() -> Result<()> {
	let scratch = Scratch::new()?;
	scratch.write("race.txt", ORIGINAL)?;
	let env = EnvHarness::spawn(&scratch, Registry::new()).await?;
	let editor = Arc::new(Editor::default());
	let editor_connection = env.connect_client("acp-editor").await?;
	let agent = editor_connection.client_clone();
	let pump = serve_editor(agent.clone(), Arc::clone(&editor))?;
	let host = env.client_clone();

	// The editor's buffer matches disk: the read anchors it.
	editor.set("race.txt", ORIGINAL);
	let clean = read(&agent, "agent-read-clean", "race.txt").await?;
	assert!(clean.contains("mid=0"), "{clean}");

	// The user types without saving; a second host edits another line on disk.
	let unsaved = "left=0\nmid=user\nright=0\n";
	editor.set("race.txt", unsaved);
	read(&host, "host-read", "race.txt").await?;
	edit(&host, "host-edit", "race.txt", ORIGINAL, "PUT 1.=1:\n+left=host\n")
		.await?
		.map_err(|fault| error(format!("host edit failed: {fault}")))?;
	assert_eq!(scratch.read("race.txt")?, b"left=host\nmid=0\nright=0\n");

	// The agent reads what the user sees, merged with the host's change.
	let merged = "left=host\nmid=user\nright=0\n";
	let seen = read(&agent, "agent-read-dirty", "race.txt").await?;
	assert!(seen.contains("mid=user") && seen.contains("left=host"), "{seen}");
	assert_eq!(scratch.read("race.txt")?, b"left=host\nmid=0\nright=0\n", "a read has no effect");

	// The agent's edit commits user delta + host delta + agent delta.
	edit(&agent, "agent-edit", "race.txt", merged, "PUT 3.=3:\n+right=agent\n")
		.await?
		.map_err(|fault| error(format!("agent edit failed: {fault}")))?;
	assert_eq!(scratch.read("race.txt")?, b"left=host\nmid=user\nright=agent\n");
	assert!(editor.reads.load(Ordering::SeqCst) >= 3, "every agent read and prepare asked");
	assert_eq!(editor.writes.load(Ordering::SeqCst), 0, "nothing is pushed to the editor");

	pump.abort();
	drop(editor_connection);
	env.shutdown().await?;
	Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p1_editor_buffer_overlapping_changes_reject_the_edit_before_any_effect() -> Result<()> {
	let scratch = Scratch::new()?;
	scratch.write("conflict.txt", ORIGINAL)?;
	let env = EnvHarness::spawn(&scratch, Registry::new()).await?;
	let editor = Arc::new(Editor::default());
	let editor_connection = env.connect_client("acp-editor").await?;
	let agent = editor_connection.client_clone();
	let pump = serve_editor(agent.clone(), Arc::clone(&editor))?;
	let host = env.client_clone();

	editor.set("conflict.txt", ORIGINAL);
	read(&agent, "agent-read-clean", "conflict.txt").await?;
	editor.set("conflict.txt", "left=user\nmid=0\nright=0\n");
	read(&host, "host-read", "conflict.txt").await?;
	edit(&host, "host-edit", "conflict.txt", ORIGINAL, "PUT 1.=1:\n+left=host\n")
		.await?
		.map_err(|fault| error(format!("host edit failed: {fault}")))?;

	let fault = edit(&agent, "agent-edit", "conflict.txt", ORIGINAL, "PUT 3.=3:\n+right=agent\n")
		.await?
		.expect_err("overlapping unsaved and disk changes reject the edit");
	let fault = fault.to_string();
	assert!(fault.contains("conflict") || fault.contains("Conflict"), "{fault}");
	assert!(fault.contains("unsaved editor changes"), "{fault}");
	assert_eq!(scratch.read("conflict.txt")?, b"left=host\nmid=0\nright=0\n", "no disk effect");
	assert_eq!(editor.writes.load(Ordering::SeqCst), 0, "no editor write");

	pump.abort();
	drop(editor_connection);
	env.shutdown().await?;
	Ok(())
}
