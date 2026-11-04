//! `Submit` through [`SessionWire`] against a real kernel turn.
//!
//! The unit tests stub the mailbox, so they prove the projection and not the
//! routing behind it. Here a held turn drains the mailbox as production does,
//! and a second kernel reopens its journal, so the replay is proven across a
//! host restart (ADR 0039: an idempotent `Submit` after a host restart creates
//! one entry).

use std::{
	future::ready,
	path::Path,
	sync::Arc,
	time::{Duration, SystemTime},
};

use futures::{StreamExt as _, stream};
use omp_agent::{
	DispatchPolicy, Inference, Kernel, RunControl, SessionTopology, StaticPrompt, TurnInput, Up,
};
use omp_ai::{
	BlockKind, ChatEvent, ChatRequest, ChatStream, Completion, ExecutionReceipt, FinishReason,
	RequestId, ResponseMeta, Usage,
};
use omp_catalog::{ProviderId, RouteId};
use omp_core::Str;
use omp_dom::{Event, Op};
use omp_driver::{
	SessionWire,
	control_visibility::ControlVisibility,
	session_bridge::BridgeLimits,
	sessions::{IrcRelayPolicy, KernelHandle, SessionId, SessionRegistry},
};
use omp_journal::{EntryId, blob::BlobStore, data::Patch, kind};
use omp_proto::session::v1 as pb;
use omp_serve::session::{ControlContext, ControlScope, ScopeSet, SessionControlAuthority as _};
use omp_session::{ComponentRegistry, Session};
use omp_tool::Registry;
use parking_lot::RwLock;
use tokio::sync::{Notify, watch};

const KEY: &str = "01JLOSTRECEIPT";

/// Answers each request only once `release` is set, so the turn stays active
/// while the test submits; `asked` fires when the kernel waits on a request.
struct Held {
	release: watch::Receiver<bool>,
	asked:   Arc<Notify>,
}

