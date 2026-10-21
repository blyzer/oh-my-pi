# 0047. Agents build through a thin capability-shaped `omp-exec` over existing primitives; `BuildExecutionPort` is deferred

Status: proposed; requires owner review. No implementation is authorized by this record.
Date: 2026-10-11
Area: runtime

## Context

OMP² agents must build and test arbitrary projects (Rust, Go, JVM, JS, Python, .NET, Bazel), locally and eventually remotely. Today:

- **Builds work only as raw text.** Every build runs through `bash@2` → `EnvToolExecutor` → envd `ShellExecHost` → `ExecHost`, sandboxed, cancellable, bounded and auto-detached into `JobBoard` (`crates/tools/src/shell.rs`, `crates/envd/src/exec.rs`, `crates/agent/src/jobs.rs`). Results are raw text, and placement is always local.
- **ADW and cleanse bypassed envd (audit F11); the ADW half was fixed in `9dd09d888f`.** `crates/driver/src/adw/production.rs` now dispatches code phases through the project Environment's exec authority, and `omp adw run` applies the operator cfg (`process_ctx_with`) and fires its cancel token on Ctrl-C (`crates/app/src/adw_cmd.rs:62-73,122`). That commit also changed `crates/driver/src/cleanse/production.rs`; whether `run_checker` is fully routed through envd now was not re-checked here (UNVERIFIED).
- **No remote execution product code exists.** ADR 0040 (proposed) defines the contract `BuildSystemAdapter → ExecutionRequest → place() → ExecutionBackend`. Its 2026-10-10 amendment selected Option D: a neutral core, capability-selected backends, and vendor features behind the backend.
- **One remote path is measured, one is refuted.**
  - Measured: BuildBuddy Run API warm snapshots (3 min 12 s against 4 min 10 s locally, with about 1 s Mac CPU; `docs/audits/remote-verification-trial.md`).
  - Refuted: raw REAPI runner recycling (Slices 0b/0c).
- **Remote invocation from CI is unproven.** `buildbuddy-remote.yml` exited silently because `actions/checkout` does not create `refs/remotes/origin/HEAD`. `scripts/remote-verify.sh` resolves it with `git symbolic-ref … | sed` under `set -euo pipefail` and stderr discarded. PR #250 (merged `506b7c143e`) fixes this. No run since has recorded a BuildBuddy invocation.
- **A single port was proposed.** It is `BuildExecutionPort → {LocalCargo, RemoteCargo, Bazel, BuildBuddy, GitHubActions}Adapter`. It conflates build system, execution environment, provider, cache, CI orchestrator and artifact store, against ADR 0040 R3, ADR 0003 (one durable authority) and ADR 0010 (one job primitive).

Full analysis: [docs/research/provider-neutral-build-execution.md](../research/provider-neutral-build-execution.md). Provider facts: [docs/research/build-execution-ecosystem-survey.md](../research/build-execution-ecosystem-survey.md). envd boundary: [docs/research/envd-responsibility-audit.md](../research/envd-responsibility-audit.md).

## Decision

1. **Model E, shaped as D.** Extend the existing architecture. Do not add a parallel execution system.
   - A new pure crate `omp-exec` holds:
     - `ExecutionRequest`, `BackendCapabilities` (`input`, `stream`, `cancel`, `warm_caches`, `snapshot`, platforms, enforced posture, `max_timeout`);
     - `ExecutionBackend`/`ExecutionRun` (enum-dispatched, copying the `ShellExec`/`ShellRun` shape);
     - typed `Outcome` that separates command failure from infrastructure failure;
     - `BuildSystemAdapter`, a pure `place()`, and a normalized `BuildReport`.
   - Hosting reuses `JobBoard`/`JobSettlement`, `BlobStore`/`artifact://`, `CancelTree` and envd `ExecHost`.
