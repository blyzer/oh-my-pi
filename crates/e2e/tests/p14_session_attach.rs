//! P14: a read-only observer converges with a live session over the owner
//! socket, and cannot steer it.
//!
//! Real components end to end: a `.oms` session folded into a DOM, the
//! production kernel (only provider output is scripted), the driver's
//! `SessionWire` and `SessionBridge`, the owner Unix socket with its
//! permissions and its identity interceptor, and the generated gRPC client.
//!
//! The proof asserts that an attachment converges on the host's tree from a
//! snapshot plus ordered events; that a detached observer resumes from its
//! cursor and is replayed rather than resent a snapshot; that a cursor from
//! another epoch is answered with a resync and a fresh snapshot; that a
//! restarted host mints a new epoch and resyncs the observer that outlived
//! it; that a caller without `observe_host_local` never sees a host-local
//! property; and that every mutating verb is refused.

use std::{path::Path, sync::Arc, time::Duration};

use omp_agent::{DispatchPolicy, Kernel, RunControl, StaticPrompt, TurnInput, Up};
use omp_ai::{BlockKind, ChatEvent, Completion, ExecutionReceipt, FinishReason, Usage};
use omp_core::Str;
use omp_dom::{Dom, PropId, Value};
use omp_driver::{
	SessionWire,
	control_visibility::ControlVisibility,
	session_bridge::BridgeLimits,
	sessions::{IrcRelayPolicy, KernelHandle, SessionId, SessionRegistry},
};
use omp_e2e::{
	Context as _, Result,
	support::{ScriptedInference, create_session, within},
};
use omp_journal::blob::BlobStore;
use omp_proto::session::v1 as pb;
use omp_serve::session::{ControlContext, ControlScope, ScopeSet, SessionControlAuthority};
use omp_session::Session;
use omp_tool::Registry;
use parking_lot::RwLock;
use tonic::transport::Channel;

const BOUND: Duration = Duration::from_secs(30);
const FIRST_QUESTION: &str = "what the observer must already see";
const FIRST_ANSWER: &str = "history that precedes the attachment";
const SECOND_QUESTION: &str = "what the observer sees arrive live";
const SECOND_ANSWER: &str = "streamed after the observer attached";
const THIRD_QUESTION: &str = "what lands while the observer is away";
const THIRD_ANSWER: &str = "replayed from the ring on resume";
/// The host-local properties the scripted kernel journals on every assistant
/// node, exactly as they appear in the DOM's canonical bytes.
const HOST_LOCAL_PROVIDER: &str = r#"["provider","scripted"]"#;
const HOST_LOCAL_ROUTE: &str = r#"["route","scripted/e2e"]"#;
/// The same two properties once the table has blanked them.
const BLANKED_PROVIDER: &str = r#"["provider",""]"#;
const BLANKED_ROUTE: &str = r#"["route",""]"#;

fn reply(text: &'static str) -> Vec<ChatEvent> {
	vec![
		ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text },
		ChatEvent::TextDelta { index: 0, text: Str::new_static(text) },
		ChatEvent::Completed(Completion {
			reason:  FinishReason::Stop,
			blocks:  1,
			usage:   Usage::default(),
			receipt: ExecutionReceipt::default().into(),
		}),
	]
}

/// The observer's local replica of the host's tree.
#[derive(Default)]
struct Replica {
	dom:             Option<Dom>,
	partial:         Vec<u8>,
	/// Cursor of the last event applied, for a later resume.
	cursor:          Option<(u64, u64)>,
	/// Every resync reason received, in order.
	resyncs:         Vec<String>,
	/// Whether this attachment was sent a snapshot at all.
	snapshot_chunks: usize,
}

