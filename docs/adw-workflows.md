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
| `isolation`   | no       | Run the whole workflow against a materialised copy of the repo          |
| `undeclaredIgnore` | no  | Globs `diff_matches_claims` treats as always accounted for              |

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

A misspelled key fails the file. Every schema uses `onUndeclaredKey("reject")`, because `isolaton: true` parsing cleanly would run the whole workflow against the real checkout while the operator believes it is sandboxed. Silence is the wrong answer for a safety flag.

Validation also rejects, with the offending phase named: a duplicate phase name, an `agent` phase with no `owner`, a `code` phase with no `command`, `model`/`thinking`/`prompt` on a `code` phase, a `fusion` phase with fewer than two panel seats or no fuser, `panel`/`fuser` on a non-fusion phase, `timeoutMs <= 0`, and an unknown gate name.

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

That file runs `api → docs → verify`. The author declares what needs what instead of hand-sorting the list, and a phase moved later cannot silently start running before its input exists.

**The graph is declared, never inferred.** A graph a model proposes changes between runs given the same prompt, and then `resume` cannot rebuild a position in a plan that no longer exists. This one is a pure function of the file: ties break on declaration order, never on hash iteration, so two runs of the same workflow execute in the same sequence and a resumed run derives the order it originally ran.

Sorting happens once, when the workflow is constructed, which is why the rest of the engine stays ignorant of dependencies — the cursor still walks a list and `resume` still matches trace names to positions.

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

An accepted envelope is persisted to `<traceDir>/envelopes/<phase>.json` and becomes the next phase's handoff. It is the one thing the event trace cannot reconstruct, which is why it is written to disk rather than kept in memory.

## Gates

Gates verify claims; they never predict. They run **after** a phase, in Rust, against the filesystem — so a gate reads what the envelope actually declared and checks whether it is true.

| Gate                  | Passes when                                                                          |
| --------------------- | ------------------------------------------------------------------------------------ |
| `artifacts_exist`     | every path in `artifacts` exists on disk                                             |
| `files_non_empty`     | every path in `artifacts` exists **and** has non-zero size                            |
| `diff_matches_claims` | every path the working tree changed is one some phase declared                        |
| `json_parses`         | every declared `.json` artifact parses                                                |

`passed` is evidence rather than silence, and that has teeth: **an envelope that declares no artifacts fails both artifact gates** rather than clearing them vacuously. Requesting the gate is an assertion that the phase produces files. This was wrong until a live run proved it — a fuser returned `artifacts: []`, cleared both gates and wrote nothing, while this page already claimed it could not. Gate names are validated at load time against `taskGateNames()`, the same list the engine builds from — a new gate in Rust needs no matching edit in TypeScript to be accepted.

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

### `schema` — checking the payload's shape

An envelope carries whatever fields the phase was asked to report, beyond `status`/`summary`/`artifacts`/`notes_for_next_agent`. A phase can declare their shape:

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

**Structure, not semantics.** `type`, `required`, `properties`, `items`, `enum`, `const` and the numeric/length bounds are enforced. Conditionals — `if`/`then`, `allOf`, `anyOf`, `oneOf`, `not` — are **rejected at load time**:

```text
w.yml: phase "review" has an unusable schema: omptype does not enforce if, then
```

That rejection exists because of a measured surprise: omptype compiles `if: {approved: {const: true}}, then: {blocking: {maxItems: 0}}` without complaint and then validates `{approved: true, blocking: ["x"]}` as fine. A conditional that silently does nothing is worse than an absent one — it reads like a guarantee. So a cross-field rule belongs in a `code` phase, where an exit code cannot lie.

**Where this check runs, and why that is not a compromise.** Schema validation needs omptype, and a JSON Schema validator inside `pi-tasks` would cost that crate its three dependencies and its ability to be tested without a JavaScript runtime. So the caller runs it and reports the verdict back through `noteGateReport` — the same bargain `code` phases already make, where the caller executes and the engine judges. The result is a `gate_check` in the trace named `payload_matches_schema` and blocks acceptance exactly like a native gate.

The envelope is located with the engine's own extraction rule, exported as `taskEnvelopeText`, rather than a second implementation of "the last top-level JSON object" that would drift from it.

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

`Run::resume` replays `events.bin` and rebuilds the cursor from the count of passed phases, the attempt tally from the rejections against the phase that was in flight, and the handoff from the last accepted envelope. The run continues at the phase that died, with the budget it had already spent — completed phases are not redone. A `run_resumed` record marks the seam, because after it a terminal record is no longer the end of the story.

Run state lives in `~/.omp/agent/adw/<adwId>/`, deliberately **outside** the session directory: a crashed run is resumed by a new process, so session-keyed state would make the dead run unfindable by exactly the caller that needs it.

Resume refuses, before any I/O:

- a trace belonging to a **different workflow**, or one whose phases do not match position-for-position — replaying a plan against a different pipeline would replay decisions nobody made about those phases;
- a run that already **finished or was accepted** — there is nothing left to continue;
- a run with `isolation: true` — the sandbox was torn down with the run, and resuming into a fresh one would silently restart completed phases from the base commit.

The driver writes a terminal record on a crash too, so a reader can tell a dead run from a running one.

## The trace format

Every event is one fixed-stride binary record. Text was the wrong wire: a JSONL line re-spells `"kind":"phase_started"` on every event and forces a parse per line on every reader poll.

```text
events.bin    magic "PITR", 16-byte header, then 32-byte records
strings.bin   magic "PIST", content-interned strings addressed by u32 id
```

| Offset | Size | Field                        |
| ------ | ---- | ---------------------------- |
| 0      | 8    | timestamp, unix millis       |
| 8      | 1    | event kind                   |
| 9      | 1    | flags — bit 0 = `ok`         |
| 10     | 2    | attempt                      |
| 12     | 4    | phase — string id            |
| 16     | 4    | owner — string id            |
| 20     | 4    | gate — string id             |
| 24     | 4    | detail — string id           |
| 28     | 4    | value — counter              |

A reader seeks event `n` at `HEADER_LEN + n * RECORD_LEN` and tails by byte offset with zero parsing: cursor, byte offset and sequence number are the same number. String id `0` is the empty string and is never written, so an absent field costs nothing. The run id is the *directory*, not a field — what the key already tells you is not repeated per record.

The encoding is the ABI. `taskTraceLayout()` exports the offsets so a reader in another language uses them directly instead of duplicating the layout; there is no `unsafe` and no transmute, only explicit `to_le_bytes`, so the layout is identical on every target.

Event kinds: `run_started`, `phase_started`, `phase_retry`, `gate_check`, `phase_rejected`, `phase_finished`, `run_finished`, `panel_opinion`, `run_resumed`, `phase_tokens`, `phase_rewound`.

`panel_opinion` exists because the fan-out happens in the caller — only it can spawn a model. Without that record a fusion phase would be one opaque span instead of N comparable answers.

`phase_rewound` cannot be skipped by a reader, which is why it forced a format-version bump: it carries the target the run went back to, and position stops being derivable by counting passed phases the moment one of them runs twice.

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
