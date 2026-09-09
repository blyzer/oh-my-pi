# AI Developer Workflows (ADW)

An **AI developer workflow** is an ordered list of phases declared in `.omp/adw/<name>.yml` and executed by `/adw <workflow> <request>`. The point is a division of labour: **deterministic code decides what runs next and whether it counts; models only do the work inside one bounded phase.** An agent proposes, code disposes.

That is the difference between a workflow and a long prompt. A long prompt asks one model to plan, build, verify and remember to verify. A workflow makes the *engine* responsible for sequencing, retries and acceptance, so the model cannot skip the check that would have caught it.

## Key implementation files

| Path                                                     | Role                                                                            |
| -------------------------------------------------------- | ------------------------------------------------------------------------------- |
| `crates/pi-tasks/src/orchestrator.rs`                    | The driven state machine: cursor, attempts, acceptance, resume                  |
| `crates/pi-tasks/src/envelope.rs`                        | Typed agent handoff envelopes and their parse rules                             |
| `crates/pi-tasks/src/gate.rs`                            | Acceptance gates (`artifacts_exist`, `files_non_empty`)                         |
| `crates/pi-tasks/src/trace.rs`                           | Append-only binary event log plus interned string table                         |
| `crates/pi-natives/src/tasks.rs`                         | N-API surface — `TaskRun`, `taskGateNames()`, `taskTraceLayout()`               |
| `packages/coding-agent/src/adw/runner.ts`                | The driver: spawns seats, runs code phases, owns isolation                      |
| `packages/coding-agent/src/adw/integration.ts`            | The integration owner: applies accepted diffs serially through a durable journal |
| `packages/coding-agent/src/adw/schema.ts`                 | Caller-owned gates: payload & verdict schema compilation and `REVIEW_RULES`      |
| `packages/coding-agent/src/adw/config.ts`                | Workflow discovery, parsing and validation                                      |
| `packages/coding-agent/src/adw/prompt.ts`                | Phase, panel and fusion prompt assembly; `ENVELOPE_CONTRACT`                    |
| `packages/coding-agent/src/adw/types.ts`                 | The on-disk schema                                                              |
| `packages/coding-agent/src/slash-commands/builtin-adw.ts` | `/adw`, `/adw list`, `/adw cancel`, `/adw resume`                               |
| `packages/adw-web`                                       | Read-only trace viewer                                                          |

## The split: Rust decides, TypeScript executes

![ADW architecture](../assets/adw/01-architecture.webp)

The engine is **driven, not autonomous**. It answers two questions — *what runs next* and *did that count* — and the caller executes the step and reports back:

```text
Rust                                    TypeScript
next_step()  ──── Step ─────────────▶   spawn a seat / run a command
                                        (only the TS layer can reach a model)
gates(envelope) ◀── Envelope ───────    submit the result
```

The split is not stylistic. Only the TypeScript layer owns model providers and sessions, so Rust cannot call a model — which means the engine needs no async runtime, no callback into JS, and no `tokio`. It is a synchronous state machine, and that is what makes it testable without a fake model client.

Values cross as `#[napi(object)]` structs through V8 accessors, not as JSON strings.

## Writing a workflow

Workflows are discovered from `.omp/adw/*.yml`, project directories first (nearest wins), then user-level — the same precedence as agent discovery in `.omp/agents/*.md`. A repo carries its own factory.

Three runnable examples ship in [`docs/adw/examples/`](adw/examples), each teaching one thing. Copy one and go:

```bash
mkdir -p .omp/adw && cp docs/adw/examples/fix.yml .omp/adw/
/adw fix "the retry helper drops the last attempt"
```

| Example | Teaches |
| --- | --- |
| [`fix.yml`](adw/examples/fix.yml) | `onFail: correct` — a red suite returns to the agent that wrote the code |
| [`ship.yml`](adw/examples/ship.yml) | `isolation` and `diff_matches_claims` — nothing lands unless the run is accepted, and nothing changes unconfessed |
| [`review.yml`](adw/examples/review.yml) | a `fusion` phase — two read-only readings merged by one writer |

A test parses every file in that directory, so a schema change that invalidates an example fails in CI rather than in your first run.

```yaml
name: ship
description: Plan, implement, verify — with a second opinion on the design
maxAttempts: 3
isolation: true

phases:
  - name: design
    kind: fusion
    description: Two models argue the approach, one writes it down
    gates: [artifacts_exist, files_non_empty]
    panel:
      - owner: reviewer
        model: anthropic/claude-opus-4-5
      - owner: scout
        model: openai/gpt-5.5
    fuser:
      owner: task
      thinking: high

  - name: build
    kind: agent
    owner: task
    prompt: Implement the plan from the previous phase. Do not redesign it.
    gates: [artifacts_exist]

  - name: verify
    kind: code
    owner: bun
    command: bun test
    timeoutMs: 600000
```

### Workflow keys

| Key           | Required | Meaning                                                                 |
| ------------- | -------- | ----------------------------------------------------------------------- |
| `name`        | yes      | The name `/adw <name>` dispatches on                                    |
| `phases`      | yes      | Ordered list; phase names must be unique                                |
| `description` | no       | Shown by `/adw list`                                                    |
| `maxAttempts` | no       | Attempts per phase before the run halts. Default `3`, minimum `1`       |
| `acceptance`  | no       | `review`: require all phases to pass and the last envelope to approve delivery; omitted: all phases passing is enough |
| `isolation`   | no       | Run the whole workflow against a materialised copy of the repo          |
| `undeclaredIgnore` | no  | Globs `diff_matches_claims` treats as always accounted for              |
| `protected`   | no       | Globs no phase may change, even when its `writes` lists them; `.omp/adw/**` is always protected |
| `concurrency` | no       | Independent ready phases in flight at once, `1`–`8`. Default `1` (serial). `> 1` requires `isolation: true` and an explicit `writes` on every writer |

