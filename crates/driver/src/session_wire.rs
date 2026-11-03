//! Driver-side session-control authority and its owner-only UDS listener
//! (ADR 0039 R1).
//!
//! [`SessionWire`] is the one component that knows both the DOM and the wire.
//! It reads [`SessionRegistry`] for what exists, owns one [`SessionBridge`] per
//! attached session, and implements [`SessionControlAuthority`], which
//! `omp-serve` projects over gRPC. Serve never names a driver type, because
//! driver already depends on serve.
//!
//! R1 is owner-local. The socket is the authenticator: the `run` directory is
//! 0700 and the socket 0600, so no other OS user can connect. The reads and
//! `Open` of a new session are served, the latter only for a context holding
//! [`ControlScope::Control`], which the owner socket grants. Device
//! authentication and TCP belong to later phases, and the projection answers
//! the remaining mutating verbs `UNIMPLEMENTED` until then.
//!
//! Redaction is a property of the caller, not of the bridge: a context without
//! [`ControlScope::ObserveHostLocal`] is served through [`ControlVisibility`],
//! and one that holds it is served unredacted. The policy is polled, never
//! held across an await, because `dyn PropPolicy` is not `Sync` and a tonic
//! stream must be `Send`. [`Subscriber::idle`] exists for exactly that split.

use std::{
	path::{Path, PathBuf},
	sync::Arc,
	time::Duration,
};

use omp_agent::{Up, steering::DomSubscription};
use omp_core::{FastHashMap, Str};
use omp_dom::redact::PropPolicy;
use omp_proto::session::v1 as pb;
use omp_serve::{
	inference::RpcStream,
	session::{
		ControlContext, ControlScope, SessionControlAuthority, SessionControlError, SessionControlRpc,
	},
};
use parking_lot::Mutex;
use strum::VariantArray as _;
use tonic::Status;

use crate::{
	control_visibility::ControlVisibility,
	session_bridge::{BridgeLimits, Closure, Cursor, Delivery, Polled, SessionBridge, Subscriber},
	sessions::{KernelHandle, SessionId, SessionRegistry},
};

omp_con::var! {
	/// Idle interval, in milliseconds, between heartbeat frames on an
	/// attachment that has nothing else to deliver.
	pub static SV_CONTROL_HEARTBEAT_MS = sv_control_heartbeat_ms: u32 {
		default: 15_000,
		min: 1,
		flags: archive,
	};
}

/// Largest snapshot payload carried by one [`pb::SnapshotChunk`].
///
/// A snapshot is split at this boundary so one frame never approaches the
/// transport's message limit, which the listener sets explicitly below.
const SNAPSHOT_CHUNK_BYTES: usize = 256 * 1024;

/// Largest encoded message the session listener accepts or produces.
const CONTROL_MESSAGE_MAX_BYTES: usize = 16 * 1024 * 1024;

/// How long a kernel has to answer [`Up::Subscribe`] before the authority
/// reports it unavailable.
///
/// A kernel mid-turn answers from its mailbox as soon as the turn yields, so
/// this bounds a wedged kernel rather than a busy one.
const SUBSCRIBE_BUDGET: Duration = Duration::from_secs(5);

/// Failure to bind or serve the owner-only session socket.
#[derive(Debug, thiserror::Error)]
pub enum SessionWireError {
	/// The owner-only `run` directory could not be created or restricted.
	#[error("could not create owner-only run directory {path}")]
	RunDirectory {
		/// Directory the listener owns.
		path:   PathBuf,
		/// Underlying filesystem failure.
		#[source]
		source: std::io::Error,
	},
	/// Another process already serves this socket.
	#[error("a session host already listens on {path}")]
	AlreadyListening {
		/// Socket a live host holds.
		path: PathBuf,
	},
	/// The socket could not be bound or restricted to its owner.
	#[error("could not bind session socket {path}")]
	BindSocket {
		/// Socket the listener owns.
		path:   PathBuf,
		/// Underlying filesystem or socket failure.
		#[source]
		source: std::io::Error,
	},
	/// The transport refused to listen on the owner socket.
	#[error("could not listen on session socket {path}")]
	Listen {
		/// Socket the listener owns.
		path:   PathBuf,
		/// Underlying transport failure.
		#[source]
		source: omp_rpc::Error,
	},
	/// The gRPC server stopped with an error.
	#[error("the session control server failed")]
	Serve {
		/// Underlying transport failure.
		#[source]
		source: tonic::transport::Error,
	},
}

/// Reads the live session registry, owns one bridge per attached session, and
/// answers the read-only session-control verbs.
pub struct SessionWire {
	sessions:  Arc<SessionRegistry>,
	bridges:   Mutex<FastHashMap<SessionId, Arc<SessionBridge>>>,
	limits:    BridgeLimits,
	redaction: Arc<ControlVisibility>,
	heartbeat: Duration,
	open_host: Option<std::sync::Arc<dyn OpenHost>>,
	/// Sessions already minted for a client's `idempotency_key`.
	///
	/// ADR 0039 journals the key on the entry a mutation creates and reads it
	/// back from the DOM, so no side table is needed. `Open{NewSession}` has no
	/// such entry: the session IS what it creates, and there is nothing to
	/// read back before it exists. A retried open would otherwise mint a
	/// second session and strand the first, so the mapping lives here.
	///
	/// The invariant is per-open, not continuous: after each successful keyed
	/// open the map holds live mappings plus the one just minted, because that
	/// open sweeps every entry whose session the registry no longer knows.
	/// Between a retirement and the next successful keyed open it can hold
	/// arbitrarily many stale entries, and an open that fails before the
	/// insert (no host, or an id the host never registered) does not sweep at
	/// all.
	///
	/// So the map is not bounded by live sessions at every instant. What it
	/// cannot do is grow without limit with the number of opens ever made,
	/// which is the unbounded case. Nothing else retires an entry: the
	/// registry owns session lifetime and does not know this map exists, and
	/// a hook from it to here would make two owners of one fact.
	///
	/// Async, because the guard is held across the host's `open_new` so that
	/// two concurrent retries of one key cannot both miss the map.
	opened:    tokio::sync::Mutex<FastHashMap<Str, SessionId>>,
}

/// Composes and registers a new session. `SessionWire` does not construct a
/// kernel.
#[tonic::async_trait]
pub trait OpenHost: Send + Sync + 'static {
	/// Registers the new kernel on the shared registry and returns its id.
	async fn open_new(
		&self,
		project_root: &std::path::Path,
	) -> Result<SessionId, SessionControlError>;
}

impl SessionWire {
	/// Builds an authority over a live registry.
	///
	/// `redaction` is the table applied to a caller that lacks
	/// [`ControlScope::ObserveHostLocal`].
	#[must_use]
	pub fn new(
		sessions: Arc<SessionRegistry>,
		limits: BridgeLimits,
		redaction: Arc<ControlVisibility>,
		heartbeat: Duration,
	) -> Self {
		Self {
			sessions,
			bridges: Mutex::default(),
			limits,
			redaction,
			heartbeat,
			open_host: None,
			opened: tokio::sync::Mutex::default(),
		}
	}

	/// Builds an authority with every bound read from `ctx`.
	///
	/// # Errors
	/// Returns the table error when `sv_control_host_local_props` holds an
	/// entry this build cannot parse.
	pub fn from_ctx(
		sessions: Arc<SessionRegistry>,
		ctx: &omp_con::Ctx,
	) -> Result<Self, crate::control_visibility::VisibilityTableError> {
		let heartbeat = Duration::from_millis(u64::from(SV_CONTROL_HEARTBEAT_MS.get(ctx)));
		Ok(Self::new(
			sessions,
			BridgeLimits::from_con(ctx),
			Arc::new(ControlVisibility::from_ctx(ctx)?),
			heartbeat,
		))
	}

