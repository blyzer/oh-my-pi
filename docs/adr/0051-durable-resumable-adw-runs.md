# 0051. ADW runs are durable, resumable and extensible over omp-journal

Status: proposed
Date: 2026-11-12
Area: state

## Context

- `omp-adw` is a pure state machine: no I/O, no clock, no spawning (`crates/adw/src/lib.rs:3-11`). Its `Run` is `Debug` only, with no serde (`crates/adw/src/run.rs:173`). The crate doc says "durability lives in `omp-journal`", but nothing writes a journal: `grep journal crates/driver/src/adw/*.rs` finds nothing, and ADR 0001 (line 59) and `docs/research/adw-on-eval-vs-omp-adw.md` (section "Journaling and resume") say the same. A run that dies loses its attempt counts, accepted input versions and outstanding work.
- The driver loop `omp_driver::adw::run` (`crates/driver/src/adw/mod.rs`) dispatches one phase at a time and awaits it. `Step::Wait` ends the run as halted (`mod.rs:270`), although the engine can model work in flight.
- A producing (`Agent`) model phase is accepted on any answer: `judge` returns `accepted: true` for every model phase except `Review` (`crates/driver/src/adw/production.rs`, function `judge`). `Code` phases are decided by command exit status instead (`run_command`, same file). A completion claim is therefore the only evidence for every `Agent` phase.
- `omp-journal` already offers what a run record needs: an `Entry` is `{id (ULID), kind, by, prior, label, data}` with single-line JSON `data` (`crates/journal/src/entry.rs:70-83`); a session is one flat `.oms` file where a blank line commits an entry (`crates/journal/src/lib.rs:3`); an `IdempotencyKey` is committed in the same entry as the message it names, so replay cannot separate them (`crates/journal/src/idempotency.rs`). The set of kind names is closed in revision 1 (`crates/journal/src/kind.rs`).
- `omp-memory` (Mnemopi) keeps durable SQLite rows as authority and rebuildable recall indexes (`crates/memory/src/lib.rs`).
- ADR 0039 Part B already specifies a coordinator with a journal-derived ledger, leases and generation fencing (B2, B3, B10). This record does not duplicate it; it makes one ADW run a durable unit that fleet work can own.
- Owner roadmap P1 (2026-11-12) acceptance test: terminate the coordinator during implementation, restart, recover the workflow, and avoid duplicate mutations or duplicate PR creation.

## Decision

**D1. One engine.** ADW stays the only workflow engine. No second DAG engine is added. The `extensions/workflows` DAG runner, whose resume state is a rebuildable cache by its own README, is an integration candidate for this record's store, not a competitor.

**D2. Identity is immutable input.** A run has a `RunId` (ULID) and a `WorkflowDigest`: SHA-256 over the canonical serialization of the `Workflow` and every `PhaseSpec`. Resume refuses a changed digest. Changing a definition never reinterprets an earlier run.

**D3. The run is event-sourced; `Run` stays pure.** The journal records facts, and `Run` is rebuilt by replaying them through `Run::accept` and `Run::reject`. `Run` gains a replay constructor, not serde of its internals. Replay re-drives `next_step_with_posture` and then applies `accept` or `reject`, because both require the phase to have been marked running (`running_index`, reviewer-cited, not re-read by the author). `Run` also gains additive states and transitions for **failed**, **uncertain** and **needs-human**, and a `redispatch` that repeats an attempt without spending budget (today `reject` increments attempts). Their signatures are not specified yet (see Not yet specified). Facts recorded, in this vocabulary (final kind names are decided with the journal owner, see O1):
- `adw.run.start`: run id, workflow digest, input digest, attempt budget, resolved posture.
- `adw.phase.dispatch`: phase, attempt, selected input versions, executor assignment (D7), workspace reference, and an idempotency key for every declared external effect.
- `adw.phase.settle`: accepted, rejected or failed, the verbatim feedback, and the digest of the evidence bundle (ADR 0053).
- `adw.phase.uncertain`: written by recovery (D5).
- `adw.run.halt` and `adw.run.done`.

