# Scope, authority and evidence ledger

Date: 2026-11-24. OMP2 inspected pin:
[`fda1315052b8b4074f9ea5acc842b2cc0b08b2b2`](https://github.com/blyzer/oh-my-pi/tree/fda1315052b8b4074f9ea5acc842b2cc0b08b2b2),
remote target `origin/omp2`. All OMP2 source citations in this directory refer to
that snapshot unless explicitly labeled historical. Local paths are relative to
the repository root; external source citations use their own full commit pins.

## Actual environment and checkout

The initially selected checkout `/workspace/oh-my-pi` was clean on
`feat/verification-native-checks@310cee5`, not current `omp2`. Fetching `omp2`
resolved the pin above. Research uses `/workspace/omp-native-research`, a fresh
**detached** worktree at that pin during the audit. The initial research phase
created no commit, pushed branch or PR. On 2026-11-25 the owner explicitly
authorized publication; branch `research/native-resource-architecture` preserves
that audit baseline. The publication-time target was
`omp2@4f625fabe4f8ff5ca4f61ec99b8670ab1977da3d`; intervening production changes
were not re-audited. Only this research directory changes; accepted ADRs and
production behavior remain untouched.

Other existing worktrees are historical context, not proof of current merges:

| Local branch/worktree | Observed local commit | Relevance |
| --- | --- | --- |
| `feat/verification-native-checks` | `310cee5` | Historical native verification slice |
| `feat/ci-path-scopes` | `b6f2220` | Prior CI routing research/implementation |
| `design/macos-hosted-fallback` | `376d671` | Historical runner design |
| `feat/macos-hosted-fallback` | `21b8d1c` | Historical implementation |
| `research/macos-hosted-fallback-plan` | `5b42294` | Historical research plan |
| detached merge check | `48c886a` | Earlier package/cache combined-check experiment |
| `fix/macos-validated-packages` | `56b4927` | Earlier release artifacts slice |
| `fix/macos-worker-cache` | `1adadf7` | Earlier cache slice |

Available remote research refs included `origin/feat/verification-input-snapshots`
and `origin/feat/verification-required-inventory`; they are not substituted for
the current source. The inspected target's history includes native verification,
resource/extension ADR amendments, runner v5 work, and the restoration of
`omp-exec-ssh` to the workspace. Current source, not the earlier conversation's
capability inventory, determines implementation status.

Additional remote branch objects were fetched without changing the checkout:
[`references/research-branches.json`](references/research-branches.json) records
seven full branch pins, tip patches/summaries and ancestry against the baseline.
The earlier verification research and native ADW executor/policy are ancestors.
Grammar bundling, async-memory and capability branches require source-level
comparison: non-ancestry alone cannot distinguish open work from squash/cherry-pick.
For example `MemoryRuntime::search_async` from `c1707cff16d20f3840a561ff507f1338e95446bb`
also exists in current `crates/memory/src/runtime.rs:441–450`; it is not a missing
primitive to introduce again. The old grammar branch changed a feature declaration
that is not the current `omp-ast` manifest shape; copy/pasting it is inappropriate.
Historical research findings are retained as context and rechecked against the
current source, not promoted wholesale to current capabilities.

The Linux execution workspace initially had no free disk space: approximately
3.8 GiB of ignored, regenerable `target/` output and 2.8 GiB of task-local
toolchain state. `cargo clean --target-dir /workspace/oh-my-pi/target` removed
15,555 generated files (Cargo reported 3.6 GiB), permitting a fresh worktree.
No source or research files were removed. Cold-build effects of that cleanup are
not a controlled filesystem-cache experiment. See the benchmark chapter/raw
results for actual environment, compiler, commands and outcomes.

This session uses the tools actually exposed by this harness: shell/Git/GitHub,
deterministic source search, native toolchain where available, and parallel
research agents. It does not claim an OMPv1 binary or unavailable LSP plugin ran
the audit merely because OMPv1 is the intended downstream execution harness.

## Authority and status vocabulary

1. The owner's present independent-OMP2 constraints and accepted ADR decisions
   define correctness. Upstream compatibility is not a requirement.
2. Accepted ADRs retain authority and historical rationale regardless of where
   an idea originated. A source implementation gap does not amend a decision.
3. Production composition and concrete consumers establish implementation;
   schemas, accepted ADRs, tests or unused methods alone do not prove activation.
4. Research recommendations are proposals, never claims of shipped behavior.

| Label | Meaning |
| --- | --- |
| **SRC** | Implemented and inspected in source; not necessarily executed here |
| **PARTIAL** | Concrete implementation with a traced missing integration/limitation |
| **STUB** | Interface/declaration only in the inspected scope |
| **PLAN** | Documented or proposed, no production implementation established |
| **ABSENT(scope)** | Bounded search found no implementation; not universal nonexistence |
| **UNK** | Evidence insufficient or unavailable |
| **RUN** | This investigation executed the specified command; raw output retained |
| **INFERENCE/HYPOTHESIS** | Architectural interpretation or expected benefit requiring measurement |

An `Arc` clone, a deep collection clone and ownership transfer are not equivalent.
Neither a dependency in Cargo.toml nor statically linked code proves runtime
initialization or physical residency. Tests mentioned as source evidence are not
reported as executed unless the benchmark/validation record says so.

## Existing knowledge reused

The deterministic [workspace inventory/graph](references/workspace-graph.json)
records all 62 package manifests, crate target kinds, declared features and 344
internal dependency edges from offline locked Cargo metadata. Optional, target-
conditional, build and test edges retain their attributes. This is not a claim
that every edge is compiled for one target, enabled at runtime or resident in RAM.

The audit consults `AGENTS.md`, `docs/adr/{README,STATUS,TASKS}.md`, exact relevant
accepted decisions and amendments, and earlier notes including:

- `docs/research/omp2-hybrid-verification/`;
- `docs/research/omp2-native-capabilities-and-composition/`;
- `docs/research/{memory-backends,exec-out-of-core,grammar-bundling}.md`;
- native crate READMEs/contracts, actual tests/benches and current workflows.

Stale Pi-compatibility wording in a historical document or rule is not permission
to restore compatibility requirements. Conversely, that change of goal is not
permission to delete the document's ownership, cancellation, sandbox, replay,
registration or behavioral invariants.

ADR 0036 actually specifies statically embedded free-threaded CPython, no
host-interpreter dependency, one extension language, content-addressed remote
bodies/out-of-band buffers, and killable extension/Eval children
(`docs/adr/0036-python-for-extensions.md:35–65`). The owner's prohibition on
unsupported independently compiled Rust plugin ABIs is an additional explicit
constraint, not a quotation falsely attributed to ADR 0036's text.

## References and licenses

[`references/repositories.json`](references/repositories.json) records read-only
GitHub repository discovery, canonical redirects, exact default-branch SHAs,
archival state, dates and license metadata. The collector is included for repeatability.
**Discovery metadata is not source verification or a license opinion.** Source
chapters record the precise revisions actually read, which can differ from a
moving default branch's later metadata snapshot. Failed lookups are retained;
corrected canonical candidates are separate entries.

Important corrections include Ethereum `fast-abi` versus native Rust ABI,
`Xuanwo/xabi`, `twittner/minicbor`, `neoeinstein/stats_alloc`,
`tobz/tracking-allocator`, Iai-Callgrind's move to Gungraun, and the paper's
official Meta-Harness artifact versus independent `s0-cli`. License conflicts or
missing license text block direct code reuse. No external source implementation
is vendored by this research. Paper quotations/ideas retain attribution; no model
weights or large third-party generated artifacts are added.

## Limits

No paid inference, real AWS account/model/region check, Apple M1 measurement,
large-model loading, security certification, or production architectural change
is performed. Linux experiments do not establish macOS physical footprint or
16 GiB coexistence. Missing tools, unavailable upstream data and unexecuted large
benchmarks remain explicit blockers, not PASS. Dataset scales not actually run
are future procedures; no speedup or leak magnitude is invented.
