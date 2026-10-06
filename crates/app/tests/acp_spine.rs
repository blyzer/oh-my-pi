//! ACP controller proofs over a scripted journal-first kernel.

use std::{
	collections::VecDeque,
	future::{Future, ready},
	sync::Arc,
	time::SystemTime,
};

use futures::StreamExt as _;
use omp_agent::{DispatchPolicy, Inference, Kernel, StaticPrompt};
use omp_ai::{
	Artifact, ArtifactBody, BlockKind, ChatEvent, ChatRequest, ChatStream, Completion,
	ExecutionReceipt, FinishReason, ProviderId, RequestId, ResponseMeta, RouteId, Usage,
};
use omp_app::{
	acp_client::{
		AcpClient, AcpFs, AcpSettings, AuthCapabilities, ClientCapabilities, ClientMethod,
		ClientRequestError, EditorDocumentsHost, FileSystemCapabilities,
	},
	acp_mode::AcpConnection,
};
use omp_core::Str;
use omp_driver::{
	headless::kernel::{KernelOptions, SessionHome},
	sessions::SessionRegistry,
};
use omp_envd::docs::{
	AcpDocumentBackend, EditorCapabilities, EditorIoError, WriteBack, WriteBackOutcome,
};
use omp_journal::blob::BlobStore;
use omp_session::{ComponentRegistry, Session};
use omp_tool::Registry;
use parking_lot::Mutex;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

#[derive(Clone, Copy)]
enum Script {
	Pending,
	Text(&'static str),
	TextAndImage(&'static str, &'static [u8]),
	/// One call of the gated test tool with this command.
	GatedCall(&'static str),
	/// One call of the named tool with these JSON arguments.
	Call(&'static str, &'static str),
}

struct ScriptedInference {
	scripts: Mutex<VecDeque<Script>>,
}

impl Inference for ScriptedInference {
	fn chat(
		&mut self,
		_request: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_ai::Error>> + Send {
		let script = self
			.scripts
			.get_mut()
			.pop_front()
			.expect("one scripted turn");
		ready(Ok(match script {
			Script::Pending => {
				let events = futures::stream::once(ready(Ok(started())))
					.chain(futures::stream::pending::<Result<ChatEvent, omp_ai::Error>>());
				ChatStream::ordinary(Box::pin(events))
			},
			Script::Text(text) => {
				let events = vec![
					started(),
					ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text },
					ChatEvent::TextDelta { index: 0, text: Str::new(text) },
					ChatEvent::Completed(Completion {
						reason:  FinishReason::Stop,
						blocks:  1,
						usage:   Usage::default(),
						receipt: ExecutionReceipt::default().into(),
					}),
				]
				.into_iter()
				.map(Ok);
				ChatStream::ordinary(Box::pin(futures::stream::iter(events)))
			},
			Script::TextAndImage(text, image) => {
				let events = vec![
					started(),
					ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text },
					ChatEvent::TextDelta { index: 0, text: Str::new(text) },
					ChatEvent::BlockStarted { index: 1, kind: BlockKind::Artifact },
					ChatEvent::Artifact {
						index:    1,
						artifact: Artifact {
							media_type: Str::new_static("image/png"),
							size:       None,
							digest:     None,
							body:       ArtifactBody::Bytes(bytes::Bytes::from_static(image)),
						},
					},
					ChatEvent::Completed(Completion {
						reason:  FinishReason::Stop,
						blocks:  2,
						usage:   Usage::default(),
						receipt: ExecutionReceipt::default().into(),
					}),
				]
				.into_iter()
				.map(Ok);
				ChatStream::ordinary(Box::pin(futures::stream::iter(events)))
			},
			Script::GatedCall(_) | Script::Call(..) => {
				let (id, name, arguments) = match script {
					Script::GatedCall(command) => {
						("call-gated", "gated", serde_json::json!({"command": command}))
					},
					Script::Call(name, arguments) => (
						"call-tool",
						name,
						serde_json::from_str(arguments).expect("scripted tool arguments"),
					),
					_ => unreachable!("tool-call scripts only"),
				};
				let call = omp_ai::ToolCall {
					id:        id.into(),
					name:      Str::new_static(name),
					arguments: omp_ai::call::OpaqueJson::new(arguments.clone()),
				};
				let events = vec![
					started(),
					ChatEvent::ToolCallStarted {
						index: 0,
						id:    call.id.clone(),
						name:  call.name.clone(),
					},
					ChatEvent::ToolArgumentsDelta {
						index: 0,
						bytes: bytes::Bytes::from(serde_json::to_vec(&arguments).expect("arguments")),
					},
					ChatEvent::ToolCallReady { index: 0, call },
					ChatEvent::Completed(Completion {
						reason:  FinishReason::ToolCalls,
						blocks:  1,
						usage:   Usage::default(),
						receipt: ExecutionReceipt::default().into(),
					}),
				]
				.into_iter()
				.map(Ok);
				ChatStream::ordinary(Box::pin(futures::stream::iter(events)))
			},
		}))
	}
}

fn started() -> ChatEvent {
	ChatEvent::Started(ResponseMeta {
		request_id:          RequestId::from("acp-script"),
		provider:            ProviderId::from("scripted"),
		route:               RouteId::from("scripted/test"),
		model:               None,
		provider_request_id: None,
		created_at:          SystemTime::UNIX_EPOCH,
	})
}

fn harness(
	directory: &tempfile::TempDir,
	scripts: impl IntoIterator<Item = Script>,
) -> (Kernel<ScriptedInference>, Session, SessionHome) {
	harness_with(directory, scripts, Arc::new(Registry::new()))
}

fn harness_with(
	directory: &tempfile::TempDir,
	scripts: impl IntoIterator<Item = Script>,
	registry: Arc<Registry>,
) -> (Kernel<ScriptedInference>, Session, SessionHome) {
	let sessions_dir = directory.path().join("sessions");
	std::fs::create_dir_all(&sessions_dir).expect("sessions directory");
	let spill = BlobStore::open(directory.path().join("blobs")).expect("blob store");
	let kernel = Kernel::new(
		ScriptedInference { scripts: Mutex::new(scripts.into_iter().collect()) },
		registry,
		DispatchPolicy::new(spill),
		StaticPrompt(Str::new_static("system")),
	);
	let live = Arc::new(SessionRegistry::new());
	let options = KernelOptions {
		sessions_dir: Some(sessions_dir.clone()),
		sessions: Some(live),
		..KernelOptions::default()
	};
	let home = SessionHome::new(
		directory.path(),
		directory.path(),
		&options,
		Str::new_static("scripted/test"),
		kernel.mailbox(),
	)
	.expect("session home");
	let session = Session::create(sessions_dir.join("startup.oms"), ComponentRegistry::standard())
		.expect("startup session");
	(kernel, session, home)
}

async fn exchange(
	kernel: Kernel<ScriptedInference>,
	session: Session,
	home: SessionHome,
	requests: &'static [u8],
) -> Vec<Value> {
	let (client_io, server_io) = tokio::io::duplex(64 * 1024);
	let (server_read, server_write) = tokio::io::split(server_io);
	let server = AcpConnection::new(AcpSettings::default()).serve(
		kernel,
		session,
		home,
		server_read,
		server_write,
	);
	let client = async move {
		let (client_read, mut client_write) = tokio::io::split(client_io);
		client_write.write_all(requests).await.expect("requests");
		client_write.shutdown().await.expect("request shutdown");
		let mut lines = BufReader::new(client_read).lines();
		let mut frames = Vec::new();
		while let Some(line) = lines.next_line().await.expect("response") {
			frames.push(serde_json::from_str(&line).expect("JSON response"));
		}
		frames
	};
	let (server, frames) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
		tokio::join!(server, client)
	})
	.await
	.expect("ACP exchange must not deadlock");
	server.expect("ACP server");
	frames
}

fn response<'a>(frames: &'a [Value], id: &str) -> &'a Value {
	frames
		.iter()
		.find(|frame| frame.get("id").and_then(Value::as_str) == Some(id))
		.unwrap_or_else(|| panic!("missing response {id}: {frames:#?}"))
}

#[tokio::test]
async fn control_requests_remain_live_during_a_prompt() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let (kernel, session, home) = harness(&directory, [Script::Pending]);
	let frames = exchange(
		kernel,
		session,
		home,
		br#"{"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":1}}
{"jsonrpc":"2.0","id":"prompt","method":"session/prompt","params":{"prompt":"wait"}}
{"jsonrpc":"2.0","id":"approval","method":"session/approve","params":{"promptId":"approval-1","approved":true}}
{"jsonrpc":"2.0","id":"cancel","method":"session/cancel","params":{}}
{"jsonrpc":"2.0","id":"shutdown","method":"shutdown","params":{}}
"#,
	)
	.await;

	assert_eq!(response(&frames, "approval")["result"], serde_json::json!({}));
	assert_eq!(response(&frames, "cancel")["result"], serde_json::json!({}));
	assert_eq!(response(&frames, "prompt")["result"]["stopReason"], "cancelled");
	let approval = frames
		.iter()
		.position(|frame| frame.get("id").and_then(Value::as_str) == Some("approval"))
		.expect("approval response");
	let prompt = frames
		.iter()
		.position(|frame| frame.get("id").and_then(Value::as_str) == Some("prompt"))
		.expect("prompt response");
	assert!(approval < prompt, "approval must dispatch before the active prompt completes");
}

