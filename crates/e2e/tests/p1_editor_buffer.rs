//! P1, editor-buffer case (ADR 0037 §3–§4): with an ACP editor bound to one
//! environment connection, Read and Edit use the editor's unsaved buffer merged
//! with disk as their base, a second host's concurrent disk edit survives,
//! every write commits through the real document authority, and only then are
//! the committed bytes written back to the editor, before the call settles.
//!
//! The editor answers the daemon's `AcpReadQuery` and `AcpWriteQuery` frames on
//! its own framed connection, through the same write-back queue
//! ([`omp_envd::editor_sync::EditorBackend`]) the `omp acp` adapter's
//! environment client runs; the adapter's `fs/read_text_file` and
//! `fs/write_text_file` hop is proven by `crates/app/tests/acp_spine.rs`.

#![cfg(unix)]

use std::{
	future::{Future, ready},
	path::PathBuf,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
};

use bytes::Bytes;
use omp_core::Str;
use omp_e2e::{
	Result, error,
	support::{DEFAULT_TIMEOUT, EnvHarness, Scratch, within},
};
use omp_env::{AcpRequest, EnvClient, InvocationEvent};
use omp_envd::{
	docs::{AcpDocumentBackend, EditorCapabilities, EditorIoError, WriteBack},
	editor_sync::{EditorBackend, EditorFiles},
};
use omp_proto::env::v1::{
	AcpBind, AcpDocumentAnswer, InvokeTool, ProtocolError, acp_document_answer,
};
use omp_tool::{CallOutcome, Registry};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// The editor on the other end of one environment connection: one buffer per
/// file name, and what it saw.
struct Editor {
	/// The project root, to observe disk when a write-back arrives.
	root:    PathBuf,
	buffers: Mutex<Vec<(String, String)>>,
	reads:   AtomicUsize,
	writes:  AtomicUsize,
	/// For each write-back: the bytes written and the file on disk then.
	written: Mutex<Vec<(String, Vec<u8>)>>,
}

impl Editor {
	fn new(scratch: &Scratch) -> Self {
		Self {
			root:    scratch.project().to_path_buf(),
			buffers: Mutex::default(),
			reads:   AtomicUsize::new(0),
			writes:  AtomicUsize::new(0),
			written: Mutex::default(),
		}
	}

	fn set(&self, name: &str, text: &str) {
		let mut buffers = self.buffers.lock();
		buffers.retain(|(stored, _)| stored != name);
		buffers.push((name.to_owned(), text.to_owned()));
	}

	fn name(path: &str) -> &str {
		path.rsplit('/').next().unwrap_or(path)
	}

	fn buffer(&self, path: &str) -> Option<String> {
		let name = Self::name(path);
		self
			.buffers
			.lock()
			.iter()
			.find(|(stored, _)| stored == name)
			.map(|(_, text)| text.clone())
	}
}

/// The editor's `fs/*` methods, as the write-back queue drives them.
struct Files(Arc<Editor>);

impl EditorFiles for Files {
	fn read_text(
		&self,
		path: Str,
	) -> impl Future<Output = std::result::Result<Str, EditorIoError>> + Send + '_ {
		self.0.reads.fetch_add(1, Ordering::SeqCst);
		ready(
			self
				.0
				.buffer(&path)
				.map(Str::from)
				.ok_or(EditorIoError::Refused),
		)
	}

	fn write_text(
		&self,
		path: Str,
		content: Str,
	) -> impl Future<Output = std::result::Result<(), EditorIoError>> + Send + '_ {
		self.0.writes.fetch_add(1, Ordering::SeqCst);
		let name = Editor::name(&path);
		let disk = std::fs::read(self.0.root.join(name)).unwrap_or_default();
		self.0.written.lock().push((content.to_string(), disk));
		self.0.set(name, &content);
		ready(Ok(()))
	}
}

/// Binds `client` as the editor's connection and answers its document queries
/// until the connection closes.
fn serve_editor(client: EnvClient, editor: Arc<Editor>) -> Result<tokio::task::JoinHandle<()>> {
	let capabilities = EditorCapabilities { read: true, write: true };
	client
		.bind_acp(AcpBind {
			documents:     true,
			fs_timeout_ms: u64::try_from(DEFAULT_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
			read_text:     capabilities.read,
			write_text:    capabilities.write,
		})
		.map_err(|source| error(format!("binding the editor failed: {source}")))?;
	let backend = Arc::new(EditorBackend::new(Files(editor), capabilities, DEFAULT_TIMEOUT));
	let requests = client.acp_requests();
	Ok(tokio::spawn(async move {
		while let Ok(request) = requests.recv_async().await {
			let client = client.clone();
			let backend = Arc::clone(&backend);
			tokio::spawn(async move {
				let (request_id, query_id, invocation_id, result) = match request {
					AcpRequest::Read { request_id, query } => {
						let result = backend
							.read_text(Str::from(query.path))
							.await
							.map(|content| acp_document_answer::Body::Content(content.to_string()));
						(request_id, query.query_id, query.invocation_id, result)
					},
					AcpRequest::Write { request_id, query } => {
						let result = backend
							.write_back(WriteBack {
								path:    Str::from(query.path),
								content: Str::from(query.content),
								base:    query.base.map(Str::from),
							})
							.await
							.map(|outcome| acp_document_answer::Body::WriteBack(outcome.into_wire()));
						(request_id, query.query_id, query.invocation_id, result)
					},
				};
				let body = result.unwrap_or_else(|_| {
					acp_document_answer::Body::Error(ProtocolError {
						code:    omp_proto::env::v1::ProtocolErrorCode::PermissionDenied as i32,
						message: "no such buffer".to_owned(),
						props:   None,
					})
				});
				let answer = AcpDocumentAnswer { query_id, invocation_id, body: Some(body) };
				let _ = client.answer_acp_document(request_id, answer).await;
			});
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
	let payload = invoke(client, id, "read", "3", json!({"path": path}))
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
	let editor = Arc::new(Editor::new(&scratch));
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

	// The agent's edit commits user delta + host delta + agent delta, and the
	// editor is written back with exactly those bytes before the call settles.
	let committed = b"left=host\nmid=user\nright=agent\n";
	let payload = edit(&agent, "agent-edit", "race.txt", merged, "PUT 3.=3:\n+right=agent\n")
		.await?
		.map_err(|fault| error(format!("agent edit failed: {fault}")))?;
	assert_eq!(scratch.read("race.txt")?, committed);
	assert!(editor.reads.load(Ordering::SeqCst) >= 3, "every agent read and prepare asked");
	assert_eq!(
		editor.writes.load(Ordering::SeqCst),
		1,
		"the write-back reached the editor before the verdict: {payload}"
	);
	let written = editor.written.lock().clone();
	assert_eq!(written.len(), 1);
	assert_eq!(written[0].0.as_bytes(), committed, "the editor received exactly the commit");
	assert_eq!(written[0].1, committed, "the commit was durable before the write-back");
	assert_eq!(editor.buffer("race.txt").as_deref().map(str::as_bytes), Some(&committed[..]));
	assert!(!payload.to_string().contains("editor_sync"), "a clean write-back: {payload}");

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
	let editor = Arc::new(Editor::new(&scratch));
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