### Phase keys

| Key           | Applies to      | Meaning                                                                    |
| ------------- | --------------- | -------------------------------------------------------------------------- |
| `name`        | all             | Unique within the workflow; also the trace and envelope key                |
| `kind`        | all             | `agent` \| `code` \| `fusion`                                              |
| `owner`       | `agent` (req.)  | An agent from the roster — never a raw model                               |
| `owner`       | `code`          | A subsystem label for the report (`git`, `bun`, `sh`)                      |
| `gates`       | all             | Checks run *after* the phase against what it claimed                       |
| `description` | all             | Surfaced in progress output and the trace                                  |
| `model`       | `agent`         | Model pattern — `provider/id[:level]` or a `@role` alias                    |
| `thinking`    | `agent`         | `off\|minimal\|low\|medium\|high\|xhigh\|max\|auto`                        |
| `prompt`      | `agent`         | Extra instructions appended after the operator's request                   |
| `command`     | `code` (req.)   | Shell command; exit code `0` is the pass                                   |
| `timeoutMs`   | `code`          | Per-command deadline. Defaults to 10 minutes; must be `> 0`                |
| `dependsOn`   | all             | Phases that must pass first; execution order follows the graph              |
| `onFail`      | `code`          | `retry` (default) re-runs the command; `correct` sends the failure back    |
| `panel`       | `fusion` (req.) | Two or more read-only seats answering the same question                    |
| `fuser`       | `fusion` (req.) | The single seat allowed to write                                           |
| `schema`      | `agent`, `fusion` | JSON Schema subset applied to the writer's entire envelope and included in its prompt |
| `onReject`    | `agent`, `fusion` with `verdict_consistent` | `{to, maxRevisions}`: a coherent rejection revisits the named earlier writer instead of refusing outright |
| `inputs`      | all             | Names of earlier phases whose accepted outputs this phase consumes |
| `writes`      | `agent`, `fusion` | Repo-relative globs the writer may change; anything else is reverted and rejected |

A misspelled key fails the file. Every schema uses `onUndeclaredKey("reject")`, because `isolaton: true` parsing cleanly would run the whole workflow against the real checkout while the operator believes it is sandboxed. Silence is the wrong answer for a safety flag.

Validation also rejects, with the offending phase named: a duplicate phase name, an `agent` phase with no `owner`, a `code` phase with no `command`, `model`/`thinking`/`prompt` on a `code` phase, a `fusion` phase with fewer than two panel seats or no fuser, `panel`/`fuser` on a non-fusion phase, `timeoutMs <= 0`, an unknown gate name, an unusable `onReject` route — a target that is unknown, is the phase itself, is a `code` phase, is not a transitive dependency of the source, or a `maxRevisions` that is not an integer from 1 through 65535 — an unusable `inputs` entry: unknown, duplicated, the phase itself, or outside the consumer's transitive dependencies — and an unusable `writes`/`protected` list: a blank or duplicated glob, or `writes` on a `code` phase.

## Commands

```text
/adw <workflow> <request>          run a workflow
/adw list                          list discovered workflows and any broken files
/adw cancel                        stop the running workflow
/adw resume <workflow> <adw-id>    continue a run that died mid-flight
```

`Esc` also cancels a run in flight. A workflow spends minutes to hours across several models, and `Esc` is the key an operator actually reaches for.

`/adw list` reports files that **failed to load** alongside the workflows that parsed. A broken file the operator is about to ask for by name has to explain itself rather than read as absent.

## Dependency order

Phases run in declaration order until `dependsOn` says otherwise:

```yaml
phases:
  - { name: docs,   kind: agent, owner: sonic, dependsOn: [api] }
  - { name: verify, kind: code,  owner: sh, command: "make check", dependsOn: [docs] }
  - { name: api,    kind: agent, owner: task }
```

That file runs `api → docs → verify`. The author declares what needs what instead of hand-sorting the list, and a phase moved later cannot silently start running before its input exists. A phase that declares nothing depends on its declaration predecessor; `dependsOn: []` declares independence — the phase needs nothing, and under `concurrency > 1` it is what makes two phases eligible to run at the same time.

**The graph is declared, never inferred.** A graph a model proposes changes between runs given the same prompt, and then `resume` cannot rebuild a position in a plan that no longer exists. This one is a pure function of the file: ties break on declaration order, never on hash iteration, so two runs of the same workflow execute in the same sequence and a resumed run derives the order it originally ran.

### Concurrent execution

`concurrency: N` lets up to `N` independent ready writers run **at the same time**. Each gets its own materialised workspace cloned from the run root; a writer never sees a sibling's unaccepted work, and a schema- or gate-rejected attempt never contaminates anyone. When a writer's result is accepted, one integration owner serially applies its workspace diff onto the run root; two accepted diffs that overlap are a real conflict — the run refuses delivery, preserves both diffs as patches under the run directory, and never auto-retries the loser. `code` phases are barriers: they run alone against the integrated tree. A failed prerequisite blocks its descendants while unrelated in-flight work still completes and keeps its evidence; a revision cancels the invalidated flights, drains them, and preserves what they had written before dispatching replacements. Resume reattaches surviving workspaces, replays the active set, and finishes an interrupted integration exactly once.

There is no cursor: the engine holds a state per phase (pending, running, passed, rejected, invalidated) and dispatches whichever pending phases have every dependency passed, up to the concurrency ceiling. `resume` rebuilds those states from the trace by name, so an interrupted run — serial or concurrent — re-dispatches exactly the attempts that were in flight and nothing that already passed.

A cycle, a self-dependency, or a name that does not exist **fails the file at load time**, naming every phase in the loop:

