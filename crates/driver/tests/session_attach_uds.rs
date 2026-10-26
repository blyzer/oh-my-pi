//! `SessionWire` over a real owner socket.
//!
//! The unit tests call the authority directly, so they prove the projection
//! but not the listener: the socket, its interceptor, and the tonic service
//! are only exercised here.

#![cfg(unix)]

use std::{path::Path, sync::Arc, time::Duration};

use omp_agent::Up;
use omp_core::Str;
use omp_dom::{Dom, KnownTag, NodeSpec, Op, PropId, Txn, Value};
use omp_driver::{
	SessionWire,
	control_visibility::ControlVisibility,
	session_bridge::BridgeLimits,
	sessions::{IrcRelayPolicy, KernelHandle, SessionId, SessionRegistry},
};
use omp_journal::EntryId;
use omp_proto::session::v1 as pb;
use parking_lot::{Mutex, RwLock};

/// A kernel that answers `Up::Subscribe` from a DOM the test drives.
struct FakeKernel {
	dom:    Dom,
	drain:  flume::Receiver<omp_dom::Event>,
	to:     Arc<Mutex<Vec<flume::Sender<omp_dom::Event>>>>,
	_serve: tokio::task::JoinHandle<()>,
}

impl FakeKernel {
	/// Publishes an edit to every attached bridge.
	fn publish(&mut self, text: &'static str) {
		let body = self.dom.body();
		let turn = self
			.dom
			.children(body)
			.last()
			.copied()
			.expect("the seeded turn");
		self
			.dom
			.apply(&Txn {
				cause: EntryId::default(),
				label: None,
				ops:   vec![Op::Ins {
					parent: turn,
					after:  None,
					node:   NodeSpec::new(KnownTag::User)
						.with_prop(PropId::Text, Value::Str(Str::new(text))),
				}],
			})
			.expect("the edit applies");
		for event in self.drain.try_iter() {
			for sender in self.to.lock().iter() {
				let _ = sender.send(event.clone());
			}
		}
	}
}

/// Registers one session whose kernel answers subscriptions.
fn registered(sessions: &SessionRegistry, id: &'static str, root: &Path) -> FakeKernel {
	let mut dom = Dom::new();
	let body = dom.body();
	dom.apply(&Txn {
		cause: EntryId::default(),
		label: None,
		ops:   vec![Op::Ins { parent: body, after: None, node: NodeSpec::new(KnownTag::Turn) }],
	})
	.expect("a turn inserts under the body");
	let turn = dom.children(body).last().copied().expect("the seeded turn");
	dom.apply(&Txn {
		cause: EntryId::default(),
		label: None,
		ops:   vec![Op::Ins {
			parent: turn,
			after:  None,
			node:   NodeSpec::new(KnownTag::Assistant)
				.with_prop(PropId::Provider, Value::Str(Str::new_static("secret-provider")))
				.with_prop(PropId::Text, Value::Str(Str::new_static("visible"))),
		}],
	})
	.expect("an assistant inserts under the turn");
	let (snapshot, drain) = dom.subscribe();
	let (up, mailbox) = flume::unbounded();
	let to: Arc<Mutex<Vec<flume::Sender<omp_dom::Event>>>> = Arc::new(Mutex::new(Vec::new()));
	let fanout = Arc::clone(&to);
	let seeded = snapshot.clone();
	let serve = tokio::spawn(async move {
		while let Ok(message) = mailbox.recv_async().await {
			if let Up::Subscribe(reply) = message {
				let (sender, events) = flume::unbounded();
				fanout.lock().push(sender);
				let _ = reply.send((seeded.clone(), events));
			}
		}
	});
	sessions.register(Str::new(id), KernelHandle {
		id: SessionId::new(id),
		name: Str::new(id),
		up,
		snapshot: Arc::new(RwLock::new(snapshot)),
		topology: omp_agent::SessionTopology::main(Str::new(id)),
		relay: IrcRelayPolicy::default(),
		autoreply: None,
		project_root: Arc::from(root),
	});
	FakeKernel { dom, drain, to, _serve: serve }
}

/// Dials the socket the wire binds, retrying until the listener is up.
async fn dial(data_dir: &Path) -> tonic::transport::Channel {
	let socket = omp_driver::session_wire::socket_path(data_dir);
	for _ in 0..200 {
		if let Ok(channel) = omp_rpc::uds::connect(&socket).await {
			return channel;
		}
		tokio::time::sleep(Duration::from_millis(25)).await;
	}
	panic!("the wire never bound {}", socket.display());
}