#[tokio::test]
async fn standard_acp_cancel_interrupts_the_active_prompt() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let (kernel, session, home) = harness(&directory, [Script::Pending]);
	let frames = exchange(
		kernel,
		session,
		home,
		br#"{"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":1}}
{"jsonrpc":"2.0","id":"prompt","method":"session/prompt","params":{"sessionId":"startup","prompt":[{"type":"text","text":"wait"}]}}
{"jsonrpc":"2.0","id":"cancel","method":"cancel","params":{"sessionId":"startup"}}
{"jsonrpc":"2.0","id":"shutdown","method":"shutdown","params":{}}
"#,
	)
	.await;

	assert_eq!(
		response(&frames, "cancel")["result"],
		serde_json::json!({}),
		"the ACP `cancel` method must be accepted, not rejected as unknown: {frames:#?}"
	);
	assert_eq!(response(&frames, "prompt")["result"]["stopReason"], "cancelled");
}

#[tokio::test]
async fn content_block_prompts_journal_text_and_image_attachments() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let (kernel, session, home) =
		harness(&directory, [Script::TextAndImage("seen", b"provider image")]);
	let journal_path = session.journal_path().to_path_buf();
	let frames = exchange(
		kernel,
		session,
		home,
		br#"{"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":1}}
{"jsonrpc":"2.0","id":"prompt","method":"session/prompt","params":{"sessionId":"startup","prompt":[{"type":"text","text":"describe this"},{"type":"resource","resource":{"uri":"file:///notes.md","mimeType":"text/markdown","text":"embedded notes"}},{"type":"resource_link","uri":"file:///spec.md","name":"spec.md"},{"type":"image","data":"iVBORw0KGgo=","mimeType":"image/png"}]}}
{"jsonrpc":"2.0","id":"shutdown","method":"shutdown","params":{}}
"#,
	)
	.await;

	let init = response(&frames, "init");
	assert_eq!(init["result"]["agentCapabilities"]["promptCapabilities"]["image"], true);
	let prompt = response(&frames, "prompt");
	assert_ne!(
		prompt["error"]["code"],
		serde_json::json!(-32602),
		"content-block prompts must be accepted as valid params: {prompt:#?}"
	);
	assert!(prompt["result"].get("text").is_none(), "ACP PromptResponse carries chunks, not text");
	assert!(
		frames.iter().any(|frame| {
			frame["method"] == "session/update"
				&& frame["params"]["sessionId"] == "startup"
				&& frame["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
				&& frame["params"]["update"]["content"]["text"] == "seen"
				&& frame["params"]["update"]["messageId"].is_string()
		}),
		"assistant text must use the ACP chunk event with a stable message id: {frames:#?}",
	);
	assert!(
		frames.iter().all(|frame| {
			!matches!(
				frame
					.pointer("/params/update/sessionUpdate")
					.and_then(Value::as_str),
				Some("patch" | "snapshot")
			)
		}),
		"private DOM patch vocabulary must never leak onto ACP: {frames:#?}",
	);
	assert!(
		frames.iter().any(|frame| {
			frame["method"] == "session/update"
				&& frame["params"]["sessionId"] == "startup"
				&& frame["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
				&& frame["params"]["update"]["content"]["type"] == "image"
				&& frame["params"]["update"]["content"]["data"]
					== omp_core::base64::encode(b"provider image").into_string()
		}),
		"provider media must resolve from the same session CAS as the prompt: {frames:#?}",
	);

	let reopened = Session::open(&journal_path, ComponentRegistry::standard()).expect("reopen");
	let dom = reopened.dom();
	let turn = *dom.children(dom.body()).last().expect("journaled turn");
	let user = dom
		.children(turn)
		.iter()
		.filter_map(|handle| dom.get(*handle))
		.find(|node| node.tag == omp_dom::Tag::Known(omp_dom::KnownTag::User))
		.expect("journaled user message");
	assert_eq!(
		user.content.as_deref(),
		Some("describe this\n\nembedded notes\n\nspec.md"),
		"text, embedded text resources, and resource links join in order"
	);
	let attachments = match user.prop(&omp_dom::PropKey::Known(omp_dom::PropId::Data)) {
		Some(omp_dom::Value::Json(raw)) => {
			serde_json::from_str::<Vec<omp_journal::blob::BlobRef>>(raw.get()).expect("blob refs")
		},
		other => panic!("user message must carry its image attachments, got {other:?}"),
	};
	assert_eq!(attachments.len(), 1);
	let stored = reopened
		.blobs()
		.get(&attachments[0])
		.expect("image blob is content-addressed in the session store");
	assert_eq!(stored.as_ref(), b"\x89PNG\r\n\x1a\n");

	let provider_blob = omp_journal::blob::BlobRef {
		hash: omp_core::Hash32::sum(b"provider image"),
		size: u64::try_from(b"provider image".len()).expect("fixture length"),
	};
	assert_eq!(
		reopened
			.blobs()
			.get(&provider_blob)
			.expect("provider image in session CAS")
			.as_ref(),
		b"provider image"
	);
	let assistant = dom
		.children(turn)
		.iter()
		.copied()
		.find(|handle| {
			dom.get(*handle)
				.is_some_and(|node| node.tag == omp_dom::Tag::Known(omp_dom::KnownTag::Assistant))
		})
		.expect("journaled assistant");
	let artifact = dom
		.children(assistant)
		.iter()
		.filter_map(|handle| dom.get(*handle))
		.find(|node| matches!(&node.tag, omp_dom::Tag::Custom(tag) if tag.as_str() == "artifact"))
		.expect("journaled provider artifact");
	let provider_uri = format!("artifact://sha256/{}", provider_blob.to_hex());
	assert_eq!(
		artifact
			.prop(&omp_dom::PropKey::Known(omp_dom::PropId::Blob))
			.and_then(omp_dom::Value::as_str),
		Some(provider_uri.as_str())
	);
	assert_eq!(
		artifact.prop(&omp_dom::PropKey::Custom(Str::new_static("size"))),
		Some(&omp_dom::Value::Int(i64::try_from(provider_blob.size).expect("fixture size"))),
		"the actual CAS size is journaled even when the provider omitted it"
	);
}

#[tokio::test]
async fn new_load_and_resume_switch_the_authoritative_durable_session() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let target_path = directory.path().join("sessions").join("target.oms");
	let resumed_path = directory.path().join("sessions").join("resumed.oms");
	std::fs::create_dir_all(target_path.parent().expect("session parent"))
		.expect("sessions directory");
	let mut target =
		Session::create(&target_path, ComponentRegistry::standard()).expect("durable load target");
	let image = target
		.store_attachment("image/png", b"switched image")
		.expect("target image");
	target.begin_turn().expect("target turn");
	target
		.user("loaded [Image #1]", vec![image])
		.expect("target image prompt");
	drop(target);
	drop(
		Session::create(&resumed_path, ComponentRegistry::standard()).expect("durable resume target"),
	);
	let (kernel, session, home) = harness(&directory, [Script::Text("written")]);
	let frames = exchange(
		kernel,
		session,
		home,
		br#"{"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":1}}
{"jsonrpc":"2.0","id":"new","method":"session/new","params":{}}
{"jsonrpc":"2.0","id":"load","method":"session/load","params":{"sessionId":"target"}}
{"jsonrpc":"2.0","id":"resume","method":"session/resume","params":{"sessionId":"resumed"}}
{"jsonrpc":"2.0","id":"prompt","method":"session/prompt","params":{"prompt":"durable marker"}}
{"jsonrpc":"2.0","id":"shutdown","method":"shutdown","params":{}}
"#,
	)
	.await;

	let new_id = response(&frames, "new")["result"]["sessionId"]
		.as_str()
		.expect("new session id");
	assert_ne!(new_id, "startup");
	assert_ne!(new_id, "target");
	assert!(
		directory
			.path()
			.join("sessions")
			.join(format!("{new_id}.oms"))
			.exists()
	);
	assert!(
		response(&frames, "load")["result"]
			.get("sessionId")
			.is_none(),
		"ACP load response identifies the already-requested session implicitly",
	);
	assert!(
		response(&frames, "resume")["result"]
			.get("sessionId")
			.is_none(),
		"ACP resume response identifies the already-requested session implicitly",
	);
	assert_eq!(response(&frames, "load")["result"]["modes"]["currentModeId"], "default",);
	assert_eq!(response(&frames, "resume")["result"]["modes"]["currentModeId"], "default",);
	assert!(
		frames.iter().any(|frame| {
			frame["method"] == "session/update"
				&& frame["params"]["sessionId"] == "target"
				&& frame["params"]["update"]["sessionUpdate"] == "user_message_chunk"
				&& frame["params"]["update"]["content"]["type"] == "image"
				&& frame["params"]["update"]["content"]["data"]
					== omp_core::base64::encode(b"switched image").into_string()
		}),
		"loading a session resolves its image from that session's CAS: {frames:#?}"
	);

	let target =
		Session::open(&target_path, ComponentRegistry::standard()).expect("load target reopens");
	let target_snapshot = target.dom().snapshot();
	assert!(
		!String::from_utf8_lossy(target_snapshot.as_bytes()).contains("durable marker"),
		"resuming another session must switch authority away from the prior load target"
	);
	let resumed =
		Session::open(&resumed_path, ComponentRegistry::standard()).expect("resume target reopens");
	let resumed_snapshot = resumed.dom().snapshot();
	assert!(
		String::from_utf8_lossy(resumed_snapshot.as_bytes()).contains("durable marker"),
		"prompt must be journaled in the requested resumed session"
	);
}

