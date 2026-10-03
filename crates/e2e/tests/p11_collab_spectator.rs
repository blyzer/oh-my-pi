//! P11-a: a spectator converges with a headless host through a real relay.
//!
//! Real components end to end: a `.oms` session folded into a DOM, the
//! production kernel (only provider output is scripted), the driver's
//! `HeadlessRoom` and collaboration owners on both sides, the revision-3 wire
//! with AES-GCM sealing, and a real socket relay. The relay is the in-process
//! `omp_collab::test_relay::TestRelay` because the hosted relay is not
//! self-hostable; it holds no key and reads no envelope.
//!
//! The proof asserts that a viewer and an editor joined to a session with
//! history converge with the host, including patches produced after they
//! joined and after a network partition; that a viewer cannot mutate the host
//! (through the client API, and through a hostile client that bypasses it with
//! no token or a forged one); that the relay never sees plaintext; that the
//! host registry lists the room while it lives; and that departures and room
//! close are announced cleanly.

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use omp_agent::{DispatchPolicy, Kernel, RunControl, StaticPrompt, TurnInput};
use omp_ai::{BlockKind, ChatEvent, Completion, ExecutionReceipt, FinishReason, Usage};
use omp_collab::{
	PROTOCOL_REVISION,
	codec::RelayRoute,
	crypto::RoomKey,
	link::CollabLink,
	presence::ConnectionState,
	relay::{Handshake, RelayClient, RelayInbound, RelayRole},
	test_relay::TestRelay,
};
use omp_core::Str;
use omp_dom::{Dom, Event, Snapshot};
use omp_driver::{
	collab::{
		host::{HeadlessRoom, RoomOptions, ServeEnd},
		observer::HostAgentBridge,
		registry::{Access, list_hosts},
		session::{
			CollabCommandFault, CollabCommandHandle, CollabOwnerCommand, CollabSessionAuthority,
			spawn_session_owner,
		},
	},
	sessions::SessionRegistry,
};
use omp_e2e::{
	Context as _, Error, Result, error,
	support::{ScriptedInference, create_session, within},
};
use omp_journal::blob::BlobStore;
use omp_proto::collab::v1::{CollabFrame, Hello, PromptRequest, SessionStateUpdate, collab_frame};
use omp_tool::Registry;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const BOUND: Duration = Duration::from_secs(30);
const HOST_QUESTION: &str = "host question before anyone joined";
const HOST_ANSWER: &str = "host answer that is already history";
const GUEST_QUESTION: &str = "editor question that steers the host";
const GUEST_ANSWER: &str = "streamed answer produced after the spectators joined";
const VIEWER_ATTEMPT: &str = "viewer attempts to prompt the host";
const HOSTILE_UNTOKENED: &str = "hostile client without a write token";
const HOSTILE_FORGED: &str = "hostile client with a forged write token";

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

/// A spectator: the production guest owner plus the DOM replica an actor
/// builds from its snapshot and ordered events.
struct Spectator {
	handle:  CollabCommandHandle,
	owner:   JoinHandle<()>,
	replica: Dom,
	events:  flume::Receiver<Event>,
	resets:  usize,
}

impl Spectator {
	async fn join(link: &str, name: &str) -> Result<Self> {
		let (authority, handle) = CollabSessionAuthority::new();
		let owner = spawn_session_owner(authority);
		within("guest join", BOUND, async {
			handle
				.request(CollabOwnerCommand::Join {
					link:         CollabLink::parse(link).context("parse link")?,
					display_name: Str::new(name),
				})
				.await
				.context("join room")
		})
		.await??;
		let snapshot = handle
			.replica_snapshot()
			.context("guest snapshot after join")?;
		let events = handle.replica_events();
		Ok(Self { handle, owner, replica: Dom::from_snapshot(&snapshot), events, resets: 0 })
	}

	/// Applies every replica event queued so far, in order.
	fn drain(&mut self) -> Result<()> {
		while let Ok(event) = self.events.try_recv() {
			if matches!(event, Event::Reset { .. }) {
				self.resets += 1;
			}
			self
				.replica
				.apply_event(&event)
				.context("apply replica event")?;
		}
		Ok(())
	}