impl Inference for Held {
	fn chat(
		&mut self,
		_request: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_ai::Error>> + Send {
		let mut release = self.release.clone();
		let asked = Arc::clone(&self.asked);
		let started = ChatEvent::Started(ResponseMeta {
			request_id:          RequestId::from("held"),
			provider:            ProviderId::from("test"),
			route:               RouteId::from("test/route"),
			model:               None,
			provider_request_id: None,
			created_at:          SystemTime::UNIX_EPOCH,
		});
		let answer = [
			ChatEvent::BlockStarted { index: 0, kind: BlockKind::Text },
			ChatEvent::TextDelta { index: 0, text: Str::new_static("done") },
			ChatEvent::Completed(Completion {
				reason:  FinishReason::Stop,
				blocks:  1,
				usage:   Usage::default(),
				receipt: ExecutionReceipt::default().into(),
			}),
		];
		let held = stream::once(async move {
			asked.notify_one();
			let _ = release.wait_for(|released| *released).await;
			stream::iter(answer)
		})
		.flatten();
		ready(Ok(ChatStream::ordinary(Box::pin(stream::iter([started]).chain(held).map(Ok)))))
	}
}

fn kernel(scratch: &Path, release: watch::Receiver<bool>, asked: &Arc<Notify>) -> Kernel<Held> {
	Kernel::new(
		Held { release, asked: Arc::clone(asked) },
		Arc::new(Registry::new()),
		DispatchPolicy::new(BlobStore::open(scratch.join("artifacts")).expect("artifact store")),
		StaticPrompt(Str::new_static("test")),
	)
}

fn register(sessions: &SessionRegistry, kernel: &Kernel<Held>, session: &Session) {
	sessions.register(Str::new_static("live"), KernelHandle {
		id:             SessionId::new("live"),
		name:           Str::new_static("live"),
		up:             kernel.mailbox(),
		snapshot:       Arc::new(RwLock::new(session.dom().snapshot())),
		topology:       SessionTopology::main(Str::new_static("live")),
		relay:          IrcRelayPolicy::default(),
		autoreply:      None,
		idle_subscribe: None,
		ask:            None,
		turn_activity:  kernel.turn_activity(),
		project_root:   Arc::from(Path::new("/workspace")),
	});
}

/// Runs one held turn, drives `client` once the kernel is mid-request, then
/// releases the turn and returns what the client produced.
async fn during_a_turn<T>(
	kernel: &mut Kernel<Held>,
	session: &mut Session,
	release: &watch::Sender<bool>,
	asked: &Notify,
	client: impl Future<Output = T>,
) -> T {
	let input = TurnInput { text: Str::new_static("hold"), attachments: Vec::new() };
	let turn = kernel.run_turn(session, input, RunControl::default());
	let drive = async {
		asked.notified().await;
		let out = client.await;
		release.send_replace(true);
		out
	};
	let (outcome, out) = tokio::join!(turn, drive);
	outcome.expect("the held turn completes once released");
	out
}

fn owner() -> ControlContext {
	ControlContext::new("owner", ScopeSet::from_iter([ControlScope::Observe, ControlScope::Prompt]))
}

fn steer() -> pb::ControlCommand {
	pb::ControlCommand {
		session_id: "live".to_owned(),
		idempotency_key: KEY.to_owned(),
		command: Some(pb::control_command::Command::Steer(pb::SteerCommand {
			text: "redirect".to_owned(),
		})),
		..pb::ControlCommand::default()
	}
}

#[tokio::test]
async fn a_lost_receipt_is_recovered_by_its_entry_and_survives_a_restart() {
	let scratch = tempfile::tempdir().expect("scratch");
	let path = scratch.path().join("live.oms");
	let sessions = Arc::new(SessionRegistry::new());
	let wire = SessionWire::new(
		Arc::clone(&sessions),
		BridgeLimits::default(),
		Arc::new(ControlVisibility::from_entries(["*/provider"]).expect("the table parses")),
		Duration::from_secs(60),
	);

	let mut session =
		Session::create(&path, ComponentRegistry::standard()).expect("session creates");
	let (release, held) = watch::channel(false);
	let asked = Arc::new(Notify::new());
	let mut first = kernel(scratch.path(), held, &asked);
	register(&sessions, &first, &session);
	let up = first.mailbox();
	let (entry, inserted) = during_a_turn(&mut first, &mut session, &release, &asked, async {
		let (reply, subscribed) = flume::bounded(1);
		up.send(Up::Subscribe(reply)).expect("the kernel is live");
		let (_, events) = subscribed
			.recv_async()
			.await
			.expect("the turn serves a subscription");
		// The client never reads this receipt: it is the one that was lost.
		wire
			.submit(owner(), steer())
			.await
			.expect("the first steer is answered");
		let retry = wire
			.submit(owner(), steer())
			.await
			.expect("the retry is answered");
		assert_eq!((retry.accepted, retry.seq), (false, 0), "a replay carries no position");
		let entry: EntryId = retry.entry_id.parse().expect("the receipt names an entry");
		let inserted = events
			.try_iter()
			.find_map(|event| match event {
				Event::Patch(patch) if patch.cause == entry => Some(patch.ops),
				_ => None,
			})
			.expect("the entry names a patch the attached client observed");
		(entry, inserted)
	})
	.await;
	assert!(
		matches!(
			inserted.as_slice(),
			[Op::Ins { node, .. }] if node.content.as_deref() == Some("redirect")
		),
		"the entry leads back to the client's own steer: {inserted:?}"
	);
	drop(first);
	drop(session);
	sessions.remove(SessionId::from_ref("live"));

	let mut session = Session::open(&path, ComponentRegistry::standard()).expect("reopen");
	let (release, held) = watch::channel(false);
	let asked = Arc::new(Notify::new());
	let mut second = kernel(scratch.path(), held, &asked);
	register(&sessions, &second, &session);
	let replay = during_a_turn(&mut second, &mut session, &release, &asked, async {
		wire
			.submit(owner(), steer())
			.await
			.expect("the retry after a restart is answered")
	})
	.await;
	assert_eq!(replay, pb::CommandReceipt {
		idempotency_key: KEY.to_owned(),
		accepted:        false,
		seq:             0,
		entry_id:        entry.to_string(),
	});
	let keyed = session
		.entries()
		.filter(|entry| entry.kind.name.as_str() == kind::PATCH)
		.filter_map(|entry| serde_json::from_str::<Patch>(entry.data.as_str()).ok())
		.filter(|patch| {
			patch
				.idempotency
				.as_ref()
				.is_some_and(|identity| identity.key.as_str() == KEY)
		})
		.count();
	assert_eq!(keyed, 1, "three submissions over two lifetimes journaled the steer once");
}
