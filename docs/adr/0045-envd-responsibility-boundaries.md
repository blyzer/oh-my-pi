[docs/adr/0045-envd-responsibility-boundaries.md (verbatim, 127 lines total)]
[docs/adr/0045-envd-responsibility-boundaries.md (verbatim, 128 lines total)]
# 0045. Proposed envd responsibility boundaries: preserve the authority core, shed the periphery

Status: accepted 2026-10-28; Alternative F selected
Date: 2026-10-11
Area: runtime/environment

## Context

`omp-envd` was created in `768749cf01` and split into its own crate in `8e3f027aae`. Its job: be "the single owner of project environment resources" (README v1). It provides:
- the daemon transport;
- filesystem, process, document and tool authorities;
- Python extension-host, worker and eval supervision (AGENTS.md:39-44; ADRs 0006, 0011, 0036; the document-authority Locked Deviation, AGENTS.md:500).

On `omp2` (`bc1e12d21b`) the crate has grown large:
- 199,669 lines in 179 files;
- it depends on 31 of 48 workspace crates;
- it exposes about 1,595 `pub` functions;
- it took part in 79 of 185 crate-touching commits in the last 400-commit window.

The research audit is `docs/research/envd-responsibility-audit.md`. It finds:

- **The core is legitimate, required centralization.** Single per-project document authority (flock + CAS + rebase), effect-token admission at the effecting process, exec/sandbox/egress with the TERM → grace → KILL boundary, blobs, workspace generations, and Python supervision. The document authority and the durable detached-process lease are the two invariants that strictly require an out-of-process singular owner.
- **About 36k lines (~18%) are a periphery assigned by no governing document.** Browser, computer, GitHub, media/TTS, security scan, report_issue, SSH, vault, schedules, model discovery, MCP config/OAuth, a duplicate VCS substrate, and the absorbed `grep` crate. They arrived through three omnibus commits and account for most of the dependency fan-out.
- **Most cross-crate co-change is the designed cost of the typed boundary.** Features cross `tool → proto → env → envd → driver → app`. envd causes two coupling sources itself:
  - the periphery;
  - a vocabulary inversion: envd depends on `omp-agent` approval and hook types, while driver imports envd-internal posture and eval-bridge types.
- **Client-side failure containment is weak.** The partition router leaves pending remote streams without an error when the daemon closes. A later remote-bound send ends the whole router, including the Local backend. `EnvClient` does not reconnect.
- **Hot-path costs are identified from code but not measured.** Each remote environment call costs 2 awaited round trips, a per-frame flush and a dual CAS write; a single-file edit costs about 5 docserver round trips.

## Decision (proposed)

Adopt Alternative F (hybrid) from the audit:

1. **Preserve** envd's authority core and the current process topology:
   - the attached daemon over UDS;
   - the in-process session host behind `PartitionedEnvTransport`;
   - the embedded and isolated fallbacks.

   Do not split the core into more processes, do not thin envd into a supervisor, and do not introduce an event bus.
2. **Optimize internally.** Split `server.rs`, `tools.rs` and `mcp/` into focused modules with no behavior change.
3. **Decouple** shared vocabulary into a contract crate below `omp-agent`, `omp-envd` and `omp-driver`:
   - approval and hook types;
   - approval posture and mode;
   - the eval parent-bridge types.

   Move `vcs/` into `omp-vcs`, restore `grep` as its own crate, and move `managed_skills_domain` and `ssh` out of envd.
4. **Extract** peripheral integrations behind the existing `RegistryBridges` / `DynamicToolFactory` / `ContentResolver` seams, one integration per change. envd keeps only their admission and supervision hook.
5. **Fix containment** in `omp-env`:
   - fail pending routes of a closed backend with a typed error;
   - keep the Local backend alive when Remote closes;
   - decide on reconnect.
6. **Change the hot path only after measurement.** Candidates are a pipelined invoke/commit, fewer docserver round trips, per-frame flush, and the dual CAS write. Each requires the P8-recorded baseline and the relative thresholds in audit §12. Nothing becomes a hard gate.
7. **Defer, pending owner decisions:**
   - admission placement when envd runs on untrusted workers (ADR 0006 vs 0039);
   - whether the approval and reflection relays are exceptions to ADR 0006's duplex prohibition;
   - the MCP process locus;
   - per-subagent daemon sharing;
   - the future of `schedules`.

## Alternatives considered

