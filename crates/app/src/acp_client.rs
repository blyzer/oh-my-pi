//! Agent → client requests over ACP (ADR 0037 §1).
//!
//! The editor's `initialize.params.clientCapabilities` parse into typed
//! [`ClientCapabilities`]. Every request the agent sends the client
//! (`session/request_permission`, `fs/read_text_file`, `fs/write_text_file`)
//! goes through the one correlation table behind [`AcpClient`], keyed by
//! JSON-RPC id. Editor file-system requests carry the `sv_acp_fs_timeout`
//! deadline, are gated by `sv_acp_fs` and by the live session's eligibility,
//! and are retired when their caller is dropped, their session ends, or the
//! transport closes; a response whose id is no longer pending is dropped.
//!
//! While the live session is eligible and the editor advertises
//! `fs.readTextFile`, the connection binds the editor into the project
//! environment as the document base (ADR 0037 §1.2, [`EditorDocumentsHost`]);
//! it rebinds on every session switch and unbinds on `session/close`, a
//! capability change that removes it, and transport loss.

use std::{
	future::Future,
	path::{Component, Path, PathBuf},
	pin::Pin,
	sync::{Arc, Weak},
	time::Duration as StdDuration,
};

use omp_con::Ctx;
use omp_core::{Duration, DurationUnit, FastHashMap, Str};
use omp_envd::docs::{AcpDocumentBackend, EditorIoError};
use parking_lot::Mutex;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use tokio::{sync::oneshot, time};

/// Operator policy for editor file I/O over ACP `fs/*` (`sv_acp_fs`).
#[derive(
	Clone,
	Copy,
	Debug,
	Default,
	Eq,
	PartialEq,
	strum::Display,
	strum::EnumString,
	strum::IntoStaticStr,
	strum::VariantNames,
)]
#[strum(serialize_all = "snake_case")]
pub enum AcpFs {
	/// Use the editor's buffers whenever the client advertises the `fs`
	/// capability and the session is eligible.
	#[default]
	Auto,
	/// Never send `fs/*`, whatever the client advertises.
	Off,
}

omp_con::con_enum!(AcpFs);

/// Default deadline for one ACP `fs/*` request.
const DEFAULT_FS_TIMEOUT: Duration = Duration::new(5, DurationUnit::Seconds);

omp_con::var! {
	/// Editor file I/O over ACP (ADR 0037). `auto` reads and writes through the
	/// editor's `fs/read_text_file` and `fs/write_text_file` when the client
	/// advertises them and the ACP session's cwd matched the project root. `off`
	/// never sends `fs/*`: the escape hatch for a remote editor whose paths are
	/// not the host's. Read once per ACP connection.
	pub static SV_ACP_FS = sv_acp_fs: AcpFs {
		default: AcpFs::Auto,
		flags: archive,
	};
	/// Deadline for each ACP `fs/*` request to the editor, reads and writes
	/// alike. An unanswered request is retired, its late answer is dropped, and
	/// the caller falls back to disk. Permission prompts wait for the user and
	/// are not bounded by it. Read once per ACP connection.
	pub static SV_ACP_FS_TIMEOUT = sv_acp_fs_timeout: Duration {
		default: DEFAULT_FS_TIMEOUT,
		flags: archive,
	};
}

/// Connection-scoped ACP settings, read from the process context once per
/// connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcpSettings {
	/// Editor file-system policy (`sv_acp_fs`).
	pub fs:         AcpFs,
	/// Deadline for each `fs/*` request (`sv_acp_fs_timeout`).
	pub fs_timeout: StdDuration,
}

impl Default for AcpSettings {
	fn default() -> Self {
		Self { fs: AcpFs::Auto, fs_timeout: StdDuration::from_secs(5) }
	}
}

impl AcpSettings {
	/// Resolves the ACP settings from the process control context.
	#[must_use]
	pub fn from_con(ctx: &Ctx) -> Self {
		Self {
			fs:         SV_ACP_FS.get(ctx),
			// A span too large for `std` is as good as unbounded.
			fs_timeout: SV_ACP_FS_TIMEOUT
				.get(ctx)
				.to_std()
				.unwrap_or(StdDuration::MAX),
		}
	}
}