impl Replica {
	/// Folds one frame exactly as `omp attach` does.
	fn fold(&mut self, frame: pb::attach_frame::Frame) -> Result<()> {
		match frame {
			pb::attach_frame::Frame::Welcome(welcome) => {
				self.cursor = Some((welcome.epoch, welcome.head_seq));
			},
			pb::attach_frame::Frame::SnapshotChunk(chunk) => {
				self.snapshot_chunks += 1;
				self.partial.extend_from_slice(&chunk.bytes);
				if chunk.r#final {
					let snapshot =
						omp_dom::Snapshot::from_bytes(&self.partial).context("decode snapshot")?;
					self.partial.clear();
					self.dom = Some(Dom::from_snapshot(&snapshot));
				}
			},
			pb::attach_frame::Frame::Event(event) => {
				let decoded = omp_dom::wire::decode_event(&event.bytes).context("decode event")?;
				self
					.dom
					.as_mut()
					.context("an event arrived before any snapshot")?
					.apply_event(&decoded)
					.context("apply event")?;
				self.cursor = Some((event.epoch, event.seq));
			},
			pb::attach_frame::Frame::Resync(resync) => {
				self.resyncs.push(resync.reason);
				self.dom = None;
				self.partial.clear();
			},
			pb::attach_frame::Frame::Heartbeat(_) => {},
		}
		Ok(())
	}

	/// The replica's transcript text, for comparison with the host's.
	fn transcript(&self) -> String {
		self.dom.as_ref().map(transcript_of).unwrap_or_default()
	}
}

/// The replica's canonical bytes, for asserting what a property carries.
fn snapshot_text(replica: &Replica) -> Result<String> {
	let dom = replica.dom.as_ref().context("the replica exists")?;
	Ok(String::from_utf8_lossy(dom.snapshot().as_bytes()).into_owned())
}

/// Every assistant and user text in document order.
///
/// The DOM's canonical bytes are not a fixed point for a non-compact JSON
/// property (recorded in `TASKS.md`), so convergence is asserted on the
/// transcript the tree projects rather than on a byte compare.
fn transcript_of(dom: &Dom) -> String {
	let mut out = String::new();
	for turn in dom.children(dom.body()) {
		for handle in dom.children(*turn) {
			let Some(node) = dom.get(*handle) else {
				continue;
			};
			if let Some(text) = node.prop(&PropId::Text.into()).and_then(Value::as_str) {
				out.push_str(text);
				out.push('\n');
			}
		}
	}
	out
}

/// Reads frames until `wanted` have arrived, folding each one.
async fn fold_frames<S>(stream: &mut S, replica: &mut Replica, wanted: usize) -> Result<()>
where
	S: futures::Stream<Item = std::result::Result<pb::AttachFrame, tonic::Status>> + Unpin,
{
	use futures::StreamExt as _;

	let mut seen = 0;
	while seen < wanted {
		let frame = within("attach frame", BOUND, stream.next())
			.await?
			.context("the stream ended early")?
			.context("the stream faulted")?;
		if let Some(body) = frame.frame {
			replica.fold(body)?;
			seen += 1;
		}
	}
	Ok(())
}

/// The owner seam a session's holder serves between turns, as the chat
/// controller does.
struct Owner {
	requests: flume::Receiver<flume::Sender<omp_agent::steering::DomSubscription>>,
}

impl Owner {
	/// Answers one subscription request from the session this test holds.
	async fn serve_one(&self, session: &mut Session) -> Result<()> {
		let reply = within("subscription request", BOUND, self.requests.recv_async())
			.await?
			.context("the seam closed")?;
		reply
			.send(session.subscribe())
			.ok()
			.context("the requester went away")?;
		Ok(())
	}
}