```text
dag.yml: dependency cycle among phases: a, b, c
```

The engine refuses to guess here: an unorderable graph is left in declaration order rather than reordered into something the author never wrote, so the check has to happen where the file name is known.

`onFail: correct` follows the graph too. A phase with dependencies sends its failure to the nearest **dependency** that has an agent to correct, not to whatever happened to be declared above it — position is the wrong answer once a graph exists.

### Dividing the work instead of asking twice

A `fusion` panel exists for a second opinion: every seat answers the same question, which is what makes the answers comparable. Give a seat its own `prompt` and you are buying something else — **concurrency**:

```yaml
- name: survey
  kind: fusion
  prompt: One sentence. Name the file you read.
  panel:
    - { owner: scout, prompt: Read ONLY parser.js. }
    - { owner: scout, prompt: Read ONLY format.js. }
    - { owner: scout, prompt: Read ONLY config.js. }
  fuser:
    owner: task
    prompt: Merge the parts into SURVEY.md, one line per file. Declare it.
  gates: [artifacts_exist, files_non_empty]
```

Three read-only seats investigate three things at once, and one writer merges them. A seat with its own part is told to answer only that part, instead of being told it is "one opinion of 3" — which would invite it to answer the whole question.

**This is safe for exactly one reason: no panel seat may write.** Parallel *writers* are a different problem and this does not solve it. Two agents writing the same tree lose writes, and `diff_matches_claims` cannot see the race — it reads the result, not the order. That is why writer phases stay serial here, and why `/fh-collaborate` in fusion-harness bounds its own DAG the same way: "parallel where possible, exactly one shared-CWD writer at a time".

The fuser is a seat too, so its `prompt` reaches it — the place to say which file the merge lands in.

## Phase kinds

### `agent`

![Phase lifecycle](../assets/adw/02-phase-lifecycle.webp)

One seat, one turn. `owner` names an agent from the roster, so the roster stays the source of truth for who the agent is; a phase may pin a `model`, but it does not invent an agent.

The prompt the seat receives is the operator's request, plus the previous phase's handoff, plus the envelope contract.

### `code`

A shell command run with `sh -c`. The exit code is the verdict — no model, no tokens, no envelope. Deterministic work belongs here: `git commit`, `cargo check`, `bun test`.

Signals are reported as signals. A command killed by `SIGTERM` says so instead of surfacing as `exited 143`, and a run cancelled by `Esc` is never labelled a timeout.

### `fusion`

![Fusion phase](../assets/adw/03-fusion-panel.webp)

Two or more models answer the same question read-only and in parallel, then a single **fuser** merges every labelled opinion and owns the phase's envelope. Panel seats are pinned to `read`, `grep`, `glob` and `yield` — no `write`, `edit` or `bash`, and no nested spawns — which is what makes running them concurrently safe.

Opinions reach the fuser **labelled and in the order the panel was declared**, never anonymised and never merged early. Panel concurrency respects `task.maxConcurrency` (default 4), so a six-seat panel under a ceiling of two runs in waves.

A retried fusion phase re-runs **the fuser only**. The opinions are cached: the panel was not what got rejected, and re-polling N models is the most expensive way to change nothing.

## Envelopes

An agent's only structured output channel is the last complete top-level JSON object in its final message. Everything before it — prose, fences, reasoning — is ignored, so an agent that narrates before answering still produces a parseable envelope. A *missing* object is a contract violation.

```json
{
  "status": "success",
  "summary": "one line on what you did",
  "artifacts": ["repo-relative/path/you/actually/wrote"],
  "notes_for_next_agent": "what the next phase needs to know",
  "...": "any fields your phase is asked to report"
}
```

`"status": "fail"` is a real answer, not an error: it reaches the next attempt with the summary attached. Claiming success you cannot back is the failure mode the gates exist to catch.

An accepted envelope is persisted to `<traceDir>/envelopes/<phase>.<version>.json` — the version is that phase's acceptance ordinal, so a phase re-run by a correction loop gets a new version instead of overwriting the evidence a consumer already read. It is the one thing the event trace cannot reconstruct, which is why it is written to disk rather than kept in memory. The *latest* accepted envelope is the next phase's default handoff.

### Declared inputs

The default handoff is positional: each phase receives whatever the previous phase happened to produce, and a `code` phase that passes replaces it — a `plan → check → build` pipeline hands `build` the check's exit summary instead of the plan. `inputs` makes consumption explicit:

```yaml
phases:
  - name: plan
    kind: agent
    owner: task
  - name: check
    kind: code
    command: bun run lint-plan
  - name: build
    kind: agent
    owner: task
    inputs: [plan]
```

A phase with `inputs` receives exactly the **current accepted output of each named producer** — one labelled section per producer for a writer, and for a `code` phase a JSON file whose path arrives as `$ADW_INPUTS`, keyed by producer name with `version`, `summary`, `artifacts`, `notes_for_next_agent` and the reported `payload`. The incidental last envelope is not delivered to it. A join names both predecessors (`inputs: [latency, throughput]`) and gets both, regardless of which ran last. Panel seats and the fuser of a fusion phase all see the phase's inputs.

Selection happens at dispatch and is recorded: one `input_selected` trace event per input names the producer and the exact version consumed, so the viewer and a resumed run can explain which results authorized an execution. After a revision re-runs a producer, the consumer's next dispatch selects the **new** version; the superseded one stays on disk for diagnostics but is never presented as current. A producer whose current output is missing or unreadable — a deleted or corrupted store file on resume — fails the run naming consumer, producer and version, rather than silently falling back to whatever envelope was last accepted.

`inputs` declares data flow, not ordering: each input must be a transitive dependency of its consumer, so the consumed output provably exists — and under concurrency was integrated — before the consumer dispatches. Anything else fails the file at load time.