/// `initialize.params.clientCapabilities`: what the editor offers the agent.
///
/// Parsing is lenient per field: an absent, `null`, or malformed capability is
/// not advertised, and unknown fields are ignored. A client never fails
/// `initialize` over its capabilities.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
	/// `fs`: the editor's text-document methods.
	#[serde(default, deserialize_with = "lenient")]
	pub fs:       FileSystemCapabilities,
	/// `terminal`: the `terminal/*` family. Parsed and never used (ADR 0037
	/// §6).
	#[serde(default, deserialize_with = "lenient")]
	pub terminal: bool,
	/// `auth`: client-side authentication support.
	#[serde(default, deserialize_with = "lenient")]
	pub auth:     AuthCapabilities,
}

/// `clientCapabilities.fs`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileSystemCapabilities {
	/// `fs.readTextFile`: the client answers `fs/read_text_file`.
	#[serde(default, deserialize_with = "lenient")]
	pub read_text_file:  bool,
	/// `fs.writeTextFile`: the client answers `fs/write_text_file`.
	#[serde(default, deserialize_with = "lenient")]
	pub write_text_file: bool,
}

/// `clientCapabilities.auth`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AuthCapabilities {
	/// `auth.terminal`: the client can launch the terminal auth method.
	#[serde(default, deserialize_with = "lenient")]
	pub terminal: bool,
}

impl ClientCapabilities {
	/// Parses the `clientCapabilities` member of `initialize` params. An
	/// absent or malformed member advertises nothing.
	#[must_use]
	pub fn from_initialize(params: &serde_json::Map<String, Value>) -> Self {
		params
			.get("clientCapabilities")
			.and_then(|capabilities| Self::deserialize(capabilities).ok())
			.unwrap_or_default()
	}
}

/// Deserializes `T`, or its default when the value has another shape.
fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
	D: Deserializer<'de>,
	T: Deserialize<'de> + Default,
{
	#[derive(Deserialize)]
	#[serde(untagged)]
	enum Field<T> {
		Typed(T),
		Other(serde::de::IgnoredAny),
	}
	Ok(match Field::<T>::deserialize(deserializer)? {
		Field::Typed(value) => value,
		Field::Other(_) => {
			tracing::debug!("ignoring a malformed ACP client capability");
			T::default()
		},
	})
}

/// A request method the agent sends the client.
#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::Display, strum::IntoStaticStr)]
pub enum ClientMethod {
	/// `session/request_permission`.
	#[strum(serialize = "session/request_permission")]
	RequestPermission,
	/// `fs/read_text_file`.
	#[strum(serialize = "fs/read_text_file")]
	ReadTextFile,
	/// `fs/write_text_file`.
	#[strum(serialize = "fs/write_text_file")]
	WriteTextFile,
}

/// Why an agent → client request produced no answer.
#[derive(Debug, thiserror::Error)]
pub enum ClientRequestError {
	/// `sv_acp_fs off`.
	#[error("editor file I/O is off (sv_acp_fs off)")]
	Disabled,
	/// The client did not advertise the capability behind the method.
	#[error("the ACP client did not advertise {method}")]
	NotAdvertised {
		/// Method the capability gates.
		method: ClientMethod,
	},
	/// No ACP session is open (before the first session, or after
	/// `session/close`).
	#[error("no ACP session is open")]
	NoSession,
	/// The live session never supplied a `cwd` matching the project root.
	#[error("the ACP session did not supply a cwd matching the project root")]
	SessionIneligible,
	/// The path is not an absolute, normalized path inside the project root.
	#[error("{} is not an absolute path inside the ACP project root", path.display())]
	OutsideProject {
		/// Path the caller asked for.
		path: PathBuf,
	},
	/// The path cannot be named on the JSON wire.
	#[error("{} is not valid UTF-8", path.display())]
	NonUtf8Path {
		/// Path the caller asked for.
		path: PathBuf,
	},
	/// The request could not be encoded.
	#[error("encoding {method} failed")]
	Encode {
		/// Method being encoded.
		method: ClientMethod,
		/// Encoder failure.
		#[source]
		source: serde_json::Error,
	},
	/// The deadline passed before the client answered.
	#[error("the ACP client did not answer {method} within {timeout:?}")]
	Timeout {
		/// Method that went unanswered.
		method:  ClientMethod,
		/// Deadline that elapsed.
		timeout: StdDuration,
	},
	/// The session the request named ended before the answer.
	#[error("the ACP session ended before the client answered {method}")]
	SessionEnded {
		/// Method that went unanswered.
		method: ClientMethod,
	},
	/// The transport closed before the answer.
	#[error("the ACP transport closed before the client answered {method}")]
	Disconnected {
		/// Method that went unanswered.
		method: ClientMethod,
	},
	/// The client answered with a JSON-RPC error.
	#[error("the ACP client refused {method} with JSON-RPC error {code}")]
	Refused {
		/// Method the client refused.
		method:  ClientMethod,
		/// JSON-RPC error code.
		code:    i64,
		/// Client-provided message; untrusted editor text.
		message: Str,
	},
	/// The client's answer does not match the method's result shape.
	#[error("the ACP client's answer to {method} is malformed")]
	Malformed {
		/// Method whose answer failed to decode.
		method: ClientMethod,
		/// Decoder failure.
		#[source]
		source: serde_json::Error,
	},
}