2. **`BuildExecutionPort` is deferred.** It is rejected in its proposed form. Bazel is a Local-placed adapter that never wraps its actions. GitHub Actions is a CI capability, not a working-tree executor. BuildBuddy remains two backends (Run API, generic REAPI) per the ADR 0040 amendment.
3. **envd does not become a build orchestrator.** It keeps its process, filesystem, blob and sandbox authorities, and later gains only a git-semantics `CaptureInputs`. Placement, adapters and vendor clients live outside envd.
4. **First slice: Local backend plus `CargoJust`** for one ADW code phase (`just test-pkg omp-envd`), returning a `BuildReport` from cargo JSON and nextest JUnit. This closes F11 for ADW. Remote backends wait for Gates 2–3. This ordering applies to agent builds generally. It does not amend ADR 0040's ordering for OMP²'s own builds; the owner must reconcile the two.
5. **Lifecycle** is `CREATED → QUEUED → PREPARING → RUNNING → {SUCCEEDED, FAILED, CANCELLED, TIMED_OUT}`. Cancel settles locally. Retries happen only before backend acceptance. Remote truth is reported as `Requested | Unsupported`.
6. **Gates.** Implementation proceeds only through these gates:
   - **G1:** owner acceptance.
   - **G2:** remote invocation proven after PR #250.
   - **G3:** credential containment.
   - **G4:** Local parity and F11 closed.
   - **G5:** capture correctness.
   - **G6:** structured results.
   - **G7:** observability correlation chain.
   - **G8:** measured relief before any remote default.
7. **Trust classes** T1 (owner workspace), T2 (third-party repo), T3 (untrusted or fork code), T4 (agent-generated changes) and T5 (release) govern placement, upload, cache writes, secrets and snapshot reuse, as tabulated in the research report. All `sv_exec_*` settings are user scope, never project.

## Consequences

- **Easy:**
  - Every agent-facing build gains structured results and journaled provenance, with no new job, spill or cancel path.
  - New build systems are adapters only.
  - New providers are backends only, selected by capabilities.
- **Hard:**
  - Adapters must report eligibility honestly.
  - Ref-based CI cannot satisfy dirty-tree builds without an approved scratch-ref push.
  - REAPI gives post-hoc logs and best-effort cancel only.
- **Prohibited:**
  - A single build/provider port.
  - Vendor names, headers or properties in `omp-exec`.
  - Automatic rerun after acceptance.
  - Treating an AC hit or a snapshot as a verdict.
  - Envd-owned placement.
  - Remote-by-default before G8.
- **Cost accepted:** one new small crate, plus a `JobOwner::RemoteExecution` variant in stage 2.
- **Not decided here:**
  - The first remote backend's credential model (G3).
  - The scratch-ref policy.
  - `Outcome::Concluded` for CI.
  - Whether remote verification becomes a CI gate.

## Status in omp

**Partially implemented.** Re-verified against `omp2` at `9e0c073d04`. The `omp-exec` crate (`crates/exec/`), `CargoJust` adapter, `place()` router, `BuildReport`, `BuildStatus`, and all four backends (`omp-exec-reapi`, `omp-exec-buildbuddy`, `omp-exec-local`, `omp-exec-ssh`) exist. `omp.exec.*` telemetry spans and `omp.exec.worker.*` tracing events from `ExecutedActionMetadata` are wired (PRs #350, #351, #352, #353). Gate 2 proven 2026-10-19: remote CI invocation dispatched and 54/54 tests passed, invocation `03d524aa-71d2-40ed-b92f-f1da843c63af` (PR #339). Open: `BuildExecutionPort` is explicitly deferred by this ADR; no `omp-envd` run through the omp REAPI backend is recorded (the 2026-10-20 attempt failed on a missing `cmake`, which the image recipe now installs, PR #360); the wall-time and client peak-memory success criteria both fail on the available `omp-envd` `bb remote` measurement (`docs/audits/remote-verification-trial.md` §G1).

## References

- ADRs 0003, 0006, 0009, 0010, 0011, 0028, 0039, 0040 (and its 2026-10-10 amendment).
- `docs/audits/remote-verification-trial.md`; `scripts/remote-verify.sh`; `.github/workflows/buildbuddy-remote.yml`; PR #250 (`506b7c143e`).
- Code: `crates/tools/src/shell.rs`, `crates/envd/src/exec.rs`, `crates/envd/src/workspace/operations.rs`, `crates/agent/src/jobs.rs`, `crates/tool/src/lib.rs`, `crates/journal/src/blob.rs`, `crates/driver/src/adw/production.rs`, `crates/driver/src/cleanse/production.rs`, `crates/app/src/adw_cmd.rs`.