/// Loading a stored session replays a user message as `user_message_chunk` and
/// the harness's `<developer>` note as `agent_thought_chunk`, in journal order,
/// so injected text is never shown as if the person had typed it.
#[tokio::test]
async fn load_replays_developer_notes_as_thoughts_not_user_messages() {
	use omp_dom::{KnownTag, NodeSpec, Op, PropId, Tag, Txn, Value as DomValue};

	let directory = tempfile::tempdir().expect("temporary directory");
	let sessions = directory.path().join("sessions");
	std::fs::create_dir_all(&sessions).expect("sessions directory");
	{
		let mut stored = Session::create(sessions.join("notes.oms"), ComponentRegistry::standard())
			.expect("stored session");
		stored.begin_turn().expect("turn");
		stored.user("please refactor", Vec::new()).expect("user");
		let turn = *stored
			.dom()
			.children(stored.dom().body())
			.last()
			.expect("turn");
		let after = stored.dom().children(turn).last().copied();
		stored
			.patch(Txn {
				cause: stored.head().expect("head"),
				label: Some(Str::new_static("director.stream-rule")),
				ops:   vec![Op::Ins {
					parent: turn,
					after,
					node: NodeSpec::new(KnownTag::Developer)
						.with_content(Str::new_static("Stream rule no-unwrap: avoid unwrap.")),
				}],
			})
			.expect("developer note");
		stored.assistant_start("m", "p", "r").expect("assistant");
		let assistant = *stored.dom().children(turn).last().expect("assistant");
		stored
			.patch(Txn {
				cause: stored.head().expect("head"),
				label: Some(Str::new_static("assistant.content")),
				ops:   vec![Op::Ins {
					parent: assistant,
					after:  None,
					node:   NodeSpec::new(Tag::Custom(Str::new_static(
						omp_session::ASSISTANT_CONTENT_TAG,
					)))
					.with_prop(PropId::Kind, DomValue::Str(Str::new_static("text")))
					.with_prop(PropId::Text, DomValue::Str(Str::new_static("refactored"))),
				}],
			})
			.expect("assistant text");
	}
	let (kernel, session, home) = harness(&directory, []);
	let frames = exchange(
		kernel,
		session,
		home,
		br#"{"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":1}}
{"jsonrpc":"2.0","id":"load","method":"session/load","params":{"sessionId":"notes"}}
{"jsonrpc":"2.0","id":"shutdown","method":"shutdown","params":{}}
"#,
	)
	.await;

	let replayed = frames
		.iter()
		.filter(|frame| {
			frame["method"] == "session/update"
				&& frame["params"]["sessionId"] == "notes"
				&& frame["params"]["update"]["sessionUpdate"]
					.as_str()
					.is_some_and(|kind| {
						kind.ends_with("_message_chunk") || kind == "agent_thought_chunk"
					})
		})
		.map(|frame| {
			let update = &frame["params"]["update"];
			(
				update["sessionUpdate"]
					.as_str()
					.unwrap_or_default()
					.to_owned(),
				update["content"]["text"]
					.as_str()
					.unwrap_or_default()
					.to_owned(),
			)
		})
		.collect::<Vec<_>>();
	assert_eq!(replayed, [
		("user_message_chunk".to_owned(), "please refactor".to_owned()),
		("agent_thought_chunk".to_owned(), "Stream rule no-unwrap: avoid unwrap.".to_owned()),
		("agent_message_chunk".to_owned(), "refactored".to_owned()),
	]);
}

/// `session/list` pages stored journals (newest first, offset cursor,
/// `cwd` scoping) and
/// `session/fork` copies a stored session into a fresh one that becomes the
/// authority, with both capabilities advertised by `initialize`.
#[tokio::test]
async fn list_and_fork_expose_stored_sessions() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let sessions = directory.path().join("sessions");
	std::fs::create_dir_all(&sessions).expect("sessions directory");
	let source_path = sessions.join("source.oms");
	{
		let mut source =
			Session::create(&source_path, ComponentRegistry::standard()).expect("stored source");
		source.begin_turn().expect("turn");
		source
			.user("remember the fixture", Vec::new())
			.expect("stored prompt");
	}
	let (kernel, session, home) = harness(&directory, [Script::Text("forked reply")]);
	let frames = exchange(
		kernel,
		session,
		home,
		br#"{"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":1}}
{"jsonrpc":"2.0","id":"list","method":"session/list","params":{}}
{"jsonrpc":"2.0","id":"page","method":"session/list","params":{"cursor":"1"}}
{"jsonrpc":"2.0","id":"elsewhere","method":"session/list","params":{"cwd":"/nonexistent/elsewhere"}}
{"jsonrpc":"2.0","id":"badcursor","method":"session/list","params":{"cursor":"later"}}
{"jsonrpc":"2.0","id":"fork","method":"session/fork","params":{"sessionId":"source"}}
{"jsonrpc":"2.0","id":"prompt","method":"session/prompt","params":{"prompt":"in the fork"}}
{"jsonrpc":"2.0","id":"after","method":"session/list","params":{}}
{"jsonrpc":"2.0","id":"shutdown","method":"shutdown","params":{}}
"#,
	)
	.await;

	let capabilities =
		&response(&frames, "init")["result"]["agentCapabilities"]["sessionCapabilities"];
	assert!(capabilities.get("list").is_some() && capabilities.get("fork").is_some());

	let listed = response(&frames, "list")["result"].clone();
	let ids = |page: &Value| {
		page["sessions"]
			.as_array()
			.expect("sessions array")
			.iter()
			.map(|row| row["sessionId"].as_str().expect("id").to_owned())
			.collect::<Vec<_>>()
	};
	let mut all = ids(&listed);
	all.sort();
	assert_eq!(all, ["source", "startup"]);
	assert!(listed.get("nextCursor").is_none(), "two rows fit one page");
	let source_row = listed["sessions"]
		.as_array()
		.expect("sessions")
		.iter()
		.find(|row| row["sessionId"] == "source")
		.expect("source row");
	assert_eq!(source_row["title"], "remember the fixture");
	assert_eq!(source_row["_meta"]["messageCount"], 1);
	assert!(
		source_row["_meta"]["size"]
			.as_u64()
			.is_some_and(|size| size > 0)
	);
	assert!(
		source_row["updatedAt"]
			.as_str()
			.is_some_and(|stamp| stamp.ends_with('Z') && stamp.contains('T'))
	);

	assert_eq!(ids(&response(&frames, "page")["result"]).len(), 1, "cursor skips one row");
	assert!(ids(&response(&frames, "elsewhere")["result"]).is_empty(), "cwd scoping");
	assert_eq!(response(&frames, "badcursor")["error"]["code"], -32602);

	let fork_id = response(&frames, "fork")["result"]["sessionId"]
		.as_str()
		.expect("fork id")
		.to_owned();
	assert_ne!(fork_id, "source");
	let fork_path = sessions.join(format!("{fork_id}.oms"));
	assert!(fork_path.exists());
	let mut after = ids(&response(&frames, "after")["result"]);
	after.sort();
	let mut expected = vec!["source".to_owned(), "startup".to_owned(), fork_id.clone()];
	expected.sort();
	assert_eq!(after, expected);

	let fork = Session::open(&fork_path, ComponentRegistry::standard()).expect("fork reopens");
	let fork_snapshot = String::from_utf8_lossy(fork.dom().snapshot().as_bytes()).into_owned();
	assert!(fork_snapshot.contains("remember the fixture"), "the fork carries its source's history");
	assert!(fork_snapshot.contains("in the fork"), "the fork is the authority for new prompts");
	let source = Session::open(&source_path, ComponentRegistry::standard()).expect("source reopens");
	assert!(
		!String::from_utf8_lossy(source.dom().snapshot().as_bytes()).contains("in the fork"),
		"the source is untouched by prompts in the fork"
	);
}

/// An in-process hook host recording each session start (`session_start`,
/// with the switch that started it), each session end (`session_shutdown`),
/// and each switch it is told to follow, by journal file name.
#[derive(Default)]
struct SwitchRecorder {
	events: Mutex<Vec<String>>,
}

impl omp_agent::NativeHookHost for SwitchRecorder {
	fn decide<'a>(
		&'a self,
		event: omp_proto::toolhost::v1::HookEventId,
		payload: &'a Value,
	) -> omp_agent::BoxFut<'a, omp_agent::NativeReply> {
		let session = payload["session_id"].as_str().unwrap_or("?");
		match event {
			omp_proto::toolhost::v1::HookEventId::HookEventSessionShutdown => {
				self.events.lock().push(format!("end {session}"));
			},
			omp_proto::toolhost::v1::HookEventId::HookEventSessionStart => {
				let reason = payload["switch_reason"].as_str().unwrap_or("launch");
				self.events.lock().push(format!("start {session} {reason}"));
			},
			_ => {},
		}
		Box::pin(ready(omp_agent::NativeReply::defer()))
	}

	fn session_switched(&self, next: &omp_agent::SessionSwitched) {
		self
			.events
			.lock()
			.push(format!("switch {}", next.session_id));
	}
}