## Gates

Gates run **after** a phase. Native Rust gates check filesystem claims; caller-run gates validate the writer's envelope and report through the same engine decision path.

| Gate                  | Passes when                                                                          |
| --------------------- | ------------------------------------------------------------------------------------ |
| `artifacts_exist`     | every path in `artifacts` exists on disk                                             |
| `files_non_empty`     | every path in `artifacts` exists **and** has non-zero size                            |
| `diff_matches_claims` | every path the working tree changed is one some phase declared                        |
| `json_parses`         | every declared `.json` artifact parses                                                |
| `verdict_consistent`  | the review has the required fields and its approval agrees with its blockers and findings |

`passed` is evidence rather than silence: **an envelope that declares no artifacts fails both artifact gates** rather than clearing them vacuously. Requesting either gate asserts that the phase produces files. Native gate names are validated against `taskGateNames()`; the caller additionally registers `verdict_consistent`. Declaring `schema` automatically enables `payload_matches_schema`—do not list that internal report name in `gates`.

A `code` phase declares no artifacts, so `artifacts_exist` and `files_non_empty` on one could only ever fail. That combination is **rejected at load time**, naming what to use instead:

```text
bad.yml: phase "check" is a code phase and cannot satisfy gate "artifacts_exist":
a code phase declares no artifacts. Use diff_matches_claims, or move the gate to
the agent phase that writes the files.
```

### `json_parses`

**Bytes are not structure.** A phase that hands the next one `plan.json` has produced nothing useful if the file is truncated or holds an apology instead of an object, and `files_non_empty` is happy either way — it counted the bytes in the apology.

Scoped by extension: which artifacts are JSON is already visible in the envelope, so the gate is not told twice. Non-JSON artifacts are left alone.

The violation carries the parse position, because *"it is invalid"* is not actionable and *"expected value at line 1 column 12"* is. Measured end to end — a phase declaring a `plan.json` containing `{"step": 1,}`:

```text
gate_check   artifacts_exist  ok=true   plan.json
gate_check   json_parses      ok=false  plan.json
phase_rejected
gate_check   artifacts_exist  ok=true   plan.json
gate_check   json_parses      ok=true   plan.json
```

That first pair is the whole point: the file exists, is non-empty, and is still wrong.

### `schema` — validating declared constraints

Agent and fusion-fuser envelopes carry fields beyond `status`/`summary`/`artifacts`/`notes_for_next_agent`. A phase can declare constraints on those fields. The actual schema is included automatically in the writer's prompt; panel opinions remain prose and are not validated as envelopes. The validator receives the whole envelope, so `additionalProperties: false` also requires declaring the core envelope fields:

```yaml
- name: review
  kind: agent
  owner: reviewer
  schema:
    type: object
    required: [approved, blocking]
    properties:
      approved: { type: boolean }
      blocking: { type: array, items: { type: string } }
```

Violations name the field, because *"expected boolean"* alone makes the agent guess which one — and guessing costs an attempt. Every problem is reported at once for the same reason.

**A JSON Schema subset, not full conformance.** ADW uses omptype’s `fromJsonSchema` importer. It rejects `if`/`then`/`else`, `oneOf`, and `not` at load time: conditionals are ignored by the importer, `oneOf` becomes an ordinary union, and `not` only supports the special case `not: {}`. ADW also rejects `dependentSchemas`, `dependentRequired`, `patternProperties`, `propertyNames`, `unevaluatedProperties`, `unevaluatedItems`, and `contains`. These checks apply to schema keywords, not to data fields named `if` or `oneOf`:

```text
w.yml: phase "review" has an unusable schema: omptype does not enforce if, then
```

`anyOf` and `allOf` are allowed: they produce unions and intersections. They can express cross-field rules using supported constraints. For example, “approved implies no blockers” can be written as two complete alternatives:

```yaml
schema:
  anyOf:
    - type: object
      required: [approved, blocking]
      properties:
        approved: { const: false }
        blocking: { type: array, items: { type: string } }
    - type: object
      required: [approved, blocking]
      properties:
        approved: { const: true }
        blocking: { type: array, items: { type: string }, maxItems: 0 }
```

This rejects `{approved: true, blocking: ["x"]}` and accepts both `{approved: true, blocking: []}` and `{approved: false, blocking: ["x"]}`.

**Importer limits still apply.** Keep each branch self-contained: the importer returns the union/intersection without applying sibling constraints such as an outer `required`. Numeric and string bounds need an explicit `type` on the same node: `{minimum: 5}` alone is ignored, whereas `{type: number, minimum: 5}` is enforced. Thus `allOf: [{type: number, minimum: 5}, {type: number, maximum: 10}]` checks both bounds; the earlier claim that `allOf` only checked its first branch was incorrect. The rejected-keyword list is not an exhaustive conformance check.

**Native omptype predicates are a separate API.** JavaScript/TypeScript code can enforce the same implication directly:

```ts
const Review = type({ approved: "boolean", blocking: "string[]" })
  .narrow(value => !value.approved || value.blocking.length === 0);
```

The predicate runs when `Review(value)` validates a value. It is not JSON data: exporting this schema with `toJsonSchema()` omits that predicate, and importing the result does not restore it. ADW's YAML `schema` does not accept JavaScript callbacks. The named `verdict_consistent` gate below supplies native review predicates; use a `code` phase for other rules requiring custom JavaScript.

**Where this check runs.** The JavaScript caller compiles contracts before execution and reports validation results through `noteGateReport` on both agent and fuser submissions. The Rust engine records a `gate_check` named `payload_matches_schema` and uses it to decide phase acceptance alongside other gates. A failed fuser contract retries only the fuser, retaining the panel opinions. This integration does not add a JSON Schema validator to `pi-tasks`.