const _: () =
	assert!(size_of::<ClientRequestError>() <= 48, "ClientRequestError must stay compact");

/// A JSON-RPC error object from the client.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub(crate) struct RpcError {
	/// JSON-RPC error code.
	#[serde(default)]
	pub(crate) code:    i64,
	/// Client-provided message.
	#[serde(default)]
	pub(crate) message: Str,
}

/// One client response, split off its JSON-RPC envelope: the `result`
/// member, or the `error` object.
pub(crate) type Answer = Result<Value, RpcError>;

/// `fs/read_text_file` params. omp reads whole files only, so `line` and
/// `limit` are never sent.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadTextFileRequest<'a> {
	session_id: &'a str,
	path:       &'a str,
}

/// `fs/read_text_file` result.
#[derive(Deserialize)]
struct ReadTextFileResponse {
	content: Str,
}

/// `fs/write_text_file` params.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WriteTextFileRequest<'a> {
	session_id: &'a str,
	path:       &'a str,
	content:    &'a str,
}

/// `session/request_permission` params.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestPermissionRequest<'a> {
	session_id: &'a str,
	tool_call:  Value,
	options:    [PermissionOption; 4],
}

/// One `session/request_permission` option.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PermissionOption {
	option_id: PermissionOptionId,
	name:      &'static str,
	kind:      PermissionOptionId,
}

/// The permission options omp offers; each option's id is also its kind.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermissionOptionId {
	/// Allow this call.
	AllowOnce,
	/// Allow for the rest of the session.
	AllowAlways,
	/// Reject this call.
	RejectOnce,
	/// Reject for the rest of the session.
	RejectAlways,
	/// An option omp never offered.
	#[serde(other, skip_serializing)]
	Unknown,
}

const PERMISSION_OPTIONS: [PermissionOption; 4] = [
	PermissionOption {
		option_id: PermissionOptionId::AllowOnce,
		name:      "Allow once",
		kind:      PermissionOptionId::AllowOnce,
	},
	PermissionOption {
		option_id: PermissionOptionId::AllowAlways,
		name:      "Always allow",
		kind:      PermissionOptionId::AllowAlways,
	},
	PermissionOption {
		option_id: PermissionOptionId::RejectOnce,
		name:      "Reject",
		kind:      PermissionOptionId::RejectOnce,
	},
	PermissionOption {
		option_id: PermissionOptionId::RejectAlways,
		name:      "Always reject",
		kind:      PermissionOptionId::RejectAlways,
	},
];

/// `session/request_permission` result.
#[derive(Deserialize)]
pub(crate) struct RequestPermissionResponse {
	/// The user's answer.
	pub(crate) outcome: PermissionOutcome,
}

/// `RequestPermissionResponse.outcome`.
#[derive(Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(crate) enum PermissionOutcome {
	/// The user picked an option.
	Selected {
		/// Picked option.
		#[serde(rename = "optionId")]
		option_id: PermissionOptionId,
	},
	/// The prompt was cancelled.
	Cancelled,
}

/// Outbound request envelope.
#[derive(Serialize)]
struct OutboundRequest<P> {
	jsonrpc: &'static str,
	id:      u64,
	method:  &'static str,
	params:  P,
}

/// Who waits for a pending request's answer.
enum Waiter {
	/// A kernel approval ticket; the controller loop answers it in line, so
	/// the decision reaches the mailbox before any control frame the client
	/// sends after it.
	Approval(Str),
	/// An awaited [`AcpClient`] call.
	Reply(oneshot::Sender<Result<Value, ClientRequestError>>),
}

/// One outstanding request.
struct Pending {
	method: ClientMethod,
	waiter: Waiter,
}

/// The session the connection serves right now.
struct LiveSession {
	/// ACP session id requests name.
	id:          Str,
	/// Cleared by `session/close` until the next switch.
	open:        bool,
	/// The session was made live by a request whose `cwd` matched the
	/// project root (ADR 0037 §2).
	cwd_matched: bool,
}

