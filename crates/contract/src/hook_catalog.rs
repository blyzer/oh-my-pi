//! The hook-event descriptor table of ADR 0046 item 3: one
//! [`HookEventDescriptor`] per [`HookEventId`].
//!
//! A descriptor's identity is the frozen `toolhost/v1` wire id. The stable
//! event name is that variant's protobuf name without its `HOOK_EVENT_`
//! prefix, lowercased: `HOOK_EVENT_TOOL_CALL` is `tool_call`.
//!
//! `payload` is the payload's name in the Python `omp.events` catalog and is
//! checked against that catalog only. It is not resolved to a Rust or
//! protobuf type, so this table does not claim ADR 0046 owner ruling 3
//! (payloads stay typed prost/serde structs).
//!
//! `tests/hook_catalog.rs` holds the table to the protobuf enum, the Python
//! `omp.events` and `omp.hooks` catalogs, and the Claude plugin hook seams.

use omp_proto::toolhost::v1::HookEventId;

/// One hook event's catalog row.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct HookEventDescriptor {
	/// Frozen wire identity.
	pub id:       HookEventId,
	/// Schema revision of the event's payload.
	pub revision: u32,
	/// The payload's name in the Python `omp.events` catalog; it need not
	/// name a Rust or protobuf type.
	pub payload:  &'static str,
}

impl HookEventDescriptor {
	/// The catalog row for `id`; `None` for
	/// [`HookEventId::HookEventUnspecified`].
	#[must_use]
	pub fn of(id: HookEventId) -> Option<&'static Self> {
		HOOK_EVENTS.iter().find(|row| row.id == id)
	}
}