The envelope is located with the engine's own extraction rule, exported as `taskEnvelopeText`, rather than a second implementation of "the last top-level JSON object" that would drift from it.

### Review consistency and delivery acceptance

`gates: [verdict_consistent]` requires a review envelope with:

```json
{
  "status": "success",
  "summary": "R1 is not satisfied",
  "approved": false,
  "blocking": ["R1: required response header is absent"],
  "findings": [
    {
      "requirement": "R1: response includes X-Request-Id",
      "met": false,
      "evidence": "The inspected response has no X-Request-Id header"
    }
  ]
}
```

`approved`, `blocking`, and `findings` are required. Each finding requires a string `requirement` and boolean `met`; string `evidence` is optional. Native omptype predicates reject:

- approval with any blocking entry;
- approval with any unmet finding;
- rejection without a blocking entry or unmet finding.

All violations return to the writer together. These rules check internal consistency, **not the truth or completeness of the review**. The writer receives both the structural schema and the predicate rules automatically. A custom `schema` can impose additional requirements; both contracts must pass. This gate is not allowed on `code` phases.

Enable the separate workflow-level decision explicitly:

```yaml
name: accept-review
acceptance: review
phases:
  - name: review
    kind: agent
    owner: reviewer
    prompt: Review the requested change against its requirements and evidence.
    gates: [verdict_consistent]
```

| Outcome | Review phase | Workflow with `acceptance: review` |
| --- | --- | --- |
| Malformed or contradictory report | Rejected; writer receives corrections within `maxAttempts` | Cannot accept |
| Coherent `approved: false` | Passed | With `onReject` and budget left: revises the named builder and re-runs the suffix. Otherwise refuses delivery without retrying the reviewer |
| Coherent `approved: true` | Passed | Accepts only if every other phase passed |

A completed review uses `status: success` even when `approved: false`. `status: fail` means the review could not be completed and follows the existing retry behavior.

The decision validates the **last accepted envelope**, including after resume. Put the final review after all writers and checks: a trailing code phase or non-review envelope does not reuse an earlier approval. The summary, terminal trace, and isolation landing use the same decision; rejection preserves the isolated patch without applying it. Without isolation, this decision does not undo edits already made in the working checkout.

Omitting `acceptance` retains the existing all-phases-success policy; a consistency gate alone does not require approval. Named phase inputs, protected write scopes and the later factory milestones are specified in the [SSSF equivalence plan](adw/sssf-equivalence-plan.md).

### `diff_matches_claims`

The first two catch a claim with no file. This catches the opposite — **a file with no claim**. An agent that edited three files and confessed one leaves two changes nobody reviewed, and an existence check cannot see them, because nothing was claimed.

The allowed set is cumulative, so no phase is blamed for another's work:

```text
changed   = git status (untracked included)
          ∪ diff from the commit the run started on
allowed   = dirty before the run started
          ∪ paths declared by earlier phases
          ∪ paths declared by this envelope
violation = changed − allowed
```

Whatever was already dirty belongs to the operator, not the agent. Untracked files are listed individually rather than collapsed into their directory, because a brand-new undeclared file is the common case. A rename reports both paths: a file moved out from under a claim is exactly what this gate is for.

**Both sources are needed, and neither alone is enough.** Untracked files never appear in a diff. And tracked changes disappear from `git status` the moment a phase commits them — a phase running `git add -A && git commit` used to pass a gate that rejected the identical command without the commit. The run's starting commit is the fixed point that closes that hole.

Some paths a build legitimately rewrites without any phase claiming them — a lockfile after an install, generated sources. Declare those, and only those:

```yaml
undeclaredIgnore: ["**/*.lock", "**/*.generated.ts"]
```

The default is **empty**, deliberately. A wide default makes the gate noisy, and an operator who cannot tell which changes it forgives stops trusting it — a gate nobody trusts gets deleted. A malformed pattern fails the run at construction, naming itself:

```text
undeclaredIgnore "src/**/[": error parsing glob 'src/**/[': unclosed character class; missing ']'
```

That check runs before a token is spent, because discovering it when the gate first fires means a phase already paid for the mistake.

It lives in `crates/pi-natives`, not `pi-tasks`, because it needs git — the engine crate stays free of I/O beyond the filesystem. And a gate that cannot gather evidence **fails**: run it outside a repository and it reports `not a repository`, never a quiet pass.

## Corrections and retries

A rejected attempt does **not** respawn the agent. It continues the seat's existing session through `runSubagentFollowUpTurn`, so the correction arrives as the next user turn in the same context window:

- The seat still remembers the attempt that was rejected, so it can fix *that* instead of re-deriving it.
- A retry costs one message instead of a cold restart.
- Seat ids are stable across attempts, which is what makes the continuation possible.

Violations name the file: *"docs/x.md was claimed but does not exist"* is actionable; *"the file you claimed is missing"* is barely so.

When a phase exhausts `maxAttempts`, the run halts with `accepted = false`. It is a decided outcome with a trace, not a crash.

### `onFail: correct` — sending a failure back

A `code` phase has no agent to correct, and a retry re-runs the identical command. For a deterministic step that is guaranteed to fail again: a red test suite burns the whole budget while the agent that wrote the code never learns it broke.

```yaml
- name: build
  kind: agent
  owner: task
- name: verify
  kind: code
  owner: bun
  command: bun test
  onFail: correct        # default is `retry`
```

The failure returns to the nearest preceding `agent` or `fusion` phase as a correction in that agent's own session:

```text
▶ build (task)  attempt 1   → advanced
▶ verify (bun)  attempt 1   → tests failed
  retry verify — "Phase `verify` failed after `build` was accepted,
                  so the run came back to you (attempt 2 of 3)"
▶ build (task)  attempt 2   → advanced
▶ verify (bun)  attempt 1   → advanced
```