/// `session/close` ends the session once; a later switch away from it runs
/// no second end, but the in-process hook hosts still follow the switch, so
/// their hooks name the session the controller now serves. Every session the
/// controller serves starts on the lifecycle surface, the launch one and each
/// one a switch committed to.
#[tokio::test]
async fn a_switch_after_close_moves_the_hook_hosts_without_ending_twice_and_starts_each_session() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let sessions = directory.path().join("sessions");
	std::fs::create_dir_all(&sessions).expect("sessions directory");
	drop(
		Session::create(sessions.join("target.oms"), ComponentRegistry::standard()).expect("target"),
	);
	let (kernel, session, home) = harness(&directory, []);
	let (gate, _dispatches) = omp_agent::HookGate::channel();
	let gate = Arc::new(gate);
	let recorder = Arc::new(SwitchRecorder::default());
	gate.attach_native(Arc::clone(&recorder) as Arc<dyn omp_agent::NativeHookHost>, &[
		omp_proto::toolhost::v1::HookEventId::HookEventSessionShutdown,
		omp_proto::toolhost::v1::HookEventId::HookEventSessionStart,
	]);
	let kernel = kernel.with_hook_gate(gate);
	let frames = exchange(
		kernel,
		session,
		home,
		br#"{"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":1}}
{"jsonrpc":"2.0","id":"new","method":"session/new","params":{}}
{"jsonrpc":"2.0","id":"close","method":"session/close","params":{}}
{"jsonrpc":"2.0","id":"load","method":"session/load","params":{"sessionId":"target"}}
{"jsonrpc":"2.0","id":"shutdown","method":"shutdown","params":{}}
"#,
	)
	.await;
	let new_id = response(&frames, "new")["result"]["sessionId"]
		.as_str()
		.expect("new session id");
	assert!(response(&frames, "close").get("error").is_none(), "{frames:#?}");
	assert!(response(&frames, "load").get("error").is_none(), "{frames:#?}");
	assert_eq!(*recorder.events.lock(), [
		"start startup.oms launch".to_owned(),
		"end startup.oms".to_owned(),
		format!("switch {new_id}.oms"),
		format!("start {new_id}.oms new"),
		format!("end {new_id}.oms"),
		"switch target.oms".to_owned(),
		"start target.oms resume".to_owned(),
		"end target.oms".to_owned(),
	]);
}

/// Bound on every wait for a frame from the agent.
const WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// A scripted editor on the client end of one live ACP connection.
struct FakeEditor {
	write: tokio::io::WriteHalf<tokio::io::DuplexStream>,
	lines: tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
	/// Every frame the agent sent, in order.
	seen:  Vec<Value>,
	/// Canonical project root the connection serves.
	root:  std::path::PathBuf,
}

impl FakeEditor {
	async fn send(&mut self, frame: Value) {
		let mut bytes = serde_json::to_vec(&frame).expect("frame encodes");
		bytes.push(b'\n');
		self.write.write_all(&bytes).await.expect("frame sent");
	}

	/// The next frame from the agent that is not a `session/update`.
	async fn next(&mut self) -> Value {
		loop {
			let line = tokio::time::timeout(WAIT, self.lines.next_line())
				.await
				.expect("the agent sends a frame in time")
				.expect("frame read")
				.expect("the agent sends a frame before EOF");
			let frame: Value = serde_json::from_str(&line).expect("JSON frame");
			self.seen.push(frame.clone());
			if frame["method"] != "session/update" {
				return frame;
			}
		}
	}

	/// Sends a request; the agent's next frame must be its response.
	async fn call(&mut self, id: &str, method: &str, params: Value) -> Value {
		self
			.send(serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
			.await;
		let frame = self.next().await;
		assert_eq!(frame["id"], id, "expected the response to {method}, got {frame:#?}");
		frame
	}

	/// The agent's next frame must be a `method` request to the client.
	async fn request(&mut self, method: &str) -> Value {
		let frame = self.next().await;
		assert_eq!(frame["method"], method, "expected a {method} request, got {frame:#?}");
		assert!(frame["id"].is_u64(), "client requests carry numeric ids: {frame:#?}");
		frame
	}

	async fn answer(&mut self, request: &Value, result: Value) {
		self
			.send(serde_json::json!({"jsonrpc": "2.0", "id": request["id"].clone(), "result": result}))
			.await;
	}

	async fn initialize(&mut self, capabilities: Value) -> Value {
		self
			.call(
				"init",
				"initialize",
				serde_json::json!({"protocolVersion": 1, "clientCapabilities": capabilities}),
			)
			.await
	}

	/// `session/new` with the project root as `cwd`; returns the session id.
	async fn eligible_session(&mut self, id: &str) -> String {
		let cwd = self.root.to_str().expect("UTF-8 root").to_owned();
		let response = self
			.call(id, "session/new", serde_json::json!({"cwd": cwd}))
			.await;
		response["result"]["sessionId"]
			.as_str()
			.expect("new session id")
			.to_owned()
	}

	/// A round trip proving nothing else is queued: the agent's next frame is
	/// the answer to this probe.
	async fn quiet(&mut self, id: &str) {
		let response = self.call(id, "session/list", serde_json::json!({})).await;
		assert!(response.get("error").is_none(), "{response:#?}");
	}

	/// Ends the transport and returns every frame the agent sent.
	async fn close(mut self) -> Vec<Value> {
		self.write.shutdown().await.expect("request shutdown");
		while let Some(line) = tokio::time::timeout(WAIT, self.lines.next_line())
			.await
			.expect("the agent ends in time")
			.expect("frame read")
		{
			self
				.seen
				.push(serde_json::from_str(&line).expect("JSON frame"));
		}
		self.seen
	}
}

/// Serves one connection to a [`FakeEditor`] driven by `script`, which must
/// end by calling [`FakeEditor::close`].
async fn with_editor<F, Fut>(
	directory: &tempfile::TempDir,
	settings: AcpSettings,
	parts: (Kernel<ScriptedInference>, Session, SessionHome),
	script: F,
) where
	F: FnOnce(AcpClient, FakeEditor) -> Fut,
	Fut: Future<Output = ()>,
{
	serve_editor(directory.path(), AcpConnection::new(settings), parts, script).await;
}

/// Serves `connection` to a [`FakeEditor`] rooted at `root`, driven by
/// `script`, which must end by calling [`FakeEditor::close`].
async fn serve_editor<F, Fut>(
	root: &std::path::Path,
	connection: AcpConnection,
	(kernel, session, home): (Kernel<ScriptedInference>, Session, SessionHome),
	script: F,
) where
	F: FnOnce(AcpClient, FakeEditor) -> Fut,
	Fut: Future<Output = ()>,
{
	let client = connection.client();
	let (client_io, server_io) = tokio::io::duplex(64 * 1024);
	let (server_read, server_write) = tokio::io::split(server_io);
	let (client_read, client_write) = tokio::io::split(client_io);
	let editor = FakeEditor {
		write: client_write,
		lines: BufReader::new(client_read).lines(),
		seen:  Vec::new(),
		root:  std::fs::canonicalize(root).expect("canonical root"),
	};
	let server = tokio::spawn(connection.serve(kernel, session, home, server_read, server_write));
	tokio::time::timeout(std::time::Duration::from_secs(20), script(client, editor))
		.await
		.expect("the editor script must not deadlock");
	tokio::time::timeout(WAIT, server)
		.await
		.expect("the ACP server ends at EOF")
		.expect("ACP server task")
		.expect("ACP server");
}

fn fs_capabilities() -> Value {
	serde_json::json!({"fs": {"readTextFile": true, "writeTextFile": true}})
}

fn no_fs_or_terminal_request(frames: &[Value]) {
	for frame in frames {
		let method = frame["method"].as_str().unwrap_or_default();
		assert!(
			!method.starts_with("fs/") && !method.starts_with("terminal/"),
			"unexpected client request {frame:#?}"
		);
	}
}

/// `clientCapabilities` parse into typed capabilities on every `initialize`:
/// present fields are advertised, absent ones are not, and a malformed field is
/// treated as absent without failing `initialize`.
#[tokio::test]
async fn initialize_parses_client_capabilities_present_absent_and_malformed() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let parts = harness(&directory, []);
	with_editor(&directory, AcpSettings::default(), parts, |client, mut editor| async move {
		let full = editor
			.initialize(serde_json::json!({
				"fs": {"readTextFile": true, "writeTextFile": true},
				"terminal": true,
				"auth": {"terminal": true},
				"_meta": {"vendor": "x"},
			}))
			.await;
		assert_eq!(client.capabilities(), ClientCapabilities {
			fs:       FileSystemCapabilities { read_text_file: true, write_text_file: true },
			terminal: true,
			auth:     AuthCapabilities { terminal: true },
		});
		assert_eq!(full["result"]["authMethods"].as_array().map(Vec::len), Some(2));
		assert!(
			full["result"]["agentCapabilities"].get("fs").is_none()
				&& full["result"]["agentCapabilities"]
					.get("terminal")
					.is_none(),
			"omp advertises nothing new: {full:#?}"
		);

		let absent = editor.initialize(serde_json::json!({})).await;
		assert_eq!(client.capabilities(), ClientCapabilities::default());
		assert_eq!(absent["result"]["authMethods"].as_array().map(Vec::len), Some(1));

		let malformed = editor
			.initialize(serde_json::json!({
				"fs": {"readTextFile": "yes", "writeTextFile": true},
				"terminal": {"create": true},
				"auth": {"terminal": 1},
			}))
			.await;
		assert!(malformed.get("error").is_none(), "{malformed:#?}");
		assert_eq!(client.capabilities(), ClientCapabilities {
			fs:       FileSystemCapabilities { read_text_file: false, write_text_file: true },
			terminal: false,
			auth:     AuthCapabilities::default(),
		});
		assert_eq!(malformed["result"]["authMethods"].as_array().map(Vec::len), Some(1));

		let not_an_object = editor.initialize(serde_json::json!("everything")).await;
		assert!(not_an_object.get("error").is_none(), "{not_an_object:#?}");
		assert_eq!(client.capabilities(), ClientCapabilities::default());
		no_fs_or_terminal_request(&editor.close().await);
	})
	.await;
}