| Alternative | Weighted score (audit §10) | Disposition |
|---|---|---|
| A. Preserve as-is | 340 | Rejected. Keeps the build-weight and inversion costs that AGENTS.md:540-542 treats as defects |
| B. Modular monolith | 385 | Accepted as the base of F. F falls back to B if measurements show no material IPC overhead |
| C. Thin supervisor | 265 | Rejected. Moving document authority into the agent process breaks per-project singularity (Locked Deviation) |
| D. Capability-oriented services | 275 | Rejected. More hops on the edit path and more lifecycles, with no invariant gained |
| E. Event-driven | 225 | Rejected. A bus between the effect gate and the effect weakens ordering and cancellation proofs |
| F. Hybrid | 420 | Proposed |

The weights come from AGENTS.md priorities:
- invariants 30 (Locked Deviations, journal-first);
- hot-path performance 20 ("load-bearing … NEVER weaken");
- build weight 15 ("cold build time is a gate");
- locality 10, containment 10, reversibility 10 (clean cutovers, no shims);
- ops 5.

The performance scores are unmeasured estimates.

## Consequences

**Positive:**
- envd stops depending on `omp-agent`, `omp-webview`, `omp-desktop`, `omp-audio`, and most `omp-ai` use.
- Driver stops reaching into envd internals for vocabulary.
- envd's churn concentrates on the authorities it is meant to own.

**Unchanged:**
- The process topology and the env/v1 protocol shape, except for measured hot-path changes, which bump `SCHEMA_REV`.
- The journal-first durability model.
- ADR 0006/0011/0036 guarantees.

**Costs:**
- About 15 sequential, self-contained changes (audit §14, ENVD-01…15), each migrating every caller with no shims.
- One new contract crate and several extracted crates, each needing a `.github/labeler.yml` `area/*` entry.

**Risks:**
- Extracted integrations still need an environment-side execution locus.
- Moving vocabulary touches approval paths, which carry recent churn under ADR 0028.

## Accepted evidence and remaining measurements

- P8 baseline for audit workloads W1–W9 in attached and embedded modes on macOS arm64 and Linux.
- **DONE** W8 daemon-kill acceptance: the partition proof and the real-daemon SIGKILL proof now pass with typed remote terminal errors, local-backend survival and no collateral local termination.
- Cold build-time and envd compile-time numbers before the first extraction.
- Owner answers to the open questions in Decision item 7.

## Implementation gate

The owner accepted Alternative F on 2026-10-28. Implementation is authorized under the sequencing and evidence gates below. Each step then proceeds as one reversible change with its acceptance criterion from audit §14. Hot-path changes additionally need a recorded baseline that meets audit §12.3.

## Amendment (2026-11-09): placement and backlog under ADR 0049

[ADR 0049](0049-tiered-integration-architecture.md) sets the placement rule for the periphery that this record extracts. The owner also ruled that the 2026-11-05 vendor rule (ADR 0040 amendment) applies to forge and CI vendors.
- **ENVD-07 widened.** Moving `ssh` to its own crate also removes `omp-exec-ssh`'s dependency on `omp-envd`.
- **ENVD-16 (landed).** The `ExecHost` contract (`ExecError`, `ExecEvent`, and the `ExecHost` / `ExecRun` traits) is in `omp-exec`. envd implements the traits; the concrete session host stays in `crates/envd/src/exec.rs`. `omp-exec-local` does not depend on `omp-envd`.
- **ENVD-08/09 realized as generic capability crates behind `cap-*` features.** The `github`/`github_url` item is not moved as a crate. It is replaced by the neutral `omp-forge`/`omp-ci` contracts and a `Delegated` adapter, with the GitHub adapter outside core (ADR 0049 Phase 4).
- Alternative F, the item-7 rulings and the evidence gates are unchanged.

## Status in omp

**Status: Partially implemented.** Alternative F is accepted. P8 W1-W9 evidence is complete; vocabulary extraction, modularization and peripheral extraction remain open; containment is implemented and accepted by the W8 proofs.

- W8 containment is implemented and verified by the partition and real-daemon proofs:
  - `w8_probe_remote_close_leaves_stream_pending_then_drops_local_stream` passes in `omp-env`.
  - `w8_sigkill_daemon_leaves_remote_pending_then_drops_local` passes against the production daemon on macOS.
- Owner answers in decision item 7 are recorded: host-side admission with envd enforcement, issuing-connection-only approval relay, bounded reflection relay, envd-owned MCP, digest-matched daemon sharing, deferred reconnect and deferred schedules. P8 W1-W9 baselines are complete through PR #450 and workflow run 41070076113.
- `adr0045-slice-b.md` remains supporting investigation evidence.

## Build-weight baseline (ADR 0049 Phase 0)