/// Every hook event, in ascending wire-id order.
///
/// One row per [`HookEventId`] variant except
/// [`HookEventId::HookEventUnspecified`]. Wire id 59 is a permanent protobuf
/// tombstone and has no row.
pub static HOOK_EVENTS: &[HookEventDescriptor] = &[
	event(HookEventId::HookEventSessionStart, 1, "SessionStartEvent"),
	event(HookEventId::HookEventSessionShutdown, 1, "SessionShutdownEvent"),
	event(HookEventId::HookEventSessionSwitch, 1, "SessionSwitchEvent"),
	event(HookEventId::HookEventSessionSwitched, 1, "SessionSwitchedEvent"),
	event(HookEventId::HookEventSessionBranch, 1, "SessionBranchEvent"),
	event(HookEventId::HookEventSessionBranched, 1, "SessionBranchedEvent"),
	event(HookEventId::HookEventSessionRewind, 1, "SessionRewindEvent"),
	event(HookEventId::HookEventSessionRewound, 1, "SessionRewoundEvent"),
	event(HookEventId::HookEventSessionReset, 1, "SessionResetEvent"),
	event(HookEventId::HookEventBeforeAgentStart, 1, "BeforeAgentStartEvent"),
	event(HookEventId::HookEventAgentStart, 1, "AgentStartEvent"),
	event(HookEventId::HookEventTurnStart, 1, "TurnStartEvent"),
	event(HookEventId::HookEventTurnEnd, 1, "TurnEndEvent"),
	event(HookEventId::HookEventAgentSettled, 2, "AgentSettledEvent"),
	event(HookEventId::HookEventAgentEnd, 1, "AgentEndEvent"),
	event(HookEventId::HookEventInterrupt, 1, "InterruptEvent"),
	event(HookEventId::HookEventDeadline, 1, "DeadlineEvent"),
	event(HookEventId::HookEventMessageStart, 1, "MessageStartEvent"),
	event(HookEventId::HookEventMessageUpdate, 1, "MessageUpdateEvent"),
	event(HookEventId::HookEventMessageEnd, 1, "MessageEndEvent"),
	event(HookEventId::HookEventItemCommitted, 1, "ItemCommittedEvent"),
	event(HookEventId::HookEventCallOpen, 1, "CallOpenEvent"),
	event(HookEventId::HookEventToolCall, 1, "ToolCallEvent"),
	event(HookEventId::HookEventToolExecutionStart, 1, "ToolExecutionStartEvent"),
	event(HookEventId::HookEventToolUpdate, 1, "ToolUpdateEvent"),
	event(HookEventId::HookEventToolExecutionEnd, 1, "ToolExecutionEndEvent"),
	event(HookEventId::HookEventToolResult, 1, "ToolResultEvent"),
	event(HookEventId::HookEventToolApprovalRequested, 1, "ToolApprovalRequestedEvent"),
	event(HookEventId::HookEventToolApprovalResolved, 1, "ToolApprovalResolvedEvent"),
	event(HookEventId::HookEventDeviceList, 1, "DeviceListEvent"),
	event(HookEventId::HookEventUserInput, 1, "UserInputEvent"),
	event(HookEventId::HookEventUserBash, 1, "UserBashEvent"),
	event(HookEventId::HookEventUserEval, 1, "UserEvalEvent"),
	event(HookEventId::HookEventCommandInvoke, 1, "CommandInvokeEvent"),
	event(HookEventId::HookEventResourcesDiscover, 1, "ResourcesDiscoverEvent"),
	event(HookEventId::HookEventResourcesChanged, 1, "ResourcesChangedEvent"),
	event(HookEventId::HookEventProviderLogin, 1, "LoginRequest"),
	event(HookEventId::HookEventProviderRefresh, 1, "RefreshRequest"),
	event(HookEventId::HookEventProviderSign, 1, "SignRequest"),
	event(HookEventId::HookEventBeforeRequest, 1, "RequestDraft"),
	event(HookEventId::HookEventModelsDiscover, 1, "DiscoveryQuery"),
	event(HookEventId::HookEventProviderError, 1, "ProviderError"),
	event(HookEventId::HookEventProviderUsage, 1, "UsageQuery"),
	event(HookEventId::HookEventCapabilityBudget, 1, "CapabilityBudgetEvent"),
	event(HookEventId::HookEventModelChanged, 1, "ModelChangedEvent"),
	event(HookEventId::HookEventCredentialDisabled, 1, "CredentialDisabledEvent"),
	event(HookEventId::HookEventCompaction, 1, "CompactionEvent"),
	event(HookEventId::HookEventCompactionDone, 1, "CompactionOutcome"),
	event(HookEventId::HookEventThreadProjection, 1, "ContextView"),
	event(HookEventId::HookEventSubagentSpawn, 1, "SubagentSpec"),
	event(HookEventId::HookEventWorkerState, 1, "WorkerInfo"),
	event(HookEventId::HookEventJobRegistered, 1, "JobRegisteredEvent"),
	event(HookEventId::HookEventJobSettled, 1, "JobSettledEvent"),
	event(HookEventId::HookEventExtensionActivate, 1, "ExtensionActivateEvent"),
	event(HookEventId::HookEventExtensionLoad, 1, "ExtensionLoadEvent"),
	event(HookEventId::HookEventExtensionUnload, 1, "ExtensionUnloadEvent"),
	event(HookEventId::HookEventHostReconnect, 1, "HostReconnectEvent"),
	event(HookEventId::HookEventStreamRuleTriggered, 1, "StreamRuleTriggeredEvent"),
	event(HookEventId::HookEventRetryStart, 1, "RetryLifecycleEvent"),
	event(HookEventId::HookEventRetryEnd, 1, "RetryLifecycleEvent"),
	event(HookEventId::HookEventFallbackApplied, 1, "FallbackLifecycleEvent"),
	event(HookEventId::HookEventFallbackSucceeded, 1, "FallbackLifecycleEvent"),
	event(HookEventId::HookEventMcpNotification, 1, "McpNotificationEvent"),
	event(HookEventId::HookEventProviderResponse, 1, "ProviderResponseEvent"),
	event(HookEventId::HookEventSessionRenamed, 1, "SessionRenamedEvent"),
];

const fn event(id: HookEventId, revision: u32, payload: &'static str) -> HookEventDescriptor {
	HookEventDescriptor { id, revision, payload }
}