/// The correlation table and the connection facts that gate it.
struct Table {
	next_id:      u64,
	pending:      FastHashMap<u64, Pending>,
	/// Outbound frame queue; `None` once the transport is gone, so the
	/// connection's writer drains and ends even while handles live on.
	output:       Option<flume::Sender<Value>>,
	capabilities: ClientCapabilities,
	session:      LiveSession,
	project_root: PathBuf,
}

impl Table {
	/// Queues one request to the client under a fresh id and registers
	/// `waiter` for its answer.
	fn send(
		&mut self,
		method: ClientMethod,
		waiter: Waiter,
		params: Value,
	) -> Result<(), ClientRequestError> {
		let Some(output) = &self.output else {
			return Err(ClientRequestError::Disconnected { method });
		};
		let id = self.next_id;
		let frame = serde_json::to_value(OutboundRequest {
			jsonrpc: "2.0",
			id,
			method: method.into(),
			params,
		})
		.map_err(|source| ClientRequestError::Encode { method, source })?;
		output
			.send(frame)
			.map_err(|_| ClientRequestError::Disconnected { method })?;
		self.next_id += 1;
		self.pending.insert(id, Pending { method, waiter });
		Ok(())
	}

	/// Checks that `method` may reach the editor now and returns the session
	/// id the request names.
	fn admit_fs(&self, method: ClientMethod, policy: AcpFs) -> Result<Str, ClientRequestError> {
		if self.output.is_none() {
			return Err(ClientRequestError::Disconnected { method });
		}
		if policy == AcpFs::Off {
			return Err(ClientRequestError::Disabled);
		}
		let advertised = match method {
			ClientMethod::ReadTextFile => self.capabilities.fs.read_text_file,
			ClientMethod::WriteTextFile => self.capabilities.fs.write_text_file,
			ClientMethod::RequestPermission => true,
		};
		if !advertised {
			return Err(ClientRequestError::NotAdvertised { method });
		}
		if !self.session.open {
			return Err(ClientRequestError::NoSession);
		}
		if !self.session.cwd_matched {
			return Err(ClientRequestError::SessionIneligible);
		}
		Ok(self.session.id.clone())
	}
}

struct Shared {
	table:     Mutex<Table>,
	settings:  AcpSettings,
	/// Where the editor is bound as the document base, and whether it is
	/// bound now.
	documents: Mutex<Option<(Arc<dyn EditorDocumentsHost>, bool)>>,
}

/// The project environment's editor binding, as the connection drives it
/// (ADR 0037 §1.2). Production binds through the driver's composition
/// ([`omp_envd::EditorDocuments`]); a test can observe the calls.
pub trait EditorDocumentsHost: Send + Sync {
	/// Binds `editor` as the document base for the live session, or unbinds
	/// it with `None`. Every bind starts unanchored.
	fn bind(&self, editor: Option<Arc<dyn AcpDocumentBackend>>);
}

impl EditorDocumentsHost for omp_envd::EditorDocuments {
	fn bind(&self, editor: Option<Arc<dyn AcpDocumentBackend>>) {
		Self::bind(self, editor);
	}
}

/// The editor's buffers as the environment reads them: `fs/read_text_file`
/// through this connection's gated request table.
///
/// Holds the connection weakly, so a binding left in the environment never
/// keeps a finished connection alive; a request after the connection is gone
/// fails as [`EditorIoError::Disconnected`].
struct EditorBuffers(Weak<Shared>);

impl EditorBuffers {
	fn client(&self) -> Result<AcpClient, EditorIoError> {
		self
			.0
			.upgrade()
			.map(AcpClient)
			.ok_or(EditorIoError::Disconnected)
	}
}

impl AcpDocumentBackend for EditorBuffers {
	fn deadline(&self) -> StdDuration {
		self
			.0
			.upgrade()
			.map_or(AcpSettings::default().fs_timeout, |shared| shared.settings.fs_timeout)
	}

	fn read_text(
		&self,
		absolute_path: Str,
	) -> Pin<Box<dyn Future<Output = Result<Str, EditorIoError>> + Send + '_>> {
		Box::pin(async move {
			self
				.client()?
				.read_text_file(Path::new(absolute_path.as_str()))
				.await
				.map_err(editor_io_error)
		})
	}

	fn write_text(
		&self,
		absolute_path: Str,
		content: Str,
	) -> Pin<Box<dyn Future<Output = Result<Str, EditorIoError>> + Send + '_>> {
		Box::pin(async move {
			self
				.client()?
				.write_text_file(Path::new(absolute_path.as_str()), content.as_str())
				.await
				.map_err(editor_io_error)?;
			Ok(content)
		})
	}
}