/// Editor file I/O needs the capability, an open session whose `cwd` matched
/// the project root, and a path inside that root; nothing that fails a gate
/// ever reaches the wire, and `terminal: true` alone sends nothing.
#[tokio::test]
async fn fs_requests_need_the_capability_an_eligible_session_and_a_project_path() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let parts = harness(&directory, []);
	with_editor(&directory, AcpSettings::default(), parts, |client, mut editor| async move {
		let path = editor.root.join("notes.md");
		editor
			.initialize(serde_json::json!({"terminal": true}))
			.await;
		assert!(!client.can_read() && !client.can_write());
		assert!(matches!(
			client.read_text_file(&path).await,
			Err(ClientRequestError::NotAdvertised { method: ClientMethod::ReadTextFile })
		));
		assert!(matches!(
			client.write_text_file(&path, "text").await,
			Err(ClientRequestError::NotAdvertised { method: ClientMethod::WriteTextFile })
		));

		editor.initialize(fs_capabilities()).await;
		assert!(
			matches!(client.read_text_file(&path).await, Err(ClientRequestError::SessionIneligible)),
			"the startup session never supplied a cwd"
		);
		editor
			.call("bare", "session/new", serde_json::json!({}))
			.await;
		assert!(
			matches!(client.read_text_file(&path).await, Err(ClientRequestError::SessionIneligible)),
			"a session created without a cwd is not eligible"
		);

		editor.eligible_session("new").await;
		assert!(client.can_read() && client.can_write());
		for outside in [
			std::path::PathBuf::from("relative.md"),
			editor.root.join("..").join("escape.md"),
			std::path::PathBuf::from("/definitely/elsewhere.md"),
		] {
			assert!(
				matches!(
					client.read_text_file(&outside).await,
					Err(ClientRequestError::OutsideProject { .. })
				),
				"{} must never be sent",
				outside.display()
			);
		}

		editor
			.call("close", "session/close", serde_json::json!({}))
			.await;
		assert!(!client.can_read());
		assert!(matches!(client.read_text_file(&path).await, Err(ClientRequestError::NoSession)));
		editor.quiet("probe").await;
		no_fs_or_terminal_request(&editor.close().await);
	})
	.await;
}

/// `sv_acp_fs off` disables editor file I/O even when the client advertises
/// it and the session is eligible.
#[tokio::test]
async fn sv_acp_fs_off_disables_editor_file_io() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let parts = harness(&directory, []);
	let settings = AcpSettings { fs: AcpFs::Off, ..AcpSettings::default() };
	with_editor(&directory, settings, parts, |client, mut editor| async move {
		editor.initialize(fs_capabilities()).await;
		editor.eligible_session("new").await;
		assert_eq!(client.settings().fs, AcpFs::Off);
		assert!(!client.can_read() && !client.can_write());
		let path = editor.root.join("notes.md");
		assert!(matches!(client.read_text_file(&path).await, Err(ClientRequestError::Disabled)));
		assert!(matches!(
			client.write_text_file(&path, "text").await,
			Err(ClientRequestError::Disabled)
		));
		editor.quiet("probe").await;
		no_fs_or_terminal_request(&editor.close().await);
	})
	.await;
}

/// Answers correlate by JSON-RPC id, whatever order they arrive in; a
/// response with an unknown id is dropped without an error frame; a
/// JSON-RPC error answer surfaces typed.
#[tokio::test]
async fn fs_answers_correlate_by_id_and_unknown_responses_are_dropped() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let parts = harness(&directory, []);
	with_editor(&directory, AcpSettings::default(), parts, |client, mut editor| async move {
		editor.initialize(fs_capabilities()).await;
		let session_id = editor.eligible_session("new").await;
		let (first_path, second_path) = (editor.root.join("a.txt"), editor.root.join("b.txt"));
		let first = tokio::spawn({
			let client = client.clone();
			let path = first_path.clone();
			async move { client.read_text_file(&path).await }
		});
		let first_request = editor.request("fs/read_text_file").await;
		let second = tokio::spawn({
			let client = client.clone();
			let path = second_path.clone();
			async move { client.read_text_file(&path).await }
		});
		let second_request = editor.request("fs/read_text_file").await;
		assert_ne!(first_request["id"], second_request["id"]);
		assert_eq!(first_request["params"], serde_json::json!({
			"sessionId": session_id,
			"path": first_path.to_str().expect("UTF-8 path"),
		}));
		assert_eq!(second_request["params"]["path"], second_path.to_str().expect("UTF-8 path"));
		assert_eq!(client.pending(), 2);

		editor
			.send(serde_json::json!({"jsonrpc": "2.0", "id": 9_999, "result": {"content": "stray"}}))
			.await;
		editor
			.send(serde_json::json!({"jsonrpc": "2.0", "id": "text-id", "error": {"code": 1, "message": "stray"}}))
			.await;
		editor
			.answer(&second_request, serde_json::json!({"content": "second buffer"}))
			.await;
		editor
			.answer(&first_request, serde_json::json!({"content": "first buffer"}))
			.await;
		assert_eq!(first.await.expect("first read").expect("first buffer").as_str(), "first buffer");
		assert_eq!(second.await.expect("second read").expect("second buffer").as_str(), "second buffer");
		assert_eq!(client.pending(), 0);
		editor.quiet("probe").await;

		let write = tokio::spawn({
			let client = client.clone();
			let path = first_path.clone();
			async move { client.write_text_file(&path, "agent text").await }
		});
		let write_request = editor.request("fs/write_text_file").await;
		assert_eq!(write_request["params"]["content"], "agent text");
		assert_eq!(write_request["params"]["sessionId"], session_id.as_str());
		editor.answer(&write_request, serde_json::json!(null)).await;
		write.await.expect("write").expect("written");

		let refused = tokio::spawn({
			let client = client.clone();
			async move { client.read_text_file(&first_path).await }
		});
		let refused_request = editor.request("fs/read_text_file").await;
		editor
			.send(serde_json::json!({
				"jsonrpc": "2.0",
				"id": refused_request["id"].clone(),
				"error": {"code": -32002, "message": "no such buffer"},
			}))
			.await;
		assert!(matches!(
			refused.await.expect("refused read"),
			Err(ClientRequestError::Refused { method: ClientMethod::ReadTextFile, code: -32002, .. })
		));
		let malformed = tokio::spawn({
			let client = client.clone();
			async move { client.read_text_file(&second_path).await }
		});
		let malformed_request = editor.request("fs/read_text_file").await;
		editor
			.answer(&malformed_request, serde_json::json!({"text": "wrong field"}))
			.await;
		assert!(matches!(
			malformed.await.expect("malformed read"),
			Err(ClientRequestError::Malformed { method: ClientMethod::ReadTextFile, .. })
		));
		let frames = editor.close().await;
		assert!(
			frames.iter().all(|frame| frame.get("error").is_none()),
			"stray responses must be dropped, not answered: {frames:#?}"
		);
	})
	.await;
}

/// An `fs/*` request the editor never answers fails at `sv_acp_fs_timeout`,
/// and its late answer is dropped.
#[tokio::test]
async fn unanswered_fs_request_times_out_and_its_late_answer_is_dropped() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let parts = harness(&directory, []);
	let timeout = std::time::Duration::from_millis(150);
	let settings = AcpSettings { fs_timeout: timeout, ..AcpSettings::default() };
	with_editor(&directory, settings, parts, |client, mut editor| async move {
		editor.initialize(fs_capabilities()).await;
		editor.eligible_session("new").await;
		let path = editor.root.join("slow.txt");
		let started = tokio::time::Instant::now();
		let read = tokio::spawn({
			let client = client.clone();
			async move { client.read_text_file(&path).await }
		});
		let request = editor.request("fs/read_text_file").await;
		let outcome = read.await.expect("read task");
		let elapsed = started.elapsed();
		assert!(
			matches!(outcome, Err(ClientRequestError::Timeout { method: ClientMethod::ReadTextFile, timeout: t }) if t == timeout),
			"{outcome:?}"
		);
		assert!(elapsed >= timeout && elapsed < WAIT, "timed out after {elapsed:?}");
		assert_eq!(client.pending(), 0, "the timed-out id is retired");
		editor
			.answer(&request, serde_json::json!({"content": "too late"}))
			.await;
		editor.quiet("probe").await;
		let frames = editor.close().await;
		assert!(frames.iter().all(|frame| frame.get("error").is_none()), "{frames:#?}");
	})
	.await;
}