**The budget belongs to the target.** `verify` never spends attempts — only the phase that can change the outcome pays, and that is what makes the loop finite. A target that has exhausted `maxAttempts` halts the run like any other exhausted phase.

The engine holds no policy about which phase can fix a failure: the caller resolves the target and Rust obeys a name. `onFail: correct` on a phase with no correctable predecessor **fails the file at load time** — discovering a malformed workflow after something already failed is the worst moment to learn it.

Phase records are a history, not a per-phase slot: a phase that ran twice appears twice, with the failure that sent the run back in between. A rewind writes a `phase_rewound` record naming both the target and the phase that failed, and `resume` reads position from those records rather than counting passed phases — the count stops meaning anything the moment one phase runs twice.

### `onReject` — a rejected review revises the builder

`onFail: correct` answers a *broken step*; `onReject` answers a *coherent verdict against the product*. They are different failures with different budgets, and conflating them is how a reviewer ends up editing implementation. The reviewer never edits; it names an earlier writer:

```yaml
- name: build
  kind: agent
  owner: task
- name: verify
  kind: code
  owner: bun
  command: bun test
- name: review
  kind: agent
  owner: reviewer
  gates: [verdict_consistent]
  onReject: { to: build, maxRevisions: 2 }
```

Three outcomes, three routes — the engine distinguishes them, not the prompt:

- **Malformed or inconsistent review** (bad envelope, failed gate, contradictory verdict): an ordinary correction to the *reviewer*, inside `maxAttempts`. `onReject` never substitutes for format correction.
- **Coherent `approved: false`** with revision budget left: the review phase *passes* — the review did its job — and the run rewinds to `build` carrying the full rejected report (blockers, unmet requirements, evidence) as the builder's correction, in the builder's own session. Every phase from the builder through the review re-runs; `verify` runs again against the revised code, and the review re-reads current files.
- **Coherent `approved: true`**: the run continues.

**Budgets are separate and cumulative.** Each consumed revision grants the target exactly **one** additional attempt — `maxAttempts: 1` with `maxRevisions: 2` permits the initial build plus two review-requested rebuilds, and no extra retry for a malformed builder envelope. Grants never replenish attempts the builder already spent, and the per-review revision count survives other routes revisiting the same phases. When the budget is exhausted, the run halts with the review's own rejection reason — a `review_exhausted` record, not a format failure.

**Revision invalidates downstream evidence.** The passed records from the target through the review are marked `invalidated` and stay in the history; the CLI summary and viewer show them struck through. No green result from before the edit can authorize delivery: a revision that breaks a previously passing check halts on that check's own failure, and the earlier pass is visibly superseded. A revisited fusion review re-polls its panel — the opinions were formed against code that no longer exists (a fuser *format* retry still reuses them).

A phase with `onReject` whose submission carries no caller-validated review decision fails closed as a reviewer correction — the engine never routes, or advances, on a decision nobody validated.

## Write scopes

Gates verify what a phase *claimed*; scopes bound what it may *touch*. They answer different questions — `diff_matches_claims` is accountability (a file with no claim), `writes`/`protected` are authorization (a change with no right) — and conflating them is how a reviewer ends up editing the evaluator it reports on.

```yaml
protected: [scripts/verify.sh]
phases:
  - name: plan
    kind: agent
    owner: scout
    writes: [PLAN.md]
  - name: build
    kind: agent
    owner: task
    writes: ["src/**", "test/**"]
  - name: review
    kind: agent
    owner: reviewer
    writes: [REVIEW.md]
```

Enforcement activates with the first declaration — a workflow that declares no `writes` and no `protected` keeps today's behavior and pays no snapshot cost. Once active:

- **Every writer attempt runs between a snapshot and a settlement.** Before the seat runs, the guard records the tree's content state; after it exits — success, malformed output, provider failure, timeout, cancellation, crash — every change since the snapshot is classified. Detection is by content hash, so a same-size rewrite cannot slip through, and it covers modifications, creations, deletions, renames and symlink retargets. Untracked files count; gitignored paths and `.git` internals are outside the guard's jurisdiction, matching what the diff gates see.
- **Unauthorized changes are reverted, then rejected.** A change outside the phase's `writes` (or touching `protected` — protection wins, so declaring the forbidden path does not authorize it) is restored byte-for-byte to its pre-attempt state, and the attempt is rejected with a `write_scope` gate report naming each path. The writer's authorized work survives; the correction tells it exactly what was reverted. Absent `writes` means unrestricted except `protected`; `writes: []` declares a phase that changes nothing on disk.
- **Authorization never leaks.** The run's baseline — the dirt that belongs to the operator — is captured once at run start and persisted, so a resumed run does not relabel interrupted work as user dirt; a file being dirty, previously claimed, or claimed by a *rejected* attempt grants nothing. Untouched user edits survive settlement byte-for-byte and are never reported. Relatedly, `diff_matches_claims` now commits an envelope's claims to the shared accepted set only when the engine accepts the attempt: a rejected attempt's confession stops expanding later permission.
- **Rollback that cannot be trusted fails closed.** When a restoration is unsafe — the path is now a directory, an I/O error mid-restore — the guard preserves the full unauthorized diff as a patch, reports the paths unrecoverable, and the run halts instead of retrying. A tree the guard could not restore must never settle as success.

Scopes bound the *filesystem*, not the model's tools — panel seats are already read-only by construction, and a writer keeps its full toolset inside its scope. The writer is told its scope in its prompt, so the first attempt is not entrapment.

## The complete SDLC recipe

