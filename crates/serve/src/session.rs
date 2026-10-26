//! Tonic transport projection over a remote session control authority.
//!
//! The authority is a trait declared here and implemented by the composition
//! layer, so this crate never names a driver, kernel, or DOM type. Attach
//! frames carry canonical `omp_dom` bytes the authority produced; serve only
//! moves them.
//!
//! R1 (ADR 0039) serves the two read verbs. The four mutating verbs are
//! answered with `unimplemented` rather than half-wired: on a versioned wire
//! that is the correct response for a verb this revision does not grant.

use std::sync::Arc;

use omp_core::Str;
use omp_proto::session::v1 as pb;
use strum::{Display, EnumString, IntoStaticStr};
use tonic::{Request, Response, Status};

use crate::inference::RpcStream;

/// Authorization scope a control principal may hold.
///
/// ADR 0039 section A4 defines the full ladder (`observe`,
/// `observe_host_local`, `prompt`, `interrupt`, `approve`, `approve_persist`,
/// `control`, `worker`, `admin`). R1 grants no mutation verb, so only the two
/// read scopes are declared here; the remaining variants land with the verbs
/// they authorize. Parsing rejects every string outside this set, so a grant
/// naming a future scope is refused rather than silently ignored.
#[derive(
	Clone, Copy, Debug, Display, EnumString, Eq, Hash, IntoStaticStr, Ord, PartialEq, PartialOrd,
)]
#[strum(serialize_all = "snake_case")]
pub enum ControlScope {
	/// `ListSessions`, and `Attach` with host-local props redacted.
	Observe,
	/// Unredacted snapshots and events: cwd, env facts, advisor subtrees, and
	/// raw provider metadata.
	ObserveHostLocal,
}

impl ControlScope {
	/// Bit this scope occupies in a [`ScopeSet`].
	const fn bit(self) -> u16 {
		1 << self as u16
	}
}

/// The set of scopes a principal was granted, as a copyable bitset.
///
/// [`ControlScope`] is closed, so a grant fits in a machine word and a context
/// clone costs no allocation on the per-request path.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScopeSet(u16);

impl ScopeSet {
	/// A grant holding no scope at all.
	pub const EMPTY: Self = Self(0);

	/// Reports whether this grant holds `scope`.
	pub const fn contains(self, scope: ControlScope) -> bool {
		self.0 & scope.bit() != 0
	}

	/// Reports whether this grant holds no scope.
	pub const fn is_empty(self) -> bool {
		self.0 == 0
	}
}

impl FromIterator<ControlScope> for ScopeSet {
	fn from_iter<I: IntoIterator<Item = ControlScope>>(scopes: I) -> Self {
		Self(scopes.into_iter().fold(0, |bits, scope| bits | scope.bit()))
	}
}

/// Transport-authenticated caller identity for one control request.
///
/// A trusted interceptor establishes the principal from the transport (owner
/// UDS ownership, or a device challenge on TCP) and inserts this value into
/// tonic request extensions. Handlers read identity only from here: the
/// session wire carries no principal field, and metadata a client sets is
/// never consulted, so a caller cannot assert who it is.
#[derive(Clone, Debug)]
pub struct ControlContext {
	principal: Str,
	scopes:    ScopeSet,
}

impl ControlContext {
	/// Binds an authenticated principal to the scopes its grant allows.
	pub fn new(principal: impl Into<Str>, scopes: ScopeSet) -> Self {
		Self { principal: principal.into(), scopes }
	}

	/// The authenticated principal this request speaks for.
	pub const fn principal(&self) -> &Str {
		&self.principal
	}

	/// The scopes this principal holds.
	pub const fn scopes(&self) -> ScopeSet {
		self.scopes
	}
}