**D4. Log first.** A `dispatch` entry is committed before the phase starts. A `settle` entry is committed before the result is applied to the in-memory `Run`. The log, not memory, is the authority. A torn tail is truncated to the last committed entry, as `.oms` already does.

**D5. Recovery distinguishes committed from uncertain.** On resume, a phase with a `dispatch` and no `settle` is **uncertain**, never accepted and never silently retried.
- A phase with a declared external effect (push, PR creation, merge, deploy, comment) carries an idempotency key derived as the SHA-256 of (run id, phase, attempt, effect name), hex-truncated to fit `IdempotencyKey`'s length and character rules (`crates/journal/src/idempotency.rs`). ADR 0039 B3's key is (item, attempt) and names a lease dispatch; this key extends it with the effect name and does not replace it. Recovery calls the effect's reconciler, which asks the owning system whether the effect already happened (for example, "does a PR for branch X exist"), then records `settle` from what it finds. If no reconciler exists or it cannot decide, the phase becomes `NeedsHuman`.
- A phase with no external effect (a deterministic check, or a model turn in a disposable worktree) is re-dispatched as the same attempt number with a `recovered` marker, and the recovery does not spend the retry budget.

**D6. Resume and leases.** `omp adw resume <run-id>` opens the run's journal, checks the digest, replays, classifies uncertain phases and continues. `omp adw runs` lists runs (`omp adw list` already lists workflow definitions, `crates/app/src/adw_cmd.rs:36`). A run is owned by one coordinator through a lease entry with a generation number, as in ADR 0039 B3, so a stale owner's `settle` is rejected. On a single host the lease is an advisory OS file lock on the run journal plus a generation entry; a second `omp adw resume` that cannot take the lock refuses to start (proposed; fleet leases follow B3). A single host is the one-worker case; it needs no fleet.

**D7. Stores are pluggable behind one trait.** A `RunStore` trait (append an entry, scan a run, take a lease) lives in a capability crate, not in `omp-adw`, which stays I/O-free. Backends, chosen by one new setting (name not decided):
1. **Journal file (default):** one `omp-journal` file per run under the data directory. Entries carry single-line JSON `data` inside the journal's raw-SSE framing (`crates/journal/src/sse.rs`); it is not JSON Lines. A JSON Lines store is an optional extension store (backend 4).
2. **In memory:** today's behavior, explicit (`--ephemeral`), for tests and throwaway runs.
3. **Mnemopi (SQLite) through `omp-memory`:** for cross-run queries; the rows are authoritative when selected, and recall indexes remain rebuildable.
4. **Extension store:** an extension declares a store implementation under ADR 0049's rules (a vendor or site-specific store is an extension, the trait is core). Exactly one store is authoritative per run; others may mirror.

**D8. Executor assignment is recorded.** The `dispatch` entry names the executor (native OMP2 or a delegated executor id, ADR 0052) and its capability snapshot digest. A resumed run does not reselect silently: if the recorded executor is unavailable, the phase is `NeedsHuman` or reassigned by an explicit, logged decision.

**D9. Acceptance does not come from the executor.** The executor reports evidence; ADW decides (ADR 0053). This record only stores the verdict and the evidence digest.

## Consequences

- A crashed `omp adw run` becomes resumable and auditable. The cost is a write-ahead append per dispatch and per settle, which is small next to a model turn or a build.
- External effects need idempotency keys and reconcilers. A forge or CI extension that mutates state must ship a reconciler, or its phases fall back to `NeedsHuman` after a crash. That is a deliberate cost.
- The journal's closed kind set must grow (O1), or runs use a separate journal file kind. Either way `omp-journal` owns the change.
- Exactly-once is not claimed. The record promises at-most-once effects where a reconciler exists, and a human decision where it does not.

## Open questions for the owner

