# Validation, deliverable coverage and unresolved experiments

Pin: `fda1315052b8b4074f9ea5acc842b2cc0b08b2b2`.
Research publication was authorized by the owner on 2026-11-25. The evidence is
not a production optimization, an accepted ADR amendment, or a release claim.
Publication preserves the original audit pin rather than claiming that later
changes on `omp2` were re-audited.

## Validation performed

- Parsed repository/experiment/decision JSON; 14 complete consolidated decisions
  have 43 fields each. Checked decision identifiers/dependency references and
  acyclic dependency traversal; final-answer numbering is exactly 1–50.
- Checked local report links, pinned source file/range targets available in the
  checkout/clones/source cache, and inline source path/range citations. Exact
  counts/outcomes are in [report-validation.json](references/report-validation.json).
- Seven additional pinned external source files were fetched through GitHub's
  contents API; path, commit, Git blob hash, SHA256, line count and range outcome
  are in [external-target-checks.json](references/external-target-checks.json).
  This does not represent an exhaustive live crawl of every official web page.
- Corrected erroneous ADR/source anchors during consolidation, including the
  cancellation ADR filename, the AST language module path, AI session revision
  path and the streaming allocation test's actual agent-owned location.
- Executed native core/journal nextest/doctest recipes and repaired standalone
  probes under the existing split-metadata toolchain. Preserved both failed-link
  attempts and the final successful collection instead of erasing the failures.
- Ran repository editing-discipline and whitespace validation. Confirmed only
  the research directory is new; no tracked production source/ADR/lockfile differs
  from the frozen baseline. The initial research phase created no commit, push
  or PR; publication was authorized separately afterward.

The local checker distinguishes files it can verify from seven remote targets
checked separately. It cannot certify semantic correctness simply by finding a
file or an anchor. Source interpretations were consolidated across independent
native/lifecycle, interop, provider, Saetta, technology and experiment tracks.
No claim of formal security certification or complete runtime coverage follows.

## Required deliverable crosswalk

| Requested deliverable | Location |
| --- | --- |
| A Executive summary | README, prioritized R01–R14 decisions |
| B Repository/revision/license inventory | 00; references repository/local-checkout/branch/workspace graph inventories; source-specific chapter pins/licenses |
| C Current-state architecture | 01/03/04; current diagram in 09 |
| D Saetta comparison | 02, including implementation/portability/fit safeguards and independent tests |
| E Technology matrices | 03 ABI/Python; 05 formats/profiling/allocators/isolation; 09 execution placement |
| F Memory/allocation audit | 01 lifecycle and queues; 03 data/frames; 04 request/capture/TUI; 06 executed probes |
| G Deferred activation inventory | 01 resource table, 07 R07/R08, 09 leases/pressure contract |
| H Serialization/IR audit | 01 scoped IR searches; 03 boundaries; 05 format criteria; 07 R02/R06/R09 |
| I ABI/execution assessment | 03 native/FFI/Python; 05 isolation; 09 placement matrix |
| J Benchmark specification | 06 plus executable research sources and raw results |
| K Target architecture | 09 current/proposed Mermaid diagrams and lifecycle sequence |
| L ADR impact | 01 exact constraints, 03 tension, 07 compliance/per-record decisions |
| M M1 Pro 16 GiB | 02 model guards, 06 platform methodology, 09 residual budget and physical memory |
| N Remote inference | 04 actual Bedrock/provider/stream/request behavior, 09 remote-first constraints |
| O IR/Meta-Harness | 05 paper versus adaptations, 07 R12, 08 questions 41–47 |
| P Prioritized roadmap | 07 dependency DAG/phase map; adoption-records.json complete records |
| Q Rejected alternatives | README, 02/03/05/09, R13 and fifty explicit answers |

## Unresolved architectural questions

| Question | Evidence required before implementation/adoption |
| --- | --- |
| How much does the HTTP capture vector retain in an actual long session? | Trace transport ownership and capture consumers; run deterministic long-session attempts and measure live/peak heap after settled failures/successes. Source growth alone does not quantify physical footprint. |
| Does bounded artifact reading preserve every adapter contract? | Empty/reversed/end/overflow/length-corruption/integrity/source-identity/cancel fixtures at the authority boundary. Native API success is not an already implemented adapter. |
| Which eager constructors materially affect startup? | Per-stage traces and process census across empty config, selected-unused memory, first recall and ordinary remote sessions; distinguish cheap handles from acquired resources. |
| Which LSP idle mechanism is actually wired and safe? | Production call trace, document/subscription/pending-request/Arc obligations, physical process cleanup and cold reactivation tests. |
| Is external Python an approved exception to ADR0036? | Trace configured/default path and owner intent; keep accepted text unchanged until explicitly resolved. |
| What is the safe CPython init failure/retry contract? | Disposable-child invalid-path then valid-init fault tests; do not reset a partially/live initialized runtime to satisfy a test. |
| Which specialized model fits with development workloads? | Real Mac OS/version, concurrent baseline, model artifact/license/quality, CPU/GPU/physical footprint/pressure and unload tests. Do not bypass Saetta's guard. |
| Does a new derived format beat selective current APIs? | Actual graph/query corpus, full construction/validation/framing/transport/materialization/fault/recovery comparison, patched dependency versions and license review. |
| Can a policy optimizer generalize? | Immutable train/held-out workloads, allowlisted actual knobs, deterministic hard constraints, evaluator-leakage checks, infeasible-candidate rejection and controlled promotion. |
| Are claimed provider cache semantics active for the user's model/region? | Explicitly authorized account/model/region/TTL interface canary and provider-reported usage; no inference from an SDK method or gateway name alone. |

## Blockers and non-claims

This environment is Linux AMD64 with an 8 GiB cgroup ceiling. macOS ARM64/Linux
ARM64, Instruments/physical-footprint profiling, large-index/1 GiB datasets, model
weights/quality and real Bedrock calls were not executed. Several profiling tools
were unavailable. Saetta's requested native test could not compile without clang.
The final raw native probe commands succeed, but they do not constitute complete
application startup, memory-retention, extension isolation or performance proof.

No production dependency upgrade/security audit was run. Primary advisory/source
history is recorded with caveats; no claim that inspected HEAD is vulnerable,
that a historical advisory is still active, or that any unmentioned package has
no vulnerabilities. Direct code reuse with incomplete/conflicting licenses is
not cleared. No provider credentials, paid model calls or local model weights
were used in this research.

## Practical next step

Review this research, then authorize a narrow implementation plan for **R02**:
strict artifact-boundary tests plus a bounded-read adapter using the existing
BlobStore API, preserving source/security semantics. R03 and R04 are independent
next candidates after their ownership/contract tests. Neither a stable Rust ABI
nor a new storage/serialization/resource-management framework is needed for those
first improvements. Historical memory can suggest candidates; only current
source-bound executable evidence and hard contracts can certify their promotion.