	fn contains(&self, needle: &str) -> bool {
		let snapshot = self.replica.snapshot();
		snapshot
			.as_bytes()
			.windows(needle.len())
			.any(|window| window == needle.as_bytes())
	}

	/// Waits, bounded, for `condition` to hold over this replica.
	async fn until(
		&mut self,
		label: &'static str,
		condition: impl Fn(&Self) -> bool + Send + Sync,
	) -> Result<()> {
		within(label, BOUND, async {
			loop {
				self.drain()?;
				if condition(self) {
					return Ok::<(), Error>(());
				}
				tokio::time::sleep(Duration::from_millis(20)).await;
			}
		})
		.await?
	}

	async fn converge_to(&mut self, label: &'static str, target: &Snapshot) -> Result<()> {
		self
			.until(label, |spectator| spectator.replica.snapshot() == *target)
			.await
	}

	async fn leave(self) -> Result<()> {
		self
			.handle
			.request(CollabOwnerCommand::Leave)
			.await
			.context("leave")?;
		drop(self.handle);
		within("owner exit", BOUND, self.owner)
			.await?
			.context("owner task")
	}
}

/// A client that bypasses the guest owner's own read-only check and speaks the
/// wire directly, as a modified client would. Returns the host's refusal
/// message.
async fn hostile_prompt(link: &str, token: Option<&[u8]>, text: &str) -> Result<String> {
	let link = CollabLink::parse(link).context("parse link")?;
	let key = RoomKey::from_bytes(*link.credentials().key()).context("room key")?;
	let mut client =
		RelayClient::new(link.room_url(), RelayRole::Guest, key).context("hostile client")?;
	client.connect().await.context("connect")?;
	let hello = Hello {
		protocol_revision: PROTOCOL_REVISION,
		display_name:      "mallory".to_owned(),
		write_token:       token.map(Bytes::copy_from_slice),
		client_version:    String::new(),
	};
	client
		.send(RelayRoute { peer_id: 0 }, &Handshake::hello(1, hello))
		.await
		.context("hello")?;
	// The host welcomes even an unprivileged peer; wait for its snapshot to end.
	within("hostile welcome", BOUND, async {
		loop {
			match client.receive().await.context("receive")? {
				Some(RelayInbound::Frame(routed)) => {
					if matches!(
						routed.frame.payload,
						Some(collab_frame::Payload::SnapshotChunk(ref chunk)) if chunk.r#final
					) {
						return Ok::<(), Error>(());
					}
				},
				Some(_) => {},
				None => return Err(error("relay closed before the welcome")),
			}
		}
	})
	.await??;
	let prompt = CollabFrame {
		protocol_revision: PROTOCOL_REVISION,
		sequence: 2,
		payload: Some(collab_frame::Payload::Prompt(PromptRequest {
			text:   text.to_owned(),
			images: Vec::new(),
		})),
		..CollabFrame::default()
	};
	client
		.send(RelayRoute { peer_id: 0 }, &prompt)
		.await
		.context("prompt")?;
	let code = within("hostile refusal", BOUND, async {
		loop {
			match client.receive().await.context("receive")? {
				Some(RelayInbound::Frame(routed)) => {
					if let Some(collab_frame::Payload::Error(refusal)) = routed.frame.payload {
						// The revision-3 JSON grammar carries only the message of an error frame.
						return Ok::<String, Error>(refusal.message);
					}
				},
				Some(_) => {},
				None => return Err(error("relay closed before the refusal")),
			}
		}
	})
	.await??;
	client.close().await.context("close")?;
	Ok(code)
}

/// Waits, bounded, until `condition` holds, polling cheap state.
async fn eventually(label: &'static str, condition: impl Fn() -> bool + Send + Sync) -> Result<()> {
	within(label, BOUND, async {
		while !condition() {
			tokio::time::sleep(Duration::from_millis(20)).await;
		}
	})
	.await
}