- **O1.** Extend `omp-journal`'s kind set with `adw.*` kinds (journal revision), or define a run journal as a separate file type that reuses `Journal`? Both options change `omp-journal`: the kind set is closed and enforced when entries are encoded and decoded (`SseError::UnknownKind`, `crates/journal/src/sse.rs:137` and `:368`, checked by the author). Recommendation: a journal constructor that accepts a registered kind vocabulary (the run journal registers `adw.*`), so session journals keep their closed set. This needs the journal owner.
- **O2.** Default store: journal file (recommended) or Mnemopi?
- **O3.** Does `NeedsHuman` block only the phase or the whole run? Recommendation: only the dependent subgraph; independent phases continue once the driver is concurrent (roadmap P3).

## Specification decisions (2026-11-12, author, under the owner's delegation; reversible by the owner)

These close the items the readiness review listed under "Not yet specified". They are decisions, not evidence; each can be revised before the code it governs lands.

**S1. Decisions on the open questions.**
- **O1 (journal kinds).** `omp-journal` gains a constructor that accepts a registered kind vocabulary (the run journal registers the `adw.*` kinds); session journals keep their closed set. Both options changed the journal, this one leaves session files untouched. This is the first code slice and it belongs to the journal crate.
- **O2 (default store).** The journal file. Mnemopi is an optional later backend and its schema is not specified here.
- **O3 (NeedsHuman scope).** Until the driver is concurrent (roadmap P3), a `NeedsHuman` phase halts the whole run in a resumable state. When phases run concurrently, it blocks only the dependent subgraph.

**S2. Identity.**
- **WorkflowDigest.** SHA-256 over a canonical byte encoding built field by field (not serde, so a serde change cannot move it): a version tag `adw-def/1`, then the workflow name, then every phase in declared order as length-prefixed UTF-8 strings: name, kind, each dependency, each input, `on_reject`, the three requirement fields, and the execution detail (prompt text and executor, or binary, arguments and cwd). Prompt text is included because changing it changes behavior. Defaults are written explicitly, never omitted.
- **InputDigest.** Runs take no inputs in v1, so it is the SHA-256 of the empty string and the field is reserved.
- **Resolved posture.** The three scopes (write, network, approval) as their kebab-case names plus the sandbox mode, exactly as `omp-adw::Posture` reports them.
- **Workspace reference.** The canonical project root path and, when the root is a git work tree, the HEAD commit id; both strings, the second optional.
- **Run id.** A ULID. **File.** `<data dir>/adw/runs/<run-id>.oms`.

**S3. Journal framing.** The first entry is the `journal@1` genesis the journal already requires. Every later entry's `by` is the previous entry's id (a linear chain); `prior` is unset. `dispatch` and `settle` data are single-line JSON objects with sorted keys.

**S4. Effects and reconcilers.**
- A phase declares its external effects in TOML as `effects = ["pr.create", "git.push"]`. Names are dotted lowercase strings; the forge and CI extensions own the vocabulary.
- At run start every declared effect must have a registered reconciler, otherwise the run refuses to start (fail closed).
- Reconciler: `fn reconcile(&self, key: &IdempotencyKey, context: &EffectContext) -> impl Future<Output = Reconciliation> + Send`, with `Reconciliation` one of `Happened(EvidenceRef)`, `NotHappened`, `Unknown`. `Unknown` becomes `NeedsHuman`. Registration is by effect name in a `ReconcilerRegistry` owned by the host.
- The idempotency key is the hex-truncated SHA-256 of (run id, phase, attempt, effect name), as in D5.

**S5. Store trait and names.** A small crate `omp-runlog` (no ADW dependency) holds `RunStore` with `create`, `open` and `list`, and `RunWriter` with `append` and `try_lock`. The setting is `adw_run_store` with values `journal` (default), `memory` and `ext:<id>`. `omp adw runs` lists runs; `omp adw resume <run-id>` resumes one.

