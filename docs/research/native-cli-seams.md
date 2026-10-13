# OMP² native CLI: existing execution seams (CLI-1)

Status: inventory, no implementation. Date: 2026-10-13.
Inputs: `docs/adr/0043-native-cli-integration.md`, `docs/research/native-cli-integration.md`,
`docs/parked/2026-10-10-native-cli-integration-next-steps.md` (task CLI-1).
Acceptance (from the handoff): exact reuse boundary; no duplicate process or credential manager.

## Evidence legend

Every claim cites `path:symbol`. Three evidence levels are used:

- **[R]** read by the author of this document (declaration and body, or the cited range).
- **[C]** symbol existence re-confirmed by the author with a targeted grep after a scout read it;
  behaviour is the scout's reading and not independently re-read.
- **[S]** read only by a scout; not re-opened here. Treat as a lead, not a proof.
- **UNVERIFIED** nothing in this pass opened it. Collected in the last section.

Line numbers are advisory and drift; the symbol is authoritative.

## 1. Reuse boundary at a glance

| Concern | REUSE (the delegated executor must call this) | ADD (does not exist) | NEVER (forbidden) |
|---|---|---|---|
| Subprocess spawn + group cleanup | `omp_shell::processes::signal_process_group` / `ProcessSignal`; `ExecSandbox::{tokio_command, resolve_env}`; the spawn recipe of `eval::process::EvalChild` and `mcp::stdio::StdioTransport` | a protocol-neutral stdio-child supervisor (today every site re-implements spawn + pgid + grace + reap) | a fifth private spawn/teardown copy; `/bin/sh` or `$PATH` shell-out |
| Job lifecycle | `omp_agent::JobBoard::{attach_restartable, attach_task}` + `JobSettlement` shape | a job kind for "external agent"; a process kill ladder (board only aborts a tokio task) | a second job registry |
| Cancellation | `omp_agent::CancelTree::session_child` / `TurnCancellation` tokens | a token chain from the session root into the job token | an executor-private cancellation hierarchy |
| Approvals | `omp_agent::{ApprovalRoute, ApprovalDecision, ApprovalScope, ApprovalSource}` | CLI-approval to OMP-policy mapping (done as a pure function in the contract) | auto-approving on the CLI's behalf; widening a grant |
| Credentials | nothing by default (CLI owns login). If OMP must hand a CLI its own credential: `AuthControlHandle` / `CredentialLease` only | – | reading `~/.codex`, `~/.claude`, Keychain items, any foreign auth file; a second credential store |
| Receipts / usage | `omp_ai::Usage`, `omp_journal::data::TurnReceipt` via the kernel journal | CLI-reported session id/model on a receipt | a parallel usage ledger |
| Observability | `omp_observability::span` / `semconv` vocabulary | first production caller (none exists) | ad-hoc logging as telemetry |
| Provider/route transport | `omp_agent::Inference` + `omp_ai::ChatEvent` only for token-stream CLIs | an agent-shaped event vocabulary (this contract) | modelling an agent CLI as a `Codec`/`TransportKind::Local` route |

## 2. Subprocess spawning and process-group cleanup

Headline: there is **no single owner of child spawn**. [S] Seven independent `Command::new(..).spawn()`
sites exist in `crates/envd` with four different teardown implementations. They share conventions only
(`kill_on_drop(true)`, own pgid/session at spawn, `killpg` of that group).

