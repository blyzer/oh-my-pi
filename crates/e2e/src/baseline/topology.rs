//! Environment authorities under measurement: how a workload reaches one, and
//! how it starts, attaches again and stops.
//!
//! An embedded target serves `EnvClient::in_process` channels from the
//! recorder process. An attached target is the production topology of ADR 0045:
//! a partition router joins an in-process local host (session tools and
//! Python extensions) to a real `envd` child process over its owner socket.

use std::{
	fmt, fs,
	path::{Path, PathBuf},
	sync::Arc,
	time::{Duration, Instant},
};

use omp_core::FastHashSet;
use omp_env::{EnvClient, PartitionError, PartitionedEnvTransport};
use omp_envd::{EnvServer, EnvdError};
use omp_proto::{SCHEMA_REV, env::v1::ClientHello};
use tokio::{task::JoinHandle, time};

use super::{
	Transport,
	census::{Ledger, Reaped},
	workloads::{EXTENSION, Plan},
};
use crate::{
	Context as _, Result,
	support::{
		AllowAdmission, DEFAULT_TIMEOUT, DocServerTask, EnvHarness, InProcessConnection,
		InProcessEnvHarness, InProcessServing, ProcessEnvHarness, Scratch, omp_binary, within,
	},
};

/// How long a released partition router gets to observe its client closing.
const ROUTER_EXIT_LIMIT: Duration = Duration::from_secs(1);

/// A project directory with its fixture written, before any authority serves
/// it.
pub(super) struct Staging {
	scratch: Scratch,
	python:  bool,
	ledger:  Ledger,
}

impl Staging {
	/// Creates a scratch project and writes the workload's fixture into it.
	pub(super) async fn new(plan: &Plan) -> Result<Self> {
		let scratch = Scratch::new().context("creating scratch project")?;
		plan.fixture.write(scratch.project())?;
		Ok(Self { scratch, python: plan.python, ledger: Ledger::open().await? })
	}

	/// Starts an authority of the requested transport over the staged project.
	pub(super) async fn start(self, transport: Transport) -> Result<Target> {
		let Self { scratch, python, ledger } = self;
		match transport {
			Transport::Embedded => {
				let harness = open_host(scratch, python).await?;
				let project = fs::canonicalize(harness.project()).context("canonical project root")?;
				Ok(Target { project, host: Host::Embedded(harness), ledger })
			},
			Transport::Attached => {
				let daemon = Daemon::start(scratch).await?;
				let local =
					open_host(Scratch::new().context("creating local host scratch")?, python).await?;
				let link = PartitionLink::connect(&local, daemon.process.socket()).await?;
				let project = daemon.project.clone();
				let attached = Box::new(Attached { link, local, daemon });
				Ok(Target { project, host: Host::Attached(attached), ledger })
			},
		}
	}
}

async fn open_host(scratch: Scratch, python: bool) -> Result<InProcessEnvHarness> {
	if python {
		InProcessEnvHarness::spawn_with_extension(scratch, &EXTENSION).await
	} else {
		InProcessEnvHarness::spawn(scratch).await
	}
}

/// A production `envd` child attached to a real docserver.
///
/// Fields drop in order: the daemon process goes before its docserver.
struct Daemon {
	process:   ProcessEnvHarness,
	docserver: DocServerTask,
	project:   PathBuf,
	_scratch:  Scratch,
}

impl Daemon {
	async fn start(scratch: Scratch) -> Result<Self> {
		let executable = omp_binary().context("resolving the daemon executable")?;
		let docserver = DocServerTask::spawn_for_daemon(
			scratch.project(),
			scratch.socket("attached-docserver.sock"),
			&executable,
		)
		.await?;
		let process = EnvHarness::spawn_attached(&scratch, &executable, docserver.socket()).await?;
		let project = fs::canonicalize(scratch.project()).context("canonical project root")?;
		Ok(Self { process, docserver, project, _scratch: scratch })
	}

	async fn shutdown(self) -> Result<()> {
		let Self { process, docserver, .. } = self;
		process.shutdown().await?;
		docserver.shutdown().await
	}
}

struct LinkGuards {
	router: Option<tokio::task::JoinHandle<Result<(), PartitionError>>>,
	pump:   JoinHandle<Result<(), EnvdError>>,
	_local: InProcessServing,
}

impl Drop for LinkGuards {
	fn drop(&mut self) {
		if let Some(router) = &self.router {
			router.abort();
		}
		self.pump.abort();
	}
}

/// A partitioned client: the local host and the daemon behind one router.
pub(super) struct PartitionLink {
	client: EnvClient,
	guards: LinkGuards,
}

impl PartitionLink {
	async fn connect(local: &InProcessEnvHarness, daemon_socket: &Path) -> Result<Self> {
		let (local_pipe, serving) = local.connect_frames();
		let (remote_pipe, pump) = EnvServer::connect_owner_uds_frames(daemon_socket)
			.await
			.context("connecting the partition router to the daemon")?;
		let (client, router) =
			PartitionedEnvTransport::spawn(local_pipe, remote_pipe, Arc::new(FastHashSet::default()));
		client.set_admitter(AllowAdmission);
		let link =
			Self { client, guards: LinkGuards { router: Some(router), pump, _local: serving } };
		within(
			"partitioned environment hello",
			DEFAULT_TIMEOUT,
			link.client.hello(ClientHello {
				client: "omp-e2e-attached".to_owned(),
				schema_rev: SCHEMA_REV,
				..Default::default()
			}),
		)
		.await??;
		Ok(link)
	}