/// Classifies a failed editor request for the environment, which falls back
/// to disk whatever the cause; the cause itself is logged here, once.
fn editor_io_error(error: ClientRequestError) -> EditorIoError {
	tracing::debug!(error = &error as &dyn std::error::Error, "ACP editor request failed");
	match error {
		ClientRequestError::Disabled
		| ClientRequestError::NotAdvertised { .. }
		| ClientRequestError::NoSession
		| ClientRequestError::SessionIneligible
		| ClientRequestError::OutsideProject { .. }
		| ClientRequestError::NonUtf8Path { .. } => EditorIoError::Unavailable,
		ClientRequestError::Timeout { .. } => EditorIoError::Timeout,
		ClientRequestError::SessionEnded { .. } | ClientRequestError::Disconnected { .. } => {
			EditorIoError::Disconnected
		},
		ClientRequestError::Refused { .. } => EditorIoError::Refused,
		ClientRequestError::Encode { .. } | ClientRequestError::Malformed { .. } => {
			EditorIoError::Malformed
		},
	}
}

/// The editor on the other end of one ACP connection, as the agent reaches
/// it: capabilities, eligibility, and the `fs/*` requests.
///
/// Cloning is cheap; every clone shares the connection's correlation table.
#[derive(Clone)]
pub struct AcpClient(Arc<Shared>);

impl AcpClient {
	/// Creates the client side of a connection whose outbound frames go to
	/// `output`. No session is open and no capability is advertised until
	/// the controller loop records them.
	pub(crate) fn new(settings: AcpSettings, output: flume::Sender<Value>) -> Self {
		Self(Arc::new(Shared {
			table: Mutex::new(Table {
				next_id:      1,
				pending:      FastHashMap::default(),
				output:       Some(output),
				capabilities: ClientCapabilities::default(),
				session:      LiveSession {
					id:          Str::new_static(""),
					open:        false,
					cwd_matched: false,
				},
				project_root: PathBuf::new(),
			}),
			settings,
			documents: Mutex::new(None),
		}))
	}

	/// Binds this connection's editor into `host` whenever the live session
	/// may read editor buffers, and keeps that binding current from here on.
	pub(crate) fn bind_documents(&self, host: Arc<dyn EditorDocumentsHost>) {
		*self.0.documents.lock() = Some((host, false));
		self.rebind_documents();
	}

	/// Brings the environment's editor binding in line with the gate: bound
	/// (freshly, so unanchored) while `fs/read_text_file` would be sent,
	/// unbound otherwise.
	fn rebind_documents(&self) {
		let eligible = self.can_read();
		let mut documents = self.0.documents.lock();
		let Some((host, bound)) = documents.as_mut() else {
			return;
		};
		if !eligible && !*bound {
			return;
		}
		*bound = eligible;
		host.bind(
			eligible.then(|| {
				Arc::new(EditorBuffers(Arc::downgrade(&self.0))) as Arc<dyn AcpDocumentBackend>
			}),
		);
	}

	/// Connection settings.
	#[must_use]
	pub fn settings(&self) -> AcpSettings {
		self.0.settings
	}

	/// Capabilities from the latest `initialize`.
	#[must_use]
	pub fn capabilities(&self) -> ClientCapabilities {
		self.0.table.lock().capabilities
	}

	/// Whether an `fs/read_text_file` would be sent now.
	#[must_use]
	pub fn can_read(&self) -> bool {
		self
			.0
			.table
			.lock()
			.admit_fs(ClientMethod::ReadTextFile, self.0.settings.fs)
			.is_ok()
	}

	/// Whether an `fs/write_text_file` would be sent now.
	#[must_use]
	pub fn can_write(&self) -> bool {
		self
			.0
			.table
			.lock()
			.admit_fs(ClientMethod::WriteTextFile, self.0.settings.fs)
			.is_ok()
	}

	/// Number of requests awaiting an answer.
	#[must_use]
	pub fn pending(&self) -> usize {
		self.0.table.lock().pending.len()
	}