| Site | Role | Verdict |
|---|---|---|
| `crates/envd/src/mcp/stdio.rs:StdioTransport` [C] (`TERM_GRACE` 1 s, `KILL_GRACE` 500 ms, `MAX_FRAME_BYTES` 8 MiB, `reap_inner`) | long-lived NDJSON JSON-RPC child; owns its pgid; TERM → grace → KILL sweep even after the leader exits; Drop-safe | **REUSE as structural template** (strongest fit for a Codex app-server-style child). Gaps [S]: tied to `McpTransport`; `StdioConfig.env` is additive with no `env_clear`; no sandbox hook; stderr drained and discarded |
| `crates/envd/src/eval/process.rs:EvalChild` [C] (`sanitized_spawn_env` [C]) | Python eval kernel: `env_clear` + allowlist + secret-name filter, optional `ExecSandbox::tokio_command`, `process_group(0)`, token handshake, Exit → TERM → KILL ladder | **REUSE the spawn recipe.** Gap [S]: private types; allowlist hard-codes `OMP_EVAL_*`; its secret-name filter would strip a CLI's own auth variables |
| `crates/envd/src/exec.rs:ExecHost` (detached launch, `SpawnBook`, `ProcessLog`) [S] | supervised, restartable, re-adoptable shell-script processes | **Do not reuse for a protocol child.** stdin is null, stdout is a log file, the program is always a shell script re-exec |
| `crates/envd/src/process_identity.rs:ProcessIdentity` [C], `process_store.rs` [S] | pid-reuse-safe identity + durable store, detached shell processes only | not applicable to a resident protocol child; no orphan re-adoption exists for protocol children |
| `crates/shell/src/processes.rs:signal_process_group`, `ProcessSignal` [C] | ESRCH-tolerant `kill(-pgid, sig)`, positive-pgid guard | **REUSE as the one signalling primitive.** Gap [S]: Windows variant only logs |
| `crates/envd/src/exec_sandbox.rs:ExecSandbox` [S] (`command`, `tokio_command`, `resolve_env`) | Seatbelt/Landlock/Bubblewrap command wrapping and `env_inherit`/`env_deny` policy | **REUSE.** Gap [S]: no "delegate" consumer; network broker assumes a shell-attempt lifecycle |
| `crates/envd/src/exthost/spawn.rs`, `docserver/lsp_process.rs`, `docserver/dap_protocol.rs`, `browser_daemon.rs` [S] | other protocol children | ideas only (admission bound, graceful protocol shutdown); weaker or private teardown |
| `crates/sandbox/src/runner.rs:Runner::run`, `ChildGuard` [S] | run-to-completion sandboxed commands | wrong shape for a resident agent child; the `ChildGuard` pattern is reusable |
| `crates/app/src/main.rs` hidden-child dispatch [S] (`__omp-eval-child`, `__omp-shell-child`, `__omp-ext-host`, `--omp-sandbox-child`) | same-binary re-entry | a third-party CLI needs no entry here |