	/// Installs the host `Open` of a new session calls.
	#[must_use]
	pub fn with_open_host(mut self, host: std::sync::Arc<dyn OpenHost>) -> Self {
		self.open_host = Some(host);
		self
	}

	/// The epoch of this session's bridge, or zero when none has started.
	fn epoch_of(&self, id: &SessionId) -> u64 {
		self
			.bridges
			.lock()
			.get(id)
			.map_or(0, |bridge| bridge.epoch().get())
	}

	/// Returns this session's bridge, starting one over a fresh kernel
	/// subscription when none is live.
	async fn bridge_for(
		&self,
		handle: &KernelHandle,
	) -> Result<Arc<SessionBridge>, SessionControlError> {
		{
			let mut bridges = self.bridges.lock();
			match bridges.get(&handle.id) {
				// A bridge whose kernel dropped its subscription is cached but
				// dead: every later attach would fault on a session that is
				// still live. Evict it and subscribe again.
				Some(bridge) if bridge.closed().is_some() => {
					bridges.remove(&handle.id);
				},
				Some(bridge) => return Ok(Arc::clone(bridge)),
				None => {},
			}
		}
		let subscription = subscribe(handle).await?;
		let bridge =
			Arc::new(SessionBridge::start(handle.id.clone(), subscription, self.limits.clone()));
		// Another attach may have started one while this task awaited the
		// kernel; the first insertion wins and the loser's bridge is dropped.
		Ok(Arc::clone(
			self
				.bridges
				.lock()
				.entry(handle.id.clone())
				.or_insert(bridge),
		))
	}

	/// The redaction policy this caller is served through, if any.
	fn policy_for(&self, context: &ControlContext) -> Option<Arc<ControlVisibility>> {
		(!context.scopes().contains(ControlScope::ObserveHostLocal))
			.then(|| Arc::clone(&self.redaction))
	}

	/// Serves session control on the owner-only socket under `data_dir`.
	///
	/// The `run` directory is created 0700 and the socket 0600, and a socket
	/// left by a dead host is replaced rather than refused. A socket a live
	/// host still answers is refused.
	///
	/// # Errors
	/// Returns [`SessionWireError`] when the socket cannot be owned, or when
	/// the server stops with a transport failure.
	#[cfg(unix)]
	pub async fn serve(self: Arc<Self>, data_dir: &Path) -> Result<(), SessionWireError> {
		use tonic::transport::Server;

		let socket = bind_owner_socket(data_dir).await?;
		let authority: Arc<dyn SessionControlAuthority> = self;
		let inner =
			pb::session_control_server::SessionControlServer::new(SessionControlRpc::new(authority))
				.max_decoding_message_size(CONTROL_MESSAGE_MAX_BYTES)
				.max_encoding_message_size(CONTROL_MESSAGE_MAX_BYTES);
		let service = tonic::service::interceptor::InterceptedService::new(inner, owner_identity);
		let incoming = omp_rpc::uds::listen(&socket.path)
			.await
			.map_err(|source| SessionWireError::Listen { path: socket.path.clone(), source })?;
		restrict_socket(&socket.path)?;
		tracing::info!(socket = %socket.path.display(), "session control listening");
		Server::builder()
			.add_service(service)
			.serve_with_incoming(incoming)
			.await
			.map_err(|source| SessionWireError::Serve { source })
	}
}

/// Establishes the caller's identity for a request on the owner socket.
///
/// The socket is the authenticator: its directory is 0700 and the socket 0600,
/// so only the owning OS user can have reached this point. The owner holds the
/// scopes ADR 0039 A4 grants this socket, written as an explicit set rather
/// than every variant: `admin` is "never grantable" and `approve_persist` is
/// "never granted" by default, so deriving from the enum would hand a later
/// variant authority nobody decided to give it. Adding a scope here is a
/// deliberate act, and `the_owner_socket_grants_only_its_decided_scopes` fails
/// until it is made.
#[cfg(unix)]
fn owner_identity(mut request: tonic::Request<()>) -> Result<tonic::Request<()>, Status> {
	request
		.extensions_mut()
		.insert(ControlContext::new(OWNER_PRINCIPAL, OWNER_SCOPES.iter().copied().collect()));
	Ok(request)
}

/// The scopes the owner socket grants, smallest set that serves every verb this
/// revision answers there. It is deliberately not `ControlScope::VARIANTS`.
#[cfg(unix)]
const OWNER_SCOPES: &[ControlScope] =
	&[ControlScope::Observe, ControlScope::ObserveHostLocal, ControlScope::Control];

/// Principal every caller on the owner socket speaks for.
#[cfg(unix)]
const OWNER_PRINCIPAL: &str = "owner";

/// The owner socket a session host binds under `data_dir`.
///
/// The client dials this path, so both sides read it from here.
#[must_use]
pub fn socket_path(data_dir: &Path) -> PathBuf {
	data_dir.join("run").join("session.sock")
}

/// An owner-only socket path this process has claimed.
#[cfg(unix)]
struct OwnerSocket {
	path: PathBuf,
}

/// Claims `<data_dir>/run/session.sock` for this process.
#[cfg(unix)]
async fn bind_owner_socket(data_dir: &Path) -> Result<OwnerSocket, SessionWireError> {
	use std::{fs, os::unix::fs::PermissionsExt as _};

	let run = data_dir.join("run");
	fs::create_dir_all(&run)
		.map_err(|source| SessionWireError::RunDirectory { path: run.clone(), source })?;
	fs::set_permissions(&run, fs::Permissions::from_mode(0o700))
		.map_err(|source| SessionWireError::RunDirectory { path: run.clone(), source })?;

	let path = socket_path(data_dir);
	if path.exists() {
		// A socket whose owner is gone refuses the connection; one a live host
		// still answers must not be stolen.
		if tokio::net::UnixStream::connect(&path).await.is_ok() {
			return Err(SessionWireError::AlreadyListening { path });
		}
		fs::remove_file(&path)
			.map_err(|source| SessionWireError::BindSocket { path: path.clone(), source })?;
	}
	// `uds::listen` binds and `restrict_socket` chmods immediately after, so
	// the socket exists briefly at the process umask. The 0700 directory above
	// is the defense during that window: no other user can traverse to it.
	Ok(OwnerSocket { path })
}

/// Restricts a bound socket to its owner.
#[cfg(unix)]
fn restrict_socket(path: &Path) -> Result<(), SessionWireError> {
	use std::{fs, os::unix::fs::PermissionsExt as _};

	fs::set_permissions(path, fs::Permissions::from_mode(0o600))
		.map_err(|source| SessionWireError::BindSocket { path: path.to_path_buf(), source })
}

/// Asks a live kernel for a DOM subscription within [`SUBSCRIBE_BUDGET`].
async fn subscribe(handle: &KernelHandle) -> Result<DomSubscription, SessionControlError> {
	// A session is held by its kernel inside a turn and by its owner between
	// turns, and only the holder can subscribe to the DOM. Ask both and take
	// whichever answers; the loser's subscription is dropped unused.
	let (reply, subscription) = flume::bounded(2);
	let mailbox = handle.up.send(Up::Subscribe(reply.clone())).is_ok();
	let idle = handle
		.idle_subscribe
		.as_ref()
		.is_some_and(|idle| idle.send(reply).is_ok());
	if !(mailbox || idle) {
		return Err(SessionControlError::SessionNotLive { session_id: handle.id.clone().into() });
	}
	match tokio::time::timeout(SUBSCRIBE_BUDGET, subscription.recv_async()).await {
		Ok(Ok(subscription)) => Ok(subscription),
		Ok(Err(_)) => {
			Err(SessionControlError::SessionNotLive { session_id: handle.id.clone().into() })
		},
		Err(_) => Err(SessionControlError::HostUnavailable),
	}
}

/// Projects one live handle into its wire reference.
fn session_ref(handle: &KernelHandle, epoch: u64) -> pb::SessionRef {
	pb::SessionRef {
		session_id: handle.id.as_str().to_owned(),
		label: handle.name.as_str().to_owned(),
		is_live: true,
		epoch,
		project_root: handle.project_root.to_string_lossy().into_owned(),
	}
}