	/// Reads the editor's whole buffer for `path`, an absolute path inside the
	/// project root.
	///
	/// Dropping the future retires the request: its late answer is dropped.
	pub async fn read_text_file(&self, path: &Path) -> Result<Str, ClientRequestError> {
		let method = ClientMethod::ReadTextFile;
		let result = self
			.call(method, path, |session_id, path| {
				serde_json::to_value(ReadTextFileRequest { session_id, path })
			})
			.await?;
		ReadTextFileResponse::deserialize(&result)
			.map(|response| response.content)
			.map_err(|source| ClientRequestError::Malformed { method, source })
	}

	/// Replaces the editor's buffer for `path`, an absolute path inside the
	/// project root, with `content`. Whether the client also saves is
	/// client-defined.
	///
	/// Dropping the future retires the request: its late answer is dropped.
	pub async fn write_text_file(
		&self,
		path: &Path,
		content: &str,
	) -> Result<(), ClientRequestError> {
		self
			.call(ClientMethod::WriteTextFile, path, |session_id, path| {
				serde_json::to_value(WriteTextFileRequest { session_id, path, content })
			})
			.await
			.map(drop)
	}

	/// Sends one gated `fs/*` request and awaits its answer within the
	/// deadline.
	async fn call(
		&self,
		method: ClientMethod,
		path: &Path,
		params: impl FnOnce(&str, &str) -> serde_json::Result<Value>,
	) -> Result<Value, ClientRequestError> {
		let (id, reply) = {
			let mut table = self.0.table.lock();
			let session_id = table.admit_fs(method, self.0.settings.fs)?;
			let path = project_path(&table.project_root, path)?;
			let params = params(session_id.as_str(), path)
				.map_err(|source| ClientRequestError::Encode { method, source })?;
			let (tx, rx) = oneshot::channel();
			let id = table.next_id;
			table.send(method, Waiter::Reply(tx), params)?;
			(id, rx)
		};
		let _retire = Retire { shared: &self.0, id };
		let timeout = self.0.settings.fs_timeout;
		match time::timeout(timeout, reply).await {
			Ok(Ok(answer)) => answer,
			// The table dropped the sender without an answer: the connection is gone.
			Ok(Err(_)) => Err(ClientRequestError::Disconnected { method }),
			Err(_) => Err(ClientRequestError::Timeout { method, timeout }),
		}
	}

	/// Sends `session/request_permission` for a kernel approval ticket. It has
	/// no deadline: the user decides, and the answer returns through
	/// [`Self::answer`].
	pub(crate) fn request_permission(
		&self,
		ticket: Str,
		tool_call: Value,
	) -> Result<(), ClientRequestError> {
		let method = ClientMethod::RequestPermission;
		let mut table = self.0.table.lock();
		let params = serde_json::to_value(RequestPermissionRequest {
			session_id: table.session.id.as_str(),
			tool_call,
			options: PERMISSION_OPTIONS,
		})
		.map_err(|source| ClientRequestError::Encode { method, source })?;
		table.send(method, Waiter::Approval(ticket), params)
	}

	/// Routes a client response to the request it answers. An approval
	/// answer returns to the caller with its ticket; a call's answer wakes
	/// the call. A response whose id is not pending is dropped.
	pub(crate) fn answer(&self, id: &Value, answer: Answer) -> Option<(Str, Answer)> {
		let Some(pending) = id
			.as_u64()
			.and_then(|id| self.0.table.lock().pending.remove(&id))
		else {
			tracing::debug!(%id, "dropping an ACP response with no pending request");
			return None;
		};
		match pending.waiter {
			Waiter::Approval(ticket) => Some((ticket, answer)),
			Waiter::Reply(reply) => {
				let method = pending.method;
				let _ = reply.send(answer.map_err(|error| ClientRequestError::Refused {
					method,
					code: error.code,
					message: error.message,
				}));
				None
			},
		}
	}

	/// Records the capabilities of a (re-)`initialize`.
	pub(crate) fn initialize(&self, capabilities: ClientCapabilities) {
		let changed = {
			let mut table = self.0.table.lock();
			let changed = table.capabilities != capabilities;
			table.capabilities = capabilities;
			changed
		};
		if changed {
			self.rebind_documents();
		}
	}

	/// Makes `id` the live session. Calls that named the previous session
	/// fail with [`ClientRequestError::SessionEnded`].
	pub(crate) fn switch_session(&self, id: Str, cwd_matched: bool, project_root: &Path) {
		{
			let mut table = self.0.table.lock();
			retire_calls(&mut table, |method| ClientRequestError::SessionEnded { method });
			table.session = LiveSession { id, open: true, cwd_matched };
			if table.project_root != project_root {
				project_root.clone_into(&mut table.project_root);
			}
		}
		self.rebind_documents();
	}

