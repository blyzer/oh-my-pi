//! Tonic transport projection over a remote session control authority.
//!
//! The authority is a trait declared here and implemented by the composition
//! layer, so this crate never names a driver, kernel, or DOM type. Attach
//! frames carry canonical `omp_dom` bytes the authority produced; serve only
//! moves them.
//!
//! R1 (ADR 0039) serves the two read verbs and `Open` of a new session, which
//! needs the `control` scope. Resume, fork, and the remaining mutating verbs
//! are answered with `unimplemented` rather than half-wired: on a versioned
//! wire that is the correct response for a verb this revision does not grant.
//!
//! R2 serves `Submit`, authorized per command arm (ADR 0039 A4) by
//! [`ControlContext::require_submit`], approval decisions under `approve` and
//! [`ControlContext::require_approve`], and UI answers under `answer_ui`, each
//! before the authority is consulted.

use std::sync::Arc;

use omp_agent::{ApprovalError, ApprovalScope, UnknownApprovalScope};
use omp_core::Str;
use omp_journal::idempotency::{IdempotencyKey, IdempotencyKeyError};
use omp_proto::session::v1 as pb;
use strum::{Display, EnumString, IntoStaticStr, VariantArray};
use tonic::{Request, Response, Status};

use crate::inference::RpcStream;

/// Authorization scope a control principal may hold.
///
/// ADR 0039 section A4 defines the full ladder (`observe`,
/// `observe_host_local`, `prompt`, `interrupt`, `approve`, `approve_persist`,
/// `control`, `worker`, `admin`). Only scopes for served or imminent verbs are
/// declared here; the remaining variants land with the verbs they authorize.
/// `answer_ui` is a separate grant for runtime-chosen UI requests.
/// Parsing rejects every string outside this set rather than ignoring a grant
/// naming a future scope.
#[derive(
	Clone,
	Copy,
	Debug,
	Display,
	EnumString,
	Eq,
	Hash,
	IntoStaticStr,
	Ord,
	PartialEq,
	PartialOrd,
	VariantArray,
)]
#[strum(serialize_all = "snake_case")]
pub enum ControlScope {
	/// `ListSessions`, and `Attach` with host-local props redacted.
	Observe,
	/// Unredacted snapshots and events: cwd, env facts, advisor subtrees, and
	/// raw provider metadata.
	ObserveHostLocal,
	/// Submit prompts, steering, and queued follow-ups.
	Prompt,
	/// Interrupt, abort tools, or pause a turn.
	Interrupt,
	/// Decide an approval for at most the current session.
	Approve,
	/// Additionally permits persisted approval decisions.
	ApprovePersist,
	/// Answer a pending host UI request.
	AnswerUi,
	/// `Open` of a new session, and the mutation verbs that land with it.
	Control,
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

	/// Requires what deciding an approval with this lifetime needs: `approve`,
	/// and for [`ApprovalScope::Persist`] also `approve_persist`, which adds to
	/// `approve` rather than replacing it.
	///
	/// # Errors
	/// [`SessionControlError::MissingScope`] naming the first missing grant.
	pub fn require_approve(&self, lifetime: &ApprovalScope) -> Result<(), SessionControlError> {
		if !self.scopes.contains(ControlScope::Approve) {
			return Err(SessionControlError::MissingScope { scope: ControlScope::Approve });
		}
		if matches!(lifetime, ApprovalScope::Persist)
			&& !self.scopes.contains(ControlScope::ApprovePersist)
		{
			return Err(SessionControlError::MissingScope { scope: ControlScope::ApprovePersist });
		}
		Ok(())
	}