/// Registers the live session and returns the owner seam.
fn register<C>(sessions: &SessionRegistry, kernel: &Kernel<C>, root: &Path) -> Owner {
	let (idle_tx, requests) = flume::unbounded();
	sessions.register(Str::new_static("p14"), KernelHandle {
		id:             SessionId::new("p14"),
		name:           Str::new_static("p14"),
		up:             kernel.mailbox(),
		snapshot:       Arc::new(RwLock::new(Dom::new().snapshot())),
		topology:       omp_agent::SessionTopology::main(Str::new_static("p14")),
		relay:          IrcRelayPolicy::default(),
		autoreply:      None,
		idle_subscribe: Some(idle_tx),
		project_root:   Arc::from(root),
	});
	Owner { requests }
}

/// Builds a wire over `sessions`, redacting `*/provider` from a caller that
/// lacks `observe_host_local`.
fn build_wire(sessions: &Arc<SessionRegistry>) -> Result<Arc<SessionWire>> {
	Ok(Arc::new(SessionWire::new(
		Arc::clone(sessions),
		BridgeLimits::default(),
		// The shipped default table, pinned in `omp_driver::control_visibility`.
		Arc::new(
			ControlVisibility::from_entries(["*/provider", "*/route"]).context("redaction table")?,
		),
		Duration::from_secs(60),
	)))
}

/// Dials the owner socket, retrying until the listener answers.
async fn dial(data_dir: &Path) -> Result<Channel> {
	let socket = omp_driver::session_wire::socket_path(data_dir);
	for _ in 0..200 {
		if let Ok(channel) = omp_rpc::uds::connect(&socket).await {
			return Ok(channel);
		}
		tokio::time::sleep(Duration::from_millis(25)).await;
	}
	Err(omp_e2e::error(format!("the wire never bound {}", socket.display())))
}

fn attach_request(session: &str, cursor: Option<(u64, u64)>) -> pb::AttachRequest {
	pb::AttachRequest {
		session_id:   session.to_owned(),
		resume_epoch: cursor.map_or(0, |(epoch, _)| epoch),
		resume_seq:   cursor.map_or(0, |(_, seq)| seq),
	}
}