`docs/adw/examples/sdlc.yml` composes every mechanism above into one factory run — `plan → build → docs → verify → review` — and is the shape the other examples are fragments of:

```text
plan    agent  writes: [PLAN.md]
build   agent  inputs: [plan]         writes: [src/**, test/**]
docs    agent  inputs: [plan, build]  writes: [docs/**, CHANGELOG.md, README.md]
verify  code   your real test/lint/typecheck commands     onFail: correct → build
review  agent  inputs: [plan, build, docs]  verdict_consistent  onReject → build
```

A red `verify` corrects the builder; a coherent reviewer rejection revises it; both re-run everything downstream, so no green result from before an edit survives into acceptance. Every edit — code and documentation — precedes the checks and the approval that authorize it.

**The delivery contract.** `acceptance: review` plus `isolation: true` defines the deliverable: the run works against a materialised copy, and the accepted diff is applied to your checkout only when the final review approves. A rejected or exhausted run leaves the checkout byte-for-byte untouched — including your own uncommitted edits — with the work preserved as `runDir/<adwId>.patch`. Nothing is staged, committed or pushed: there is no `git add -A` anywhere in the pipeline, and landing the applied diff, committing it, or any remote action (PR, push, deploy) remains an explicit operator decision outside the workflow.

**Adaptation is the installation.** Copy the file into `.omp/adw/`, then bind `verify` to the target repository's real entry points — the shipped command deliberately exits 1 naming itself, because a placeholder that exits 0 would deliver unverified code on every run. Adjust the `writes` globs to the repo's layout and put its evaluator scripts and lockfiles under `protected`. Owners resolve through the ordinary agent roster (`.omp/agents/*.md` or bundled), model selection through the ordinary configuration precedence — the recipe is a workflow file, not a second engine, and local prompt or roster customization applies to it like to any other run.

## Isolation

![Isolation](../assets/adw/04-isolation.webp)

`isolation: true` runs the **whole workflow** against one materialised copy of the repo — APFS clone, reflink, or overlay, whichever the platform offers.

One sandbox per run, not one per spawn: a `plan` phase writes files that a `build` phase reads and gates verify on disk. Per-spawn isolation would destroy exactly the continuity the phases depend on.

On accept, the diff is applied to your checkout — but only after git confirms it applies cleanly, because a half-applied patch is worse than none. On reject, the checkout is never touched and the work survives as `runDir/<adwId>.patch`. Nested-repo patches are captured and applied only *after* the root landed; a nested repo that refuses its patch is non-fatal but never silent, surfacing as `nestedWarnings`.

## Resume

![Resume](../assets/adw/05-resume.webp)

A run that dies mid-flight is resumable, because the trace is the record:

```text
/adw resume ship adw-1574a6b1b268f57c
```

`Run::resume` replays `events.bin` and rebuilds the cursor, the attempt tally, the handoff from the last accepted envelope, the pending correction — for revisions, rewinds AND plain in-place rejections, so a resumed retry knows *what* was wrong, not just that something was — and, for runs that revised, the consumed revision counts, granted attempts and invalidated records. The run continues at the phase that died, with the budget it had already spent: completed phases are not redone, a consumed revision cannot be replenished by resuming, and a `maxAttempts` differing from the traced base is refused naming both numbers rather than silently substituted. A `run_resumed` record marks the seam, because after it a terminal record is no longer the end of the story. Resume also refuses a trace whose review routes no longer match the workflow's `onReject` declarations.

Run state lives in `~/.omp/agent/adw/<adwId>/`, deliberately **outside** the session directory: a crashed run is resumed by a new process, so session-keyed state would make the dead run unfindable by exactly the caller that needs it. Beside the trace it holds the run's durable contract: `request.txt`, `workflow.json` (the definition the decisions were made against — a resume under an edited file is refused naming the record), `claims.json` (the accepted `diff_matches_claims` authorization, loaded on resume instead of recapturing the tree so a crashed attempt's undeclared edits are never relabelled as operator dirt), the write-guard baseline, `isolation.json` and `delivery.json`.

**Isolated runs resume by reattaching.** When the process died outright, the sandbox usually survives; resume reattaches it — re-claiming ownership so a sweeping `omp worktree clear` does not reap it mid-run — and continues where the trace stopped, with the completed phases' work exactly where they left it. The sandbox is never rebuilt from the base: that would silently redo completed phases from the base commit, so a sandbox that is genuinely gone refuses instead.

**Delivery happens exactly once.** Settling a run — applying the accepted diff, or preserving a rejected one as a patch — records its outcome in `delivery.json` before anything else can happen to the tree. A run that accepted and then died inside the delivery window is recovered by settling from the record and the surviving sandbox, without re-running any phase; a run whose delivery already settled refuses to resume rather than risk applying the patch twice.

Resume refuses:

- a trace belonging to a **different workflow**, a `workflow.json` that no longer matches the file, or phases that do not match position-for-position — replaying a plan against a different pipeline would replay decisions nobody made about those phases;
- a run that already **finished accepted with its delivery settled** — there is nothing left to continue;
- an **attempt budget** differing from the one the trace recorded;
- an isolated run whose **sandbox is gone** and whose delivery never settled.

The driver writes a terminal record on a crash too, so a reader can tell a dead run from a running one.

## The trace format

Every event is one fixed-stride binary record. Text was the wrong wire: a JSONL line re-spells `"kind":"phase_started"` on every event and forces a parse per line on every reader poll.

```text
events.bin    magic "PITR", 16-byte header, then 36-byte records
strings.bin   magic "PIST", content-interned strings addressed by u32 id
```