/// Reads frames until `wanted` of them have arrived or the stream stalls.
async fn frames(
	stream: &mut tonic::Streaming<pb::AttachFrame>,
	wanted: usize,
) -> Vec<pb::attach_frame::Frame> {
	use futures::StreamExt as _;

	let mut out = Vec::new();
	while out.len() < wanted {
		let next = tokio::time::timeout(Duration::from_secs(5), stream.next())
			.await
			.expect("the stream produces a frame")
			.expect("the stream does not end")
			.expect("the stream does not fault");
		if let Some(frame) = next.frame {
			out.push(frame);
		}
	}
	out
}

#[tokio::test(flavor = "multi_thread")]
async fn the_owner_socket_serves_a_listing_an_attachment_and_refuses_mutation() {
	let data_dir = tempfile::tempdir().expect("a data directory");
	let project = tempfile::tempdir().expect("a project root");
	let sessions = Arc::new(SessionRegistry::new());
	let mut kernel = registered(&sessions, "live", project.path());
	let wire = Arc::new(SessionWire::new(
		Arc::clone(&sessions),
		BridgeLimits::default(),
		Arc::new(ControlVisibility::from_entries(["*/provider"]).expect("the table parses")),
		Duration::from_secs(60),
	));
	let host = tokio::spawn({
		let wire = Arc::clone(&wire);
		let data_dir = data_dir.path().to_path_buf();
		async move { wire.serve(&data_dir).await }
	});

	let channel = dial(data_dir.path()).await;
	let mut client = pb::session_control_client::SessionControlClient::new(channel);

	// The listener grants the owner's scopes from the socket itself; a caller
	// that had to assert its identity would be answered UNAUTHENTICATED.
	let listed = client
		.list_sessions(pb::ListSessionsRequest::default())
		.await
		.expect("the host answers a listing")
		.into_inner()
		.sessions;
	assert_eq!(listed.len(), 1, "the registry's one session is visible");
	assert_eq!(listed[0].session_id, "live");
	assert_eq!(
		listed[0].project_root,
		project.path().to_string_lossy(),
		"the reference carries the session's project root"
	);

	let mut stream = client
		.attach(pb::AttachRequest { session_id: "live".to_owned(), ..pb::AttachRequest::default() })
		.await
		.expect("the host serves the attachment")
		.into_inner();
	let opening = frames(&mut stream, 2).await;
	assert!(
		matches!(opening.first(), Some(pb::attach_frame::Frame::Welcome(_))),
		"the attachment opens with a welcome, got {opening:?}"
	);
	let mut replica = match &opening[1] {
		pb::attach_frame::Frame::SnapshotChunk(chunk) => {
			assert!(chunk.r#final, "the seeded tree fits one chunk");
			Dom::from_snapshot(
				&omp_dom::Snapshot::from_bytes(&chunk.bytes).expect("the snapshot decodes"),
			)
		},
		other => panic!("the welcome is followed by the snapshot, got {other:?}"),
	};
	// The caller holds `observe_host_local`, so the host-local property is
	// served as the kernel holds it.
	assert!(
		String::from_utf8_lossy(replica.snapshot().as_bytes()).contains("secret-provider"),
		"an owner attachment is not redacted"
	);

	kernel.publish("over the socket");
	let live = frames(&mut stream, 1).await;
	match &live[0] {
		pb::attach_frame::Frame::Event(event) => {
			let decoded = omp_dom::wire::decode_event(&event.bytes).expect("the event decodes");
			replica
				.apply_event(&decoded)
				.expect("the replica applies it");
		},
		other => panic!("the published edit arrives as an event, got {other:?}"),
	}
	assert!(
		String::from_utf8_lossy(replica.snapshot().as_bytes()).contains("over the socket"),
		"the replica converged on the host's tree"
	);

	let refused = client
		.submit(pb::ControlCommand { session_id: "live".to_owned(), ..pb::ControlCommand::default() })
		.await
		.expect_err("this build steers nothing");
	assert_eq!(
		refused.code(),
		tonic::Code::Unimplemented,
		"a mutating verb is refused, not silently accepted"
	);

	drop(stream);
	host.abort();
}