/// A cancelled caller, a session switch, `session/close`, and EOF each retire
/// pending `fs/*` requests; later requests name the live session only.
#[tokio::test]
async fn cancellation_switch_close_and_eof_retire_pending_fs_requests() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let parts = harness(&directory, []);
	with_editor(&directory, AcpSettings::default(), parts, |client, mut editor| async move {
		editor.initialize(fs_capabilities()).await;
		let first_session = editor.eligible_session("new").await;
		let path = editor.root.join("file.txt");
		let spawn_read = |client: &AcpClient| {
			let client = client.clone();
			let path = path.clone();
			tokio::spawn(async move { client.read_text_file(&path).await })
		};

		let dropped = spawn_read(&client);
		let dropped_request = editor.request("fs/read_text_file").await;
		dropped.abort();
		assert!(dropped.await.expect_err("aborted").is_cancelled());
		assert_eq!(client.pending(), 0, "a dropped caller retires its id");
		editor
			.answer(&dropped_request, serde_json::json!({"content": "orphan"}))
			.await;
		editor.quiet("after-drop").await;

		let switched = spawn_read(&client);
		let switched_request = editor.request("fs/read_text_file").await;
		assert_eq!(switched_request["params"]["sessionId"], first_session.as_str());
		let second_session = editor.eligible_session("switch").await;
		assert_ne!(second_session, first_session);
		assert!(matches!(
			switched.await.expect("switched read"),
			Err(ClientRequestError::SessionEnded { method: ClientMethod::ReadTextFile })
		));
		editor
			.answer(&switched_request, serde_json::json!({"content": "old session"}))
			.await;
		let current = spawn_read(&client);
		let current_request = editor.request("fs/read_text_file").await;
		assert_eq!(current_request["params"]["sessionId"], second_session.as_str());
		editor
			.answer(&current_request, serde_json::json!({"content": "live"}))
			.await;
		assert_eq!(
			current
				.await
				.expect("current read")
				.expect("live buffer")
				.as_str(),
			"live"
		);

		let closing = spawn_read(&client);
		editor.request("fs/read_text_file").await;
		editor
			.call("close", "session/close", serde_json::json!({}))
			.await;
		assert!(matches!(
			closing.await.expect("closing read"),
			Err(ClientRequestError::SessionEnded { method: ClientMethod::ReadTextFile })
		));

		let third_session = editor.eligible_session("reopen").await;
		let orphaned = spawn_read(&client);
		let orphaned_request = editor.request("fs/read_text_file").await;
		assert_eq!(orphaned_request["params"]["sessionId"], third_session.as_str());
		let frames = editor.close().await;
		assert!(matches!(
			orphaned.await.expect("orphaned read"),
			Err(ClientRequestError::Disconnected { method: ClientMethod::ReadTextFile })
		));
		assert!(matches!(
			client.read_text_file(&path).await,
			Err(ClientRequestError::Disconnected { method: ClientMethod::ReadTextFile })
		));
		assert_eq!(client.pending(), 0);
		assert!(frames.iter().all(|frame| frame.get("error").is_none()), "{frames:#?}");
	})
	.await;
}

/// A tool that asks the kernel's approval route before acting.
struct GatedTool {
	spec:  omp_tool::ToolSpec,
	route: Arc<Mutex<Option<omp_agent::ApprovalRoute>>>,
}

impl omp_tool::Tool for GatedTool {
	type Fault = Value;
	type Params = Value;
	type Payload = Value;
	type Update = Value;

	fn spec(&self) -> &omp_tool::ToolSpec {
		&self.spec
	}

	fn call<'c>(
		&'c self,
		mut params: omp_tool::IncomingParams<'c>,
	) -> impl futures::Stream<Item = omp_tool::Ev<Self::Update, Self::Payload, Self::Fault>> + Send + 'c
	{
		async_stream::stream! {
			let args = params.whole::<Value>().await.expect("args");
			let command = args["command"].as_str().unwrap_or_default().to_owned();
			let route = self.route.lock().clone().expect("route bound before the turn");
			let spec = omp_agent::ApprovalSpec {
				title:         Str::new_static("Run bash"),
				body:          Str::new(format!("$ {command}")),
				subject:       Str::new(&command),
				kind:          Str::new_static("exec"),
				scopes:        vec![Str::new_static("once"), Str::new_static("session")],
				default:       Some(false),
				route:         Str::new_static("user"),
				approver:      None,
				timeout_ms:    0,
				unreachable:   Str::new_static("deny"),
				require_human: true,
				pattern:       None,
				evidence:      Vec::new(),
			};
			let ticket = route.request(Some(Str::new_static("gated-1")), vec![spec], 1).await;
			let approved = ticket.decision.is_some_and(|decision| decision.approved);
			yield omp_tool::Ev::Done(omp_tool::ToolTerminal::Done {
				result: if approved {
					Ok(serde_json::json!({"gated": "approved"}))
				} else {
					Err(serde_json::json!({"gated": "rejected"}))
				},
				useless: false,
			});
		}
	}

	fn prompt(&self, view: Result<&Value, &Value>, _: &omp_tool::PromptCaps) -> Vec<omp_tool::Part> {
		vec![omp_tool::Part::Json {
			json: bytes::Bytes::from(
				serde_json::to_vec(view.unwrap_or_else(|fault| fault)).expect("JSON"),
			),
		}]
	}
}

fn gated_registry(route: Arc<Mutex<Option<omp_agent::ApprovalRoute>>>) -> Arc<Registry> {
	let mut registry = Registry::new();
	registry
		.register(
			GatedTool {
				spec: omp_tool::ToolSpec {
					name: Str::new_static("gated"),
					rev: omp_tool::Rev { family: Str::new_static("test"), n: 1 },
					description: Str::new_static("asks before acting"),
					schema: bytes::Bytes::from_static(
						br#"{"type":"object","properties":{"command":{"type":"string"}},"required":["command"],"additionalProperties":false}"#,
					),
					constraint: omp_tool::Constraint::None,
					effects: omp_tool::Effects::empty(),
					projection_code: [9; 32],
				},
				route,
			},
			omp_tool::Presentation::Slot,
			omp_tool::Claims {
				precedence: omp_tool::Precedence::CORE,
				claimant:   Str::new_static("omp/core"),
				replaces:   None,
			},
		)
		.expect("gated tool registers");
	Arc::new(registry)
}

/// `session/request_permission` and `fs/*` share one id space and one table:
/// each answer reaches the request it names, and the selected permission
/// option still decides the kernel's approval ticket.
#[tokio::test]
async fn permission_and_fs_requests_share_one_correlation_table() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let route = Arc::new(Mutex::new(None));
	let (kernel, session, home) = harness_with(
		&directory,
		[Script::GatedCall("echo gated"), Script::Text("done")],
		gated_registry(Arc::clone(&route)),
	);
	*route.lock() = Some(kernel.approval_route());
	with_editor(
		&directory,
		AcpSettings::default(),
		(kernel, session, home),
		|client, mut editor| async move {
			editor.initialize(fs_capabilities()).await;
			let session_id = editor.eligible_session("new").await;
			editor
				.send(serde_json::json!({
					"jsonrpc": "2.0",
					"id": "prompt",
					"method": "session/prompt",
					"params": {"sessionId": session_id, "prompt": "run the gated tool"},
				}))
				.await;
			let permission = editor.request("session/request_permission").await;
			assert_eq!(permission["params"]["sessionId"], session_id.as_str());
			assert_eq!(permission["params"]["toolCall"]["kind"], "execute");
			assert_eq!(permission["params"]["options"][0]["optionId"], "allow_once");

			let path = editor.root.join("open.rs");
			let read = tokio::spawn({
				let client = client.clone();
				async move { client.read_text_file(&path).await }
			});
			let read_request = editor.request("fs/read_text_file").await;
			assert_ne!(read_request["id"], permission["id"], "one id space for every client request");
			assert_eq!(client.pending(), 2);

			editor
				.answer(&read_request, serde_json::json!({"content": "editor buffer"}))
				.await;
			assert_eq!(read.await.expect("read").expect("buffer").as_str(), "editor buffer");
			editor
				.answer(
					&permission,
					serde_json::json!({"outcome": {"outcome": "selected", "optionId": "allow_once"}}),
				)
				.await;
			let prompt = editor.next().await;
			assert_eq!(prompt["id"], "prompt", "{prompt:#?}");
			assert_eq!(prompt["result"]["stopReason"], "end_turn", "{prompt:#?}");
			assert_eq!(client.pending(), 0);
			let frames = editor.close().await;
			let transcript = serde_json::to_string(&frames).expect("frames encode");
			assert!(
				transcript.contains("approved") && !transcript.contains("rejected"),
				"the selected option approves the ticket: {frames:#?}"
			);
		},
	)
	.await;
}

/// Blank lines are skipped, an unparseable line gets a parse error, and a
/// frame that is neither a request nor a response is invalid; the connection
/// keeps serving through all of them.
#[tokio::test]
async fn blank_and_malformed_frames_keep_the_connection_serving() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let parts = harness(&directory, []);
	with_editor(&directory, AcpSettings::default(), parts, |_client, mut editor| async move {
		editor
			.write
			.write_all(b"\n   \r\n{not json\n")
			.await
			.expect("junk sent");
		let parse_error = editor.next().await;
		assert_eq!(parse_error["error"]["code"], -32700, "{parse_error:#?}");
		editor.initialize(serde_json::json!({})).await;
		editor
			.send(serde_json::json!({"jsonrpc": "2.0", "id": "noop"}))
			.await;
		let no_method = editor.next().await;
		assert_eq!(no_method["id"], "noop");
		assert_eq!(no_method["error"]["code"], -32600, "{no_method:#?}");
		editor.quiet("probe").await;
		editor.close().await;
	})
	.await;
}