	/// Requires the scope ADR 0039 A4 assigns to this command's arm: `prompt`
	/// for prompt, steer and queue, `interrupt` for interrupt, abort tools and
	/// pause, `control` for cancel. It reads the request alone, so it runs
	/// before any registry lookup and a refusal says nothing about sessions.
	pub fn require_submit(&self, request: &pb::ControlCommand) -> Result<(), SessionControlError> {
		use pb::control_command::Command;
		if !self.scopes.contains(ControlScope::Observe) {
			return Err(SessionControlError::MissingScope { scope: ControlScope::Observe });
		}
		let scope = match request
			.command
			.as_ref()
			.ok_or(SessionControlError::MissingCommand)?
		{
			Command::Prompt(_) | Command::Steer(_) | Command::Queue(_) => ControlScope::Prompt,
			Command::Interrupt(_) | Command::AbortTools(_) | Command::Pause(_) => {
				ControlScope::Interrupt
			},
			Command::Cancel(_) => ControlScope::Control,
		};
		if self.scopes.contains(scope) {
			Ok(())
		} else {
			Err(SessionControlError::MissingScope { scope })
		}
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
	/// The requested approval lifetime is not a named scope.
	#[error(transparent)]
	InvalidApprovalScope {
		/// Strict parser failure.
		#[from]
		source: UnknownApprovalScope,
	},
	/// The idempotency key is empty or longer than the durable bound.
	#[error(transparent)]
	InvalidIdempotencyKey {
		/// Strict parser failure.
		#[from]
		source: IdempotencyKeyError,
	},
	/// No approval with this ticket is pending: it is unknown, already
	/// decided, or withdrawn.
	#[error("approval ticket {ticket_id} is not pending")]
	ApprovalNotPending {
		/// The requested ticket.
		ticket_id: Str,
	},
	/// The session refused the decision.
	#[error(transparent)]
	Approval {
		/// Typed approval failure.
		#[from]
		source: ApprovalError,
	},
	/// The session has no UI route, so no request on it can wait for an
	/// answer.
	#[error("session {session_id} has no UI answer route")]
	NoUiRoute {
		/// The requested session.
		session_id: Str,
	},
	/// No UI request with this identity is waiting on the session.
	///
	/// This conflates three cases the route cannot tell apart: an identity it
	/// never registered, a request already answered, and a request whose turn
	/// was interrupted. Separating them needs either a bounded record of
	/// settled identities written when a waiting entry is removed, or an
	/// outcome enum returned by `AskRoute::answer` in place of its `bool`.
	#[error("UI request {request_id} is not waiting")]
	UiRequestNotWaiting {
		/// The UI request identity.
		request_id: Str,
	},
	/// The UI answer names no request.
	#[error("the UI request identity is empty")]
	InvalidUiRequestId,
	/// The UI answer payload is empty or does not decode.
	///
	/// The payload encoding is provisional (ADR 0039 R2), so the source names
	/// the decoder in use rather than a stable wire contract.
	#[error("the answer for UI request {request_id} does not decode")]
	InvalidUiAnswer {
		/// The UI request identity.
		request_id: Str,
		/// The provisional decoder's failure.
		#[source]
		source:     serde_json::Error,
	},
	/// A mutating verb arrived with no session selected.
	#[error("no session is selected")]
	NoSessionSelected,
	/// Submit did not contain a command.
	#[error("no submit command is selected")]
	MissingCommand,
	/// The kernel cannot consume commands while no turn is active.
	#[error("the session is idle")]
	SessionIdle,
	/// An idle-only prompt was submitted during an active turn.
	#[error("the session already has an active turn")]
	SessionBusy,
	/// A timed-out mutation may have committed; retry using the same key.
	#[error("the mutation outcome is unknown; retry with the same idempotency key")]
	MutationOutcomeUnknown,
	/// A command carried a stale bridge epoch.
	#[error("expected session epoch {expected}, current epoch is {actual}")]
	EpochMismatch {
		/// The epoch supplied by the client.
		expected: u64,
		/// The epoch currently known to the host.
		actual:   u64,
	},
	/// The session refused the mutation: a key reused for another mutation, an
	/// invalid key, or a failed write.
	#[error("the session rejected the mutation")]
	Mutation {
		/// The typed session failure.
		#[source]
		source: omp_session::SessionError,
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
	/// | [`SessionControlError::SessionIdle`], [`SessionControlError::SessionBusy`], [`SessionControlError::EpochMismatch`] | `failed_precondition` |
	/// | [`SessionControlError::MissingCommand`] | `invalid_argument` |
	/// | [`SessionControlError::MutationOutcomeUnknown`] | `deadline_exceeded` |
	/// | [`SessionControlError::Mutation`] | `already_exists`, `invalid_argument`, or `internal` |
	/// | [`SessionControlError::OutOfGrant`] | `permission_denied` |
	/// | [`SessionControlError::MissingScope`] | `permission_denied` |
	/// | [`SessionControlError::NoUiRoute`] | `unimplemented` |
	/// | [`SessionControlError::UiRequestNotWaiting`] | `not_found` |
	/// | [`SessionControlError::InvalidUiRequestId`] | `invalid_argument` |
	/// | [`SessionControlError::InvalidUiAnswer`] | `invalid_argument` |
	/// | [`SessionControlError::NoSessionSelected`] | `invalid_argument` |
	/// | [`SessionControlError::InvalidApprovalScope`] | `invalid_argument` |
	/// | [`SessionControlError::InvalidIdempotencyKey`] | `invalid_argument` |
	/// | [`SessionControlError::ApprovalNotPending`] | `not_found` |
	/// | [`SessionControlError::Approval`] | `already_exists` for a reused key, else `internal` |
	/// | [`SessionControlError::HostUnavailable`] | `unavailable` |
	/// | [`SessionControlError::Backend`] | `internal` |
	/// The codes are diagnostic, not protective. R1 serves one principal, the
	/// owner, over a filesystem-authenticated socket, so a caller already knows
	/// which sessions exist and `not_found` discloses nothing it could not read
	/// from the session directory. Once a device holds a grant over a subset of
	/// sessions, that stops being true: `UnknownSession` and `OutOfGrant` must
	/// then answer the same code and the same text, or the pair becomes an
	/// oracle for session ids outside the grant.
	fn from(error: SessionControlError) -> Self {
		let message = error.to_string();
		match error {
			SessionControlError::UnknownSession { .. } => Self::not_found(message),
			SessionControlError::SessionNotLive { .. } => Self::failed_precondition(message),
			SessionControlError::OutOfGrant { .. } | SessionControlError::MissingScope { .. } => {
				Self::permission_denied(message)
			},
			SessionControlError::NoUiRoute { .. } => Self::unimplemented(message),
			SessionControlError::UiRequestNotWaiting { .. } => Self::not_found(message),
			SessionControlError::InvalidUiRequestId
			| SessionControlError::InvalidUiAnswer { .. }
			| SessionControlError::NoSessionSelected
			| SessionControlError::MissingCommand
			| SessionControlError::InvalidApprovalScope { .. }
			| SessionControlError::InvalidIdempotencyKey { .. } => Self::invalid_argument(message),
			SessionControlError::SessionIdle
			| SessionControlError::SessionBusy
			| SessionControlError::EpochMismatch { .. } => Self::failed_precondition(message),
			SessionControlError::ApprovalNotPending { .. } => Self::not_found(message),
			SessionControlError::Approval {
				source: ApprovalError::Session(omp_session::SessionError::KeyConflict { .. }),
			} => Self::already_exists(message),
			SessionControlError::Approval { .. } | SessionControlError::Backend(_) => {
				Self::internal(message)
			},
			SessionControlError::MutationOutcomeUnknown => Self::deadline_exceeded(message),
			SessionControlError::Mutation {
				source: omp_session::SessionError::KeyConflict { .. },
			} => Self::already_exists(message),
			SessionControlError::Mutation {
				source: omp_session::SessionError::InvalidIdempotencyKey(_),
			} => Self::invalid_argument(message),
			SessionControlError::Mutation { .. } => Self::internal(message),
			SessionControlError::HostUnavailable => Self::unavailable(message),
		}
	}
}

/// Authoritative session reads, installed by the composition layer.
///
/// Every verb receives the authenticated [`ControlContext`] alongside the wire
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

	/// Decides a pending approval as the caller's principal.
	///
	/// `lifetime` and `key` arrive parsed from the wire. An answer the
	/// authority stops waiting for is
	/// [`SessionControlError::MutationOutcomeUnknown`], never a refusal: the
	/// decision may still commit, and a retry under the same key reports it.
	async fn approve(
		&self,
		context: ControlContext,
		request: pb::ApproveRequest,
		lifetime: ApprovalScope,
		key: IdempotencyKey,
	) -> Result<pb::CommandReceipt, SessionControlError>;

	/// Delivers an answer to a UI request waiting on a live session.
	///
	/// Answering appends nothing to the journal, so the receipt's `seq` is zero
	/// and its `entry_id` empty. A retried answer finds nothing waiting and is
	/// [`SessionControlError::UiRequestNotWaiting`].
	async fn answer_ui(
		&self,
		context: ControlContext,
		request: pb::UiAnswer,
	) -> Result<pb::CommandReceipt, SessionControlError>;

	/// Opens a new session. Resume and fork of an id that is not live is
	/// [`SessionControlError::UnknownSession`]. This does not submit a prompt.
	async fn open_session(
		&self,
		context: ControlContext,
		request: pb::OpenSessionRequest,
	) -> Result<pb::SessionRef, SessionControlError>;
	/// Submits a command to a live session after checking its command scope.
	async fn submit(
		&self,
		context: ControlContext,
		request: pb::ControlCommand,
	) -> Result<pb::CommandReceipt, SessionControlError>;
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
		"session control does not serve {verb} in this revision (ADR 0039)"
	))
}

