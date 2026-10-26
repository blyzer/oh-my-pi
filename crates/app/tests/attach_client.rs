//! `omp attach` against a real owner socket.
//!
//! The unit tests fold frames the host never sent; this one dials the socket
//! the driver binds, so the command's own path — socket resolution, dial,
//! and the listing it prints — is exercised end to end.

#![cfg(unix)]

use std::{path::Path, sync::Arc, time::Duration};

use omp_app::{attach_cmd, cli::AttachArgs};
use omp_core::Str;
use omp_driver::{
	SessionWire,
	control_visibility::ControlVisibility,
	session_bridge::BridgeLimits,
	sessions::{IrcRelayPolicy, KernelHandle, SessionId, SessionRegistry},
};
use parking_lot::RwLock;

/// Registers one session. Listing never reaches the kernel, so the mailbox
/// this handle carries is never read.
fn registered(sessions: &SessionRegistry, id: &'static str, root: &Path) {
	let (up, _mailbox) = flume::unbounded();
	sessions.register(Str::new(id), KernelHandle {
		id: SessionId::new(id),
		name: Str::new(id),
		up,
		snapshot: Arc::new(RwLock::new(omp_dom::Dom::new().snapshot())),
		topology: omp_agent::SessionTopology::main(Str::new(id)),
		relay: IrcRelayPolicy::default(),
		autoreply: None,
		project_root: Arc::from(root),
	});
}

fn args(session: Option<&str>) -> AttachArgs {
	AttachArgs {
		session: session.map(Str::new),
		observe: true,
		socket:  None,
		resume:  None,
		json:    false,
		project: None,
	}
}

#[tokio::test(flavor = "multi_thread")]
async fn the_command_lists_what_the_local_host_serves() {
	let data_dir = tempfile::tempdir().expect("a data directory");
	let project = tempfile::tempdir().expect("a project root");
	let sessions = Arc::new(SessionRegistry::new());
	registered(&sessions, "live", project.path());
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
	let socket = omp_driver::session_wire::socket_path(data_dir.path());
	for _ in 0..200 {
		if socket.exists() {
			break;
		}
		tokio::time::sleep(Duration::from_millis(25)).await;
	}

	attach_cmd::run(args(None), data_dir.path())
		.await
		.expect("the command lists the host's sessions");

	// A session the host does not serve is refused rather than reported as an
	// empty attachment.
	let missing = attach_cmd::run(args(Some("absent")), data_dir.path())
		.await
		.expect_err("an unknown session has nothing to observe");
	assert!(
		format!("{missing:?}").contains("refused the attachment"),
		"the client reports the host's refusal, got {missing:?}"
	);

	host.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn no_host_is_an_error_naming_the_socket() {
	let data_dir = tempfile::tempdir().expect("a data directory");
	let error = attach_cmd::run(args(Some("live")), data_dir.path())
		.await
		.expect_err("nothing serves this data directory");
	let rendered = format!("{error:?}");
	assert!(
		rendered.contains("no session host answers") && rendered.contains("session.sock"),
		"the error names the socket it dialled, got {rendered}"
	);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_attachment_must_be_declared_read_only() {
	let data_dir = tempfile::tempdir().expect("a data directory");
	let sessions = Arc::new(SessionRegistry::new());
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
	let socket = omp_driver::session_wire::socket_path(data_dir.path());
	for _ in 0..200 {
		if socket.exists() {
			break;
		}
		tokio::time::sleep(Duration::from_millis(25)).await;
	}

	let mut bare = args(Some("live"));
	bare.observe = false;
	let error = attach_cmd::run(bare, data_dir.path())
		.await
		.expect_err("an attachment without --observe is refused");
	assert!(
		format!("{error:?}").contains("read-only"),
		"the client says why it refused, got {error:?}"
	);

	host.abort();
}