	const fn client(&self) -> &EnvClient {
		&self.client
	}

	/// Drops the client, which ends the router once every clone is gone, and
	/// hands back the router task with the guards that keep both backends open
	/// until the caller has observed it end.
	fn release(self) -> (Option<JoinHandle<Result<(), PartitionError>>>, LinkGuards) {
		let Self { client, mut guards } = self;
		drop(client);
		(guards.router.take(), guards)
	}
}

/// One more client connection to a running target.
pub(super) enum Attachment {
	Direct(InProcessConnection),
	Partitioned(PartitionLink),
}

impl Attachment {
	/// Returns the hello-complete client.
	pub(super) const fn client(&self) -> &EnvClient {
		match self {
			Self::Direct(connection) => connection.client(),
			Self::Partitioned(link) => link.client(),
		}
	}
}

struct Attached {
	link:   PartitionLink,
	local:  InProcessEnvHarness,
	daemon: Daemon,
}

enum Host {
	Embedded(InProcessEnvHarness),
	Attached(Box<Attached>),
}

/// How a partition router ended once its client closed.
#[derive(Debug)]
pub(super) enum RouterExit {
	/// The router saw the client close and stopped, the only clean ending.
	ClientClosed { after: Duration },
	/// The router stopped for another reason.
	Failed(String),
	/// The router was still running after `ROUTER_EXIT_LIMIT`.
	Pending,
}

impl fmt::Display for RouterExit {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::ClientClosed { after } => {
				write!(formatter, "closed with its client after {after:?}")
			},
			Self::Failed(reason) => write!(formatter, "stopped with {reason}"),
			Self::Pending => {
				write!(formatter, "still running {ROUTER_EXIT_LIMIT:?} after its client closed")
			},
		}
	}
}

/// What stopping a target observed.
pub(super) struct Closed {
	/// How the partition router ended; `None` for an embedded target.
	pub(super) router: Option<RouterExit>,
	/// Processes the stopped authority left running and the harness killed.
	pub(super) reaped: Reaped,
}

/// A running environment authority and the client a workload drives.
pub(super) struct Target {
	project: PathBuf,
	host:    Host,
	ledger:  Ledger,
}

impl Target {
	/// Returns the canonical project root requests are rooted at.
	pub(super) fn project(&self) -> &Path {
		&self.project
	}

	/// Returns the hello-complete client of the target's primary connection.
	pub(super) const fn client(&self) -> &EnvClient {
		match &self.host {
			Host::Embedded(harness) => harness.client(),
			Host::Attached(attached) => attached.link.client(),
		}
	}

	/// Returns the daemon's process id; its process group has the same id
	/// because the harness starts it as a group leader.
	pub(super) fn daemon_pid(&self) -> Result<u32> {
		match &self.host {
			Host::Embedded(_) => Err(crate::error("an embedded target has no daemon process")),
			Host::Attached(attached) => attached
				.daemon
				.process
				.pid()
				.context("daemon process has no id"),
		}
	}

	/// Opens another client connection to the running authority, as a second
	/// session attaching to a warm daemon does.
	pub(super) async fn attach_again(&self) -> Result<Attachment> {
		match &self.host {
			Host::Embedded(harness) => {
				Ok(Attachment::Direct(harness.connect_client("omp-e2e-embedded-again").await?))
			},
			Host::Attached(attached) => Ok(Attachment::Partitioned(
				PartitionLink::connect(&attached.local, attached.daemon.process.socket()).await?,
			)),
		}
	}

	/// Stops the authority, reporting how the partition router ended. Every
	/// clone of the primary client must already be dropped for the router to
	/// end cleanly.
	pub(super) async fn close(self) -> Result<Closed> {
		let Self { host, ledger, .. } = self;
		let router = match host {
			Host::Embedded(harness) => {
				harness.shutdown().await;
				None
			},
			Host::Attached(attached) => {
				let Attached { link, local, daemon } = *attached;
				let released = Instant::now();
				let (router, guards) = link.release();
				let exit = match router {
					Some(handle) => Some(router_exit(handle, released).await),
					None => None,
				};
				{
					let mut guards = guards;
					guards.pump.abort();
					let _ = (&mut guards.pump).await;
					drop(guards);
				}
				local.shutdown().await;
				daemon.shutdown().await?;
				exit
			},
		};
		let reaped = ledger.reap().await?;
		if reaped.count > 0 {
			eprintln!(
				"note: reaped {} process(es) the stopped authority left running: {}",
				reaped.count, reaped.description
			);
		}
		Ok(Closed { router, reaped })
	}
}

async fn router_exit(
	mut router: JoinHandle<Result<(), PartitionError>>,
	released: Instant,
) -> RouterExit {
	match time::timeout(ROUTER_EXIT_LIMIT, &mut router).await {
		Ok(Ok(Err(PartitionError::ClientClosed))) => {
			RouterExit::ClientClosed { after: released.elapsed() }
		},
		Ok(Ok(other)) => RouterExit::Failed(format!("{other:?}")),
		Ok(Err(joined)) => RouterExit::Failed(joined.to_string()),
		Err(_) => {
			router.abort();
			RouterExit::Pending
		},
	}
}