P8 W1–W9 evidence in this record is the runtime baseline (PR #450, workflow run 41070076113; the recorder is `crates/e2e/tests/p8_baselines.rs`). ADR 0049 Phase 0 adds the build-weight baseline audit §12.3 rule 5 asks for. HTML reports from `cargo build --timings` are not committed. Each cold build used a fresh `CARGO_TARGET_DIR`. The incremental build reused the cold `omp` directory after one added line in `crates/envd/src/lib.rs` (`//! Phase 0 incremental fingerprint.`), then that line was removed. No code change remains.

Measured 2026-11-09 03:36–04:02 America/Santo_Domingo on this Linux host: Intel Xeon, 4 vCPU (`nproc` 4), kernel 6.12.113+, `x86_64-unknown-linux-gnu`. Toolchain: rustc 1.99.0-nightly (1a98b1e13 2026-08-07), cargo 1.99.0-nightly (c79e8f894 2026-08-04). Profile `dev` (workspace members use the Cranelift backend; `debug = false`). Max concurrency reported by cargo: 6, with `jobs=4` and `ncpu=4`.

| Build | Wall time | Total units | Fresh | Dirty |
|---|---|---|---|---|
| `cargo build --timings --bin omp --locked`, cold | 282.2s | 1433 | 0 | 1433 |
| same, after the one-line `omp-envd` edit | 90.8s | 1433 | 1428 | 5 |
| `cargo build --timings -p omp-envd --locked`, cold | 259.8s | 1398 | 0 | 1398 |
| `cargo build --timings -p omp-exec-local --locked`, cold | 251.9s | 1399 | 0 | 1399 |
| `cargo build --timings -p omp-exec-ssh --locked`, cold | 252.5s | 1399 | 0 | 1399 |

The five dirty units in the incremental build, slowest first: `omp-envd` 53.10s, `omp-app` lib 23.02s, `omp-driver` 21.37s, `omp-app` bin `omp` 4.68s, `omp-exec-local` 0.74s.

Ten slowest units, cold `omp` (1433 units): `omp-envd` 53.04s, `omp-ai` 51.10s, `omp-proto` 27.77s, `omp-app` 22.09s, `omp-driver` 20.93s, `omp-tools` 19.93s, `jj-lib` 17.59s, `tree-sitter-ocaml` build-script run 10.48s, `omp-catalog` 10.27s, `omp-tui` 10.24s.

Ten slowest units, cold `omp-envd` (1398): `omp-ai` 53.18s, `omp-envd` 52.89s, `omp-proto` 29.17s, `jj-lib` 19.05s, `omp-tools` 18.54s, `tree-sitter-ocaml` build-script run 11.29s, `omp-catalog` 11.12s, `omp-shell-builtins` 10.79s, `x11rb-protocol` 8.00s, `anydoc` 7.97s.

Ten slowest units, cold `omp-exec-local` (1399): `omp-ai` 51.51s, `omp-envd` 51.38s, `omp-proto` 27.61s, `jj-lib` 18.68s, `omp-tools` 17.48s, `omp-catalog` 10.67s, `tree-sitter-ocaml` build-script run 10.49s, `omp-shell-builtins` 10.47s, `anydoc` 7.75s, `x11rb-protocol` 7.69s.

Ten slowest units, cold `omp-exec-ssh` (1399): `omp-ai` 51.63s, `omp-envd` 51.18s, `omp-proto` 27.28s, `jj-lib` 18.46s, `omp-tools` 17.50s, `omp-catalog` 10.81s, `tree-sitter-ocaml` build-script run 10.50s, `omp-shell-builtins` 10.32s, `x11rb-protocol` 7.83s, `anydoc` 7.79s.

Closure counts are `cargo tree -p <crate> -e normal --prefix none --locked | sort -u | wc -l`. There is no package `omp`; the `omp` binary is package `omp-app`, and `-p omp` fails with "did not match any packages". On this cargo (1.99 nightly) the workspace `[resolver] feature-unification = "workspace"` makes `cargo tree -p <crate>` print every workspace member, so the command as written returns one number per target for all four crates: 1420 with no `--target`, 1428 with `--target x86_64-unknown-linux-gnu`, 1372 with `--target aarch64-apple-darwin`. The per-crate numbers below pass `--config 'resolver.feature-unification="selected"'` so `-p` selects one crate. This machine is Linux, not macOS. `aarch64-apple-darwin` is not installed; that column is cargo's resolution for the triple, not a count taken on a Mac.

| Crate | Host, no `--target` | `--target x86_64-unknown-linux-gnu` | `--target aarch64-apple-darwin` (resolved, not a Mac) |
|---|---|---|---|
| `omp-app` (bin `omp`) | 1327 | 1335 | 1303 |
| `omp-envd` | 1144 | 1152 | 1120 |
| `omp-exec-local` | 1146 | 1154 | 1122 |
| `omp-exec-ssh` | 1146 | 1154 | 1122 |