/// A control authority failure, mapped once to a gRPC status at this boundary.
#[derive(Debug, thiserror::Error)]
pub enum SessionControlError {
	/// No session with this id is known to the host.
	#[error("no session {session_id}")]
	UnknownSession {
		/// The requested session.
		session_id: Str,
	},
	/// The session exists but has no live bridge to attach to.
	#[error("session {session_id} has no live bridge")]
	SessionNotLive {
		/// The requested session.
		session_id: Str,
	},
	/// The session falls outside the caller's granted session set.
	#[error("session {session_id} is outside the caller's grant")]
	OutOfGrant {
		/// The requested session.
		session_id: Str,
	},
	/// The verb requires a scope the caller was not granted.
	#[error("the caller does not hold the {scope} scope")]
	MissingScope {
		/// The scope the verb requires.
		scope: ControlScope,
	},
	/// The host is shutting down or its session store is not reachable.
	#[error("the session host is unavailable")]
	HostUnavailable,
	/// The authority failed for a reason the transport cannot classify.
	#[error("the session authority failed")]
	Backend(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
}

impl From<SessionControlError> for Status {
	/// Stable mapping, so a client may branch on the code rather than the text:
	///
	/// | Failure | Code |
	/// | --- | --- |
	/// | [`SessionControlError::UnknownSession`] | `not_found` |
	/// | [`SessionControlError::SessionNotLive`] | `failed_precondition` |
	/// | [`SessionControlError::OutOfGrant`] | `permission_denied` |
	/// | [`SessionControlError::MissingScope`] | `permission_denied` |
	/// | [`SessionControlError::HostUnavailable`] | `unavailable` |
	/// | [`SessionControlError::Backend`] | `internal` |
	///
	/// `OutOfGrant` and `MissingScope` share `permission_denied` deliberately:
	/// the wire must not disclose whether an unreachable session exists.
	fn from(error: SessionControlError) -> Self {
		let message = error.to_string();
		match error {
			SessionControlError::UnknownSession { .. } => Self::not_found(message),
			SessionControlError::SessionNotLive { .. } => Self::failed_precondition(message),
			SessionControlError::OutOfGrant { .. } | SessionControlError::MissingScope { .. } => {
				Self::permission_denied(message)
			},
			SessionControlError::HostUnavailable => Self::unavailable(message),
			SessionControlError::Backend(_) => Self::internal(message),
		}
	}
}

/// Authoritative session reads, installed by the composition layer.
///
/// Both verbs receive the authenticated [`ControlContext`] alongside the wire
/// request; the implementation applies the caller's session set and redacts
/// host-local props unless [`ControlScope::ObserveHostLocal`] is held.
#[tonic::async_trait]
pub trait SessionControlAuthority: Send + Sync + 'static {
	/// Lists the sessions this caller may observe.
	async fn list(
		&self,
		context: ControlContext,
		request: pb::ListSessionsRequest,
	) -> Result<pb::SessionList, SessionControlError>;

	/// Opens an attachment stream: welcome, snapshot chunks, events, resync
	/// notices, and heartbeats, in that order.
	async fn attach(
		&self,
		context: ControlContext,
		request: pb::AttachRequest,
	) -> Result<RpcStream<pb::AttachFrame>, SessionControlError>;
}

/// gRPC projection of [`SessionControlAuthority`].
#[derive(Clone)]
pub struct SessionControlRpc {
	authority: Arc<dyn SessionControlAuthority>,
}

impl SessionControlRpc {
	/// Serves session control from the given authority.
	pub fn new(authority: Arc<dyn SessionControlAuthority>) -> Self {
		Self { authority }
	}
}

/// Rejection for a verb this revision does not serve.
fn read_only(verb: &str) -> Status {
	Status::unimplemented(format!(
		"session control R1 is read-only: {verb} is not served; only ListSessions and Attach are \
		 (ADR 0039)"
	))
}

/// Extracts the interceptor-inserted identity and checks the read scope.
///
/// Absence means the request reached a handler without passing an
/// authenticating interceptor, which is a transport misconfiguration, not a
/// caller error: it answers `unauthenticated` and never falls back to the wire.
fn observer<T>(request: &Request<T>) -> Result<ControlContext, Status> {
	let context = request
		.extensions()
		.get::<ControlContext>()
		.cloned()
		.ok_or_else(|| {
			tracing::warn!(
				rpc.service = "session_control",
				"request carried no authenticated context"
			);
			Status::unauthenticated("session control requires a transport-authenticated identity")
		})?;
	if context.scopes().contains(ControlScope::Observe) {
		Ok(context)
	} else {
		Err(SessionControlError::MissingScope { scope: ControlScope::Observe }.into())
	}
}

#[tonic::async_trait]
impl pb::session_control_server::SessionControl for SessionControlRpc {
	type AttachStream = RpcStream<pb::AttachFrame>;

	async fn list_sessions(
		&self,
		request: Request<pb::ListSessionsRequest>,
	) -> Result<Response<pb::SessionList>, Status> {
		let context = observer(&request)?;
		let sessions = self.authority.list(context, request.into_inner()).await?;
		Ok(Response::new(sessions))
	}

	async fn attach(
		&self,
		request: Request<pb::AttachRequest>,
	) -> Result<Response<Self::AttachStream>, Status> {
		let context = observer(&request)?;
		let frames = self.authority.attach(context, request.into_inner()).await?;
		Ok(Response::new(frames))
	}

	async fn submit(
		&self,
		_request: Request<pb::ControlCommand>,
	) -> Result<Response<pb::CommandReceipt>, Status> {
		Err(read_only("Submit"))
	}