#[tokio::test]
async fn p11a_spectators_converge_through_a_relay_and_a_viewer_cannot_mutate() -> Result<()> {
	let scratch = tempfile::tempdir().context("scratch")?;
	let registry_dir = scratch.path().join("run").join("collab-hosts");
	let relay = TestRelay::start().await.context("start relay")?;

	// A durable session with history from before anyone spectates.
	let mut session = create_session(&scratch.path().join("host.oms")).context("session")?;
	let (inference, _) = ScriptedInference::new([reply(HOST_ANSWER), reply(GUEST_ANSWER)]);
	let mut kernel = Kernel::new(
		inference,
		Arc::new(Registry::new()),
		DispatchPolicy::new(BlobStore::open(scratch.path().join("blobs")).context("blobs")?),
		StaticPrompt(Str::new_static("P11")),
	);
	kernel
		.run_turn(
			&mut session,
			TurnInput { text: Str::new_static(HOST_QUESTION), attachments: Vec::new() },
			RunControl::default(),
		)
		.await
		.context("history turn")?;
	let history = session.dom().snapshot();

	let mut room = HeadlessRoom::open(&mut session, RoomOptions {
		relay:        relay.endpoint().context("relay endpoint")?,
		session_id:   Str::new_static("p11a-session"),
		access:       Access::Control,
		agents:       HostAgentBridge::new(
			Arc::new(SessionRegistry::new()),
			scratch.path().join("sessions"),
		),
		state:        SessionStateUpdate {
			session_name: "p11a".to_owned(),
			host_cwd: "/p11a".to_owned(),
			..SessionStateUpdate::default()
		},
		registry_dir: Some(registry_dir.clone()),
	})
	.await
	.context("open room")?;
	let viewer_link = room.viewer_link().to_string();
	let editor_link = room.editor_link().to_string();
	assert_ne!(viewer_link, editor_link, "the two tiers carry different credentials");
	let host = room.handle().clone();

	let shutdown = CancellationToken::new();
	let (settled_tx, settled_rx) = flume::bounded::<Snapshot>(1);

	// The host serves guest mutations for as long as the spectators need it.
	let host_flow = async {
		let end = room
			.serve(&mut kernel, &mut session, &shutdown)
			.await
			.context("serve")?;
		settled_tx
			.send(session.dom().snapshot())
			.context("publish settled snapshot")?;
		Ok::<ServeEnd, Error>(end)
	};

	let guests = async {
		// Join with history already in the session: the snapshot carries it.
		let mut viewer = Spectator::join(&viewer_link, "Vera").await?;
		let mut editor = Spectator::join(&editor_link, "Ed").await?;
		assert_eq!(viewer.replica.snapshot(), history, "viewer starts from the host's history");
		assert_eq!(editor.replica.snapshot(), history, "editor starts from the host's history");
		assert!(viewer.contains(HOST_ANSWER));
		assert!(
			viewer
				.handle
				.presence()
				.is_some_and(|facts| facts.read_only())
		);
		assert!(
			editor
				.handle
				.presence()
				.is_some_and(|facts| !facts.read_only())
		);

		// The room is discoverable locally, without any link on disk.
		eventually("host counts both guests", || {
			host
				.presence()
				.is_some_and(|facts| facts.participant_count() == 3)
		})
		.await?;
		let listed = list_hosts(&registry_dir, Duration::from_secs(5))
			.await
			.context("list")?;
		assert_eq!(listed.len(), 1);
		assert_eq!(listed[0].session_id.as_str(), "p11a-session");
		assert_eq!(listed[0].participants, 3);
		assert!(listed[0].relay_connected);
		assert_eq!(listed[0].access, Access::Control);

		// Read-only enforcement: the client refuses locally, and the host
		// refuses a client that skips that check.
		let attempt = viewer
			.handle
			.request(CollabOwnerCommand::Prompt {
				text:   Str::new_static(VIEWER_ATTEMPT),
				images: Vec::new(),
			})
			.await;
		assert!(matches!(attempt, Err(CollabCommandFault::ReadOnly)), "{attempt:?}");
		let untokened = hostile_prompt(&viewer_link, None, HOSTILE_UNTOKENED).await?;
		assert!(untokened.contains("read-only"), "a viewer link carries no write token: {untokened}");
		let forged = hostile_prompt(&viewer_link, Some(&[0xaa; 16]), HOSTILE_FORGED).await?;
		assert!(forged.contains("read-only"), "a forged write token is read-only: {forged}");
		eventually("hostile peers leave", || {
			relay.guest_count() == 2
				&& host
					.presence()
					.is_some_and(|facts| facts.participant_count() == 3)
		})
		.await?;

		// A partition drops both guests without a close handshake; each
		// reconnects, re-authenticates, and receives a fresh snapshot.
		relay.sever_guests();
		viewer
			.until("viewer resync", |spectator| spectator.resets >= 1)
			.await?;
		editor
			.until("editor resync", |spectator| spectator.resets >= 1)
			.await?;
		viewer
			.converge_to("viewer converges after the partition", &history)
			.await?;

		// The editor drives the host; spectators see the patches that follow.
		editor
			.handle
			.request(CollabOwnerCommand::Prompt {
				text:   Str::new_static(GUEST_QUESTION),
				images: Vec::new(),
			})
			.await
			.context("editor prompt")?;
		viewer
			.until("viewer sees the streamed answer", |s| s.contains(GUEST_ANSWER))
			.await?;
		editor
			.until("editor sees the streamed answer", |s| s.contains(GUEST_ANSWER))
			.await?;

		shutdown.cancel();
		let settled = within("settled snapshot", BOUND, settled_rx.recv_async())
			.await?
			.context("host settled")?;
		viewer
			.converge_to("viewer converges with the host", &settled)
			.await?;
		editor
			.converge_to("editor converges with the host", &settled)
			.await?;

		// Exactly two turns ran: the history and the editor's. Nothing a viewer
		// or hostile client sent reached the host.
		assert_eq!(
			Dom::from_snapshot(&settled)
				.count("body turn")
				.context("count turns")?,
			2
		);
		for refused in [VIEWER_ATTEMPT, HOSTILE_UNTOKENED, HOSTILE_FORGED] {
			assert!(!viewer.contains(refused), "refused mutation {refused:?} reached the transcript");
		}
		assert!(viewer.contains(GUEST_QUESTION), "the editor's prompt is in the transcript");

		// The relay only ever carried ciphertext.
		let forwarded = relay.forwarded();
		assert!(!forwarded.is_empty());
		for plaintext in [
			HOST_QUESTION,
			HOST_ANSWER,
			GUEST_QUESTION,
			GUEST_ANSWER,
			VIEWER_ATTEMPT,
			HOSTILE_UNTOKENED,
			HOSTILE_FORGED,
		] {
			assert!(
				forwarded.iter().all(|envelope| !envelope
					.windows(plaintext.len())
					.any(|w| w == plaintext.as_bytes())),
				"{plaintext:?} crossed the relay in the clear"
			);
		}

		// A viewer leaves cleanly; the host and relay account for it.
		viewer.leave().await?;
		eventually("host counts one guest", || {
			relay.guest_count() == 1
				&& host
					.presence()
					.is_some_and(|facts| facts.participant_count() == 2)
		})
		.await?;
		Ok::<Spectator, Error>(editor)
	};

	let (end, editor) = Box::pin(within("P11-a", Duration::from_secs(120), async {
		tokio::try_join!(host_flow, guests)
	}))
	.await??;
	assert_eq!(end, ServeEnd::Shutdown);

	// Closing the room disconnects the remaining guest and withdraws the host.
	room.stop().await;
	eventually("editor sees the room close", || {
		editor
			.handle
			.presence()
			.is_none_or(|facts| facts.connection() == ConnectionState::Disconnected)
	})
	.await?;
	assert!(!relay.has_host());
	assert!(
		list_hosts(&registry_dir, Duration::from_secs(5))
			.await
			.context("list after stop")?
			.is_empty(),
		"a closed room leaves the registry"
	);
	editor.leave().await.ok();
	Ok(())
}