/// The resume cursor a request carries, if it carries one this bridge could
/// possibly serve.
fn resume_cursor(session: &SessionId, request: &pb::AttachRequest) -> Option<Cursor> {
	(request.resume_epoch != 0).then(|| Cursor {
		session: session.clone(),
		epoch:   crate::session_bridge::Epoch::from_wire(request.resume_epoch),
		seq:     crate::session_bridge::Seq::from_wire(request.resume_seq),
	})
}

/// The scopes a welcome frame reports back to the caller.
///
/// A grant is a ceiling the client is entitled to read back (ADR 0039 A4), so
/// this walks every `ControlScope` rather than a hand-kept list: a scope added
/// for a new verb is advertised by construction instead of being dropped here.
fn granted_scopes(context: &ControlContext) -> Vec<String> {
	ControlScope::VARIANTS
		.iter()
		.copied()
		.filter(|scope| context.scopes().contains(*scope))
		.map(|scope| <&'static str>::from(scope).to_owned())
		.collect()
}

/// Splits one snapshot into ordered chunks, the last marked final.
///
/// An empty snapshot still produces one final chunk, so a client always sees
/// the boundary it folds on.
fn snapshot_frames(epoch: u64, seq: u64, snapshot: &[u8]) -> Vec<pb::AttachFrame> {
	let mut chunks = snapshot.chunks(SNAPSHOT_CHUNK_BYTES).peekable();
	let mut frames = Vec::new();
	let mut index = 0_u32;
	while let Some(chunk) = chunks.next() {
		frames.push(frame(pb::attach_frame::Frame::SnapshotChunk(pb::SnapshotChunk {
			epoch,
			seq,
			index,
			bytes: bytes::Bytes::copy_from_slice(chunk),
			r#final: chunks.peek().is_none(),
		})));
		index = index.saturating_add(1);
	}
	if frames.is_empty() {
		frames.push(frame(pb::attach_frame::Frame::SnapshotChunk(pb::SnapshotChunk {
			epoch,
			seq,
			index: 0,
			bytes: bytes::Bytes::new(),
			r#final: true,
		})));
	}
	frames
}

const fn frame(body: pb::attach_frame::Frame) -> pb::AttachFrame {
	pb::AttachFrame { frame: Some(body) }
}

/// Renders a closure as the stream's terminal status.
///
/// Only [`Closure::Detached`] ends a stream successfully. Every other closure
/// truncated the stream mid-session, so the client must learn that its cursor
/// outlived what this host can serve.
fn closure_status(closure: Closure) -> Option<Status> {
	match closure {
		Closure::Detached => None,
		Closure::SubscriptionLost => Some(Status::aborted(
			"the host dropped this session's kernel subscription; the stream is truncated",
		)),
		Closure::Replica | Closure::Encode | Closure::Panicked => {
			Some(Status::internal("the session bridge failed; the stream is truncated"))
		},
	}
}

/// Pumps one subscriber into attach frames until the bridge closes.
fn attach_stream(
	mut subscriber: Subscriber,
	bridge: Arc<SessionBridge>,
	policy: Option<Arc<ControlVisibility>>,
	label: Str,
	context: ControlContext,
	heartbeat: Duration,
	opening: Option<pb::WelcomeFrame>,
) -> RpcStream<pb::AttachFrame> {
	let scopes = granted_scopes(&context);
	Box::pin(async_stream::stream! {
		// The synthesized welcome is emitted lazily: a cursor the bridge
		// refuses is answered with a resync and a welcome of its own, and the
		// client must not see two headers for one attachment.
		let mut opening = opening;
		let mut idle = tokio::time::interval(heartbeat);
		idle.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
		idle.tick().await;
		let mut pending: std::collections::VecDeque<pb::AttachFrame> =
			std::collections::VecDeque::new();
		loop {
			if let Some(next) = pending.pop_front() {
				yield Ok(next);
				continue;
			}
			// The policy reference never crosses an await: polling here and
			// awaiting in `idle` is what keeps this stream `Send`.
			let polled = {
				let policy = policy.as_deref().map(|policy| policy as &dyn PropPolicy);
				subscriber.try_next(policy)
			};
			match polled {
				Polled::Ready(Delivery::Welcome { cursor, snapshot }) => {
					opening = None;
					let epoch = cursor.epoch.get();
					let seq = cursor.seq.get();
					yield Ok(frame(pb::attach_frame::Frame::Welcome(pb::WelcomeFrame {
						epoch,
						head_seq: seq,
						label: label.as_str().to_owned(),
						granted_scopes: scopes.clone(),
					})));
					pending.extend(snapshot_frames(epoch, seq, snapshot.as_bytes()));
				},
				Polled::Ready(Delivery::Event { cursor, bytes }) => {
					if let Some(welcome) = opening.take() {
						yield Ok(frame(pb::attach_frame::Frame::Welcome(welcome)));
					}
					yield Ok(frame(pb::attach_frame::Frame::Event(pb::EventFrame {
						epoch: cursor.epoch.get(),
						seq:   cursor.seq.get(),
						bytes,
					})));
				},
				Polled::Ready(Delivery::Resync { reason }) => {
					opening = None;
					yield Ok(frame(pb::attach_frame::Frame::Resync(pb::ResyncFrame {
						reason: reason.to_string(),
					})));
				},
				Polled::Closed(closure) => {
					if let Some(status) = closure_status(closure) {
						yield Err(status);
					}
					return;
				},
				Polled::Empty => {
					tokio::select! {
						live = subscriber.idle() => if !live {
							// The wake channel closed; the next poll reports
							// the closure that explains why.
							continue;
						},
						_ = idle.tick() => {
							if let Some(welcome) = opening.take() {
								yield Ok(frame(pb::attach_frame::Frame::Welcome(welcome)));
							}
							yield Ok(frame(pb::attach_frame::Frame::Heartbeat(
								pb::HeartbeatFrame { head_seq: bridge.head().get() },
							)));
						},
					}
				},
			}
		}
	})
}

#[tonic::async_trait]
impl SessionControlAuthority for SessionWire {
	async fn list(
		&self,
		_context: ControlContext,
		request: pb::ListSessionsRequest,
	) -> Result<pb::SessionList, SessionControlError> {
		let root = request.project_root.trim();
		let mut sessions = self
			.sessions
			.list()
			.into_iter()
			.filter(|handle| root.is_empty() || handle.project_root.as_os_str() == root)
			.map(|handle| {
				let epoch = self.epoch_of(&handle.id);
				session_ref(&handle, epoch)
			})
			.collect::<Vec<_>>();
		// The registry is a hash map, so its iteration order is arbitrary. A
		// session id is a ULID, whose lexical order is its mint order, so
		// sorting descending is most-recent first as the wire promises.
		sessions.sort_unstable_by(|left, right| right.session_id.cmp(&left.session_id));
		Ok(pb::SessionList { sessions })
	}

	async fn attach(
		&self,
		context: ControlContext,
		request: pb::AttachRequest,
	) -> Result<RpcStream<pb::AttachFrame>, SessionControlError> {
		let id = SessionId::from_ref(request.session_id.as_str());
		let handle = self
			.sessions
			.lookup(id)
			.ok_or_else(|| SessionControlError::UnknownSession {
				session_id: Str::new(request.session_id.as_str()),
			})?;
		let bridge = self.bridge_for(&handle).await?;
		let cursor = resume_cursor(&handle.id, &request);
		let subscriber = bridge.subscribe(cursor.as_ref());
		// A served cursor replays events and nothing else, but the proto makes
		// the welcome the first frame of every attachment: it is where a client
		// learns the session header and its granted scopes. Synthesize it here
		// rather than make the bridge resend a snapshot the client already has.
		let opening = cursor.is_some().then(|| pb::WelcomeFrame {
			epoch:          bridge.epoch().get(),
			head_seq:       bridge.head().get(),
			label:          handle.name.as_str().to_owned(),
			granted_scopes: granted_scopes(&context),
		});
		Ok(attach_stream(
			subscriber,
			Arc::clone(&bridge),
			self.policy_for(&context),
			handle.name.clone(),
			context,
			self.heartbeat,
			opening,
		))
	}

	async fn open_session(
		&self,
		context: ControlContext,
		request: pb::OpenSessionRequest,
	) -> Result<pb::SessionRef, SessionControlError> {
		// The authority is public, so a projection other than
		// `SessionControlRpc` can reach this directly. Minting a session is a
		// mutation (ADR 0039 A4 grants `Open` to `control`), so the grant is
		// checked here too rather than trusting every future caller to have
		// run its own gate.
		if !context.scopes().contains(ControlScope::Control) {
			return Err(SessionControlError::MissingScope { scope: ControlScope::Control });
		}
		let opens_new = matches!(request.kind, Some(pb::open_session_request::Kind::NewSession(_)));
		if !opens_new {
			let id = SessionId::from_ref(request.session_id.as_str());
			return if self.sessions.lookup(id).is_none() {
				Err(SessionControlError::UnknownSession {
					session_id: Str::new(request.session_id.as_str()),
				})
			} else {
				Err(SessionControlError::HostUnavailable)
			};
		}
		// A retry carries the key its first attempt used (ADR 0039: every
		// mutation carries one). Answering with the session that key already
		// minted is what makes `Open` exactly-once as observed; minting a
		// second one would strand the first, which stays live and attached to
		// nothing the client knows about.
		//
		// The guard is held across the mint, so two concurrent retries of one
		// key cannot both miss the map and both open. It is an async mutex
		// because a blocking one must never be held across an await. Opens
		// compose a kernel and are rare, so serializing them costs nothing
		// that matters. An absent key cannot be matched to anything, so it
		// skips the gate entirely rather than queueing behind keyed opens.
		let key = Str::new(request.idempotency_key.as_str());
		let mut minted = if key.is_empty() {
			None
		} else {
			Some(self.opened.lock().await)
		};
		if let Some(seen) = minted.as_ref()
			&& let Some(id) = seen.get(&key)
			&& let Some(handle) = self.sessions.lookup(id)
		{
			return Ok(session_ref(&handle, self.epoch_of(&handle.id)));
		}
		let host = self
			.open_host
			.as_ref()
			.ok_or(SessionControlError::HostUnavailable)?;
		let id = host
			.open_new(std::path::Path::new(request.project_root.as_str()))
			.await?;
		let handle =
			self
				.sessions
				.lookup(&id)
				.ok_or_else(|| SessionControlError::UnknownSession {
					session_id: Str::new(id.as_str()),
				})?;
		// Recorded only now: a host that answered an id it never registered
		// has not opened anything, and a key pointing at one would refuse
		// every later retry instead of opening.
		if let Some(seen) = minted.as_mut() {
			// Swept here rather than on retirement, because the registry owns
			// lifetime and knows nothing of this map. A key whose session has
			// gone is simply stale, and dropping every stale entry under the
			// guard already held bounds the map by live sessions rather than
			// by opens ever made. It is not prompt: an entry survives until
			// some later keyed open gets this far.
			seen.retain(|_, minted| self.sessions.lookup(minted).is_some());
			seen.insert(key, id);
		}
		Ok(session_ref(&handle, self.epoch_of(&handle.id)))
	}
}

#[cfg(test)]
mod tests {
	use std::path::Path;

	use omp_core::Str;
	use omp_dom::{Dom, KnownTag, NodeSpec, Op, PropId, Txn, Value};
	use omp_journal::EntryId;
	use omp_serve::session::ScopeSet;
	use parking_lot::RwLock;

	use super::*;
	use crate::sessions::IrcRelayPolicy;

	/// A kernel that answers `Up::Subscribe` from a DOM the test drives.
	struct FakeKernel {
		dom:    Dom,
		drain:  flume::Receiver<omp_dom::Event>,
		to:     Arc<parking_lot::Mutex<Vec<flume::Sender<omp_dom::Event>>>>,
		_serve: tokio::task::JoinHandle<()>,
	}

	/// Registers one session whose kernel answers subscriptions, and returns
	/// the producer side.
	fn registered(
		sessions: &SessionRegistry,
		id: &'static str,
		root: &'static str,
	) -> (FakeKernel, Arc<RwLock<omp_dom::Snapshot>>) {
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
		let to: Arc<parking_lot::Mutex<Vec<flume::Sender<omp_dom::Event>>>> =
			Arc::new(parking_lot::Mutex::new(Vec::new()));
		let fanout = Arc::clone(&to);
		let seeded = snapshot.clone();
		// One channel per subscription, as a real kernel hands out: dropping
		// the senders is how a test makes a bridge lose its subscription.
		let serve = tokio::spawn(async move {
			while let Ok(message) = mailbox.recv_async().await {
				if let Up::Subscribe(reply) = message {
					let (sender, events) = flume::unbounded();
					fanout.lock().push(sender);
					let _ = reply.send((seeded.clone(), events));
				}
			}
		});
		let detached = Arc::new(RwLock::new(snapshot));
		sessions.register(Str::new(id), KernelHandle {
			id: SessionId::new(id),
			name: Str::new(id),
			up,
			snapshot: Arc::clone(&detached),
			topology: omp_agent::SessionTopology::main(Str::new(id)),
			relay: IrcRelayPolicy::default(),
			autoreply: None,
			idle_subscribe: None,
			project_root: Arc::from(Path::new(root)),
		});
		(FakeKernel { dom, drain, to, _serve: serve }, detached)
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
				.expect("edit applies");
			for event in self.drain.try_iter() {
				for sender in self.to.lock().iter() {
					let _ = sender.send(event.clone());
				}
			}
		}

		/// Drops every subscription this kernel has handed out.
		fn drop_subscribers(&self) {
			self.to.lock().clear();
		}
	}

	fn wire(sessions: Arc<SessionRegistry>) -> Arc<SessionWire> {
		Arc::new(SessionWire::new(
			sessions,
			BridgeLimits::default(),
			Arc::new(
				ControlVisibility::from_entries(["*/provider"]).expect("the seeded table parses"),
			),
			Duration::from_secs(60),
		))
	}

	fn observer() -> ControlContext {
		ControlContext::new("owner", ScopeSet::from_iter([ControlScope::Observe]))
	}

	/// The grant a caller on the owner socket actually arrives with. It mirrors
	/// `OWNER_SCOPES`, which `the_owner_socket_grants_only_its_decided_scopes`
	/// pins against the real interceptor: a helper that withheld `Control`
	/// would make every verb below look reachable without it.
	fn owner() -> ControlContext {
		ControlContext::new(
			"owner",
			ScopeSet::from_iter([
				ControlScope::Observe,
				ControlScope::ObserveHostLocal,
				ControlScope::Control,
			]),
		)
	}

	/// Drains the frames already queued on an attachment.
	async fn ready(stream: &mut RpcStream<pb::AttachFrame>) -> Vec<pb::attach_frame::Frame> {
		use futures::StreamExt as _;

		let mut out = Vec::new();
		while let Ok(Some(frame)) =
			tokio::time::timeout(Duration::from_millis(250), stream.next()).await
		{
			out.push(
				frame
					.expect("the stream does not fault")
					.frame
					.expect("a frame body"),
			);
		}
		out
	}

	/// The heartbeat carries the bridge's head, which is what lets an idle
	/// client tell "current" from "hung".
	#[tokio::test]
	async fn a_heartbeat_reports_the_bridge_head() {
		use futures::StreamExt as _;

		let sessions = Arc::new(SessionRegistry::new());
		let (mut kernel, _) = registered(&sessions, "live", "/workspace/live");
		let wire = Arc::new(SessionWire::new(
			Arc::clone(&sessions),
			BridgeLimits::default(),
			Arc::new(
				ControlVisibility::from_entries(["*/provider"]).expect("the seeded table parses"),
			),
			Duration::from_millis(50),
		));

		let mut stream = wire
			.attach(owner(), pb::AttachRequest {
				session_id: "live".to_owned(),
				..pb::AttachRequest::default()
			})
			.await
			.expect("attaching succeeds");
		kernel.publish("one more");

		// A 50 ms heartbeat never lets the stream fall idle, so read a bounded
		// prefix rather than draining it.
		let mut head = None;
		let mut heartbeat = None;
		for _ in 0..24 {
			let next = tokio::time::timeout(Duration::from_millis(500), stream.next())
				.await
				.expect("the stream produces a frame")
				.expect("the stream does not end")
				.expect("the stream does not fault")
				.frame
				.expect("a frame body");
			match next {
				pb::attach_frame::Frame::Event(event) => head = Some(event.seq),
				pb::attach_frame::Frame::Heartbeat(beat) if head.is_some() => {
					heartbeat = Some(beat.head_seq);
					break;
				},
				_ => {},
			}
		}
		assert_eq!(
			heartbeat.expect("an idle stream heartbeats"),
			head.expect("the published edit arrives"),
			"the heartbeat carries the bridge head"
		);
	}

	/// A cursor the bridge can serve replays events, and the proto still makes
	/// the welcome the first frame of the attachment.
	#[tokio::test]
	async fn a_served_cursor_still_opens_with_a_welcome() {
		let sessions = Arc::new(SessionRegistry::new());
		let (mut kernel, _) = registered(&sessions, "live", "/workspace/live");
		let wire = wire(Arc::clone(&sessions));

		let mut first = wire
			.attach(owner(), pb::AttachRequest {
				session_id: "live".to_owned(),
				..pb::AttachRequest::default()
			})
			.await
			.expect("attaching succeeds");
		let opening = ready(&mut first).await;
		let (epoch, head) = opening
			.iter()
			.find_map(|frame| match frame {
				pb::attach_frame::Frame::Welcome(welcome) => Some((welcome.epoch, welcome.head_seq)),
				_ => None,
			})
			.expect("the first attachment opens with a welcome");
		drop(first);
		kernel.publish("after the detach");

		let mut resumed = wire
			.attach(owner(), pb::AttachRequest {
				session_id: "live".to_owned(),
				resume_epoch: epoch,
				resume_seq: head,
				..pb::AttachRequest::default()
			})
			.await
			.expect("attaching succeeds");
		let frames = ready(&mut resumed).await;
		assert!(
			matches!(frames.first(), Some(pb::attach_frame::Frame::Welcome(_))),
			"a served cursor opens with a welcome, got {frames:?}"
		);
		assert!(
			frames
				.iter()
				.any(|frame| matches!(frame, pb::attach_frame::Frame::Event(_))),
			"the replayed event follows it, got {frames:?}"
		);
		assert_eq!(
			frames
				.iter()
				.filter(|frame| matches!(frame, pb::attach_frame::Frame::Welcome(_)))
				.count(),
			1,
			"one header per attachment, got {frames:?}"
		);
		assert!(
			!frames
				.iter()
				.any(|frame| matches!(frame, pb::attach_frame::Frame::SnapshotChunk(_))),
			"a served cursor resends no snapshot, got {frames:?}"
		);
	}

	/// A grant is a ceiling the client is entitled to read back (ADR 0039 A4).
	/// The welcome used to list the two read scopes and nothing else, so a
	/// caller holding `control` was told it held less than it did. A fresh
	/// attach and a resumed one build their welcome apart, so both must report
	/// the grant exactly.
	#[tokio::test]
	async fn the_welcome_frame_reports_control_exactly_when_it_is_held() {
		let sessions = Arc::new(SessionRegistry::new());
		let (mut kernel, _) = registered(&sessions, "live", "/workspace/live");
		let wire = wire(Arc::clone(&sessions));

		let welcome_for = async |held: ScopeSet, resume: (u64, u64)| {
			let mut stream = wire
				.attach(ControlContext::new("device", held), pb::AttachRequest {
					session_id: "live".to_owned(),
					resume_epoch: resume.0,
					resume_seq: resume.1,
					..pb::AttachRequest::default()
				})
				.await
				.expect("attaching succeeds");
			let welcome = ready(&mut stream)
				.await
				.into_iter()
				.find_map(|frame| match frame {
					pb::attach_frame::Frame::Welcome(welcome) => Some(welcome),
					_ => None,
				})
				.expect("an attachment opens with a welcome");
			let mut names = welcome.granted_scopes.clone();
			names.sort();
			(welcome, names)
		};

		let controlling = ScopeSet::from_iter([ControlScope::Observe, ControlScope::Control]);
		let observing = ScopeSet::from_iter([ControlScope::Observe]);

		let (opened, controller_names) = welcome_for(controlling, (0, 0)).await;
		assert_eq!(controller_names, ["control", "observe"], "a fresh attach lists control");
		let (_, observer_names) = welcome_for(observing, (0, 0)).await;
		assert_eq!(observer_names, ["observe"], "a fresh attach does not list it");

		kernel.publish("after the detach");
		let cursor = (opened.epoch, opened.head_seq);
		let (_, controller_names) = welcome_for(controlling, cursor).await;
		assert_eq!(controller_names, ["control", "observe"], "a resumed attach lists control");
		let (_, observer_names) = welcome_for(observing, cursor).await;
		assert_eq!(observer_names, ["observe"], "a resumed attach does not list it");
	}

	/// A bridge that lost its subscription is dead; caching it would kill
	/// every later attach to a session that is still live.
	#[tokio::test]
	async fn a_closed_bridge_is_evicted_rather_than_reused() {
		let sessions = Arc::new(SessionRegistry::new());
		let (kernel, _) = registered(&sessions, "live", "/workspace/live");
		let wire = wire(Arc::clone(&sessions));
		let request =
			|| pb::AttachRequest { session_id: "live".to_owned(), ..pb::AttachRequest::default() };

		let mut first = wire
			.attach(owner(), request())
			.await
			.expect("attaching succeeds");
		assert!(!ready(&mut first).await.is_empty(), "the first attachment is served");
		// Hold the Arc, do not record its address: once the evicted bridge is
		// dropped the allocator is free to hand the same address back, and the
		// identity check below would then compare two different bridges equal.
		let cached = {
			let bridges = wire.bridges.lock();
			Arc::clone(
				bridges
					.get(&SessionId::new("live"))
					.expect("the bridge is cached"),
			)
		};

		let mut second = wire
			.attach(owner(), request())
			.await
			.expect("attaching succeeds");
		assert!(!ready(&mut second).await.is_empty(), "the second attachment is served");
		assert!(
			Arc::ptr_eq(&cached, &{
				let bridges = wire.bridges.lock();
				Arc::clone(
					bridges
						.get(&SessionId::new("live"))
						.expect("the bridge is cached"),
				)
			}),
			"a live bridge is shared, not rebuilt"
		);

		drop(first);
		drop(second);
		kernel.drop_subscribers();
		let closed = tokio::time::timeout(Duration::from_secs(5), async {
			loop {
				let bridge = {
					let bridges = wire.bridges.lock();
					Arc::clone(
						bridges
							.get(&SessionId::new("live"))
							.expect("the bridge is cached"),
					)
				};
				if bridge.closed().is_some() {
					return;
				}
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		})
		.await;
		assert!(closed.is_ok(), "the bridge notices its lost subscription");

		let mut third = wire
			.attach(owner(), request())
			.await
			.expect("attaching succeeds");
		let frames = ready(&mut third).await;
		assert!(
			matches!(frames.first(), Some(pb::attach_frame::Frame::Welcome(_))),
			"the session is still live, so a fresh bridge serves it, got {frames:?}"
		);
		assert!(
			!Arc::ptr_eq(&cached, &{
				let bridges = wire.bridges.lock();
				Arc::clone(
					bridges
						.get(&SessionId::new("live"))
						.expect("the bridge is cached"),
				)
			}),
			"the dead bridge was evicted"
		);
		assert!(cached.closed().is_some(), "the evicted bridge is the dead one");
	}

	#[tokio::test]
	async fn list_filters_by_project_root_and_fills_every_reference() {
		let sessions = Arc::new(SessionRegistry::new());
		let (_here, _) = registered(&sessions, "here", "/workspace/here");
		let (_elsewhere, _) = registered(&sessions, "elsewhere", "/workspace/elsewhere");
		let wire = wire(Arc::clone(&sessions));

		let all = wire
			.list(owner(), pb::ListSessionsRequest::default())
			.await
			.expect("listing succeeds");
		assert_eq!(all.sessions.len(), 2);

		let scoped = wire
			.list(owner(), pb::ListSessionsRequest { project_root: "/workspace/here".to_owned() })
			.await
			.expect("listing succeeds");
		let [only] = scoped.sessions.as_slice() else {
			panic!("one session lives under that root, got {scoped:?}");
		};
		assert_eq!(only.session_id, "here");
		assert_eq!(only.label, "here");
		assert_eq!(only.project_root, "/workspace/here");
		assert!(only.is_live);
		assert_eq!(only.epoch, 0, "no bridge has started for it yet");
	}

	#[tokio::test]
	async fn attach_emits_a_welcome_then_a_final_snapshot_chunk_then_events() {
		let sessions = Arc::new(SessionRegistry::new());
		let (mut kernel, _) = registered(&sessions, "live", "/workspace");
		let wire = wire(Arc::clone(&sessions));

		let mut stream = wire
			.attach(owner(), pb::AttachRequest {
				session_id: "live".to_owned(),
				..pb::AttachRequest::default()
			})
			.await
			.expect("attach succeeds");

		let opening = ready(&mut stream).await;
		let [pb::attach_frame::Frame::Welcome(welcome), rest @ ..] = opening.as_slice() else {
			panic!("an attachment opens with a welcome, got {opening:?}");
		};
		assert_eq!(welcome.label, "live");
		assert!(welcome.epoch != 0, "a started bridge has an epoch");
		let chunks = rest
			.iter()
			.map(|frame| match frame {
				pb::attach_frame::Frame::SnapshotChunk(chunk) => chunk,
				other => panic!("the welcome is followed by snapshot chunks, got {other:?}"),
			})
			.collect::<Vec<_>>();
		assert!(!chunks.is_empty(), "a snapshot always produces at least one chunk");
		assert!(chunks.last().expect("a chunk").r#final, "the last chunk is marked final");
		assert!(
			chunks.iter().rev().skip(1).all(|chunk| !chunk.r#final),
			"only the last chunk is final",
		);
		assert!(
			chunks
				.iter()
				.all(|chunk| chunk.bytes.len() <= SNAPSHOT_CHUNK_BYTES),
			"no chunk exceeds the frame bound",
		);

		kernel.publish("after the snapshot");
		let live = ready(&mut stream).await;
		let [pb::attach_frame::Frame::Event(event)] = live.as_slice() else {
			panic!("a published edit arrives as one event, got {live:?}");
		};
		assert_eq!(event.epoch, welcome.epoch);
		assert!(event.seq > welcome.head_seq, "an event advances the cursor");
	}

	#[tokio::test]
	async fn an_observer_without_host_local_scope_sees_provider_metadata_blanked() {
		let sessions = Arc::new(SessionRegistry::new());
		let (_kernel, _) = registered(&sessions, "live", "/workspace");
		let wire = wire(Arc::clone(&sessions));

		let snapshot_of = async |context: ControlContext| {
			let mut stream = wire
				.attach(context, pb::AttachRequest {
					session_id: "live".to_owned(),
					..pb::AttachRequest::default()
				})
				.await
				.expect("attach succeeds");
			let frames = ready(&mut stream).await;
			frames
				.into_iter()
				.filter_map(|frame| match frame {
					pb::attach_frame::Frame::SnapshotChunk(chunk) => Some(chunk.bytes),
					_ => None,
				})
				.fold(Vec::new(), |mut all, chunk| {
					all.extend_from_slice(&chunk);
					all
				})
		};

		let unredacted = String::from_utf8(snapshot_of(owner()).await).expect("utf-8 snapshot");
		assert!(
			unredacted.contains("secret-provider"),
			"a caller holding observe_host_local sees provider metadata",
		);

		let redacted = String::from_utf8(snapshot_of(observer()).await).expect("utf-8 snapshot");
		assert!(
			!redacted.contains("secret-provider"),
			"a caller without observe_host_local does not, got {redacted}",
		);
		assert!(redacted.contains("visible"), "redaction blanks the property, not the node");
	}

	#[tokio::test]
	async fn a_stale_cursor_resyncs_before_its_fresh_snapshot() {
		let sessions = Arc::new(SessionRegistry::new());
		let (_kernel, _) = registered(&sessions, "live", "/workspace");
		let wire = wire(Arc::clone(&sessions));

		let mut stream = wire
			.attach(owner(), pb::AttachRequest {
				session_id:   "live".to_owned(),
				resume_epoch: 1,
				resume_seq:   9_999,
			})
			.await
			.expect("attach succeeds");

		let frames = ready(&mut stream).await;
		let [pb::attach_frame::Frame::Resync(resync), pb::attach_frame::Frame::Welcome(_), rest @ ..] =
			frames.as_slice()
		else {
			panic!("a stale cursor resyncs before its welcome, got {frames:?}");
		};
		assert!(!resync.reason.is_empty(), "a resync says why");
		assert!(
			rest
				.iter()
				.any(|frame| matches!(frame, pb::attach_frame::Frame::SnapshotChunk(_))),
			"the welcome is followed by a fresh snapshot",
		);
	}

	/// A kernel drains its mailbox only inside a turn, so an idle session is
	/// served by whoever holds it between turns.
	#[tokio::test]
	async fn an_idle_kernel_is_served_through_the_owner_seam() {
		let sessions = Arc::new(SessionRegistry::new());
		let (kernel, _) = registered(&sessions, "live", "/workspace/live");
		let (idle_tx, idle_rx) = flume::unbounded::<flume::Sender<DomSubscription>>();
		// Replace the handle with one whose mailbox nobody drains: that is an
		// interactive session sitting between turns.
		let (stalled, stalled_mailbox) = flume::unbounded();
		let handle = sessions
			.lookup(SessionId::from_ref("live"))
			.expect("the session is registered");
		sessions.register(Str::new("live"), KernelHandle {
			up: stalled,
			idle_subscribe: Some(idle_tx),
			..handle
		});
		let seam = {
			let snapshot = kernel.dom.snapshot();
			let drain = kernel.drain.clone();
			tokio::spawn(async move {
				// A real owner answers from the session it holds; this one
				// answers from the same tree and event stream.
				while let Ok(reply) = idle_rx.recv_async().await {
					let _ = reply.send((snapshot.clone(), drain.clone()));
				}
			})
		};
		let wire = wire(Arc::clone(&sessions));

		let mut stream = wire
			.attach(owner(), pb::AttachRequest {
				session_id: "live".to_owned(),
				..pb::AttachRequest::default()
			})
			.await
			.expect("the owner seam serves the attachment");
		let frames = ready(&mut stream).await;
		assert!(
			matches!(frames.first(), Some(pb::attach_frame::Frame::Welcome(_))),
			"the attachment opens even though the kernel never answered, got {frames:?}"
		);
		assert!(
			stalled_mailbox.try_recv().is_ok(),
			"the mailbox was asked too, so a session mid-turn is still served"
		);
		seam.abort();
	}

	struct RegisteringHost {
		sessions: Arc<SessionRegistry>,
		register: bool,
	}

	#[tonic::async_trait]
	impl OpenHost for RegisteringHost {
		async fn open_new(&self, _project_root: &Path) -> Result<SessionId, SessionControlError> {
			if self.register {
				registered(&self.sessions, "01JNEW", "/work/new");
				Ok(SessionId::new("01JNEW"))
			} else {
				Ok(SessionId::new("not-registered"))
			}
		}
	}

	fn wire_with(sessions: Arc<SessionRegistry>, host: Arc<RegisteringHost>) -> SessionWire {
		SessionWire::new(
			sessions,
			BridgeLimits::default(),
			Arc::new(
				ControlVisibility::from_entries(["*/provider"]).expect("the seeded table parses"),
			),
			Duration::from_secs(60),
		)
		.with_open_host(host)
	}

	#[tokio::test]
	async fn open_new_returns_only_a_registered_session() {
		let sessions = Arc::new(SessionRegistry::new());
		let wire = wire_with(
			Arc::clone(&sessions),
			Arc::new(RegisteringHost { sessions: Arc::clone(&sessions), register: true }),
		);
		let opened = wire
			.open_session(owner(), pb::OpenSessionRequest {
				project_root: "/work/new".to_owned(),
				kind: Some(pb::open_session_request::Kind::NewSession(pb::NewSession {})),
				..pb::OpenSessionRequest::default()
			})
			.await
			.expect("a registered kernel is a session");
		assert_eq!(opened.session_id, "01JNEW");
		assert!(sessions.lookup(&SessionId::new("01JNEW")).is_some());

		let missing = wire_with(
			Arc::clone(&sessions),
			Arc::new(RegisteringHost { sessions: Arc::clone(&sessions), register: false }),
		);
		let refusal = missing
			.open_session(owner(), pb::OpenSessionRequest {
				project_root: "/work/new".to_owned(),
				kind: Some(pb::open_session_request::Kind::NewSession(pb::NewSession {})),
				..pb::OpenSessionRequest::default()
			})
			.await
			.expect_err("an unregistered id is not a session");
		assert!(matches!(refusal, SessionControlError::UnknownSession { .. }));
	}

	/// Mints a distinct id per call and counts them, so a second mint is
	/// visible rather than masked by a fake that always answers the same id.
	struct CountingHost {
		sessions: Arc<SessionRegistry>,
		opens:    Arc<std::sync::atomic::AtomicUsize>,
	}

	#[tonic::async_trait]
	impl OpenHost for CountingHost {
		async fn open_new(&self, _project_root: &Path) -> Result<SessionId, SessionControlError> {
			let n = self.opens.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
			// Suspends before registering, so a caller that reached here has
			// given every other caller a chance to reach it too. Without a
			// suspension point the host is atomic to the scheduler and a
			// concurrency test cannot observe overlap at all. An
			// implementation that released its guard before awaiting the
			// host lets the second caller in here; one that holds it leaves
			// the second blocked on the mutex, so the yield cannot deadlock.
			tokio::task::yield_now().await;
			// `SessionId` borrows its text, so the ids are literals rather
			// than a formatted counter. Distinctness is what the tests read.
			let id = ["01JNEW1", "01JNEW2", "01JNEW3"][n % 3];
			registered(&self.sessions, id, "/work/new");
			Ok(SessionId::new(id))
		}
	}

	/// A retry carries the key its first attempt used, and must answer the
	/// session that key already minted (ADR 0039: exactly-once as observed).
	/// Minting a second would strand the first, which stays live and attached
	/// to nothing the client can name. A key whose session is gone is stale
	/// and opens a fresh one, and an absent key never deduplicates at all.
	#[tokio::test]
	async fn a_retried_open_answers_the_session_its_key_already_minted() {
		let sessions = Arc::new(SessionRegistry::new());
		let opens = Arc::new(std::sync::atomic::AtomicUsize::new(0));
		let wire = SessionWire::new(
			Arc::clone(&sessions),
			BridgeLimits::default(),
			Arc::new(
				ControlVisibility::from_entries(["*/provider"]).expect("the seeded table parses"),
			),
			Duration::from_secs(60),
		)
		.with_open_host(Arc::new(CountingHost {
			sessions: Arc::clone(&sessions),
			opens:    Arc::clone(&opens),
		}));
		let keyed = |key: &str| pb::OpenSessionRequest {
			project_root: "/work/new".to_owned(),
			idempotency_key: key.to_owned(),
			kind: Some(pb::open_session_request::Kind::NewSession(pb::NewSession {})),
			..pb::OpenSessionRequest::default()
		};

		let first = wire
			.open_session(owner(), keyed("retry-me"))
			.await
			.expect("the first open mints a session");
		let again = wire
			.open_session(owner(), keyed("retry-me"))
			.await
			.expect("the retry answers rather than refusing");
		assert_eq!(first.session_id, again.session_id, "a retry is not a second session");
		assert_eq!(opens.load(std::sync::atomic::Ordering::SeqCst), 1, "the host was asked once");

		// A different key is a different intent, and no key cannot be matched
		// to anything, so neither deduplicates.
		wire
			.open_session(owner(), keyed("another"))
			.await
			.expect("a different key opens its own session");
		wire
			.open_session(owner(), keyed(""))
			.await
			.expect("an absent key always opens");
		assert_eq!(
			opens.load(std::sync::atomic::Ordering::SeqCst),
			3,
			"only the matching key deduplicated",
		);
	}

	/// Two retries of one key can arrive at once. The lookup and the mint are
	/// one critical section for exactly that reason: were the guard dropped
	/// before the host call, both would miss the map and both would open,
	/// which is the stranded session the key exists to prevent.
	#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
	async fn concurrent_retries_of_one_key_mint_once() {
		let sessions = Arc::new(SessionRegistry::new());
		let opens = Arc::new(std::sync::atomic::AtomicUsize::new(0));
		let wire = Arc::new(
			SessionWire::new(
				Arc::clone(&sessions),
				BridgeLimits::default(),
				Arc::new(
					ControlVisibility::from_entries(["*/provider"]).expect("the seeded table parses"),
				),
				Duration::from_secs(60),
			)
			.with_open_host(Arc::new(CountingHost {
				sessions: Arc::clone(&sessions),
				opens:    Arc::clone(&opens),
			})),
		);
		let race = |wire: Arc<SessionWire>| async move {
			wire
				.open_session(owner(), pb::OpenSessionRequest {
					project_root: "/work/new".to_owned(),
					idempotency_key: "raced".to_owned(),
					kind: Some(pb::open_session_request::Kind::NewSession(pb::NewSession {})),
					..pb::OpenSessionRequest::default()
				})
				.await
				.expect("both callers are answered")
		};
		let (left, right) = tokio::join!(race(Arc::clone(&wire)), race(Arc::clone(&wire)));

		assert_eq!(left.session_id, right.session_id, "both callers name one session");
		assert_eq!(opens.load(std::sync::atomic::Ordering::SeqCst), 1, "the host was asked once");
	}

	/// A host that answers an id it never registered has not opened anything.
	/// Recording the key against it would refuse every later retry instead of
	/// opening, so the mapping is committed only after registration holds.
	#[tokio::test]
	async fn a_failed_open_leaves_its_key_free() {
		let sessions = Arc::new(SessionRegistry::new());
		let wire = wire_with(
			Arc::clone(&sessions),
			Arc::new(RegisteringHost { sessions: Arc::clone(&sessions), register: false }),
		);
		let keyed = pb::OpenSessionRequest {
			project_root: "/work/new".to_owned(),
			idempotency_key: "first-failed".to_owned(),
			kind: Some(pb::open_session_request::Kind::NewSession(pb::NewSession {})),
			..pb::OpenSessionRequest::default()
		};
		wire
			.open_session(owner(), keyed.clone())
			.await
			.expect_err("an unregistered id is not a session");

		let refusal = wire
			.open_session(owner(), keyed)
			.await
			.expect_err("the key is free, so the retry opens again and fails again");
		assert!(
			matches!(refusal, SessionControlError::UnknownSession { .. }),
			"a stale mapping would have refused differently",
		);
	}

	/// The map is bounded by live sessions, not by opens ever made. Nothing
	/// else retires an entry, so without the sweep a key whose session has
	/// been removed stays forever and the map grows with every distinct key
	/// a client ever sends.
	#[tokio::test]
	async fn retiring_a_session_drops_the_key_that_minted_it() {
		let sessions = Arc::new(SessionRegistry::new());
		let opens = Arc::new(std::sync::atomic::AtomicUsize::new(0));
		let wire = SessionWire::new(
			Arc::clone(&sessions),
			BridgeLimits::default(),
			Arc::new(
				ControlVisibility::from_entries(["*/provider"]).expect("the seeded table parses"),
			),
			Duration::from_secs(60),
		)
		.with_open_host(Arc::new(CountingHost {
			sessions: Arc::clone(&sessions),
			opens:    Arc::clone(&opens),
		}));
		let keyed = |key: &str| pb::OpenSessionRequest {
			project_root: "/work/new".to_owned(),
			idempotency_key: key.to_owned(),
			kind: Some(pb::open_session_request::Kind::NewSession(pb::NewSession {})),
			..pb::OpenSessionRequest::default()
		};

		let first = wire
			.open_session(owner(), keyed("retired"))
			.await
			.expect("the first open");
		assert_eq!(wire.opened.lock().await.len(), 1, "the key that minted it is recorded");

		sessions.remove(&SessionId::from_ref(first.session_id.as_str()));
		// A second key, so the sweep is the only thing that can drop the
		// first: overwriting the same key would hide an unbounded map.
		wire
			.open_session(owner(), keyed("live"))
			.await
			.expect("a later open");

		let held = wire.opened.lock().await;
		assert_eq!(held.len(), 1, "the retired session's key is gone, not merely shadowed");
		assert!(held.contains_key(&Str::new("live")), "the live session keeps its key");
	}

	/// The RPC projection gates `Open` on `control`, but the authority is
	/// public and a second projection would reach it directly. A caller holding
	/// only the read scopes must be refused here, not merely at the seam that
	/// happens to front it today, and the host must never be asked to mint
	/// anything for it.
	#[tokio::test]
	async fn opening_without_the_control_scope_is_refused_at_the_authority() {
		let sessions = Arc::new(SessionRegistry::new());
		let host = Arc::new(RegisteringHost { sessions: Arc::clone(&sessions), register: true });
		let wire = wire_with(Arc::clone(&sessions), Arc::clone(&host));
		let observing = ControlContext::new(
			"device",
			ScopeSet::from_iter([ControlScope::Observe, ControlScope::ObserveHostLocal]),
		);

		let refusal = wire
			.open_session(observing, pb::OpenSessionRequest {
				project_root: "/work/new".to_owned(),
				kind: Some(pb::open_session_request::Kind::NewSession(pb::NewSession {})),
				..pb::OpenSessionRequest::default()
			})
			.await
			.expect_err("an observe grant does not open a session");

		assert!(matches!(refusal, SessionControlError::MissingScope {
			scope: ControlScope::Control,
		}));
		assert!(
			sessions.lookup(&SessionId::new("01JNEW")).is_none(),
			"a refused open never reaches the host"
		);
	}

	/// Resume and fork are not served by this host, and an id it does not hold
	/// is no session, so both answer `UnknownSession` rather than invent one.
	#[tokio::test]
	async fn resuming_or_forking_a_session_that_is_not_live_is_unknown() {
		let sessions = Arc::new(SessionRegistry::new());
		let wire = wire(Arc::clone(&sessions));

		for kind in [
			pb::open_session_request::Kind::Resume(pb::ResumeSession {}),
			pb::open_session_request::Kind::Fork(pb::ForkSession::default()),
		] {
			let refusal = wire
				.open_session(owner(), pb::OpenSessionRequest {
					session_id: "absent".to_owned(),
					kind: Some(kind),
					..pb::OpenSessionRequest::default()
				})
				.await
				.expect_err("resume and fork need a session this host holds");
			assert!(
				matches!(refusal, SessionControlError::UnknownSession { .. }),
				"an id that is not live is unknown, got {refusal:?}"
			);
		}
	}

	/// The scope check runs before the registry lookup, for every kind. A
	/// caller without `Control` must not learn which ids are live by comparing
	/// `MissingScope` against `UnknownSession`, so resume and fork answer the
	/// scope first even though they are refused either way. Moving the check
	/// below the kind branch would turn this verb into an id oracle, and the
	/// `NewSession` tests alone would not notice.
	#[tokio::test]
	async fn resume_and_fork_answer_the_missing_scope_before_the_registry() {
		let sessions = Arc::new(SessionRegistry::new());
		let wire = wire(Arc::clone(&sessions));
		let observing = ControlContext::new(
			"device",
			ScopeSet::from_iter([ControlScope::Observe, ControlScope::ObserveHostLocal]),
		);

		for kind in [
			pb::open_session_request::Kind::Resume(pb::ResumeSession {}),
			pb::open_session_request::Kind::Fork(pb::ForkSession::default()),
		] {
			let refusal = wire
				.open_session(observing.clone(), pb::OpenSessionRequest {
					session_id: "absent".to_owned(),
					kind: Some(kind),
					..pb::OpenSessionRequest::default()
				})
				.await
				.expect_err("an observe grant does not open a session");

			assert!(
				matches!(refusal, SessionControlError::MissingScope { scope: ControlScope::Control }),
				"the scope is answered before the registry, got {refusal:?}"
			);
		}
	}

	#[tokio::test]
	async fn attaching_to_an_unknown_session_is_not_found() {
		let sessions = Arc::new(SessionRegistry::new());
		let wire = wire(Arc::clone(&sessions));

		let refusal = wire
			.attach(owner(), pb::AttachRequest {
				session_id: "absent".to_owned(),
				..pb::AttachRequest::default()
			})
			.await
			.err()
			.expect("an unknown session cannot be attached");
		assert!(matches!(refusal, SessionControlError::UnknownSession { .. }));
	}

	/// ADR 0039 A4 decides what the owner socket grants. `admin` is "never
	/// grantable" and `approve_persist` is "never granted" by default, so the
	/// grant is a list, not `ControlScope::VARIANTS`: deriving it would hand a
	/// later variant authority nobody chose. Every scope is a row below, so a
	/// variant added to the enum fails here until someone decides whether the
	/// owner holds it, and a swapped scope fails where a count would still pass.
	#[cfg(unix)]
	#[test]
	fn the_owner_socket_grants_only_its_decided_scopes() {
		use std::collections::BTreeMap;

		let request = owner_identity(tonic::Request::new(()))
			.expect("a caller that reached the owner socket is the owner");
		let context = request
			.extensions()
			.get::<ControlContext>()
			.expect("the interceptor inserts the caller's identity");

		let held = ControlScope::VARIANTS
			.iter()
			.map(|&scope| (scope, context.scopes().contains(scope)))
			.collect::<BTreeMap<_, _>>();
		assert_eq!(
			held,
			BTreeMap::from([
				(ControlScope::Observe, true),
				(ControlScope::ObserveHostLocal, true),
				(ControlScope::Control, true),
			]),
			"the owner socket holds exactly the scopes ADR 0039 A4 decided",
		);
	}

	#[tokio::test]
	async fn a_truncated_stream_faults_and_a_detached_one_does_not() {
		assert!(
			closure_status(Closure::Detached).is_none(),
			"the host detaching on its own terms ends the stream successfully",
		);
		for truncated in
			[Closure::SubscriptionLost, Closure::Replica, Closure::Encode, Closure::Panicked]
		{
			assert!(
				closure_status(truncated).is_some(),
				"{truncated} truncated the stream, so the client must learn it faulted",
			);
		}
	}
}