/// A mutating verb needs a selected session. `Open` of a new session is the
/// exception.
fn require_session(session_id: &str) -> Result<(), Status> {
	if session_id.is_empty() {
		Err(SessionControlError::NoSessionSelected.into())
	} else {
		Ok(())
	}
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

/// Extracts the identity and checks the control scope.
///
/// `Open` mints a session, so an observe grant is not enough: a read-only
/// principal that could create one would escape the ceiling its grant names
/// (ADR 0039 A4). The observe check still runs first, so a principal with no
/// read scope is refused for the scope it is most obviously missing.
fn controller<T>(request: &Request<T>) -> Result<ControlContext, Status> {
	let context = observer(request)?;
	if context.scopes().contains(ControlScope::Control) {
		Ok(context)
	} else {
		Err(SessionControlError::MissingScope { scope: ControlScope::Control }.into())
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
		request: Request<pb::ControlCommand>,
	) -> Result<Response<pb::CommandReceipt>, Status> {
		let context = observer(&request)?;
		require_session(request.get_ref().session_id.as_str())?;
		context.require_submit(request.get_ref())?;
		let receipt = self.authority.submit(context, request.into_inner()).await?;
		Ok(Response::new(receipt))
	}

	async fn approve(
		&self,
		request: Request<pb::ApproveRequest>,
	) -> Result<Response<pb::CommandReceipt>, Status> {
		let context = observer(&request)?;
		require_session(request.get_ref().session_id.as_str())?;
		// Authorized before parsing, so a caller without `approve` learns
		// nothing about how its input would have been read.
		if !context.scopes().contains(ControlScope::Approve) {
			return Err(SessionControlError::MissingScope { scope: ControlScope::Approve }.into());
		}
		let request = request.into_inner();
		let lifetime =
			ApprovalScope::parse_external(&request.scope).map_err(SessionControlError::from)?;
		context.require_approve(&lifetime)?;
		let key =
			IdempotencyKey::parse(&request.idempotency_key).map_err(SessionControlError::from)?;
		let receipt = self
			.authority
			.approve(context, request, lifetime, key)
			.await?;
		Ok(Response::new(receipt))
	}

	async fn answer_ui(
		&self,
		request: Request<pb::UiAnswer>,
	) -> Result<Response<pb::CommandReceipt>, Status> {
		let context = observer(&request)?;
		require_session(request.get_ref().session_id.as_str())?;
		if !context.scopes().contains(ControlScope::AnswerUi) {
			return Err(SessionControlError::MissingScope { scope: ControlScope::AnswerUi }.into());
		}
		let receipt = self
			.authority
			.answer_ui(context, request.into_inner())
			.await?;
		Ok(Response::new(receipt))
	}

	async fn open(
		&self,
		request: Request<pb::OpenSessionRequest>,
	) -> Result<Response<pb::SessionRef>, Status> {
		let inner = request.get_ref();
		let opens_new = matches!(inner.kind, Some(pb::open_session_request::Kind::NewSession(_)));
		if !opens_new {
			let _context = observer(&request)?;
			require_session(inner.session_id.as_str())?;
			return Err(read_only("Open"));
		}
		let context = controller(&request)?;
		let opened = self
			.authority
			.open_session(context, request.into_inner())
			.await?;
		Ok(Response::new(opened))
	}
}

#[cfg(test)]
mod tests {
	use std::{str::FromStr as _, sync::Arc};

	use futures::StreamExt as _;
	use omp_agent::{ApprovalError, ApprovalScope};
	use omp_journal::{EntryId, idempotency::IdempotencyKey};
	use omp_session::SessionError;
	use tonic::{Code, Request, Status, metadata::MetadataValue};

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

		async fn approve(
			&self,
			context: ControlContext,
			_request: pb::ApproveRequest,
			_lifetime: ApprovalScope,
			key: IdempotencyKey,
		) -> Result<pb::CommandReceipt, SessionControlError> {
			self.record(context)?;
			Ok(pb::CommandReceipt {
				idempotency_key: key.as_str().to_owned(),
				accepted: true,
				seq: 1,
				..pb::CommandReceipt::default()
			})
		}

		async fn answer_ui(
			&self,
			context: ControlContext,
			request: pb::UiAnswer,
		) -> Result<pb::CommandReceipt, SessionControlError> {
			self.record(context)?;
			Ok(pb::CommandReceipt {
				idempotency_key: request.idempotency_key,
				accepted: true,
				..pb::CommandReceipt::default()
			})
		}

		async fn open_session(
			&self,
			context: ControlContext,
			request: pb::OpenSessionRequest,
		) -> Result<pb::SessionRef, SessionControlError> {
			self.record(context)?;
			Err(SessionControlError::UnknownSession {
				session_id: omp_core::Str::new(request.session_id.as_str()),
			})
		}

		async fn submit(
			&self,
			context: ControlContext,
			request: pb::ControlCommand,
		) -> Result<pb::CommandReceipt, SessionControlError> {
			self.record(context)?;
			Ok(pb::CommandReceipt { idempotency_key: request.idempotency_key, ..Default::default() })
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
	async fn mutating_verbs_without_a_context_are_unauthenticated() {
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
		assert_eq!(codes, [Code::Unauthenticated; 4]);
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

	/// `Open` mints a session, so the observe grant that serves every read verb
	/// must not reach it (ADR 0039 A4). A control grant does, which is how this
	/// separates the scope check from the verb being refused outright.
	#[tokio::test]
	async fn opening_a_new_session_needs_the_control_scope() {
		let rpc = SessionControlRpc::new(FakeAuthority::ok());
		let new_session = pb::OpenSessionRequest {
			kind: Some(pb::open_session_request::Kind::NewSession(pb::NewSession::default())),
			..pb::OpenSessionRequest::default()
		};

		let observing = authenticated(
			new_session.clone(),
			ControlContext::new("device", ScopeSet::from_iter([ControlScope::Observe])),
		);
		assert_eq!(rpc.open(observing).await.unwrap_err().code(), Code::PermissionDenied);

		let controlling = authenticated(
			new_session,
			ControlContext::new(
				"device",
				ScopeSet::from_iter([ControlScope::Observe, ControlScope::Control]),
			),
		);
		// The fake answers `UnknownSession`, so reaching it at all proves the
		// gate passed rather than the verb being refused.
		assert_eq!(rpc.open(controlling).await.unwrap_err().code(), Code::NotFound);
	}

	/// Only a new session is served. Resume and fork answer `UNIMPLEMENTED`
	/// even to a caller holding `control`, and never reach the authority.
	#[tokio::test]
	async fn resume_and_fork_are_refused_even_for_a_caller_holding_control() {
		let authority = FakeAuthority::ok();
		let rpc = SessionControlRpc::new(Arc::clone(&authority) as Arc<dyn SessionControlAuthority>);
		let controlling = ControlContext::new(
			"device",
			ScopeSet::from_iter([ControlScope::Observe, ControlScope::Control]),
		);

		for kind in [
			pb::open_session_request::Kind::Resume(pb::ResumeSession::default()),
			pb::open_session_request::Kind::Fork(pb::ForkSession::default()),
		] {
			let request = authenticated(
				pb::OpenSessionRequest {
					session_id: "01JSESSION".to_owned(),
					kind: Some(kind),
					..pb::OpenSessionRequest::default()
				},
				controlling.clone(),
			);
			assert_eq!(rpc.open(request).await.unwrap_err().code(), Code::Unimplemented);
		}
		assert!(authority.seen.lock().is_none(), "a refused verb never reaches the authority");
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
			(SessionControlError::NoSessionSelected, Code::InvalidArgument),
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
		assert_eq!(ControlScope::from_str("control").expect("control parses"), ControlScope::Control);
		for name in ["prompt", "interrupt", "approve", "approve_persist", "answer_ui"] {
			assert_eq!(
				ControlScope::from_str(name)
					.expect("scope parses")
					.to_string(),
				name
			);
		}
		assert!(ControlScope::from_str("admin").is_err());

		let granted = ScopeSet::from_iter([ControlScope::Observe]);
		assert!(granted.contains(ControlScope::Observe));
		assert!(!granted.contains(ControlScope::ObserveHostLocal));
		assert!(!granted.contains(ControlScope::Control));
		assert!(ScopeSet::EMPTY.is_empty());
	}
	#[test]
	fn submit_scope_is_selected_per_command_arm() {
		use pb::control_command::Command;
		let request = |command| pb::ControlCommand {
			session_id: "present-or-absent".to_owned(),
			command: Some(command),
			..Default::default()
		};
		let prompt = ControlContext::new(
			"device",
			ScopeSet::from_iter([ControlScope::Observe, ControlScope::Prompt]),
		);
		let interrupt = ControlContext::new(
			"device",
			ScopeSet::from_iter([ControlScope::Observe, ControlScope::Interrupt]),
		);
		let steer = || request(Command::Steer(pb::SteerCommand { text: "redirect".to_owned() }));
		let stop = || request(Command::Interrupt(pb::InterruptCommand {}));
		assert!(prompt.require_submit(&steer()).is_ok());
		assert!(matches!(
			prompt.require_submit(&stop()),
			Err(SessionControlError::MissingScope { scope: ControlScope::Interrupt })
		));
		assert!(interrupt.require_submit(&stop()).is_ok());
		assert!(matches!(
			interrupt.require_submit(&steer()),
			Err(SessionControlError::MissingScope { scope: ControlScope::Prompt })
		));
		let cancel = request(Command::Cancel(pb::CancelCommand {}));
		assert!(matches!(
			prompt.require_submit(&cancel),
			Err(SessionControlError::MissingScope { scope: ControlScope::Control })
		));
	}

	#[tokio::test]
	async fn observer_cannot_submit_a_steer() {
		let rpc = SessionControlRpc::new(FakeAuthority::ok());
		let request = authenticated(
			pb::ControlCommand {
				session_id: "01JSESSION".to_owned(),
				idempotency_key: "key".to_owned(),
				command: Some(pb::control_command::Command::Steer(pb::SteerCommand {
					text: "redirect".to_owned(),
				})),
				..pb::ControlCommand::default()
			},
			observing("observer"),
		);
		assert_eq!(rpc.submit(request).await.unwrap_err().code(), Code::PermissionDenied);
	}

	#[tokio::test]
	async fn mutating_verbs_refuse_when_no_session_is_selected() {
		let rpc = SessionControlRpc::new(FakeAuthority::ok());
		let submit = authenticated(pb::ControlCommand::default(), observing("owner"));
		assert_eq!(rpc.submit(submit).await.unwrap_err().code(), Code::InvalidArgument);
		let approve = authenticated(pb::ApproveRequest::default(), observing("owner"));
		assert_eq!(rpc.approve(approve).await.unwrap_err().code(), Code::InvalidArgument);
		let answer = authenticated(pb::UiAnswer::default(), observing("owner"));
		assert_eq!(rpc.answer_ui(answer).await.unwrap_err().code(), Code::InvalidArgument);
		let open = authenticated(pb::OpenSessionRequest::default(), observing("owner"));
		assert_eq!(rpc.open(open).await.unwrap_err().code(), Code::InvalidArgument);

		let named = authenticated(
			pb::ControlCommand {
				session_id: "01JSESSION".to_owned(),
				..pb::ControlCommand::default()
			},
			observing("owner"),
		);
		assert_eq!(rpc.submit(named).await.unwrap_err().code(), Code::InvalidArgument);
	}

	fn approval_request(scope: &str) -> pb::ApproveRequest {
		pb::ApproveRequest {
			session_id: "live".to_owned(),
			idempotency_key: "request-1".to_owned(),
			ticket_id: "ticket-1".to_owned(),
			approved: true,
			scope: scope.to_owned(),
			..pb::ApproveRequest::default()
		}
	}

	fn approval_context(scopes: &[ControlScope]) -> ControlContext {
		ControlContext::new("remote-principal", scopes.iter().copied().collect())
	}

	#[tokio::test]
	async fn approve_holder_decides_a_session_scoped_approval() {
		let authority = FakeAuthority::ok();
		let rpc = SessionControlRpc::new(Arc::clone(&authority) as Arc<dyn SessionControlAuthority>);
		let request = authenticated(
			approval_request("session"),
			approval_context(&[ControlScope::Observe, ControlScope::Approve]),
		);
		assert!(
			rpc.approve(request)
				.await
				.expect("approval reaches authority")
				.into_inner()
				.accepted
		);
		assert_eq!(authority.principal(), "remote-principal");
	}

	#[tokio::test]
	async fn control_holder_without_approve_is_refused() {
		let rpc = SessionControlRpc::new(FakeAuthority::ok());
		let request = authenticated(
			approval_request("session"),
			approval_context(&[ControlScope::Observe, ControlScope::Control]),
		);
		let error = rpc
			.approve(request)
			.await
			.expect_err("control alone cannot approve");
		assert_eq!(error.code(), Code::PermissionDenied);
		assert!(error.message().contains("approve"));
	}

	#[tokio::test]
	async fn approve_persist_without_approve_is_refused() {
		let rpc = SessionControlRpc::new(FakeAuthority::ok());
		let request = authenticated(
			approval_request("persist"),
			approval_context(&[ControlScope::Observe, ControlScope::ApprovePersist]),
		);
		let error = rpc.approve(request).await.expect_err("persist is additive");
		assert_eq!(error.code(), Code::PermissionDenied);
		assert!(error.message().contains("approve scope"));
	}

	#[tokio::test]
	async fn approve_holder_cannot_decide_persist_and_error_names_approve_persist() {
		let rpc = SessionControlRpc::new(FakeAuthority::ok());
		let request = authenticated(
			approval_request("persist"),
			approval_context(&[ControlScope::Observe, ControlScope::Approve]),
		);
		let error = rpc
			.approve(request)
			.await
			.expect_err("persist needs the additional grant");
		assert_eq!(error.code(), Code::PermissionDenied);
		assert!(error.message().contains("approve_persist"));
	}

	#[tokio::test]
	async fn approve_and_approve_persist_holder_decides_persist() {
		let authority = FakeAuthority::ok();
		let rpc = SessionControlRpc::new(Arc::clone(&authority) as Arc<dyn SessionControlAuthority>);
		let request = authenticated(
			approval_request("persist"),
			approval_context(&[
				ControlScope::Observe,
				ControlScope::Approve,
				ControlScope::ApprovePersist,
			]),
		);
		assert!(
			rpc.approve(request)
				.await
				.expect("persist reaches authority")
				.into_inner()
				.accepted
		);
	}

	#[tokio::test]
	async fn unrecognized_approval_scope_is_refused_at_the_wire() {
		let authority = FakeAuthority::ok();
		let rpc = SessionControlRpc::new(Arc::clone(&authority) as Arc<dyn SessionControlAuthority>);
		for scope in ["", "unknown-scope"] {
			let request = authenticated(
				approval_request(scope),
				approval_context(&[ControlScope::Observe, ControlScope::Approve]),
			);
			let error = rpc
				.approve(request)
				.await
				.expect_err("unknown scope is refused");
			assert_eq!(error.code(), Code::InvalidArgument);
			assert!(error.message().contains(&format!("{scope:?}")));
		}
		assert!(authority.seen.lock().is_none(), "no unknown scope reaches the authority");
	}

	#[tokio::test]
	async fn observer_cannot_approve() {
		let authority = FakeAuthority::ok();
		let rpc = SessionControlRpc::new(Arc::clone(&authority) as Arc<dyn SessionControlAuthority>);
		let request = authenticated(approval_request("secret-invalid-scope"), observing("device"));
		let error = rpc
			.approve(request)
			.await
			.expect_err("observer cannot approve");
		assert_eq!(error.code(), Code::PermissionDenied);
		assert!(error.message().contains("approve"));
		assert!(!error.message().contains("secret-invalid-scope"));
		assert!(authority.seen.lock().is_none());
	}

	#[tokio::test]
	async fn an_invalid_idempotency_key_is_refused_before_the_authority() {
		let authority = FakeAuthority::ok();
		let rpc = SessionControlRpc::new(Arc::clone(&authority) as Arc<dyn SessionControlAuthority>);
		let oversized = "k".repeat(IdempotencyKey::MAX_BYTES + 1);
		for key in ["", oversized.as_str()] {
			let mut request = approval_request("session");
			key.clone_into(&mut request.idempotency_key);
			let request = authenticated(
				request,
				approval_context(&[ControlScope::Observe, ControlScope::Approve]),
			);
			let error = rpc
				.approve(request)
				.await
				.expect_err("an unusable key is refused");
			assert_eq!(error.code(), Code::InvalidArgument);
			assert!(error.message().starts_with("idempotency key is"), "{}", error.message());
		}
		assert!(authority.seen.lock().is_none(), "no unkeyed approval reaches the authority");
	}

	#[test]
	fn approval_failures_answer_stable_codes() {
		let code = |error: SessionControlError| Status::from(error).code();
		let reused = SessionError::KeyConflict {
			key:      IdempotencyKey::new_static("approve-7"),
			existing: EntryId::default(),
		};
		assert_eq!(code(SessionControlError::from(ApprovalError::from(reused))), Code::AlreadyExists);
		assert_eq!(code(SessionControlError::from(ApprovalError::MissingPrompts)), Code::Internal);
		assert_eq!(
			code(SessionControlError::ApprovalNotPending { ticket_id: "ticket-1".into() }),
			Code::NotFound
		);
		assert_eq!(code(SessionControlError::MutationOutcomeUnknown), Code::DeadlineExceeded);
		assert_eq!(
			code(SessionControlError::EpochMismatch { expected: 3, actual: 4 }),
			Code::FailedPrecondition
		);
	}

	fn ui_answer_request() -> pb::UiAnswer {
		pb::UiAnswer {
			session_id: "live".to_owned(),
			idempotency_key: "answer-1".to_owned(),
			request_id: "call-1".to_owned(),
			..pb::UiAnswer::default()
		}
	}

	#[tokio::test]
	async fn answer_ui_holder_reaches_the_authority() {
		let authority = FakeAuthority::ok();
		let rpc = SessionControlRpc::new(Arc::clone(&authority) as Arc<dyn SessionControlAuthority>);
		let holder = ControlContext::new(
			"device",
			ScopeSet::from_iter([ControlScope::Observe, ControlScope::AnswerUi]),
		);
		let receipt = rpc
			.answer_ui(authenticated(ui_answer_request(), holder))
			.await
			.expect("an answer_ui holder reaches the authority")
			.into_inner();
		assert_eq!(receipt.idempotency_key, "answer-1");
		assert_eq!(authority.principal(), "device");
	}

	#[tokio::test]
	async fn answer_ui_is_refused_without_its_own_scope() {
		for (holder, scopes) in [
			("prompt", [ControlScope::Observe, ControlScope::Prompt].as_slice()),
			("control", [ControlScope::Observe, ControlScope::Control].as_slice()),
			("observer", [ControlScope::Observe].as_slice()),
		] {
			let authority = FakeAuthority::ok();
			let rpc =
				SessionControlRpc::new(Arc::clone(&authority) as Arc<dyn SessionControlAuthority>);
			let context = ControlContext::new("device", scopes.iter().copied().collect());
			let error = rpc
				.answer_ui(authenticated(ui_answer_request(), context))
				.await
				.expect_err("answer_ui is a grant of its own");
			assert_eq!(error.code(), Code::PermissionDenied, "{holder}");
			assert_eq!(error.message(), "the caller does not hold the answer_ui scope", "{holder}");
			assert!(authority.seen.lock().is_none(), "a {holder} holder never reaches the authority");
		}
	}
}