	/// `session/close`: no `fs/*` request is sent until the next switch, and
	/// calls in flight fail with [`ClientRequestError::SessionEnded`].
	pub(crate) fn close_session(&self) {
		{
			let mut table = self.0.table.lock();
			retire_calls(&mut table, |method| ClientRequestError::SessionEnded { method });
			table.session.open = false;
		}
		self.rebind_documents();
	}

	/// The transport is gone: every call in flight fails with
	/// [`ClientRequestError::Disconnected`], approval tickets are forgotten,
	/// and later requests fail at once.
	pub(crate) fn disconnect(&self) {
		{
			let mut table = self.0.table.lock();
			table.output = None;
			retire_calls(&mut table, |method| ClientRequestError::Disconnected { method });
			table.pending.clear();
		}
		self.rebind_documents();
	}
}

/// Fails every awaited call in `table` with `error(method)`; approval tickets
/// stay pending.
fn retire_calls(table: &mut Table, error: impl Fn(ClientMethod) -> ClientRequestError) {
	let calls = table
		.pending
		.extract_if(|_, pending| matches!(pending.waiter, Waiter::Reply(_)));
	for (_, Pending { method, waiter }) in calls {
		if let Waiter::Reply(reply) = waiter {
			let _ = reply.send(Err(error(method)));
		}
	}
}

/// Removes a call's table entry when its future finishes or is dropped, so a
/// late answer finds nothing to wake.
struct Retire<'a> {
	shared: &'a Shared,
	id:     u64,
}

impl Drop for Retire<'_> {
	fn drop(&mut self) {
		self.shared.table.lock().pending.remove(&self.id);
	}
}