Cross-cutting gaps for an executor [S]: sandbox and env scrubbing apply only to shell sessions/detached
processes and the eval child (MCP, LSP, DAP and the browser relay inherit the full parent environment and are
unsandboxed); working directory is never containment-checked at spawn (`tool_shell.rs:ShellExecHost::resolve_cwd`
only checks `is_dir()`, confinement is the sandbox's job); `write_scope` governs tool writes, not spawns.

Consequence for the contract: it must not own a spawn path. It defines what a spawn-owning adapter has to
guarantee (process-tree teardown on `terminate`, idempotent, also on `Drop`), and the future adapter (CLI-2)
should extract one supervisor from `StdioTransport`/`EvalChild` rather than add a fifth copy.

## 3. Job lifecycle and the cancellation tree

| Seam | File:symbol | Verdict |
|---|---|---|
| Job index | `crates/agent/src/jobs.rs:JobBoard` [C: `attach_task`, `attach_restartable`, `terminate_runtime`]; durable state lives in the session DOM under `<meta><jobs>` | **REUSE** `attach_restartable` + `JobSettlement{status, output, error}` as the supervision model for a CLI-backed "subagent" [S]. Only valid in-process under a `Kernel` |
| Job kinds | `jobs.rs:JobKind` = `Tool | Subagent | Process` [S] | **GAP:** no "external agent" kind; `Process` delivers as `bash` |
| Board termination | `jobs.rs:terminate_runtime` [C] | **GAP:** cancels, waits 1 s, then `JoinHandle::abort()` [S]; sends no SIGTERM → SIGKILL. The executor must own the process kill ladder itself |
| Cancellation tree | `crates/agent/src/cancel.rs:CancelTree`, `TurnCancellation`, `ForegroundMutationCancellation`, `ReadOnlyToolCancellation`, `BackgroundToolCancellation` [R] (tokio-util `CancellationToken`) | **REUSE.** `CancelTree::session_child` [R] is the sanctioned token for "host-owned work that must end at the session boundary but may outlast one turn"; `Kernel::session_cancellation` [C] exposes it |
| Token chain gap | `jobs.rs:attach_restartable` creates `CancellationToken::new()`; `crates/driver/src/subagent/spawn.rs` overwrites the child's token with the board's factory token [S] | **GAP (suspected):** `cancel_session` does not cascade into running subagent jobs. An executor must derive its token from `Kernel::session_cancellation()` itself. Not independently re-read |
| Interrupt ladder | `crates/agent/src/dispatch.rs` (ADR 0011): interrupt → bounded grace (default 1 s) → abort, journaled as effects-unknown [S] | the contract mirrors this shape (interrupt, grace, forced terminate) |
| Turn budget | `crates/agent/src/loop.rs:RunControl::new(CancellationToken, Option<Instant>)` [S] | the contract's `TurnLimits` is the executor-side analogue |
| Subagent spawn | `crates/driver/src/subagent/spawn.rs:TaskSessionTool`, `run_child`, `compose_kernel` [S]; `crates/tools/src/task.rs:ChildResult` [S] | **REUSE the contract, not the implementation.** `run_child` builds a full omp-owned kernel + `.oms` journal; a CLI-owned loop has neither. A delegated executor is another `attach_restartable` factory yielding a `ChildResult`-shaped `JobSettlement` |

Approvals (same crate):

| Symbol | Fact | Verdict |
|---|---|---|
| `crates/agent/src/approvals.rs:ApprovalDecision{approved, scope, source, decided_by, reason, audited}` [R] | there is **no** approve/deny/abort enum: a decision is `approved: bool` + scope + source | **REUSE**; contract maps it onto the CLI's own options |
| `approvals.rs:ApprovalScope` = `Once | Call | Turn | Session | Persist | Custom(Str)` [R] | `Custom` is a forward-compatible host scope | unmappable to a CLI → fail closed |
| `approvals.rs:ApprovalSource` = `User | External | Forwarded | Config | Extension | Timeout | Unavailable` [R] | `Forwarded` has no production producer [S] | a delegated answer is `External`/`User` |
| `approvals.rs:ApprovalRoute::request_cancellable` [C], `ApprovalDesk`, `ApprovalBook` [R signatures] | journals the prompt, remembers session grants, publishes `KernelEvent::ApprovalRequested` | **REUSE**: the production `PermissionResolver` should wrap a kernel-bound `ApprovalRoute` [S] |
| `crates/agent/src/dispatch.rs:ToolAdmission`, `ToolAdmissionVerdict` [R declarations] | policy decides before any prompt | stays authoritative; delegated approvals do not bypass it |
| `crates/envd/src/admission.rs:ApprovalMode` = `AlwaysAsk | Write | Yolo` [R], `ApprovalTier` = `Read < Fetch < Write < Exec` [R] | OMP's approval posture | contract mirrors the three postures (see §8 for why it cannot import them) |
| `crates/envd/src/exec_settings/sandbox.rs:ExecSandboxMode` = `Off | ReadOnly | WorkspaceWrite` [R], `SandboxNetworkMode` = `Disabled | Open | Scoped` [R] | OMP's sandbox posture | same |
| `crates/ext/src/workspace_trust.rs:WorkspaceTrust` = `Untrusted | Trusted` [R] | explicit `Yolo` is clamped to `Write` in an untrusted workspace (`admission.rs:effective_approval_mode` [R]) | the driver adapter applies this clamp before building the contract's policy |
| Print mode auto-denies every `ApprovalRequested` (`crates/app/src/print_mode.rs`) [S] | no interactive host | a delegated approval in print mode resolves to deny |

Gap [S]: nothing translates between a foreign CLI's sandbox/approval flags and `ApprovalMode`/`ExecSandboxMode`;
`SandboxState` describes omp's own exec sandbox, not a CLI's.

Events: `crates/agent/src/events.rs:KernelEvent` [R: variants `InferenceStarted`, `StreamRedirected`, `StreamObserved`,
`InferenceRetry`, `Usage`, `TurnEnded`, `TextDelta`, `ThinkingDelta`, `ToolReady`, `ToolUpdate`, `ToolSettled`,
`ApprovalRequested`, …] is explicitly ephemeral (doc: "dropping these events cannot change replay"). `ToolReady`/`ToolSettled`
carry only ids [S], so rendering a CLI's tool activity needs journaling, which is outside this contract.

## 4. Credential access

| Seam | File:symbol | Verdict |
|---|---|---|
| Storage | `crates/ai/src/auth/store.rs:CredentialStore` [S]; opened by `crates/driver/src/registry.rs:open_credential_store` [S]; omp's own keychain item `dev.omp.llm` | the **only** credential store. The executor adds none |
| Selection | `crates/ai/src/account/pool.rs:AccountPool::select` [S] | provider-id keyed; not needed by a CLI-managed executor |
| Lease/refresh | `crates/ai/src/auth/lease.rs:CredentialSource`, secret-opaque `CredentialLease` [S]; `auth/manager.rs:AuthManager`, `AuthControlHandle` [S] | REUSE only if OMP must hand a CLI its *own* provider credential; never otherwise |
| OAuth primitives | `crates/oauth` (module doc says it owns no selection/persistence/leases) [S] | not an executor concern |
| Secret masking | `crates/secrets`, `crates/cache/src/secret_key.rs` [S] | masking, not provider credentials |

Code that reads **other applications'** files, which the executor MUST NOT use or imitate [S]:

1. `crates/driver/src/v1_import/auth_credentials.rs:import_credentials` – omp v1 `agent.db` (sibling-install migration).
2. `crates/app/src/auth_broker_cmd.rs:import` / `load_import_plan` – explicit operator command over CLIProxy `*.json`.
3. `crates/ai/src/auth/aws.rs` – `~/.aws/credentials`, `~/.aws/config`, `~/.aws/sso/cache`.
4. `crates/ai/src/auth/adc.rs:AdcSourceSpec::CredentialFile` – Google ADC file.
5. `crates/app/src/session_import.rs` – foreign **transcripts** under `.claude` / `.codex` (not credentials).

A scout grep for `auth.json`, `.credentials.json`, `CODEX_HOME`, `CLAUDE_CONFIG_DIR` and `security find-generic`
across `crates/` found no production hit [S]. The contract therefore adds no API surface that accepts a credential,
environment map or auth path (`SessionRequest` has none) so a future adapter cannot be handed one through the contract.

## 5. Receipts, usage and observability

| Seam | File:symbol | Verdict |
|---|---|---|
| In-memory receipt | `crates/ai/src/receipt.rs:ExecutionReceipt` [C] (carried in `Completion.receipt`) | produced by the Tower stack; a CLI-owned loop produces none |
| Durable receipt | `crates/journal/src/data.rs:TurnReceipt` [C], written by `crates/agent/src/loop.rs` `receipt_facts` / `Session::receipt` [S]; folded by `crates/session/src/fold.rs` [S] | **REUSE** to journal a delegated turn. Gap [S]: no field for CLI-reported model/session id (`ReceiptIdentity{role, provider, model}` only) |
| Usage | `crates/ai/src/receipt.rs:Usage` (input/output/reasoning/cache tokens, `UsageSource{Unknown, Provider, Measured, Estimated, Mixed}`) [R declaration] | **REUSE** as the contract's usage payload; CLI-reported usage is `UsageSource::Provider` |
| Telemetry API | `crates/observability/src/span.rs:start_invoke_agent`, `start_chat`, `start_execute_tool`; `semconv.rs:Operation::InvokeAgent` [S] | vocabulary fits; **GAP:** a scout found zero production callers outside the crate, so executor spans would be the first |
| Per-request hook | `crates/ai/src/layer/observe.rs:Observer` [S] | belongs to the `omp-ai` stack; not used by a CLI-owned loop |

## 6. Provider and route transport contracts

| Seam | File:symbol | Verdict |
|---|---|---|
| Agent kernel model seam | `crates/agent/src/loop.rs:Inference` [R signature via grep, scout body]: `chat(&mut self, ChatRequest) -> impl Future<Output = Result<ChatStream, omp_ai::Error>> + Send` (RPITIT, not object-safe; `director.rs:ErasedInference` is the boxed companion [S]) | **Not the integration point for an agent-loop CLI.** The kernel owns the loop, dispatch and journal around it; a CLI that runs its own tools does not fit. Useful only for token-stream CLIs |
| Stream vocabulary | `crates/ai/src/event.rs:ChatEvent` [S] – `ToolCallReady` is "the sole execution authorization" | **Hazard:** emitting a CLI's tool activity as `ToolCallReady` would make the kernel execute it a second time. The contract's tool events are observation-only and have no execution-authorizing variant |
| Transport trait | `tower::Service<TransportRequest>` in `crates/ai/src/codec/mod.rs`; `provider/builtin.rs:LocalRouteBackend` [S] | applefm precedent (`crates/ai/src/local/applefm.rs`: `AppleFmCodec`, `AppleFmTransport`, `CodecProfile::AppleFm` + `TransportKind::Local`) lowers to a text-only prompt with no tools [S] – inadequate for an agentic CLI |
| Bidirectional stream | `ChatStream::duplex` / `ChatControl` are `pub(crate)` [S] | an external crate cannot build one |
| Availability evidence | `applefm.rs:AppleFmAvailabilityEvidence`, `AppleFmSupportState` [S] | **REUSE the pattern** (probe → typed evidence → `RouteUnavailable` reason) – mirrored by `ProbeReport` + typed errors |
| Terminology | `AGENTS.md` ("transport = provider wire protocol ≠ dialect = thread rendering") | a delegated executor is neither; it is a third thing and must not be registered in the catalog as a provider |

## 7. Error-style precedent

Conforming (typed `#[source]`, named facts, static `#[error]`): `crates/ai/src/registry.rs:RouteUnavailable` [S],
`crates/ai/src/auth/store.rs:StoreError` [S]. Not to copy: `applefm.rs:AppleFmError{code, message: Str}` [S] (string payload).

## 8. Where the contract lives (decision)

ADR 0043 and the research note leave open "driver or a new `omp-agent-exec` library; decide after tracing". Findings:

- `AGENTS.md` Architecture: `omp-driver` is the *composition* boundary; `omp-agent` owns "job board, cancellation tree,
  Directors, hooks, extensions, and approvals". The contract is vocabulary and traits over exactly those, not composition.
- A contract placed in `omp-driver` could not be tested without linking envd (and embedded Python); in `omp-agent` it is
  testable alone.
- `omp-agent` cannot depend on `omp-envd`: `crates/envd/Cargo.toml` depends on `omp-agent` [R], so `ApprovalMode` /
  `ExecSandboxMode` (both in envd) are unreachable from the agent crate. The contract defines its own three-axis policy
  vocabulary and the driver adapter converts (after applying `effective_approval_mode`).
- No dependency edge to `omp-ext` exists in `crates/agent/Cargo.toml` [R], so `WorkspaceTrust` is likewise converted by the driver.
- ADR 0043 does not demand a new crate; AGENTS.md requires a labeler entry for each new crate. Decision: a `delegated`
  module in `omp-agent`. Adapters (CLI-2+), which spawn processes, belong in `omp-driver`/`omp-envd` where the spawn,
  sandbox and approval-relay authorities already live.

## 9. Gaps CLI-2 must close (not done here)

1. Extract one protocol-neutral stdio-child supervisor from `StdioTransport`/`EvalChild` (spawn, pgid, `ExecSandbox`, env policy, stderr capture, TERM → grace → KILL, Drop).
2. Decide the env policy for a child that legitimately needs the user's own CLI login (eval's secret filter would strip it; MCP inherits everything).
3. Token chain from `Kernel::session_cancellation()` into the job token; a job kind for external agents; a process-aware terminate.
4. Journal/receipt shape for observed foreign tool activity and CLI session ids.
5. First production wiring for observability spans.
6. A wrapper that implements the contract's `PermissionResolver` over `ApprovalRoute::request_cancellable`.

## UNVERIFIED

Not opened in this pass:

- `crates/envd/src/worker.rs`, `worker_pool.rs` beyond signatures; `exthost/{control,lifecycle,dispatch}.rs`; `browser_daemon.rs` teardown; `tool_shell.rs` beyond `resolve_cwd`; sandbox backend internals; `crates/env/src/client.rs` beyond `signal`/`start_process`/`ProcessAttachment`; `crates/env` README.
- `Dispatcher::drive`/`admit` bodies in `crates/agent/src/dispatch.rs`; whether `Kernel` shutdown cancels the `JobBoard`; how the TUI answers child approvals; `crates/proto` approval wire types; `headless/kernel.rs` `ProductionInference`/`ComposedInference` bodies; `workpool*`, `autoreply`, `yield_assembly`.
- `crates/ai` `RawEvent` variants, layer ordering, `AdmissionControl`; full `ErrorKind` list; serve `InferenceRpc` auth gating; observability metrics/export bodies; whether `omp_proto` `Usage` mirrors `omp_ai::Usage`.
- Every item tagged [S] above (scout-read only).
- Whether any real third-party CLI (Codex app-server) behaves as the research note describes; no CLI was run.
