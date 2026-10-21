//! Typed JSON-RPC wire structs for the Agent Client Protocol (ACP) v1.
//!
//! Field names and enum variant string values follow the ACP v1 schema
//! published at <https://agentclientprotocol.com/protocol/v1/schema> and
//! retrieved 2026-10-19.
//!
//! **Spec version**: ACP v1 (protocol version integer `1`), schema as of
//! 2026-10-19.  Breaking changes that bump the protocol major version will
//! require a new module or a version-negotiation branch here.
//!
//! Rules (same as the Codex wire module):
//! - No `json!` and no untyped `Value` traversal anywhere in this crate.
//! - Inbound lines are borrowed via [`serde_json::value::RawValue`] until the
//!   method is known, then decoded into the concrete type.
//! - Every `#[serde(rename_all = "camelCase")]` is intentional: ACP uses
//!   camelCase throughout.

use omp_core::Str;
use serde::{Deserialize, Serialize, de::IgnoredAny};
use serde_json::value::RawValue;

// ─── JSON-RPC envelope ───────────────────────────────────────────────────────

/// A JSON-RPC request id: ACP allows a string or an integer (like Codex).
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(untagged)]
pub enum RequestId {
	/// Integer id.
	Int(i64),
	/// String id (server-assigned, or from a future client).
	Text(Str),
}

/// The `error` member of a JSON-RPC error response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct RpcError {
	/// Standard JSON-RPC error code.
	pub code: i64,
}

/// Every member an ACP JSON-RPC line may carry, borrowed from the line.
///
/// ACP sends `"jsonrpc": "2.0"` on every message; we accept it but do not
/// require it (the Codex wire omits it).
#[derive(Debug, Deserialize)]
pub struct Envelope<'a> {
	#[serde(default)]
	pub id:      Option<RequestId>,
	#[serde(default)]
	pub method:  Option<Str>,
	#[serde(default, borrow)]
	pub params:  Option<&'a RawValue>,
	#[serde(default, borrow)]
	pub result:  Option<&'a RawValue>,
	#[serde(default)]
	pub error:   Option<RpcError>,
	/// Accepted but ignored; ACP includes `"jsonrpc": "2.0"`.
	#[serde(default)]
	pub jsonrpc: Option<IgnoredAny>,
}

/// An inbound line classified into its JSON-RPC role.
#[derive(Debug)]
pub enum JsonRpcMessage<'a> {
	/// The agent asks the client something and waits for a response
	/// (agent-to-client request, e.g. `session/request_permission`).
	Request { id: RequestId, method: Str, params: Option<&'a RawValue> },
	/// The agent sends a notification; no response is expected
	/// (e.g. `session/update`).
	Notification { method: Str, params: Option<&'a RawValue> },
	/// A successful response to a client request.
	Response { id: RequestId, result: &'a RawValue },
	/// An error response to a client request.
	Error { id: Option<RequestId>, code: i64 },
}

impl<'a> Envelope<'a> {
	/// Classify a decoded envelope.
	pub fn classify(self) -> Option<JsonRpcMessage<'a>> {
		match (self.id, self.method, self.result, self.error, self.params) {
			// Agent-to-client request: has id + method.
			(Some(id), Some(method), None, None, params) => {
				Some(JsonRpcMessage::Request { id, method, params })
			},
			// Notification: has method, no id.
			(None, Some(method), None, None, params) => {
				Some(JsonRpcMessage::Notification { method, params })
			},
			// Successful response.
			(Some(id), None, Some(result), None, _) => Some(JsonRpcMessage::Response { id, result }),
			// Error response (id may be absent for parse errors).
			(id, None, None, Some(err), _) => Some(JsonRpcMessage::Error { id, code: err.code }),
			_ => None,
		}
	}
}

// ─── Outbound helpers ────────────────────────────────────────────────────────

/// A client request sent to the agent.
#[derive(Serialize)]
pub struct OutRequest<'a, P: Serialize> {
	pub jsonrpc: &'static str,
	pub id:      i64,
	pub method:  &'a str,
	pub params:  P,
}

/// A notification sent to the agent (no id, no response expected).
#[derive(Serialize)]
pub struct OutNotification<P: Serialize> {
	pub jsonrpc: &'static str,
	pub method:  &'static str,
	pub params:  P,
}

/// A successful response to an agent request.
#[derive(Serialize)]
pub struct OutResponse<'a, R: Serialize> {
	pub jsonrpc: &'static str,
	pub id:      &'a RequestId,
	pub result:  R,
}

/// An error response to an agent request.
#[derive(Serialize)]
pub struct OutError<'a> {
	pub jsonrpc: &'static str,
	pub id:      &'a RequestId,
	pub error:   OutErrorBody,
}

/// The `error` member of an outbound error response.
#[derive(Serialize)]
pub struct OutErrorBody {
	pub code:    i64,
	pub message: Str,
}

// ─── ACP method enum ─────────────────────────────────────────────────────────