**S6. `Run` states and replay.** `omp-adw::Run` gains, additively, the states `Failed`, `Uncertain` and `NeedsHuman` and a `redispatch` that repeats an attempt without spending the budget. Replay re-drives `next_step_with_posture` and then applies the recorded `accept` or `reject`. The replay constructor takes the recorded entries and returns a `Run` or a typed error naming the first entry that disagrees with what the engine would have done.

**S7. Tests.** "Same terminal state" means equal `RunSnapshot` values: per phase its status, attempt number, published version, plus the run's halted flag and the ordered settle records. Boundary kills use named failpoints (`BeforeDispatchAppend`, `AfterDispatchAppend`, `BeforeSettleAppend`, `AfterSettleAppend`, `BeforeApply`) injected through a test-only hook in the driver loop; each is killed and resumed in turn.

**First code slice (unblocked by this section).** The journal vocabulary constructor in `omp-journal` with tests that a session journal still rejects `adw.*` kinds and a run journal accepts only its registered ones. Second: `omp-runlog` with the journal store and the `RunSnapshot`. Third: `Run` replay and the new states in `omp-adw`. The driver integration, `omp adw resume` and the first reconciler follow.

**Still open (needs a measurement or the owner).** Write amplification of per-dispatch and per-settle appends; the Mnemopi schema; how an extension store authenticates; a lease mechanism that works across hosts (B3 owns it).

## Not yet specified (readiness review, 2026-11-12; resolved above except where S-items say open)

A reviewer running the EGM implementation-readiness persona judged that an implementer could not start this record without making these decisions. They are left open on purpose and must be settled before code: the canonical serialization of the workflow digest (format, key order, defaults, whether prompt text is included); the input digest; the resolved-posture and workspace-reference fields; the field on `PhaseSpec` that declares external effects and its TOML schema; the reconciler trait (sync or async, return type, who registers it); the `RunStore` signatures, error type, crate name and setting name; the data directory and file naming; the Mnemopi schema; how `by`, `prior` and the genesis entry are used by a run journal; a failpoint mechanism for acceptance test 1 and a definition of "same terminal state"; and whether the concurrent driver (roadmap P3) is a prerequisite for O3.

## Acceptance tests

1. Kill the process at every dispatch and settle boundary (before append, after append, before apply). Restart; the recovered run reaches the same terminal state as an uninterrupted run.
2. A phase that creates a PR is killed after the PR exists but before `settle`. Recovery reconciles and does not create a second PR.
3. A changed workflow digest refuses to resume.
4. A stale owner's `settle` is rejected by generation.
5. The same run replayed with the in-memory, journal and Mnemopi stores gives identical `Run` state.

## Non-goals

A second workflow engine; distributed scheduling (ADR 0039 Part B owns it); exactly-once delivery to systems without reconcilers.

## Status in omp

**Status: Started.** `omp_adw::replay` and `Run::snapshot` (`crates/adw/src/replay.rs, run.rs@S) rebuild a `Run` from recorded dispatch and settle events and stop at the first event that diverges from what the engine would do; the key test shows a replayed run has the same `RunSnapshot` as the original, including a rejection and a retry. Slice 1 of S1 is done: `omp_journal::Vocabulary` with `Journal::create_with`, `open_with` and `scan_with` (`crates/journal/src/kind.rs, lib.rs, sse.rs, tests/vocabulary.rs: a run journal accepts only its registered kinds; session journals and session readers are unchanged and refuse a run journal). Nothing writes a run journal yet. `omp-adw::Run` and `omp_driver::adw::run` exist and are in memory only. Unverified: whether `crates/driver/src/adw/remote.rs` keeps any state across processes (not read).

## References

[ADR 0039](0039-remote-control-and-factory-modes.md) Part B; [ADR 0040](0040-remote-build-and-test-execution.md); [ADR 0049](0049-tiered-integration-architecture.md); [ADR 0052](0052-one-executor-contract.md); [ADR 0053](0053-independent-verdicts.md); `docs/research/adw-on-eval-vs-omp-adw.md`; `extensions/workflows/README.md`.