/// Records every editor binding the connection makes into the environment.
#[derive(Default)]
struct RecordingDocuments {
	binds: Mutex<Vec<Option<Arc<dyn AcpDocumentBackend>>>>,
}

impl EditorDocumentsHost for RecordingDocuments {
	fn bind(&self, editor: Option<Arc<dyn AcpDocumentBackend>>) {
		self.binds.lock().push(editor);
	}
}

impl RecordingDocuments {
	/// Whether each bind so far bound an editor (`true`) or unbound it.
	fn bound(&self) -> Vec<bool> {
		self.binds.lock().iter().map(Option::is_some).collect()
	}

	/// The editor of the latest bind.
	fn latest(&self) -> Arc<dyn AcpDocumentBackend> {
		self
			.binds
			.lock()
			.last()
			.cloned()
			.flatten()
			.expect("the latest bind bound an editor")
	}
}

/// The editor is bound into the environment as the document base only while
/// the live session is eligible (ADR 0037 §1.2, §2): the capability is
/// advertised and the session was made live with a matching `cwd`. Every
/// switch rebinds (a fresh, unanchored binding); a withdrawn capability,
/// `session/close` and EOF unbind. The bound editor reads through the gated
/// `fs/read_text_file` of the live session, and never sends a path outside the
/// project root.
#[tokio::test]
async fn the_editor_is_bound_as_the_document_base_only_while_the_session_is_eligible() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let parts = harness(&directory, []);
	let documents = Arc::new(RecordingDocuments::default());
	let settings =
		AcpSettings { fs_timeout: std::time::Duration::from_secs(3), ..AcpSettings::default() };
	let connection = AcpConnection::new(settings);
	connection.bind_documents(Arc::clone(&documents) as Arc<dyn EditorDocumentsHost>);
	let recorded = Arc::clone(&documents);
	serve_editor(directory.path(), connection, parts, |_client, mut editor| async move {
		editor
			.initialize(serde_json::json!({"terminal": true}))
			.await;
		editor.initialize(fs_capabilities()).await;
		editor
			.call("bare", "session/new", serde_json::json!({}))
			.await;
		assert!(recorded.bound().is_empty(), "no eligible session yet: {:?}", recorded.bound());

		let first = editor.eligible_session("first").await;
		assert_eq!(recorded.bound(), [true]);
		let backend = recorded.latest();
		assert_eq!(backend.deadline(), std::time::Duration::from_secs(3), "sv_acp_fs_timeout");
		let path = editor.root.join("notes.md");
		let wire_path = Str::new(path.to_str().expect("UTF-8 path"));
		let read = tokio::spawn({
			let backend = Arc::clone(&backend);
			let wire_path = wire_path.clone();
			async move { backend.read_text(wire_path).await }
		});
		let request = editor.request("fs/read_text_file").await;
		assert_eq!(
			request["params"],
			serde_json::json!({"sessionId": first, "path": wire_path.as_str()})
		);
		editor
			.answer(&request, serde_json::json!({"content": "unsaved buffer"}))
			.await;
		assert_eq!(read.await.expect("read task").expect("buffer").as_str(), "unsaved buffer");
		assert_eq!(
			backend
				.read_text(Str::new_static("/definitely/elsewhere.md"))
				.await
				.expect_err("outside the project"),
			EditorIoError::Unavailable,
			"a path outside the project root is never sent"
		);

		editor.eligible_session("second").await;
		assert_eq!(recorded.bound(), [true, true], "a switch rebinds, unanchored");
		editor.initialize(serde_json::json!({})).await;
		assert_eq!(recorded.bound(), [true, true, false], "a withdrawn capability unbinds");
		editor.initialize(fs_capabilities()).await;
		assert_eq!(recorded.bound(), [true, true, false, true]);
		editor
			.call("close", "session/close", serde_json::json!({}))
			.await;
		assert_eq!(recorded.bound(), [true, true, false, true, false], "session/close unbinds");
		assert_eq!(
			backend.read_text(wire_path).await.expect_err("closed"),
			EditorIoError::Unavailable,
			"an editor kept past its binding reaches nothing"
		);
		editor.eligible_session("third").await;
		editor.quiet("probe").await;
		let frames = editor.close().await;
		assert_eq!(recorded.bound(), [true, true, false, true, false, true, false], "EOF unbinds");
		assert_eq!(
			frames
				.iter()
				.filter(|frame| frame["method"] == "fs/read_text_file")
				.count(),
			1,
			"only the eligible read reached the editor"
		);
	})
	.await;
}

/// Without the capability, or with `sv_acp_fs off`, no editor is ever bound,
/// even for an eligible session.
#[tokio::test]
async fn no_capability_or_sv_acp_fs_off_never_binds_the_editor() {
	for (settings, capabilities) in [
		(AcpSettings::default(), serde_json::json!({"terminal": true})),
		(AcpSettings { fs: AcpFs::Off, ..AcpSettings::default() }, fs_capabilities()),
	] {
		let directory = tempfile::tempdir().expect("temporary directory");
		let parts = harness(&directory, []);
		let documents = Arc::new(RecordingDocuments::default());
		let connection = AcpConnection::new(settings);
		connection.bind_documents(Arc::clone(&documents) as Arc<dyn EditorDocumentsHost>);
		let recorded = Arc::clone(&documents);
		serve_editor(directory.path(), connection, parts, |_client, mut editor| async move {
			editor.initialize(capabilities).await;
			editor.eligible_session("new").await;
			editor.quiet("probe").await;
			no_fs_or_terminal_request(&editor.close().await);
			assert!(recorded.bound().is_empty(), "{:?}", recorded.bound());
		})
		.await;
	}
}

/// An editor advertising only `fs.writeTextFile` is bound for write-backs: its
/// buffers are never read, and committed bytes are written as they are.
#[tokio::test]
async fn a_write_only_editor_is_bound_for_write_backs_without_reads() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let parts = harness(&directory, []);
	let documents = Arc::new(RecordingDocuments::default());
	let connection = AcpConnection::new(AcpSettings::default());
	connection.bind_documents(Arc::clone(&documents) as Arc<dyn EditorDocumentsHost>);
	let recorded = Arc::clone(&documents);
	serve_editor(directory.path(), connection, parts, |_client, mut editor| async move {
		editor
			.initialize(serde_json::json!({"fs": {"writeTextFile": true}}))
			.await;
		let session = editor.eligible_session("new").await;
		assert_eq!(recorded.bound(), [true]);
		let backend = recorded.latest();
		assert_eq!(backend.capabilities(), EditorCapabilities { read: false, write: true });
		let path = Str::new(editor.root.join("w.txt").to_str().expect("UTF-8 path"));
		assert_eq!(
			backend.read_text(path.clone()).await.expect_err("no reads"),
			EditorIoError::Unavailable
		);
		let write_back = tokio::spawn({
			let backend = Arc::clone(&backend);
			async move {
				backend
					.write_back(WriteBack {
						path,
						content: Str::new_static("committed\n"),
						base: Some(Str::new_static("before\n")),
					})
					.await
			}
		});
		let write = editor.request("fs/write_text_file").await;
		assert_eq!(write["params"]["sessionId"], session.as_str());
		assert_eq!(write["params"]["content"], "committed\n");
		editor.answer(&write, serde_json::json!(null)).await;
		assert_eq!(
			write_back.await.expect("write-back task"),
			Ok(WriteBackOutcome::Written { merged: None, read_back: None })
		);
		editor.quiet("probe").await;
		let frames = editor.close().await;
		assert!(
			frames
				.iter()
				.all(|frame| frame["method"] != "fs/read_text_file"),
			"{frames:#?}"
		);
	})
	.await;
}