/// Checks that `path` is absolute, normalized, and inside `root`, and returns
/// its wire text. Canonical resolution stays with the document authority;
/// this only keeps a malformed path from ever reaching the client.
fn project_path<'p>(root: &Path, path: &'p Path) -> Result<&'p str, ClientRequestError> {
	let inside = path.is_absolute()
		&& !root.as_os_str().is_empty()
		&& path.starts_with(root)
		&& path
			.components()
			.all(|component| !matches!(component, Component::CurDir | Component::ParentDir));
	if !inside {
		return Err(ClientRequestError::OutsideProject { path: path.to_path_buf() });
	}
	path
		.to_str()
		.ok_or_else(|| ClientRequestError::NonUtf8Path { path: path.to_path_buf() })
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::*;

	fn capabilities(value: Value) -> ClientCapabilities {
		let params = json!({"protocolVersion": 1, "clientCapabilities": value});
		ClientCapabilities::from_initialize(params.as_object().expect("object params"))
	}

	#[test]
	fn capabilities_parse_present_fields() {
		let parsed = capabilities(json!({
			"fs": {"readTextFile": true, "writeTextFile": true},
			"terminal": true,
			"auth": {"terminal": true},
			"_meta": {"vendor": 1},
		}));
		assert_eq!(parsed, ClientCapabilities {
			fs:       FileSystemCapabilities { read_text_file: true, write_text_file: true },
			terminal: true,
			auth:     AuthCapabilities { terminal: true },
		});
	}

	#[test]
	fn absent_capabilities_advertise_nothing() {
		assert_eq!(
			ClientCapabilities::from_initialize(
				json!({"protocolVersion": 1}).as_object().expect("object")
			),
			ClientCapabilities::default()
		);
		assert_eq!(capabilities(json!({})), ClientCapabilities::default());
		assert_eq!(capabilities(json!({"fs": {"readTextFile": true}})).fs, FileSystemCapabilities {
			read_text_file:  true,
			write_text_file: false,
		});
	}

	#[test]
	fn malformed_capabilities_are_not_advertised_field_by_field() {
		let parsed = capabilities(json!({
			"fs": {"readTextFile": "yes", "writeTextFile": true},
			"terminal": {"enabled": true},
			"auth": null,
		}));
		assert_eq!(parsed, ClientCapabilities {
			fs:       FileSystemCapabilities { read_text_file: false, write_text_file: true },
			terminal: false,
			auth:     AuthCapabilities::default(),
		});
		assert_eq!(capabilities(json!({"fs": "all"})).fs, FileSystemCapabilities::default());
		assert_eq!(capabilities(json!(42)), ClientCapabilities::default());
		assert_eq!(capabilities(json!(null)), ClientCapabilities::default());
	}

	#[test]
	fn settings_project_the_convars() {
		let ctx = Ctx::new();
		assert_eq!(AcpSettings::from_con(&ctx), AcpSettings::default());
		SV_ACP_FS.set(&ctx, AcpFs::Off).expect("set sv_acp_fs");
		SV_ACP_FS_TIMEOUT
			.set(&ctx, Duration::new(250, DurationUnit::Milliseconds))
			.expect("set sv_acp_fs_timeout");
		assert_eq!(AcpSettings::from_con(&ctx), AcpSettings {
			fs:         AcpFs::Off,
			fs_timeout: StdDuration::from_millis(250),
		});
		assert_eq!("off".parse::<AcpFs>().ok(), Some(AcpFs::Off));
		assert_eq!(<&str>::from(AcpFs::Auto), "auto");
	}

	#[test]
	fn project_paths_stay_inside_the_root() {
		let root = Path::new("/work/project");
		assert_eq!(
			project_path(root, Path::new("/work/project/src/lib.rs")).ok(),
			Some("/work/project/src/lib.rs")
		);
		for outside in
			["/work/other/a.rs", "src/lib.rs", "/work/project/../other/a.rs", "/work/projectx"]
		{
			assert!(
				matches!(
					project_path(root, Path::new(outside)),
					Err(ClientRequestError::OutsideProject { .. })
				),
				"{outside} must stay unsent"
			);
		}
		assert!(project_path(Path::new(""), Path::new("/a")).is_err(), "no root, no path");
	}

	/// Approval tickets and awaited calls share one id space and one table,
	/// and each answer reaches the request it names.
	#[tokio::test]
	async fn approvals_and_calls_correlate_in_one_table() {
		let (output, frames) = flume::unbounded();
		let client = AcpClient::new(AcpSettings::default(), output);
		client.initialize(ClientCapabilities {
			fs: FileSystemCapabilities { read_text_file: true, write_text_file: false },
			..ClientCapabilities::default()
		});
		client.switch_session(Str::new_static("s1"), true, Path::new("/work"));
		client
			.request_permission(Str::new_static("ticket-1"), json!({"toolCallId": "call-1"}))
			.expect("permission request");
		let permission = frames.recv().expect("permission frame");
		assert_eq!(permission["method"], "session/request_permission");
		assert_eq!(permission["params"]["sessionId"], "s1");
		assert_eq!(permission["params"]["options"][3]["optionId"], "reject_always");

		let read = {
			let client = client.clone();
			tokio::spawn(async move { client.read_text_file(Path::new("/work/a.txt")).await })
		};
		let request = frames.recv_async().await.expect("read frame");
		assert_eq!(request["method"], "fs/read_text_file");
		assert_eq!(request["params"], json!({"sessionId": "s1", "path": "/work/a.txt"}));
		assert_ne!(request["id"], permission["id"]);
		assert_eq!(client.pending(), 2);

		assert!(
			client
				.answer(&request["id"], Ok(json!({"content": "buffer"})))
				.is_none()
		);
		assert_eq!(read.await.expect("read task").expect("read").as_str(), "buffer");
		let (ticket, answer) = client
			.answer(&permission["id"], Ok(json!({"outcome": {"outcome": "cancelled"}})))
			.expect("the approval answer returns to the controller");
		assert_eq!(ticket.as_str(), "ticket-1");
		assert!(answer.is_ok());
		assert!(client.answer(&permission["id"], Ok(json!({}))).is_none(), "answered once");
		assert_eq!(client.pending(), 0);
	}

	#[test]
	fn permission_outcomes_decode_typed() {
		let selected: RequestPermissionResponse = serde_json::from_value(
			json!({"outcome": {"outcome": "selected", "optionId": "allow_always"}}),
		)
		.expect("selected");
		assert!(matches!(selected.outcome, PermissionOutcome::Selected {
			option_id: PermissionOptionId::AllowAlways,
		}));
		let unknown: RequestPermissionResponse =
			serde_json::from_value(json!({"outcome": {"outcome": "selected", "optionId": "maybe"}}))
				.expect("unknown option");
		assert!(matches!(unknown.outcome, PermissionOutcome::Selected {
			option_id: PermissionOptionId::Unknown,
		}));
		assert!(
			serde_json::from_value::<RequestPermissionResponse>(json!({"outcome": "selected"}))
				.is_err()
		);
	}
}
