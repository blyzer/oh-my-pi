# 0052. One executor contract: native OMP2, delegated agents and external harnesses

Status: proposed
Date: 2026-11-12
Area: runtime

## Context

- The delegated boundary exists. `DelegatedExecutor` and `DelegatedSession` (`crates/contract/src/delegated/executor.rs:123,156`) open sessions, stream events, resolve permissions and interrupt. `Capabilities` names an executor, its mode, supported CLI versions, a feature set and "the exact policies this executor can enforce"; a requested policy is accepted only if listed verbatim, with no "closest stricter" fallback (`crates/contract/src/delegated/capability.rs:140-160`).
- It is not connected to normal work. `ExecutorRegistry` and `run_delegated_turn` are defined in `crates/driver/src/delegated_host.rs:131,338`; a search of `crates` finds no use of `ExecutorRegistry` outside that file and no caller of `run_delegated_turn` outside it. No scheduler, `task` dispatch or ADW phase reaches a delegated executor. Roadmap item P0 (owner, 2026-11-12) is exactly this wiring.
- ADW selects a model per phase but no executor. Every model phase uses the one selector the run resolved ("explicit `--model`, then `ai_model`, then `@smol`" per the doc comment on `model_phase_selector`, `crates/driver/src/adw/production.rs`). A phase always runs a native OMP2 child kernel (`run_child`).
- Vendor integration is already decided elsewhere: ADR 0043 (ACP client with launch profiles, amended 2026-10-20 and 2026-11-11), ADR 0049 (vendors are extensions; the Codex app-server adapter is a recorded exception) and `docs/design/acp-extension-profiles.md` (merged as PR #591), which covers the extension-declared profile manifest, A2A, MCP-server-as-agent and Agent SDK as shims. This record does not redo that. It adds the missing layer above it.
- OMP1 has a Vibemon integration at `packages/metaharness/src/tb/vmon.ts` that adapts Vibemon sandbox commands and guest processes to the OMP metaharness interface (owner-stated, 2026-11-12; read in upstream can1357/oh-my-pi, 349 lines, on 2026-11-12). It defines `TrialVm`, one persistent Vibemon microVM per Terminal-Bench trial: it boots an OCI image, runs captured commands (`exec` with timeout, env and cwd), copies files and directories in (`copyTo`, `copyDirectory`), reads a guest file (`readFile`), attaches a host gateway so the guest can reach a host-local HTTP target (`startGateway`, used for inference), builds an RPC transport that runs the agent in the guest over a streaming exec and adapts it to the OMP RPC client (`rpcTransport`), and removes the sandbox (`rm`). The rest of the metaharness (its adapters and runner) was not read. OMP2 has `omp-exec-local`, `omp-exec-ssh` and `omp-exec-reapi` plus `omp-sandbox`, and no Vibemon adapter (a search of crates, docs, extensions, scripts and the justfile finds no mention).
- Owner roadmap P2: run the same workload through two harnesses without rewriting the workflow.

## Decision

**D1. A phase does not name a vendor.** A phase declares what it needs: the capabilities (today's `Feature` variants: `Resume`, `Interrupt`, `Permissions`, `StreamingText`, `ToolEvents`, `UsageReporting`, `WorkingDirectoryInheritance`; additions such as subagents, filesystem isolation and model selection are proposed, and `FeatureSet` is a `u8` with room for one more bit, so adding them widens it, a change to the delegated contract the owner should see), the policy it requires (a `DelegatedPolicy`: sandbox, network, approval, matched verbatim), a budget and acceptance criteria. A router picks an executor that satisfies all of it.

**D2. The router refuses; it never weakens.** If no executor satisfies the required policy, the assignment is rejected or routed to another executor. It is never run with a weaker policy. This keeps the existing exact-match rule of `Capabilities`.

**D3. Assignment and outcome types (design sketch, owner's, adopted as a starting point).** A `WorkAssignment` carries work item, phase, attempt, executor, workspace reference, acceptance criteria, budget and policy. An executor is `NativeOmp` or `Delegated(ExecutorId)`. A `WorkOutcome` is `Accepted(EvidenceBundle)`, `Rejected(VerificationFailure)`, `NeedsHuman(ApprovalRequest)`, `Cancelled` or `Uncertain(RecoveryReference)`. Names are provisional; the existing `delegated` contract keeps its types and gains these as a layer, so only the `FeatureSet` widening in D1 changes an existing type.

**D4. The executor reports evidence; ADW decides.** An executor never marks its own work accepted. Acceptance is decided by ADW from deterministic checks and an independent verdict (ADR 0053).

**D5. Native OMP2 is an executor.** The existing child-kernel path (`run_child`) becomes the `NativeOmp` implementation behind the same interface, so a workflow can move a phase between native and delegated without edits.

**D6. Wire production first (P0).** Before any new vendor work: connect `ExecutorRegistry` and `run_delegated_turn` to one production path (the `task` dispatch and an ADW phase), and prove it with one real Codex task that streams, cancels, keeps its journal and enforces permissions.

**D7. Protocols are transports, not executors.** ACP (stdio) is the default transport for vendor agents, per ADR 0043. A2A, MCP-server-as-agent and Agent SDK bridges stay shims in v1, per the profile design §3. A native A2A or MCP executor is considered only when the three criteria in that design (§3.6) are met. The Codex app-server adapter stays a recorded exception with its two exits. Vendor profiles (Claude, Cursor, Devin, DeepSeek harness, Grok, Antigravity and others) are extension data under the design's `delegated-agents` manifest, not new Rust code in core.

**D8. External harness means two things, and both get an extension point.**
1. *Harness as executor:* a third-party agent harness OMP drives (D7).
2. *Harness as host:* an outside runner (a benchmark or evaluation harness, the OMP1 "metaharness") that drives OMP and supplies its sandbox. The OMP2 surface is the SDK and RPC (`omp-sdk`, `omp-rpc`, `omp-serve`) plus an execution-backend extension point (`omp-exec`). A sandbox provider such as Vibemon is an execution backend extension that maps sandbox commands and guest processes onto `omp-exec`, under ADR 0049; `vmon.ts` is its reference. Vibemon (`stencil-hq/vibemon`) is, per its README, a KVM and HVF microVM platform that speaks the E2B API, forks each sandbox copy-on-write from a template snapshot, and can pause a sandbox to disk and resume it on another node (external source, fit unverified). That makes it a candidate for the isolated-workspace and leased-worker needs of ADR 0007 and ADR 0039 as well as for evaluation hosts. A lighter option for the isolated workspace alone is `stencil-hq/ovnfs` (copy-on-write checkouts, diff and three-way apply; see the ADR 0007 amendment). From `vmon.ts` the shape of such an adapter is: a sandbox lifecycle (create, exec, copy in, read file, remove), a host gateway so the sandboxed OMP reaches inference on the host (this is the same idea as ADR 0039's K2, where the worker's kernel reaches inference through the coordinator's gateway), and an RPC stdio transport to the OMP inside it. In OMP2 these map to an `omp-exec` backend extension, the 0039 gateway, and `omp-rpc`. Reading the rest of the metaharness is still needed before fixing the interface.

**D9. Capability negotiation is advertised, then verified.** An advertised capability is not proof of confinement. The documented case: Grok's built-in sandbox may continue without enforcing the requested confinement. The router therefore treats a delegated executor's own sandbox as unverified unless OMP enforces confinement itself (for example by running the child inside `omp-sandbox`), and records which it relied on in the assignment.

**D10. Authority never escalates through delegation.** A child executor receives the parent's effective authority or less, and its budget is carved from the parent's (budgets are hierarchical across goals, workflows, subagents and external harnesses).

## Implementation guide: a MicroVM execution backend (owner-supplied recommendation, 2026-11-12)

This section records the owner's recommended direction as a guide for the harness-as-host and sandbox work in D8. The vendor facts (SmolVM, Microsandbox, Vibemon behavior) come from the owner and were not checked here, except where marked as read. It is a proposal; the open items must be settled before code.

**Shape.** One generic MicroVM capability behind the execution contract, with vendors as adapters:

```
OMP2 Factory / ADW
      v
ExecutionBackend            (crates/exec/src/backend.rs)
  +-- LocalBackend, SshBackend, ReapiBackend      (existing)
  +-- MicroVmBackend                              (proposed)
        +-- E2B/Vibemon adapter                   (implement first)
        +-- SmolVM adapter, Microsandbox adapter  (later, to test the contract)
```

**What already exists.** `ExecutionBackend` has `start` and `reattach`; `ExecutionRun` has a streamed `next_event`, `cancel` and `detach` (dropping an undetached run cancels it); `BackendCapabilities` is where optional behavior is declared (read at `crates/exec/src/backend.rs:139-175`). So creation, execution, streaming, cancellation, reattachment and teardown already have a home. Gaps to specify: **status** (a query without an event stream), **artifact transfer** in and out of the guest (the `vmon.ts` `copyTo`, `copyDirectory`, `readFile` shape), and whether `ExecutionRequest` can express an arbitrary guest command, since `crates/exec` is shaped around cargo and just recipes (`cargo_just.rs`; not checked whether the request type is general).

**Minimum production contract:** create, exec, stream, cancel, status, reattach, teardown, artifact transfer. **Optional capabilities, never assumed by every backend:** checkpoint, restore, fork, pause to disk and resume on another node. They are declared in `BackendCapabilities`; a caller that needs one asks for it, and a backend without it is refused for that assignment, not degraded (D2).

**Three safeguards (normative for this backend):**
1. **Workspace isolation.** A VM is not automatically safe to modify the main repository. The owner reports that SmolVM's coding-agent example mounts the host project read/write, so guest edits change host files at once (unchecked here). Every worker receives an isolated worktree or staged workspace (ADR 0007 and its OVNFS note); its changes reach the main tree only through the verification and integration gate (ADR 0039 B12, ADR 0053). A host mount that is writable to the real tree is refused.
2. **Authority.** OMP2 owns job status, permissions, budgets and acceptance. A VM checkpoint preserves running processes; it never certifies that acceptance criteria passed (ADR 0053 D6).
3. **Recovery.** The VM identity, checkpoint reference and execution id are written to the journal before the effect is dispatched (ADR 0051 D4, D5). After a restart the coordinator reattaches (`reattach` with a `ReattachHandle`) or reconciles the worker; it never launches the same work again blindly. A VM that cannot be reattached settles as `EffectsUnknown`, the outcome the contract already defines for a lost run.

**Order of work (proposal):** (1) read the rest of the OMP1 metaharness and decide how `TrialVm`'s lifecycle maps to `start` and `reattach`; (2) a mock MicroVM backend in `omp-exec` tests to fix the contract gaps above; (3) the E2B/Vibemon adapter as an extension (Vibemon speaks the E2B API, per its README), proven on one workload; (4) a second adapter only if it exposes a gap; (5) isolation per safeguard 1 before any write access to a real tree.

**Open:** where MicroVM-specific capability flags live; how the gateway for inference (as in `startGateway`) is expressed (ADR 0039 K2); host requirements (KVM or HVF) and CI coverage; cost and quota accounting for VM time (B8).

## Consequences

- One workflow can run a phase on native OMP2, Codex or an ACP agent by changing assignment policy, not the workflow.
- Capability mismatches fail loudly at assignment time instead of degrading policy.
- Adds a router and an assignment layer; the executor contract itself is unchanged.
- A harness-as-host adapter (Vibemon, Terminal-Bench style) is possible without touching core.

## Open questions for the owner

- **O1.** Router inputs beyond capabilities and policy: cost and availability are named in the roadmap; are they hard filters or only tie-breakers? Recommendation: policy and capabilities are hard filters, cost and availability only order the survivors.
- **O2.** Where does the router live: `omp-driver`, or a new capability crate beside `omp-adw`? Recommendation: a small module in `omp-driver` first; extract only when a second consumer exists.

## Not yet specified (readiness review, 2026-11-12)

The reviewer found these undefined: the field lists of `WorkAssignment`, `WorkOutcome`, `EvidenceBundle`, `VerificationFailure`, `RecoveryReference`, `Budget` and `ApprovalRequest` (and how `ApprovalRequest` relates to `crates/contract/src/approvals.rs`); the router algorithm (what "satisfies" means, the reject-or-route rule, tie-breaking); how native OMP2 reports `Capabilities` and an `ExecutionMode` (`run_child` returns a string and no events today); the `PhaseSpec` and TOML fields for capabilities and policy; the exact insertion points of the P0 path in `task` dispatch and in an ADW phase; the field that records which sandbox the router relied on (D9); and a CI rule for the live-Codex test (needs credentials). The harness-as-host item (D8) has a concrete reference shape (`vmon.ts`) but no OMP2 interface: the sandbox backend trait, the gateway contract and the RPC transport are not specified.

## Acceptance tests

1. P0: one real Codex task through the production path: stream, cancel, journal preserved, a denied permission stays denied.
2. P2: the same acceptance workload on native OMP2 and on one delegated ACP agent with no workflow edit.
3. A required policy that no executor can enforce yields a rejected assignment, never a weaker run.
4. A fake "advertises sandbox but does not enforce" executor is not trusted for a phase that requires confinement.

## Status in omp

**Status: Partially implemented.** P0 is partly done: an ADW prompt phase can name an `executor` and is routed through `run_delegated_prompt` (`crates/driver/src/delegated_host.rs`), the first production caller of `run_delegated_turn`; delegated phases run read-only with no approver (`DelegatedPolicy::STRICTEST`, `DenyAll`) until the requirement-to-policy mapping is specified, and no real Codex run has been proven. The delegated contract, `Capabilities`, the ACP client and the Codex adapter exist (see ADR 0043 status). The router, the assignment layer, the native-as-executor path and the production wiring do not. Unverified: the OMP1 metaharness beyond `vmon.ts`; whether the `task` tool can name an executor today.

## References

ADR 0043; ADR 0049 D5, D6; ADR 0051; ADR 0053; `docs/design/acp-extension-profiles.md` §1, §3; `crates/contract/src/delegated/`; `crates/driver/src/delegated_host.rs`; `crates/driver/src/adw/production.rs`.