| Offset | Size | Field                        |
| ------ | ---- | ---------------------------- |
| 0      | 8    | timestamp, unix millis       |
| 8      | 1    | event kind                   |
| 9      | 1    | flags — bit 0 = `ok`         |
| 10     | 2    | reserved                     |
| 12     | 4    | phase — string id            |
| 16     | 4    | owner — string id            |
| 20     | 4    | gate — string id             |
| 24     | 4    | detail — string id           |
| 28     | 4    | value — counter              |
| 32     | 4    | attempt                      |

A reader seeks event `n` at `HEADER_LEN + n * RECORD_LEN` and tails by byte offset with zero parsing: cursor, byte offset and sequence number are the same number. String id `0` is the empty string and is never written, so an absent field costs nothing. The run id is the *directory*, not a field — what the key already tells you is not repeated per record.

The encoding is the ABI. `taskTraceLayout()` exports the offsets so a reader in another language uses them directly instead of duplicating the layout; there is no `unsafe` and no transmute, only explicit `to_le_bytes`, so the layout is identical on every target.

Event kinds: `run_started`, `phase_started`, `phase_retry`, `gate_check`, `phase_rejected`, `phase_finished`, `run_finished`, `panel_opinion`, `run_resumed`, `phase_tokens`, `phase_rewound`, `review_revision`, `review_exhausted`, `input_selected`, `correction_pending`, `phase_invalidated`.

`panel_opinion` exists because the fan-out happens in the caller — only it can spawn a model. Without that record a fusion phase would be one opaque span instead of N comparable answers.

`phase_rewound` cannot be skipped by a reader, which is why it forced a format-version bump: it carries the target the run went back to, and position stops being derivable by counting passed phases the moment one of them runs twice.

`review_revision` carries the revision route end to end: target and source phases, the target's spent attempts, the cumulative revision count and the complete correction text — enough for `resume` to rebuild grants, invalidation and the pending correction without guessing. `review_exhausted` closes the loop with the review's own reason. Both forced the bump to format version 4, which also widened `attempt` to a u32 at offset 32: a granted budget can exceed what a u16 held, and truncating an attempt number silently would corrupt exactly the replay that needs it. Readers refuse older versions by path instead of half-decoding them — a resumable run is worth less than a wrong one.

`input_selected` records which producer version a consuming phase was handed at dispatch — consumer in `phase`, producer in `owner`, the producer's acceptance version in `value`. It is the record that lets the viewer and a resumed run answer *which results authorized this execution*, and it took the format to version 5 (the layout is unchanged; the kind is new, and a reader that ignored it would explain a consumer's work with outputs it never saw).

`correction_pending` carries an in-place rejection's full correction text (format version 6). `phase_rejected` interns only the summary, which was enough while the correction lived in memory — but a resumed retry that knows only *that* it failed repeats the attempt blind, and counting attempts without restoring the diagnostic is not recovery. Rewinds and revisions already carried theirs; this closes the last gap. As with every bump, older versions are refused by path instead of half-decoded.

`phase_invalidated` closes the loop on retries under the phase state machine: it carries a superseded phase run — target in `phase`, the superseding owner in `owner`, the target's spent attempt budget in `attempt` — where an empty `owner` means a crash reset on resume rather than supersession of prior evidence. It holds the same stride as every other record (format version 6, layout unchanged): a reader that ignored it would keep a stale acceptance as current and explain a consumer's work with output that was already superseded.

A trace can hold **several terminal records**: the driver writes a failed one on a crash so a reader can tell a dead run from a running one, and a continuation writes its own verdict after the `run_resumed` seam. The current state of the run is the LAST terminal record; the viewer reads it that way and keeps the earlier ones in the timeline as history.

`phase_tokens` is what an attempt cost, reported by the caller for the same reason. It is charged **per attempt, before the verdict**, because a rejected attempt spent real tokens: a run whose cost counted only its successes would hide the retries that made it expensive — in a measured two-attempt run the rejected try cost about as much as the accepted one. It needs its own record because `value` already carries the violation count on both rejection paths.

The figure is `usage.totalTokens`, not the subagent's `tokens` counter: that counter deliberately excludes cache reads, which is right for a cumulative billing-volume number and wrong for "what did this attempt cost".

**The `ok` flag on the record means *accounted for*.** Not every provider reports usage — `omniroute/auto` returns an all-zero usage record — and a model turn cannot genuinely cost zero. A zero charge is therefore marked unaccounted, the viewer says `provider reported no usage` rather than `0 tokens`, and a run with any unaccounted phase shows the count beside its total. The total is a floor, never a confident sum over numbers the provider never gave.

## Viewing a run

`packages/adw-web` serves a read-only waterfall of any run under `~/.omp/agent/adw/`:

```bash
cd packages/adw-web && bun server.ts     # http://localhost:4600
```

| Endpoint          | Returns                                             |
| ----------------- | --------------------------------------------------- |
| `GET /api/runs`   | every run: workflow, duration, verdict, event count |
| `GET /api/runs/:id` | decoded events plus a per-phase timeline           |

Decoding stays server-side behind `TaskTraceReader`. The native addon cannot load in a browser, so the page only ever receives JSON — the alternative would be a second implementation of the record layout in JavaScript, free to drift from the ABI.

## Diagram sources

The diagrams above are exported from checked-in [draw.io](https://www.drawio.com/) sources sitting next to them in `assets/adw/`. Edit the `.drawio`, then re-export — draw.io writes PNG, and `cwebp -lossless` gets the same pixels in a third of the bytes:

```bash
drawio --no-sandbox --export --format png --scale 2 \
  --output /tmp/01-architecture.png assets/adw/01-architecture.drawio
cwebp -lossless /tmp/01-architecture.png -o assets/adw/01-architecture.webp
```

Rendered images are committed, not generated at build time, so the docs read correctly on GitHub without a toolchain. The `.drawio` files are the editable source of truth — do not hand-edit the `.webp`.
