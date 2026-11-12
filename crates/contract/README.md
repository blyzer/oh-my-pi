# omp-contract

Shared vocabulary for the agent kernel and the project environment.

Approvals, hook gates, the hook-event descriptor table, the object-safe
Director surface, session routing views, environment events, approval
posture, and the eval parent-bridge traits live here. `omp-agent`
implements the kernel over them.
`omp-envd` admits tools and hosts extensions over them. `omp-driver`
composes both and names these types directly.

## Structural philosophy

The crate is below `omp-agent`, `omp-envd`, and `omp-driver`. It does not
depend on any of them, and it does not run a turn or own a project
environment.

Two cuts keep that boundary:

- **Vocabulary versus the kernel mailbox.** Environment code sends
  `EnvEvent`s. Peer routing sends through `SessionMailbox`. Neither type
  names the kernel's `Up` enum. The agent adapts those sinks onto its
  single mailbox.
- **Traits versus the stack's built-ins.** `Director` and
  `DirectorRegistry` are contracts. The built-in directors register from
  `omp-agent` when that crate is linked. `ApprovalDesk` publishes through
  `ApprovalPublisher` so the kernel event enum stays in the agent.

Posture (`ApprovalMode`, `ApprovalPosture`, `SandboxState`) and the eval
parent bridge (`ParentSessionHost`, `BridgeHost`) are the same kind of
shared vocabulary. Decision logic that needs the live sandbox probe or the
Python worker stays in `omp-envd` and speaks these types.

The hook-event descriptor table (`HOOK_EVENTS`) is one `HookEventDescriptor`
per `HookEventId`: the frozen wire identity, the payload schema revision, and
the payload's name in the Python `omp.events` catalog. That name is checked
against `omp.events` only and is not resolved to a Rust or protobuf type, so
the table does not claim ADR 0046 owner ruling 3. `tests/hook_catalog.rs`
holds the table to the protobuf enum, the Python `omp.events` and `omp.hooks`
catalogs, and the Claude plugin hook seams in `omp-ext`.