/// Write-backs still queued when the session switches or closes are drained
/// first, bounded by `sv_acp_fs_timeout` (ADR 0037 §4.6): their requests name
/// the session they were made in, the controller keeps routing the editor's
/// answers meanwhile, and a request that arrives during the drain is answered
/// after the switch, in order. EOF drops what is left.
#[tokio::test]
async fn queued_write_backs_drain_before_a_switch_or_close_and_drop_at_eof() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let parts = harness(&directory, []);
	let documents = Arc::new(RecordingDocuments::default());
	let timeout = std::time::Duration::from_millis(300);
	let connection =
		AcpConnection::new(AcpSettings { fs_timeout: timeout, ..AcpSettings::default() });
	connection.bind_documents(Arc::clone(&documents) as Arc<dyn EditorDocumentsHost>);
	let recorded = Arc::clone(&documents);
	serve_editor(directory.path(), connection, parts, |client, mut editor| async move {
		editor.initialize(fs_capabilities()).await;
		let first = editor.eligible_session("first").await;
		let path = Str::new(editor.root.join("a.txt").to_str().expect("UTF-8 path"));
		let write_back = |backend: Arc<dyn AcpDocumentBackend>| {
			let path = path.clone();
			tokio::spawn(async move {
				backend
					.write_back(WriteBack { path, content: Str::new_static("committed\n"), base: None })
					.await
			})
		};

		let drained = write_back(recorded.latest());
		let write = editor.request("fs/write_text_file").await;
		assert_eq!(write["params"]["sessionId"], first.as_str());
		assert_eq!(client.write_backs_outstanding(), 1);
		let cwd = editor.root.to_str().expect("UTF-8 root").to_owned();
		editor
			.send(serde_json::json!({
				"jsonrpc": "2.0", "id": "switch", "method": "session/new", "params": {"cwd": cwd},
			}))
			.await;
		editor
			.send(serde_json::json!({
				"jsonrpc": "2.0", "id": "listed", "method": "session/list", "params": {},
			}))
			.await;
		editor.answer(&write, serde_json::json!(null)).await;
		let read_back = editor.request("fs/read_text_file").await;
		assert_eq!(read_back["params"]["sessionId"], first.as_str(), "drained in its own session");
		editor
			.answer(&read_back, serde_json::json!({"content": "committed\n"}))
			.await;
		let switched = editor.next().await;
		assert_eq!(switched["id"], "switch", "the switch waited for the drain: {switched:#?}");
		let second = switched["result"]["sessionId"]
			.as_str()
			.expect("new session id")
			.to_owned();
		assert_ne!(second, first);
		let listed = editor.next().await;
		assert_eq!(listed["id"], "listed", "the deferred request is answered after: {listed:#?}");
		assert_eq!(
			drained.await.expect("write-back task"),
			Ok(WriteBackOutcome::Written { merged: None, read_back: None })
		);
		assert_eq!(client.write_backs_outstanding(), 0);

		// An editor that never answers holds `session/close` for at most the
		// bound.
		let unanswered = write_back(recorded.latest());
		let write = editor.request("fs/write_text_file").await;
		assert_eq!(write["params"]["sessionId"], second.as_str());
		let started = tokio::time::Instant::now();
		let closed = editor
			.call("close", "session/close", serde_json::json!({}))
			.await;
		assert!(closed.get("error").is_none(), "{closed:#?}");
		assert!(started.elapsed() < WAIT, "the drain is bounded");
		assert_eq!(
			unanswered.await.expect("write-back task"),
			Err(EditorIoError::Timeout),
			"the unanswered write timed out"
		);

		// EOF drops the queue: a write-back in flight fails at once.
		editor.eligible_session("third").await;
		let dropped = write_back(recorded.latest());
		editor.request("fs/write_text_file").await;
		editor.close().await;
		assert_eq!(dropped.await.expect("write-back task"), Err(EditorIoError::Disconnected));
	})
	.await;
}

/// Joined proof over a real project environment and document authority: in an
/// eligible ACP session, the Read tool returns the editor's unsaved buffer
/// (asked for through `fs/read_text_file` by the environment), the Write tool
/// commits through the authority to disk, and only then is the committed
/// content written back with `fs/write_text_file` (ADR 0037 §4.3): after the
/// commit, before the write tool settles.
#[tokio::test]
async fn an_acp_session_reads_the_editor_buffer_and_writes_back_after_the_commit() {
	let directory = tempfile::tempdir().expect("temporary directory");
	let root = directory.path().join("workspace");
	let state = directory.path().join("state");
	let sessions_dir = directory.path().join("sessions");
	for path in [&root, &state, &sessions_dir] {
		std::fs::create_dir_all(path).expect("directory");
	}
	let root = std::fs::canonicalize(&root).expect("canonical workspace");
	std::fs::write(root.join("notes.txt"), "on disk\n").expect("fixture");
	let con = Arc::new(omp_con::Ctx::new());
	let environment = omp_envd::ProjectEnvironment::attach(&root, &state, omp_envd::AttachOptions {
		py_eval:            false,
		approval_mode:      None,
		trusted_extensions: Vec::new(),
		contributed_values: Vec::new(),
		con:                Arc::clone(&con),
		bridges:            omp_envd::RegistryBridges::default(),
		spawn_idle_timeout: Some(2),
	})
	.await
	.expect("environment");
	let spill = BlobStore::open(directory.path().join("blobs")).expect("blob store");
	let kernel = Kernel::new(
		ScriptedInference {
			scripts: Mutex::new(
				[
					Script::Call("read", r#"{"path":"notes.txt"}"#),
					Script::Call("write", r#"{"path":"notes.txt","content":"agent wrote\n"}"#),
					Script::Text("done"),
				]
				.into_iter()
				.collect(),
			),
		},
		environment.registry(),
		DispatchPolicy::new(spill.clone()),
		StaticPrompt(Str::new_static("system")),
	);
	let approvals = kernel.approval_route();
	environment.bind_approval_authority(
		Some(Arc::new(omp_agent::ApprovalBook::new())),
		Some(approvals.clone()),
	);
	let kernel = kernel
		.with_external_executor(Arc::new(omp_driver::headless::kernel::EnvToolExecutor::new(
			environment.client().clone(),
			approvals,
		)))
		.with_tool_admission(Arc::new(omp_driver::headless::kernel::SettingsAdmission::new(
			&con, None, &root,
		)));
	let home = SessionHome {
		sessions_dir:  sessions_dir.clone(),
		project_root:  root.clone(),
		model:         Str::new_static("scripted/test"),
		prompt:        Default::default(),
		facts:         Default::default(),
		live:          Arc::new(SessionRegistry::new()),
		tools_enabled: true,
		up:            kernel.mailbox(),
		rules:         None,
	};
	let session = Session::create_with_blob_store(
		sessions_dir.join("startup.oms"),
		ComponentRegistry::standard(),
		spill,
	)
	.expect("startup session");
	let connection = AcpConnection::new(AcpSettings::default());
	connection.bind_documents(Arc::new(environment.editor_documents()));
	let disk = root.join("notes.txt");
	serve_editor(&root, connection, (kernel, session, home), |_client, mut editor| async move {
		editor.initialize(fs_capabilities()).await;
		let session_id = editor.eligible_session("new").await;
		editor
			.send(serde_json::json!({
				"jsonrpc": "2.0",
				"id": "prompt",
				"method": "session/prompt",
				"params": {"sessionId": session_id, "prompt": "read, then rewrite the notes"},
			}))
			.await;
		let mut buffer = String::from("on disk\nUNSAVED EDITOR LINE\n");
		let mut reads = Vec::new();
		let mut writes = Vec::new();
		let response = loop {
			let frame = editor.next().await;
			match frame["method"].as_str() {
				Some("fs/read_text_file") => {
					reads.push(frame["params"].clone());
					editor
						.answer(&frame, serde_json::json!({"content": buffer.clone()}))
						.await;
				},
				Some("session/request_permission") => {
					editor
						.answer(
							&frame,
							serde_json::json!({"outcome": {"outcome": "selected", "optionId": "allow_once"}}),
						)
						.await;
				},
				Some("fs/write_text_file") => {
					assert_eq!(
						std::fs::read_to_string(&disk).expect("disk"),
						"agent wrote\n",
						"the commit is durable before the editor is written"
					);
					writes.push(frame["params"].clone());
					frame["params"]["content"]
						.as_str()
						.expect("text content")
						.clone_into(&mut buffer);
					editor.answer(&frame, serde_json::json!(null)).await;
				},
				_ if frame["id"] == "prompt" => break frame,
				_ => panic!("unexpected frame {frame:#?}"),
			}
		};
		assert_eq!(response["result"]["stopReason"], "end_turn", "{response:#?}");
		assert!(
			!reads.is_empty(),
			"the environment asked the editor for its buffer: {:#?}",
			editor.seen
		);
		for read in &reads {
			assert_eq!(read["sessionId"], session_id.as_str());
			assert_eq!(read["path"], disk.to_str().expect("UTF-8 path"));
		}
		assert_eq!(writes, [serde_json::json!({
			"sessionId": session_id,
			"path": disk.to_str().expect("UTF-8 path"),
			"content": "agent wrote\n",
		})]);
		let frames = editor.close().await;
		let transcript = serde_json::to_string(&frames).expect("frames encode");
		assert!(
			transcript.contains("UNSAVED EDITOR LINE"),
			"the Read tool returned the editor's buffer: {frames:#?}"
		);
		assert!(!transcript.contains("editor_sync"), "a clean write-back adds no notice");
		assert_eq!(
			std::fs::read_to_string(&disk).expect("disk"),
			"agent wrote\n",
			"the Write committed through the authority"
		);
		// Commit → write-back → settle: the write tool's call completes only
		// after the editor was written.
		let announced = frames
			.iter()
			.position(|frame| {
				let update = &frame["params"]["update"];
				update["sessionUpdate"] == "tool_call"
					&& update["rawInput"]["content"] == "agent wrote\n"
			})
			.expect("the write tool call is announced");
		let after = |predicate: &dyn Fn(&Value) -> bool| {
			frames[announced..]
				.iter()
				.position(predicate)
				.map(|index| announced + index)
				.expect("the frame was sent")
		};
		let written = after(&|frame| frame["method"] == "fs/write_text_file");
		let settled = after(&|frame| {
			let update = &frame["params"]["update"];
			update["sessionUpdate"] == "tool_call_update" && update["status"] == "completed"
		});
		assert!(written < settled, "the write tool settles after its write-back: {frames:#?}");
	})
	.await;
	drop(environment);
}