	async fn approve(
		&self,
		_request: Request<pb::ApproveRequest>,
	) -> Result<Response<pb::CommandReceipt>, Status> {
		Err(read_only("Approve"))
	}

	async fn answer_ui(
		&self,
		_request: Request<pb::UiAnswer>,
	) -> Result<Response<pb::CommandReceipt>, Status> {
		Err(read_only("AnswerUi"))
	}

	async fn open(
		&self,
		_request: Request<pb::OpenSessionRequest>,
	) -> Result<Response<pb::SessionRef>, Status> {
		Err(read_only("Open"))
	}
}

#[cfg(test)]
mod tests {
	use std::{str::FromStr as _, sync::Arc};

	use futures::StreamExt as _;
	use tonic::{Code, Request, metadata::MetadataValue};

	use super::{
		ControlContext, ControlScope, RpcStream, ScopeSet, SessionControlAuthority,
		SessionControlError, SessionControlRpc, pb, pb::session_control_server::SessionControl as _,
	};

	/// Authority that records the context it was handed and replays a scripted
	/// outcome.
	struct FakeAuthority {
		seen:    parking_lot::Mutex<Option<ControlContext>>,
		failure: parking_lot::Mutex<Option<SessionControlError>>,
	}

	impl FakeAuthority {
		fn ok() -> Arc<Self> {
			Arc::new(Self {
				seen:    parking_lot::Mutex::new(None),
				failure: parking_lot::Mutex::new(None),
			})
		}

		fn failing(failure: SessionControlError) -> Arc<Self> {
			Arc::new(Self {
				seen:    parking_lot::Mutex::new(None),
				failure: parking_lot::Mutex::new(Some(failure)),
			})
		}

		fn principal(&self) -> String {
			self
				.seen
				.lock()
				.as_ref()
				.expect("authority was reached")
				.principal()
				.as_str()
				.to_owned()
		}

		fn record(&self, context: ControlContext) -> Result<(), SessionControlError> {
			*self.seen.lock() = Some(context);
			self.failure.lock().take().map_or(Ok(()), Err)
		}
	}

	#[tonic::async_trait]
	impl SessionControlAuthority for FakeAuthority {
		async fn list(
			&self,
			context: ControlContext,
			request: pb::ListSessionsRequest,
		) -> Result<pb::SessionList, SessionControlError> {
			self.record(context)?;
			Ok(pb::SessionList {
				sessions: vec![pb::SessionRef {
					session_id: "01JSESSION".to_owned(),
					project_root: request.project_root,
					..pb::SessionRef::default()
				}],
			})
		}

		async fn attach(
			&self,
			context: ControlContext,
			request: pb::AttachRequest,
		) -> Result<RpcStream<pb::AttachFrame>, SessionControlError> {
			self.record(context)?;
			let welcome = pb::AttachFrame {
				frame: Some(pb::attach_frame::Frame::Welcome(pb::WelcomeFrame {
					epoch:          7,
					head_seq:       11,
					label:          request.session_id,
					granted_scopes: vec![ControlScope::Observe.to_string()],
				})),
			};
			Ok(Box::pin(futures::stream::once(async move { Ok(welcome) })))
		}
	}

	fn observing(principal: &str) -> ControlContext {
		ControlContext::new(principal, ScopeSet::from_iter([ControlScope::Observe]))
	}

	fn authenticated<T>(message: T, context: ControlContext) -> Request<T> {
		let mut request = Request::new(message);
		request.extensions_mut().insert(context);
		request
	}

	#[tokio::test]
	async fn mutating_verbs_are_unimplemented_in_a_read_only_revision() {
		let rpc = SessionControlRpc::new(FakeAuthority::ok());
		let codes = [
			rpc.submit(Request::new(pb::ControlCommand::default()))
				.await
				.unwrap_err()
				.code(),
			rpc.approve(Request::new(pb::ApproveRequest::default()))
				.await
				.unwrap_err()
				.code(),
			rpc.answer_ui(Request::new(pb::UiAnswer::default()))
				.await
				.unwrap_err()
				.code(),
			rpc.open(Request::new(pb::OpenSessionRequest::default()))
				.await
				.unwrap_err()
				.code(),
		];
		assert_eq!(codes, [Code::Unimplemented; 4]);
	}