#[tokio::test(flavor = "multi_thread")]
async fn p14_an_observer_converges_resumes_and_cannot_steer() -> Result<()> {
	let scratch = tempfile::tempdir().context("scratch")?;
	let data_dir = scratch.path().join("data");
	let project = scratch.path().join("project");
	std::fs::create_dir_all(&project).context("project root")?;

	let mut session = create_session(&scratch.path().join("p14.oms")).context("session")?;
	let (inference, _) =
		ScriptedInference::new([reply(FIRST_ANSWER), reply(SECOND_ANSWER), reply(THIRD_ANSWER)]);
	let mut kernel = Kernel::new(
		inference,
		Arc::new(Registry::new()),
		DispatchPolicy::new(BlobStore::open(scratch.path().join("blobs")).context("blobs")?),
		StaticPrompt(Str::new_static("P14")),
	);
	kernel
		.run_turn(
			&mut session,
			TurnInput { text: Str::new_static(FIRST_QUESTION), attachments: Vec::new() },
			RunControl::default(),
		)
		.await
		.context("history turn")?;

	let sessions = Arc::new(SessionRegistry::new());
	let owner = register(&sessions, &kernel, &project);
	let wire = build_wire(&sessions)?;
	let host = tokio::spawn({
		let wire = Arc::clone(&wire);
		let data_dir = data_dir.clone();
		async move { wire.serve(&data_dir).await }
	});

	let mut client = pb::session_control_client::SessionControlClient::new(dial(&data_dir).await?);

	// The owner socket is the authenticator: the listing answers without the
	// caller asserting any identity.
	let listed = within("listing", BOUND, client.list_sessions(pb::ListSessionsRequest::default()))
		.await?
		.context("list sessions")?
		.into_inner()
		.sessions;
	assert_eq!(listed.len(), 1, "the one live session is visible");
	assert_eq!(listed[0].session_id, "p14");
	assert_eq!(
		listed[0].project_root,
		project.to_string_lossy(),
		"the reference carries the session's project root"
	);

	// 1. Convergence. The session is idle, so the owner seam answers.
	let (attached, served) =
		tokio::join!(client.attach(attach_request("p14", None)), owner.serve_one(&mut session));
	served?;
	let mut stream = attached.context("attach")?.into_inner();
	let mut observer = Replica::default();
	fold_frames(&mut stream, &mut observer, 2).await?;
	assert!(observer.snapshot_chunks > 0, "the attachment opened with a snapshot");
	assert_eq!(
		observer.transcript(),
		transcript_of(session.dom()),
		"the replica starts at the host's tree"
	);
	assert!(
		observer.transcript().contains(FIRST_ANSWER),
		"the snapshot carries the history that precedes the attachment"
	);
	let owner_bytes = snapshot_text(&observer)?;
	assert!(
		owner_bytes.contains(HOST_LOCAL_PROVIDER) && owner_bytes.contains(HOST_LOCAL_ROUTE),
		"the owner sees host-local metadata as the kernel journaled it"
	);

	// 2. A turn the observer watches arrive.
	kernel
		.run_turn(
			&mut session,
			TurnInput { text: Str::new_static(SECOND_QUESTION), attachments: Vec::new() },
			RunControl::default(),
		)
		.await
		.context("watched turn")?;
	let host_transcript = transcript_of(session.dom());
	while !observer.transcript().contains(SECOND_ANSWER) {
		fold_frames(&mut stream, &mut observer, 1).await?;
	}
	assert_eq!(
		observer.transcript(),
		host_transcript,
		"the replica followed the host through a live turn"
	);
	let cursor = observer.cursor.context("the observer holds a cursor")?;

	// 3. Detach, miss a turn, and resume from the cursor: replayed, not resent.
	drop(stream);
	kernel
		.run_turn(
			&mut session,
			TurnInput { text: Str::new_static(THIRD_QUESTION), attachments: Vec::new() },
			RunControl::default(),
		)
		.await
		.context("missed turn")?;
	let host_transcript = transcript_of(session.dom());
	let mut resumed = client
		.attach(attach_request("p14", Some(cursor)))
		.await
		.context("resume")?
		.into_inner();
	let mut replayed = Replica { dom: observer.dom.take(), ..Replica::default() };
	while !replayed.transcript().contains(THIRD_ANSWER) {
		fold_frames(&mut resumed, &mut replayed, 1).await?;
	}
	assert_eq!(
		replayed.snapshot_chunks, 0,
		"a serviceable cursor is replayed from the ring, not resent a snapshot"
	);
	assert!(replayed.resyncs.is_empty(), "a serviceable cursor is not resynchronized");
	assert_eq!(
		replayed.transcript(),
		host_transcript,
		"the resumed replica caught up on what it missed"
	);
	drop(resumed);

	// 4. A cursor from another epoch is resynchronized and resent a snapshot.
	let stale = (cursor.0.wrapping_add(1), cursor.1);
	let mut rewound = client
		.attach(attach_request("p14", Some(stale)))
		.await
		.context("stale resume")?
		.into_inner();
	let mut fresh = Replica::default();
	fold_frames(&mut rewound, &mut fresh, 3).await?;
	assert_eq!(
		fresh.resyncs,
		vec!["epoch".to_owned()],
		"a cursor from another epoch is answered with a resync naming why"
	);
	assert!(fresh.snapshot_chunks > 0, "the resync is followed by a fresh snapshot");
	assert_eq!(fresh.transcript(), host_transcript, "the resynchronized replica is the host's tree");
	drop(rewound);

	// 5. The host dies without unlinking its socket (what a SIGKILL leaves behind),
	//    a new one binds the same path, and the observer that outlived it
	//    reconnects onto a new epoch.
	host.abort();
	// `abort` only requests cancellation: the listener's descriptor lives
	// until the task future is dropped, and a socket still holding it answers
	// the liveness probe in `bind_owner_socket`, so the replacement host would
	// refuse to bind. Await the handle, which resolves once the task is gone.
	let _ = host.await;
	let socket = omp_driver::session_wire::socket_path(&data_dir);
	assert!(socket.exists(), "a killed host leaves its socket behind");
	let restarted = build_wire(&sessions)?;
	let host = tokio::spawn({
		let wire = Arc::clone(&restarted);
		let data_dir = data_dir.clone();
		async move { wire.serve(&data_dir).await }
	});
	let mut client = pb::session_control_client::SessionControlClient::new(dial(&data_dir).await?);
	let (attached, served) = tokio::join!(
		client.attach(attach_request("p14", Some(cursor))),
		owner.serve_one(&mut session)
	);
	served?;
	let mut after_restart = attached.context("attach after restart")?.into_inner();
	let mut survivor = Replica::default();
	fold_frames(&mut after_restart, &mut survivor, 3).await?;
	assert_eq!(
		survivor.resyncs,
		vec!["epoch".to_owned()],
		"a bridge from a new host is a new epoch, and the cursor that outlived the old one is \
		 refused"
	);
	assert!(
		survivor.snapshot_chunks > 0,
		"the reconnected observer is resent a snapshot rather than replayed from a ring it never saw"
	);
	assert_eq!(
		survivor.transcript(),
		host_transcript,
		"the observer that outlived the host converges again"
	);
	drop(after_restart);

	// 6. Redaction. The socket grants the owner every read scope, so the caller
	//    that lacks `observe_host_local` is exercised at the authority.
	// This wire already holds a bridge for the session, so the attachment is
	// served from it and asks the owner for nothing.
	let mut blind = restarted
		.attach(
			ControlContext::new("observer", ScopeSet::from_iter([ControlScope::Observe])),
			attach_request("p14", None),
		)
		.await
		.context("attach as a plain observer")?;
	let mut redacted = Replica::default();
	fold_frames(&mut blind, &mut redacted, 2).await?;
	let blind_bytes = snapshot_text(&redacted)?;
	assert!(
		!blind_bytes.contains(HOST_LOCAL_PROVIDER) && !blind_bytes.contains(HOST_LOCAL_ROUTE),
		"a caller without observe_host_local never sees a host-local property"
	);
	assert!(
		blind_bytes.contains(BLANKED_PROVIDER) && blind_bytes.contains(BLANKED_ROUTE),
		"the properties are blanked in place, not pruned"
	);
	assert_eq!(
		redacted.transcript(),
		host_transcript,
		"redaction blanks properties without pruning the transcript"
	);

	// 7. Every mutating verb is refused, over the socket the owner trusts.
	for (verb, status) in [
		(
			"submit",
			client
				.submit(pb::ControlCommand {
					session_id: "p14".to_owned(),
					..pb::ControlCommand::default()
				})
				.await
				.err(),
		),
		(
			"approve",
			client
				.approve(pb::ApproveRequest {
					session_id: "p14".to_owned(),
					..pb::ApproveRequest::default()
				})
				.await
				.err(),
		),
		(
			"answer_ui",
			client
				.answer_ui(pb::UiAnswer { session_id: "p14".to_owned(), ..pb::UiAnswer::default() })
				.await
				.err(),
		),
		("open", client.open(pb::OpenSessionRequest::default()).await.err()),
	] {
		let status = status.with_context(|| format!("{verb} must be refused"))?;
		assert_eq!(
			status.code(),
			tonic::Code::Unimplemented,
			"{verb} is refused, not silently accepted"
		);
	}

	// The kernel is still the only writer: nothing the observer did journaled.
	assert_eq!(
		transcript_of(session.dom()),
		host_transcript,
		"a read-only attachment left the host's tree untouched"
	);
	assert!(kernel.mailbox().send(Up::Interrupt).is_ok(), "the kernel outlived every attachment");

	host.abort();
	let _ = host.await;
	Ok(())
}