/// All ACP v1 methods this transport cares about.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Method {
	// Client→Agent requests
	Initialize,
	SessionNew,
	SessionPrompt,
	SessionCancel,
	// Agent→Client requests
	SessionRequestPermission,
	// Agent→Client notifications
	SessionUpdate,
	// Unknown
	Unknown,
}

impl Method {
	/// Classify a raw method string.
	pub fn classify(s: &str) -> Self {
		match s {
			"initialize" => Self::Initialize,
			"session/new" => Self::SessionNew,
			"session/prompt" => Self::SessionPrompt,
			"session/cancel" => Self::SessionCancel,
			"session/request_permission" => Self::SessionRequestPermission,
			"session/update" => Self::SessionUpdate,
			_ => Self::Unknown,
		}
	}

	/// Whether this method expects a response from the receiver.
	pub fn is_request(self) -> bool {
		matches!(
			self,
			Self::Initialize | Self::SessionNew | Self::SessionPrompt | Self::SessionRequestPermission
		)
	}
}

// ─── Protocol version ────────────────────────────────────────────────────────

/// The ACP major protocol version this module targets.
pub const PROTOCOL_VERSION: u16 = 1;

// ─── initialize ──────────────────────────────────────────────────────────────

/// `initialize` request params (client → agent).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams<'a> {
	pub protocol_version:    u16,
	pub client_info:         Option<Implementation<'a>>,
	pub client_capabilities: ClientCapabilities,
}

/// Minimal implementation info block.
#[derive(Serialize)]
pub struct Implementation<'a> {
	pub name:    &'a str,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub version: Option<&'a str>,
}

/// Client capabilities sent during initialization.
///
/// We advertise only what the ACP session transport actually implements.
/// The `fs` and `terminal` capabilities are absent because this transport
/// does not implement those client-side methods.
#[derive(Serialize, Default)]
pub struct ClientCapabilities {}

/// `initialize` response (agent → client).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
	/// The negotiated protocol version.
	pub protocol_version:   u16,
	/// Agent capabilities, parsed leniently.
	#[serde(default)]
	pub agent_capabilities: AgentCapabilities,
	/// Agent implementation info.
	#[serde(default)]
	pub agent_info:         Option<AgentInfo>,
}

/// Minimal subset of agent capabilities we inspect.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentCapabilities {
	/// Whether `session/load` is available.
	pub load_session: bool,
}

/// Agent name and version from `initialize`.
#[derive(Clone, Debug, Deserialize)]
pub struct AgentInfo {
	pub name:    Str,
	#[serde(default)]
	pub version: Option<Str>,
}

// ─── session/new ─────────────────────────────────────────────────────────────

/// `session/new` request params (client → agent).
///
/// ACP requires `mcpServers` on every `session/new`, even when empty: a real
/// agent (the `dsh` harness) rejects a request without it. We send an empty
/// list; the agent connects to MCP servers under its own configuration.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionParams<'a> {
	/// Working directory for the session; must be an absolute path.
	pub cwd:         &'a str,
	/// MCP server list. Always serialized, as `[]` when unused.
	pub mcp_servers: &'a [&'a str],
}

/// `session/new` response (agent → client).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResult {
	/// The agent-assigned session id.
	pub session_id: Str,
}

// ─── session/prompt ──────────────────────────────────────────────────────────

/// `session/prompt` request params (client → agent).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptParams<'a> {
	pub session_id: &'a str,
	/// Content blocks; we always send at least one `text` block.
	pub prompt:     Vec<TextContentBlock<'a>>,
}

/// The only content block type this transport sends.
#[derive(Serialize)]
pub struct TextContentBlock<'a> {
	pub r#type: &'static str,
	pub text:   &'a str,
}

impl<'a> TextContentBlock<'a> {
	/// Constructs a text content block.
	pub fn text(text: &'a str) -> Self {
		Self { r#type: "text", text }
	}
}

/// `session/prompt` response (agent → client).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResult {
	/// Why the agent stopped processing the turn.
	pub stop_reason: StopReason,
}

/// ACP v1 stop reasons.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
	/// The language model finished without requesting more tools.
	EndTurn,
	/// The maximum token limit was reached.
	MaxTokens,
	/// The maximum number of model requests in a single turn was exceeded.
	MaxTurnRequests,
	/// The agent refuses to continue.
	Refusal,
	/// The client cancelled the turn via `session/cancel`.
	Cancelled,
	/// An unknown stop reason (forward-compat); treated as end_turn.
	#[serde(other)]
	Unknown,
}

// ─── session/update ──────────────────────────────────────────────────────────

/// `session/update` notification params (agent → client).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateParams {
	pub session_id: Str,
	pub update:     SessionUpdateEnvelope,
}