	#[tokio::test]
	async fn list_passes_the_authenticated_context_and_returns_the_authority_result() {
		let authority = FakeAuthority::ok();
		let rpc = SessionControlRpc::new(Arc::clone(&authority) as Arc<dyn SessionControlAuthority>);
		let request = authenticated(
			pb::ListSessionsRequest { project_root: "/work/omp".to_owned() },
			observing("owner"),
		);

		let sessions = rpc
			.list_sessions(request)
			.await
			.expect("observe grant lists")
			.into_inner();

		assert_eq!(authority.principal(), "owner");
		assert_eq!(sessions.sessions.len(), 1);
		assert_eq!(sessions.sessions[0].project_root, "/work/omp");
	}

	#[tokio::test]
	async fn attach_passes_the_authenticated_context_and_streams_the_authority_frames() {
		let authority = FakeAuthority::ok();
		let rpc = SessionControlRpc::new(Arc::clone(&authority) as Arc<dyn SessionControlAuthority>);
		let request = authenticated(
			pb::AttachRequest { session_id: "01JSESSION".to_owned(), ..pb::AttachRequest::default() },
			observing("owner"),
		);

		let mut frames = rpc
			.attach(request)
			.await
			.expect("observe grant attaches")
			.into_inner();
		let frame = frames
			.next()
			.await
			.expect("welcome frame")
			.expect("frame is not an error");

		assert_eq!(authority.principal(), "owner");
		let pb::attach_frame::Frame::Welcome(welcome) = frame.frame.expect("frame payload") else {
			panic!("the first frame must be the welcome");
		};
		assert_eq!(welcome.epoch, 7);
		assert_eq!(welcome.label, "01JSESSION");
		assert!(frames.next().await.is_none());
	}

	#[tokio::test]
	async fn caller_asserted_identity_cannot_reach_the_authority() {
		let authority = FakeAuthority::ok();
		let rpc = SessionControlRpc::new(Arc::clone(&authority) as Arc<dyn SessionControlAuthority>);
		let mut request = authenticated(pb::ListSessionsRequest::default(), observing("owner"));
		request
			.metadata_mut()
			.insert("omp-principal", MetadataValue::from_static("forged"));

		rpc.list_sessions(request)
			.await
			.expect("observe grant lists");

		assert_eq!(authority.principal(), "owner");
	}

	#[tokio::test]
	async fn a_request_without_an_interceptor_context_is_unauthenticated() {
		let rpc = SessionControlRpc::new(FakeAuthority::ok());
		let mut request = Request::new(pb::ListSessionsRequest::default());
		request
			.metadata_mut()
			.insert("omp-principal", MetadataValue::from_static("forged"));

		assert_eq!(rpc.list_sessions(request).await.unwrap_err().code(), Code::Unauthenticated,);
	}

	#[tokio::test]
	async fn a_grant_without_the_observe_scope_is_denied() {
		let rpc = SessionControlRpc::new(FakeAuthority::ok());
		let request = authenticated(
			pb::ListSessionsRequest::default(),
			ControlContext::new("device", ScopeSet::EMPTY),
		);

		assert_eq!(rpc.list_sessions(request).await.unwrap_err().code(), Code::PermissionDenied,);
	}

	#[tokio::test]
	async fn authority_failures_map_to_the_documented_codes() {
		let session_id = omp_core::Str::from("01JSESSION");
		let cases = [
			(SessionControlError::UnknownSession { session_id: session_id.clone() }, Code::NotFound),
			(
				SessionControlError::SessionNotLive { session_id: session_id.clone() },
				Code::FailedPrecondition,
			),
			(SessionControlError::OutOfGrant { session_id }, Code::PermissionDenied),
			(
				SessionControlError::MissingScope { scope: ControlScope::ObserveHostLocal },
				Code::PermissionDenied,
			),
			(SessionControlError::HostUnavailable, Code::Unavailable),
			(
				SessionControlError::Backend(Box::new(std::io::Error::other("journal is sealed"))),
				Code::Internal,
			),
		];

		for (failure, expected) in cases {
			let rpc = SessionControlRpc::new(FakeAuthority::failing(failure));
			let request = authenticated(pb::ListSessionsRequest::default(), observing("owner"));
			assert_eq!(rpc.list_sessions(request).await.unwrap_err().code(), expected);
		}
	}

	#[test]
	fn scopes_round_trip_and_reject_names_this_revision_does_not_grant() {
		assert_eq!(ControlScope::ObserveHostLocal.to_string(), "observe_host_local");
		assert_eq!(ControlScope::from_str("observe").expect("observe parses"), ControlScope::Observe);
		assert!(ControlScope::from_str("control").is_err());

		let granted = ScopeSet::from_iter([ControlScope::Observe]);
		assert!(granted.contains(ControlScope::Observe));
		assert!(!granted.contains(ControlScope::ObserveHostLocal));
		assert!(ScopeSet::EMPTY.is_empty());
	}
}