/// The `update` object within `session/update` params; the `sessionUpdate`
/// discriminator is read first, then the remainder decoded per variant.
#[derive(Debug, Deserialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum SessionUpdateEnvelope {
	/// A chunk of the agent's response text.
	AgentMessageChunk {
		#[serde(default, rename = "messageId")]
		message_id: Option<Str>,
		content:    TextContent,
	},
	/// A chunk of the user's own message being echoed back.
	UserMessageChunk {
		#[serde(default, rename = "messageId")]
		message_id: Option<Str>,
		content:    TextContent,
	},
	/// A chunk of the agent's reasoning.
	AgentThoughtChunk {
		#[serde(default, rename = "messageId")]
		message_id: Option<Str>,
		content:    TextContent,
	},
	/// A new tool call has been initiated.
	ToolCall {
		#[serde(rename = "toolCallId")]
		tool_call_id: Str,
		#[serde(default)]
		title:        Option<Str>,
		status:       ToolCallStatus,
	},
	/// An update to an existing tool call.
	ToolCallUpdate {
		#[serde(rename = "toolCallId")]
		tool_call_id: Str,
		#[serde(default)]
		status:       Option<ToolCallStatus>,
		#[serde(default)]
		title:        Option<Str>,
		/// Raw content; we ignore its shape.
		#[serde(default)]
		content:      Option<IgnoredAny>,
	},
	/// The agent's execution plan.
	Plan { entries: Vec<IgnoredAny> },
	/// Available slash commands changed.
	AvailableCommandsUpdate {
		#[serde(rename = "availableCommands")]
		available_commands: Vec<IgnoredAny>,
	},
	/// Current mode changed.
	CurrentModeUpdate {
		#[serde(rename = "currentModeId")]
		current_mode_id: Str,
	},
	/// Configuration options changed.
	ConfigOptionUpdate {
		#[serde(rename = "configOptions")]
		config_options: Vec<IgnoredAny>,
	},
	/// Session metadata (title, timestamp) changed.
	SessionInfoUpdate {
		#[serde(default)]
		title:      Option<Str>,
		#[serde(default, rename = "updatedAt")]
		updated_at: Option<Str>,
	},
	/// Context window usage and cumulative cost.
	UsageUpdate {
		used: u64,
		size: u64,
		#[serde(default)]
		cost: Option<IgnoredAny>,
	},
	/// A live user-facing notice (not session history).
	Notice {
		severity:    Str,
		title:       Str,
		#[serde(default)]
		description: Option<Str>,
	},
	/// A context compaction update.
	CompactionUpdate {
		#[serde(rename = "compactionId")]
		compaction_id: Str,
		status:        Str,
		#[serde(default)]
		summary:       Option<IgnoredAny>,
		#[serde(default)]
		error:         Option<Str>,
	},
	/// A content block appended to a compaction's summary.
	CompactionSummaryChunk {
		#[serde(rename = "compactionId")]
		compaction_id: Str,
		content:       IgnoredAny,
	},
	/// An unknown update variant; stored as raw bytes for forward-compat.
	#[serde(other)]
	Unknown,
}

/// Inline text content block used in `AgentMessageChunk` and friends.
#[derive(Debug, Deserialize)]
pub struct TextContent {
	pub r#type: Str,
	#[serde(default)]
	pub text:   Option<Str>,
	/// Absorb all other fields without allocating.
	#[serde(flatten)]
	pub extra:  IgnoredAny,
}

/// Tool call lifecycle status.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
	Pending,
	InProgress,
	Completed,
	Cancelled,
	Error,
	#[serde(other)]
	Unknown,
}

// ─── session/cancel ──────────────────────────────────────────────────────────

/// `session/cancel` notification params (client → agent).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelParams<'a> {
	pub session_id: &'a str,
}

// ─── session/request_permission ──────────────────────────────────────────────

/// `session/request_permission` request params (agent → client).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionParams {
	pub session_id: Str,
	pub tool_call:  ToolCallSnapshot,
	pub options:    Vec<PermissionOption>,
}

/// Snapshot of the tool call requiring permission.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallSnapshot {
	pub tool_call_id: Str,
	#[serde(default)]
	pub title:        Option<Str>,
	#[serde(default)]
	pub status:       Option<ToolCallStatus>,
	/// Raw content; we don't inspect it.
	#[serde(flatten)]
	pub extra:        IgnoredAny,
}

/// One option the agent offers.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
	pub option_id: Str,
	pub name:      Str,
	pub kind:      PermissionOptionKind,
}

/// The semantic kind of a permission option.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
	AllowOnce,
	AllowAlways,
	RejectOnce,
	RejectAlways,
	/// An unknown kind; treat as a generic option.
	#[serde(other)]
	Unknown,
}

/// The response we send to `session/request_permission`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionResponse {
	pub outcome: PermissionOutcome,
}

/// ACP v1 `RequestPermissionOutcome`.
#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PermissionOutcome {
	/// The user picked an option.
	Selected {
		#[serde(rename = "optionId")]
		option_id: Str,
	},
	/// The prompt was cancelled before the user responded.
	Cancelled,
}

/// JSON-RPC error code −32800 from the ACP schema (request cancelled).
pub const ERR_REQUEST_CANCELLED: i64 = -32800;
