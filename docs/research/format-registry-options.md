# Format registry options for cleanse parsers

Status: owner scope decided; carrier conditional and registry not implemented.
Date: 2026-11-05

Baseline: `origin/omp2` at `230efc0df8`. A pull request cited by number is open and is not on the
baseline, unless the text says that it merged.

Owner ruling 1 chose option B for cleanse output formats. Core keeps the published interchange
formats, which are JUnit XML, SARIF 2.1.0 and TAP. The per-tool `ParserKind` variants move out of
core. Nothing outside core can supply a format today, so option B first needs a format registry.
This document prices three registries, and the choice to build none, against the 28 per-tool
variants that exist.

Evidence labels:

- **OBSERVED**: read at the baseline and cited as `path:line`, or read in a commit or pull request
  body that records its own runs.
- **[INFERENCE]**: reasoned from observed facts or from a tool's documented output format, not run.
- **NOT MEASURED**: a cost that nobody has timed.
- **REPORTED**: stated by another study or record, and not re-read for this document.
- **[GUESS]**: an estimate with no measurement behind it.

[How to recount](#how-to-recount) lists the commands behind each count.

## Owner decision, 2026-11-07

Core retains JUnit, SARIF, TAP and generic parsing. Existing per-tool formats move outside core, not only future formats. First-party formats must be installed by default so existing checks continue to work.

The Python extension host is the preferred carrier only if its required host changes are already planned and verified. Phase 0 reported no existing host route, a manifest mismatch, incomplete deadline cancellation, and no dedicated model-inaccessible format route. ADR 0040 records these as unbuilt prerequisites; verify their current status before selecting the carrier. If they are not planned or feasible, reassess the carrier rather than silently choosing Python.

This decision does not implement the registry. Complete the ordered tasks in `docs/adr/TASKS.md` after the current minimizer work.

## Recommendation

1. **Record first.** Record a failing run for each of the 16 per-tool kinds that have none. Then
   add a test that fails for any format without a recorded run and an asserted `Vec<Diagnostic>`.
   Every migration below needs these recordings to show that its output did not change. Each of the
   12 kinds recorded so far was misread until its recording existed.
2. **Delete what needs no registry.** Seven text kinds run exactly the `Generic` code path. Three
   JSON kinds have recorded SARIF output that the core SARIF parser already reads. Removing those
   ten kinds leaves 18 for a registry.
3. **Carry the other 18 on the embedded-Python extension host, option 1, after the dimension-1
   host fixes land.** The ADR 0040 dimension-1 study in progress (branch `docs/exec-out-of-core`,
   not yet on `omp2`) recommends the same host for out-of-core execution backends. REPORTED. That
   study needs Phase 0 changes 1 and 3, a call route that the model cannot use, and a public envd
   entry point. Formats need the same host work, so the two rulings pay for it once. On top of it,
   formats add the cleanse-to-host edge, an async parse seam, and one short call per checker run.
   Use the call route that dimension 1 builds for calls the model cannot make. Until that work
   lands, the 18 kinds stay in core under the step-1 test.
4. **Do not build option 2 or option 3 as the registry.** Option 2 defines a new plugin protocol
   that any language can implement, and ADR 0036 allows a second extension language only by
   superseding that record. Option 3 runs no third-party code, but it cannot express cargo or
   `go test` until its spec language gains line-state rules. After that, each tool shape that it
   cannot express comes back to core as a language change.

The choice of carrier depends on a fact that is not settled. If dimension 1 does not adopt the
Python host, option 1 pays for the host fixes alone, and option 3 has the better bill.
[What would change my mind](#what-would-change-my-mind) lists that falsifier and four others.

| | 1. Python host | 2. External command | 3. Declarative extractor | 4. No registry |
|---|---|---|---|---|
| Meets ruling 1 | Yes | Yes | Only when the definitions ship outside the binary | No |
| Crates changed for formats alone | driver, app | driver; exec and exec-local for stdin | driver, ext | driver tests |
| Host work it waits for | Phase 0 changes 1 and 3, and a route the model cannot call | None | None | None |
| Async parse seam | Yes | Yes | No | No |
| Kinds it can express | 28 | 28 | 23 to 25 with one-line rules, 28 with line-state rules | 28, all kept in core |
| What shows that output did not change | 12 failing-run recordings | The Rust code moves unchanged | 12 failing-run recordings | The code does not change |
| Cost per parse | One call, which returned in 41 ms in Phase 0, and one host start per run (NOT MEASURED) | One process start per checker run (NOT MEASURED) | In process (NOT MEASURED) | Unchanged |
| Runs third-party code | Yes, in a killable child | Yes, in a process under the session sandbox mode | No | No |

The per-tool Rust that ruling 1 moves out is small. It is 402 production lines in three modules,
five branches in the shared parser, and 28 enum variants. Each registry adds about as much core
code as that, or more. [GUESS] A registry earns its place through the ownership line that ruling 1
draws and through third-party formats (`docs/adr/TASKS.md:93`, row N3), not through a smaller core.

## What the 28 kinds are

`ParserKind` (`crates/driver/src/cleanse/parsers.rs:73`) has 32 variants. Four are not per-tool.
`JUnit`, `Sarif` and `Tap` are the standards. `Generic` is the heuristic that seven built-in
families use. The other 28 are the per-tool kinds that ruling 1 moves out. OBSERVED.

The 28 kinds are not 28 parsers. `parse` (`parsers.rs:162`) sends them down four kinds of path.
OBSERVED.

- **Four kinds have a module of their own.** `rust` and `rust-test` go to `parsers/cargo.rs`,
  `stylelint` goes to `parsers/stylelint.rs`, and `terraform` goes to `parsers/terraform.rs`. The
  three modules hold 402 production lines: `cargo.rs` 1-224, `stylelint.rs` 1-100 and
  `terraform.rs` 1-78.
- **Five kinds use the shared JSON walker with one branch each.** `go-test` reads its `Output`
  field as text (`parsers.rs:317`) and falls back to a `panicked at` line (`parsers.rs:628`).
  `shellcheck` turns a numeric code into `SC<n>` (`parsers.rs:418`). `pyright` counts lines and
  columns from 0, and `deno-lint` and `pylint` count columns from 0 (`parsers.rs:521-523`).
- **Twelve kinds use the shared JSON walker with no code of their own.** For them the variant
  means only that the output must hold a JSON report. Output that holds no JSON value and no
  diagnostic line is unreadable, not clean (`parsers.rs:186-191`).
- **Seven kinds share the `Generic` arm** (`parsers.rs:192-199`). No other branch names them, so
  `go`, `staticcheck`, `mypy`, `flake8`, `ty`, `oxlint` and `dart` produce exactly what `generic`
  produces at the baseline.

The twelve and the seven depend on tool knowledge that lives in the shared walker, not in their
variants. The walker recurses into a fixed list of container keys (`parsers.rs:327-337`). It reads
fields through key lists such as `FILE_KEYS` (`parsers.rs:352`) and the code keys at
`parsers.rs:406`. Several of those keys come from one tool. `Issues`, `Pos` and `FromLinter` come
from golangci-lint, `generalDiagnostics` comes from Pyright, and `spans` and `children` come from
rustc. The text parser reads the rustc forms `error[...]:` and `-->` (`CODE_PREFIX` and
`RUST_ARROW`, `parsers.rs:59-68`). [Questions for the owner](#questions-for-the-owner) asks
whether ruling 1 covers that knowledge.

| Kind | Built-in command (`checkers.rs:FAMILIES`) | Parsed by | Recorded failing run |
|---|---|---|---|
| `rust` | `cargo check --message-format=json`, `cargo clippy --message-format=json -- -Dwarnings` | `parsers/cargo.rs` | `fixtures/cargo/{check,clippy}-*` |
| `rust-test` | `cargo test --message-format=json` | `parsers/cargo.rs` | `fixtures/cargo/test-*` |
| `stylelint` | `stylelint "**/*.{css,scss,sass,less}" --formatter=json` | `parsers/stylelint.rs` | `fixtures/stylelint/` |
| `terraform` | `terraform validate -json` | `parsers/terraform.rs` | `fixtures/terraform/` |
| `go-test` | `go test -json ./...` | Walker and its `go-test` branch | None |
| `shellcheck` | `shellcheck --format=json` | Walker and its `shellcheck` branch | `fixtures/shellcheck/` |
| `pyright` | `pyright --outputjson` | Walker and its 0-based axes | `fixtures/pyright/` |
| `deno-lint` | `deno lint --json` | Walker and its 0-based column | `fixtures/deno/` |
| `pylint` | `pylint --output-format=json .` | Walker and its 0-based column | `fixtures/pylint/` |
| `golangci` | `golangci-lint run --output.json.path=stdout` | Walker | `fixtures/golangci/`, and SARIF in `fixtures/sarif/` |
| `ruff` | `ruff check --output-format=json .` | Walker | `fixtures/ruff/`, and SARIF in `fixtures/sarif/` |
| `biome` | `biome check --reporter=json .` | Walker | `fixtures/biome/`, and SARIF from `biome lint` in `fixtures/sarif/` |
| `eslint` | `eslint . --format=json` | Walker | `fixtures/eslint/` |
| `rubocop` | `rubocop --format json` | Walker | None; clean run in #518 |
| `phpstan` | `phpstan analyse --error-format=json` | Walker | None; clean run in #518 |
| `psalm` | `psalm --output-format=json` | Walker | None; clean run in #518 |
| `swiftlint` | `swiftlint lint --reporter json` | Walker | None; clean run in #518 |
| `credo` | `mix credo --format json` | Walker | None; clean runs in #518 |
| `hlint` | `hlint . --json` | Walker | None; clean run in #518 |
| `tflint` | `tflint --format=json` | Walker | None; clean run in #518 |
| `actionlint` | `actionlint -format '{{json .}}'` | Walker | None |
| `go` | `go vet ./...` | `Generic` arm | None |
| `staticcheck` | `staticcheck ./...` | `Generic` arm | None |
| `mypy` | `mypy .` | `Generic` arm | None |
| `flake8` | `flake8 .` | `Generic` arm | None |
| `ty` | `ty check` | `Generic` arm | None |
| `oxlint` | `oxlint .` | `Generic` arm | None |
| `dart` | `dart analyze --format machine` | `Generic` arm | None |

### Every recorded kind was misread before its recording

The body of commit `230efc0df8` (#516) names eleven tools whose real output the parser misread
until someone recorded it. OBSERVED.

1. Every Ruff line and column read as `None`.
2. Every ESLint file read as `None`, because ESLint names the file once, on the parent entry.
3. Biome lost its positions and its rule, and one of its 1-based axes was shifted.
4. Deno lines were shifted by one, and its 0-based columns were lost.
5. Pylint's 0-based columns were not shifted.
6. ShellCheck's numeric code was dropped.
7. golangci-lint version 2 rejected `--out-format`, and its `Issues[].Pos` went unread.
8. Every cargo JSON record became an error, so a clean cargo run never read as clean.
9. Pyright's `generalDiagnostics` went unread, so a failing run parsed to nothing.
10. Stylelint's report on stderr, with its `source` and `warnings`, went unread.
11. Terraform's `summary`, `detail` and `range.filename` went unread.

Those eleven tools are exactly the 12 kinds that have a failing-run recording, because cargo covers
`rust` and `rust-test`. Open #518 records clean runs for ten JSON checkers and finds two more
defects. SwiftLint's clean run read as unparseable. Credo's first run after `mix deps.get` reported
two failures that came from Logger lines. OBSERVED in the #518 body.

The other 16 kinds have no failing-run recording. For ten of them, the tool's documented output
uses keys or a layout that the baseline parser does not read. [INFERENCE] Nobody has run these
cases, and each one needs a recording before anyone relies on it.

- `rubocop` nests offenses under `files[].offenses`. The walker never enters a key named
  `offenses`, so a failing run would read as no diagnostic. For a run that exits non-zero,
  `CheckResult::diagnostics` (`types.rs:138`) then reports one `checker-failed` diagnostic.
- `phpstan` keys `files` by path. The walker reads no map key as a file, so the outcome matches
  RuboCop.
- `hlint` names the problem `hint`, which is not one of the walker's message keys. No entry becomes
  a diagnostic.
- `psalm` names the file `file_name` and the line `line_from`. Each diagnostic would have a message
  and no location.
- `tflint` puts the file at `range.filename`, the column at `range.start.column` and the rule at
  `rule.name`. Each diagnostic would have a line and no file, column or code.
- `actionlint` names the file `filepath`, while `FILE_KEYS` holds `filePath`. No diagnostic would
  have a file.
- `swiftlint` names the column `character` and the rule `rule_id`. No diagnostic would have a column
  or a code.
- `credo` names the line `line_no`. No diagnostic would have a line.
- `dart analyze --format machine` writes pipe-separated rows, and the text pattern `LOCATION`
  (`parsers.rs:55`) needs `file:line`. A failing run would read as no diagnostic.
- `oxlint` writes a multi-line graphical report by default, and the text parser reads one line at a
  time.

## Work that every registry option shares

### The format seam

Options 1, 2 and 3 all need the same change at the parse seam. OBSERVED for the current code.

| `file:symbol` | Change |
|---|---|
| `crates/driver/src/cleanse/parsers.rs:ParserKind` (73) | Keep `JUnit`, `Sarif`, `Tap` and `Generic`. Delete the 28 per-tool variants. |
| `crates/driver/src/cleanse/types.rs:Checker::parser` (67) | Name either a core format or a registered format ID. The field stops being `Copy`. |
| `crates/driver/src/cleanse/checkers.rs:CustomCheckerSpec::parser` (744) | Deserialize the same two-arm type. `deny_unknown_fields` stays. |
| `crates/driver/src/cleanse/mod.rs:discovery_schema` (259) | Take the installed format IDs as input. The `parser` enum at `mod.rs:273` lists 32 fixed names today. |
| `crates/driver/src/cleanse/checkers.rs:FAMILIES` (160) and `discover` (597) | A family whose format is not installed goes to `Suite::skipped` with a reason, as a missing binary does in `custom_suite` (772). |
| `crates/driver/src/cleanse/parsers.rs:ParseError` (29) | Add one variant for a registered format that failed, so `unparseable` (246) renders it and the run never reads as clean. |
| The tests in `parsers.rs` and `parsers/*.rs` | A test for a kind that moves out moves with its parser. |

The 36 `FAMILIES` rows also hold per-tool knowledge: binary names, arguments and marker files. This
document prices formats only. A family whose format left core does nothing until the user installs
that format. Out of the box, `omp cleanse` then stops checking those tools. Options 1, 2 and 3
share that regression. A first-party format package that the installer adds by default removes it.

### The async parse seam

Options 1 and 2 parse in another process, so their parse call is async.
`normalize_result` (`checkers.rs:952`) and `parse_output` (`checkers.rs:961`) are sync, but both run
inside async functions, `run_suite_streaming` (`checkers.rs:829`) and `run_checker_streaming`
(`checkers.rs:887`). The change therefore needs no `block_on` bridge. `parse_output` becomes
`async` and reaches the parse route through `CheckerRunner` (`checkers.rs:123`) or a new bound on
`CleanseHost` (`mod.rs:37`). Both runners, `ProductionCleanseHost` (`production.rs:279`) and the
test host in `continuous.rs:265`, must implement the new method. OBSERVED.

One trap exists. `run_checker_streaming` parses partial output inside a `tokio::select!` arm
(`checkers.rs:909`). An awaited external parse in that arm stops the loop from polling the checker
process. A registered format must skip partial parsing. Production loses nothing by that today,
because `ProductionCleanseHost::run_checker` sends partial output once, when the run settles
(`production.rs:396`). OBSERVED.

Option 3 evaluates in process and stays sync.

### The result contract

Options 1 and 2 cross a process boundary, so they share one wire contract.

- The request is one JSON object with `schema`, `format`, `exit_code`, `stdout` and `stderr`.
- The response is `{"schema": 1, "parsed": [...]}` or `{"schema": 1, "unreadable": "<reason>"}`.
- A parsed entry has the fields of `Diagnostic` (`types.rs:31`) except `checker`, which core fills
  in. `Diagnostic` already derives `Deserialize`, but it requires `checker`, so the wire entry is a
  separate struct with `deny_unknown_fields`.
- Core validates each response. Positions are 1-based or absent, an end is not before its start,
  and core applies `relative_path` (`parsers.rs:652`) itself. The response has a cap on its size
  and on its diagnostic count.
- A response that fails validation counts as unreadable output. It degrades through `unparseable`
  and never reads as clean.

Phase 0 already sent the request half of this contract over the trusted extension route. The
received object equalled the submitted stdout, stderr, exit code and format payload. OBSERVED in
ADR 0040, "Phase 0: out-of-core host feasibility", item 5
(`docs/adr/0040-remote-build-and-test-execution.md:56`).

### Removals that need no registry

1. **Seven text kinds are `Generic` under another name.** Change their seven `FAMILIES` rows to
   `ParserKind::Generic` and delete the variants. At the baseline the output does not change.
   OBSERVED. Open #518 stops every walker kind except `generic` and `go-test` from reading text
   lines once a JSON report was read. After #518, the seven still match `Generic` on any output
   that holds no JSON value.
2. **Three JSON kinds can switch to SARIF.** `fixtures/sarif/` holds verbatim SARIF from ruff
   0.17.3, biome 2.6.4 and golangci-lint 2.14.0 (`fixtures/sarif/PROVENANCE.txt`). The
   `parsers/sarif.rs` tests read all three. The switch deletes three variants, but it changes what
   the run reports. Biome's recording ran `biome lint`, while the family runs `biome check`, which
   also reports formatting. Each family needs a recording of its own command.
3. **Other tools may emit a standard format.** [INFERENCE] staticcheck, tflint, psalm, hlint,
   swiftlint and credo document a SARIF output mode. None is recorded. Step 1 of the
   recommendation records them before any family changes.

After removals 1 and 2, 18 kinds remain for a registry. They are `rust`, `rust-test`, `stylelint`,
`terraform`, `go-test`, `shellcheck`, `pyright`, `deno-lint`, `pylint`, `eslint`, `rubocop`,
`phpstan`, `psalm`, `swiftlint`, `credo`, `hlint`, `tflint` and `actionlint`.

## Option 1: the embedded-Python extension host

A Python extension declares a parser. Cleanse sends one checker run's output to it and reads
diagnostics back. Two call routes exist. Route T reuses tool invocation, which Phase 0 timed.
Route C adds a dedicated callback.

### Core changes for formats alone

| `file:symbol` | Change |
|---|---|
| `crates/driver/src/cleanse/production.rs:ProductionCleanseHost::environment_owner` (177) | Attach with the user's trusted and user-layer extension specs, not `trusted_extensions: Vec::new()` (`production.rs:186`). Pass a `RegistryBridges` that carries the session's workspace trust, not `RegistryBridges::default()` (`production.rs:189`). Keep workspace roots out. A project must not choose the code that its own cleanse loads, as `child_kernel_options` (`production.rs:112`) records. |
| `production.rs:ProductionCleanseHost::open`, `crates/app/src/cleanse_cmd.rs:run`, `crates/app/src/chat_services/misc.rs:cleanse` | Carry the resolved extension specs. Both callers pass only `root`, `data_dir`, `ctx` and a presentation today. The kernel resolves the specs at `crates/driver/src/headless/kernel.rs:2635-2649`. |
| The async parse seam | As described above. |
| Route T: `omp_envd::ProjectEnvironment::client` (`crates/envd/src/lib.rs:1213`) and `EnvClient::invoke` (`crates/env/src/client.rs:2154`) | Drive one tool invocation per checker run with the request envelope, and read the response. Phase 0 saw one call on this route return in 41 ms. The Phase 0 probe registered its normalizer as `@omp.tool("p0_normalize", rev=1)` (`docs/audits/manifest-tool-row-defect.md`), so the Python SDK needs no change. |
| Route C: `ProjectEnvironment::extension_callback_dispatcher` (`crates/envd/src/lib.rs:1371`) | This public method has no caller in the workspace. Route C needs a lookup from format ID to the connection identity of the sealed declaration, and a documented CONTROL operation (operations must start with `omp.`, `crates/envd/src/exthost/control.rs:3437`). It also needs a declaration kind in the manifest kind split (`crates/ext/src/config.rs:2192-2215`), and a decorator and handler in `crates/py/python/omp/_registry.py`. `PyCallbackRoute::dispatch` (`crates/envd/src/exthost/extensions.rs:1715`) is `pub(crate)` and stays internal. |
| The format seam's discovery input | Read the installed format IDs from the cleanse environment, and pass them to `discovery_schema`. |

### Host work shared with dimension 1

ADR 0040 lists four Phase 0 changes (`docs/adr/0040-remote-build-and-test-execution.md:60-66`).
Formats need two of them, plus the two items that the dimension-1 study names.

| `file:symbol` | Change |
|---|---|
| `crates/driver/src/discovery/native.rs:lower_manifest` (512, tool rows at 550-564), `crates/ext/src/config.rs:ToolManifestEntry` (1405), `crates/py/python/omp/_registry.py:2527` | Phase 0 change 1. The Rust lowering and the Python validator must agree on `name@family.rev`. `ToolManifestEntry` has no `name`, `family` or `rev` field. Route T needs this fix for any installed extension. Only the hidden `--trusted-extension` route works without it (`docs/audits/manifest-tool-row-defect.md`). |
| `crates/envd/src/exthost/dispatch.rs:DispatchRouter::expire` (1500) | Phase 0 change 3. Today the caller's deadline fires, but the body keeps running. A parser stuck in a loop keeps a Python thread busy until the host is replaced. |
| A route the model cannot call | Route T registers the parser as a tool. A name search for a model-visibility flag (`model_visible`, `model-callable`, `hidden_from_model`, `ModelVisibility`) in `crates/tool/src`, `crates/envd/src/exthost` and `_registry.py` found none, and the dimension-1 study reports the same gap (REPORTED). Without such a route, every session that loads the extension offers the parser to the model. |
| A public envd entry point | Route C needs it. Route T can use the public `client` accessor instead. |

Phase 0 change 2 (reattach) and change 4 (`env.process`) are not needed, because a parse is one
short call.

### What it can and cannot express

A Python function can compute anything from the two streams and the exit code, so option 1
expresses all 28 kinds. Its limits come from the transport.

- A CONTROL frame is at most 64 MiB (`MAX_CONTROL_FRAME_BYTES`,
  `crates/envd/src/exthost/control.rs:1045`). The checker output travels as a JSON string inside a
  frame. [INFERENCE] Escaping grows a JSON report, so the usable limit is below 64 MiB. Larger
  output degrades to `unparseable`.
- One call returns one result, so a registered format streams no partial diagnostics.
- Route C's default policy is `CallbackConcurrency::Serialized`
  (`crates/envd/src/exthost/dispatch.rs:153`), which lets one callback enter Python at a time.
  Cleanse runs its read-only checkers concurrently (`checkers.rs:860`), so their parses queue.
  Route T's concurrency is NOT MEASURED.

### Trust and failure

The parser is extension code in a child process that the host can kill (ADR 0036, decision 5). An
installed parser arrives through the signed extension index in `crates/ext`. The hidden
`--trusted-extension` flag loads one without that index. A parse needs no capability grant.
Workspace extensions stay excluded.

Every failure ends in the new `ParseError` variant and an `unparseable` diagnostic. None reads as
clean.

- The format is not installed. Discovery skips the checker and records the reason.
- The host is down, crashes or disconnects. Phase 0 saw a sibling call aborted when its host
  disconnected.
- The parser raises, or it returns a response that fails validation.
- The deadline passes. Phase 0 set a 1500 ms deadline. The caller received its abort at 1538 ms,
  but the body completed at 4.006 s. A sync body observed no cancellation, and the host was killed
  and respawned. Change 3 sends the cancel. A CPU-bound Python body can still run until the host is
  killed.
- The output exceeds the frame limit.

### How many kinds can migrate

All 28 can be written in Python. The 12 kinds with a failing-run recording can be checked against
it. The other 16 would be rewritten with nothing to compare against, which step 1 of the
recommendation fixes. After the removals that need no registry, the port covers 18 kinds. The code
that moves is the three dedicated modules, the five branches, and the walker behavior that the
JSON kinds rely on, together with their tests over the same fixture files.

### Maintenance

- Core keeps the format seam, the edge, contract validation and the shared host fixes.
- The parsers become a Python package outside core, with its own release and fixture tests.
- A new tool is a change to that package, with no core change.
- The contract's `schema` field handles version skew, and core refuses an unknown schema.
- Each cleanse run that uses a registered format starts an extension host for the cleanse
  composition (NOT MEASURED). Each checker run makes one call, and Phase 0 saw one call return in
  41 ms on macOS arm64.

## Option 2: an external parser command

Cleanse runs a parser executable for a registered format. It writes the request to the parser's
stdin and reads the response from its stdout.

### Core changes

| `file:symbol` | Change |
|---|---|
| `crates/driver/src/cleanse/checkers.rs:FilesystemResolver` (36), and a user setting in the shape of `SV_MINIMIZER_FILTERS` (`crates/minimizer/src/config.rs:49`) | Map a format ID to an argv, and resolve the executable at discovery. Search only absolute `$PATH` directories, never a project-local file, for the reason that `checkers.rs:28-35` gives. A missing parser goes to `Suite::skipped`. |
| `crates/exec/src/request.rs:ExecutionRequest` (152) and `crates/exec/src/backend.rs:ExecutionRun` | The backend contract has no stdin. Add a stdin payload to the request. `LocalBackend::start` (`crates/exec-local/src/lib.rs:76`) writes it with `ExecHost::stdin` (`crates/envd/src/exec.rs:1229`), then closes the stream. The remote backends refuse a request that carries stdin, and parser requests are `Eligibility::LocalRequired`. |
| The alternative to stdin | Pass the retained output blobs (`ProductionCleanseHost::retain`, `production.rs:238`) to the parser as file paths. The exec crates do not change. [INFERENCE] The parser must be able to read the blob store under the session's sandbox mode. |
| `crates/driver/src/cleanse/production.rs` | Run the parser beside `run_checker` (`production.rs:282`) with a short timeout. The checker timeout is 30 minutes (`production.rs:335`). |
| Contract types and validation in `crates/driver/src/cleanse/` | As described in the result contract. |
| The async parse seam | As described above. |

Option 2 needs no Python host, no extension edge, no manifest fix and no Phase 0 change.

### What it can and cannot express

The parser can be any program, so option 2 expresses all 28 kinds. It is also the only option
that can move the existing Rust out unchanged. One out-of-tree binary takes the format ID as its
first argument and wraps today's `parse` in the contract. Its tests and fixtures move with it, so
the output stays the same for all 28 kinds by construction. That includes the 16 unrecorded kinds
and any defects that they carry.

### Trust and failure

- The parser has the trust of a checker. The user installs it, and cleanse finds it on absolute
  `$PATH`.
- It runs through the same `LocalBackend` and posture as a checker
  (`ProductionCleanseHost::posture`, `production.rs:199`). This path has no per-request narrowing.
  `ExecutionRequest::requirement` is a ceiling that placement compares, not an enforcement. Under
  `workspace-write`, a parser can therefore write the workspace, although it never needs to.
- ADR 0036 decision 2 requires superseding that ADR to add a second extension language. A stdin and
  stdout protocol embeds no runtime, but it lets anyone write a parser in any language. MCP stdio
  servers are a precedent for foreign subprocesses, but MCP is an external standard, and this
  protocol would be omp's own. The owner must rule on whether it counts.
- A missing parser skips its checker. A non-zero exit, a timeout, or a malformed or oversized
  response makes the output unreadable. Each case degrades through `unparseable` and never reads
  as clean.
- Each checker run starts one process (NOT MEASURED).

### How many kinds can migrate

All 28 can migrate, and all 28 keep their output if the Rust code moves unchanged.

### Maintenance

- Core keeps the format seam, the process call and the contract.
- A first-party parser binary in Rust is a new release artifact that needs a build for each
  platform, as `omp` does.
- A new tool is a change in the parser binary's repository.
- Third-party authors need a conformance kit: the recorded fixtures and their expected responses.

## Option 3: a declarative extractor

A TOML definition names, for one format, where each `Diagnostic` field lives. Core evaluates the
definition in process.

### Core changes

| `file:symbol` | Change |
|---|---|
| A new module, for example `crates/driver/src/cleanse/parsers/extract.rs` | A schema with `deny_unknown_fields` and `schema_version`, as `crates/minimizer/src/pipeline.rs:PipelineFile` (142) has. A load step compiles every selector and pattern, and it reports every failure, as `PipelineError::Compile` (`pipeline.rs:182`) does. An evaluator walks `serde_json::Value` and text lines. |
| Inline tests | Each definition carries `[[tests.<format>]]` entries with a recorded input and the expected diagnostics, as `PipelineTest` (`pipeline.rs:128`) and `run_tests` (`pipeline.rs:560`) do for the minimizer. |
| Delivery | Add a static content kind beside `lsp-servers` and `dap-adapters` (`crates/ext/src/config.rs:1925-1960` and `2209`). Project it the way `skills` rows are projected (`crates/driver/src/discovery/native.rs:85`). Add a user file setting in the shape of `SV_MINIMIZER_FILTERS`. |
| `crates/driver/src/cleanse/parsers.rs:parse` (162) | Call the evaluator synchronously. No async seam is needed. |

The minimizer compiles its definitions in. `crates/minimizer/build.rs` concatenates
`src/defs/*.toml`, which holds 7 definitions in this port. Upstream holds 32. REPORTED. Compiling
cleanse definitions in that way would put per-tool knowledge back in the binary as data, which
ruling 1 excludes. The same evaluator can load the same files from an extension content row or a
user file, and that delivery is what meets ruling 1.

A content row cannot carry `id`, `module`, `key`, `trigger`, `api` or `failure`
(`crates/ext/src/config.rs:1936-1953`). OBSERVED. [INFERENCE] The tool identity defect therefore
does not apply, and delivery runs no Python.

### The spec language

These constructs cover the 28 kinds as they behave today.

1. **Input.** Read stdout, stderr, stderr unless it is blank and then stdout (Stylelint), both
   streams joined (today's walker), or each stream in turn (cargo).
2. **Framing.** Read one JSON document, JSON values among other text (the #518 rule), one JSON
   value per line, or plain text.
3. **Records.** Select records with filters such as `reason == "compiler-message"`. Group scoping
   lets an entry see fields that its parent names once: ESLint's `filePath`, Stylelint's `source`
   and RuboCop's `path`. A map key can be read as a value, for PHPStan's `files` map.
4. **Fields.** Each field reads the first of an ordered list of paths. A path can pick the first
   array element that matches a filter, which gives cargo's primary span and its first `help` or
   `note` child. Fixed transforms add 1 to a 0-based axis, treat 0 as absent, apply a template such
   as `SC{}`, join the non-empty parts of a list, map severities through a table, or set a constant.
5. **Completeness.** The output is unreadable unless a given record appears, such as cargo's
   `build-finished`, or unless a JSON value was read, as `NoJson` does today.
6. **One-line text rules.** A regular expression with named captures for each field.
7. **Line-state text rules.** A header line waits for the location line under it, as rustc's
   `error[...]:` waits for `-->`. A block runs from a start line to an end line, as a panic message
   does. A fallback fires when nothing matched, as `panicked at` does. A JSON string field can go
   through the text rules, as `go test -json` `Output` does. Text rules can read the runs of lines
   between JSON records, as cargo needs.

Tier A means constructs 1 to 6. Tier B adds construct 7.

### What it can and cannot express

**cargo's `reason` discriminator.** Tier A expresses the record half: a filter on `reason`, a
level that drops `failure-note`, the primary span, the first `help` or `note` child, and
`build-finished` as the completeness rule. It cannot express the text half, which
`parsers/cargo.rs:read` (57) handles. Text between records goes to the shared text parser, so an
`error[...]` line pairs with the `-->` line under it. A panic header collects its message lines
(`PANIC` at `cargo.rs:29`, `ends_panic` at `cargo.rs:116`). `rust` needs that text half for
build-script panics and doctest errors (`fixtures/cargo/check-buildpanic.*` and
`test-doctest.*`). `rust-test` needs it for every test failure, because libtest reports a failure
only as panic text. Tier B expresses both.

**Stylelint's parent-held `source`.** Tier A expresses it. Group scoping gives each warning the
`source` of its result. A second record rule turns `invalidOptionWarnings` into project-wide
errors. The input rule reads stderr unless it is blank. Stylelint repeats an invalid option on
every result, and the shared dedup in `parse` (`parsers.rs:209`) still collapses the repeats.

**Terraform's `summary` and `detail`.** Tier A expresses it with the join transform. A plain
template would add a trailing newline whenever `detail` is empty, which changes the message.

| Verdict | Kinds |
|---|---|
| Tier A expresses them (23) | `golangci`, `ruff`, `pyright`, `pylint`, `eslint`, `biome`, `deno-lint`, `stylelint`, `rubocop`, `phpstan`, `psalm`, `swiftlint`, `credo`, `shellcheck`, `hlint`, `terraform`, `tflint`, `actionlint`, `go`, `staticcheck`, `mypy`, `flake8`, `dart` |
| Depends on an unrecorded output format (2) | `ty`: [INFERENCE] its default output is the two-line `error[...]` and `-->` form, which needs the header rule. `oxlint`: [INFERENCE] its default output is a multi-line graphical report. A one-line output flag would put it in Tier A. |
| Tier B only (3) | `rust`, `rust-test`, `go-test` |

For the 16 unrecorded kinds, expressible means that someone can write a definition after they
record the tool's real output. It does not mean that a definition can match today's behavior,
which nobody has recorded.

### Trust and failure

- No third-party code runs, because a definition is data.
- Evaluation must be total. The language has no recursion and no loops, the `regex` crate matches
  in linear time, and the input has a size cap.
- jq is the tempting alternative. The workspace already builds jaq for the shell's `jq` builtin
  (`crates/shell-builtins/src/jq.rs`), and jq can express all 28 kinds. But a jq program can loop
  forever, and the builtin checks cancellation only between inputs and between outputs
  (`jq.rs:485-495`). Nothing can kill a thread, so a looping definition would hold its thread until
  the process exits. A jq evaluator needs a process boundary, which turns it into option 2 with a
  bundled interpreter.
- A definition that fails to load makes its format unavailable, and the load reports every compile
  error. A record that matches but has no message field makes the output unreadable. A failed
  completeness rule does the same. Each case degrades through `unparseable` and never reads as
  clean.

### How many kinds can migrate

Tier A expresses 23 kinds, or 25 if `ty` and `oxlint` record as one-line formats. Tier B expresses
all 28. Twelve kinds can be checked against a failing-run recording.

### Maintenance

- Core keeps the language and its evaluator. A tool shape that the language cannot express comes
  back to core as a language change, which is the pressure that ruling 1 removes.
- The model for this option did not cover its whole tool set. Upstream's minimizer keeps 24
  per-tool Rust filters (`docs/adr/TASKS.md:94`) beside its 32 definitions (REPORTED). The port in
  `crates/minimizer/src/filters/` has 15 per-tool filter modules beside 7 definitions. OBSERVED.
  [INFERENCE] Diagnostics would meet the same limit.
- The minimizer's line-filter language takes about 600 production lines
  (`crates/minimizer/src/pipeline.rs` 1-605). OBSERVED. An extractor with constructs 1 to 7 is
  larger than the 402 lines of dedicated per-tool modules that it replaces. [GUESS] The walker and
  the text parser stay in core for `Generic`.

## Option 4: no registry

Keep the 28 per-tool kinds in core, and make the recording rule a test instead of a habit.

### Core changes

| `file:symbol` | Change |
|---|---|
| The tests in `crates/driver/src/cleanse/parsers.rs` | An exhaustive `match` over `ParserKind` maps each variant to its recordings and expected diagnostics, so a new variant fails to compile until it has a row. A table test loads each recording and asserts the full `Vec<Diagnostic>`. |
| `fixtures/<tool>/` | Add a failing-run recording and a `PROVENANCE.txt` for each of the 16 unrecorded kinds, in the shape of `fixtures/sarif/PROVENANCE.txt`. |
| Nothing else | `ParserKind` stays closed. |

### What it can and cannot express

Rust code can express all 28 kinds, and they stay where they are. No third party can register a
format.

### Trust and failure

Trust and failure do not change. No third-party code runs, and the typed `ParseError` outcome from
#516 stays as it is.

### How many kinds can migrate

None migrate. This option is the baseline that the other three are priced against.

### Maintenance

- Each new tool or output change is a core pull request with a recording. golangci-lint version 2
  rejecting `--out-format` is one observed output change (#516).
- #518 recorded the clean runs of ten tools in one pull request. A failing run also needs a project
  that triggers diagnostics, like the `sources/` trees under `fixtures/sarif` and `fixtures/tap`.
- Option 4 does not meet row N3 of `docs/adr/TASKS.md`, "an extension may register a format", and
  it conflicts with ruling 1.
- Its recording half is not wasted under any choice. It is the evidence that each other option
  needs to show that a migration kept its output, and it is the practice that found all eleven
  defects above.

## What would change my mind

1. **Dimension 1 does not adopt the embedded-Python host, or Phase 0 changes 1 and 3 do not land
   for it.** Option 1 would then pay for those fixes alone and would have the largest bill. I would
   choose option 3 at Tier B, delivered as extension content rows. It needs no host and no Phase 0
   work, and it runs in process.
2. **The extension host is slow to start in the cleanse composition.** If a measured start adds
   more than about one second to a cleanse run, option 3 serves users better, because it runs in
   process. Nobody has measured that start.
3. **The step-1 recordings move most of the 18 kinds to SARIF, JUnit XML or TAP.** The first-party
   load would then be a few kinds, and the registry would exist mainly for third-party formats. The
   cheapest carrier for that load is option 3 at Tier A.
4. **The owner rules that a stdin and stdout parser protocol is not a second extension language
   under ADR 0036, and wants cleanse free of the Python host.** Option 2 would then win. It is the
   only option that moves all 28 parsers out unchanged, with their tests.
5. **Route T cannot be kept away from the model, and dimension 1 builds no other route.** Option 1
   would then put a parser tool in front of the model in every session that loads the extension.
   I would build Route C for formats alone, or switch to option 3.

## Questions for the owner

These questions scope ruling 1. They do not reopen it.

1. **Is `Generic` in scope?** Its walker and text parser hold keys and forms learned from specific
   tools (see [What the 28 kinds are](#what-the-28-kinds-are)). If that counts as per-tool
   parsing, `pytest`, `luacheck`, `clang-tidy`, `dotnet-build`, `zig-build`, `gradle-check` and
   `maven-verify` lose their parser too, and the seven text kinds cannot fold into `Generic`.
2. **Do the `FAMILIES` rows move out with their formats?** This document prices formats only.
3. **Does a first-party format package that the installer adds by default count as outside core?**
   If it does, out-of-the-box checking survives under options 1, 2 and 3.
4. **Does ruling 1 reach the minimizer?** Its per-tool Rust filters in
   `crates/minimizer/src/filters/` have the same shape as the per-tool `ParserKind` variants. This
   document does not extend the ruling to them.

## How to recount

Run these commands from the repository root at `230efc0df8`.

```sh
# 32 ParserKind variants: 28 per-tool kinds plus JUnit, Sarif, Tap and Generic
sed -n '73,139p' crates/driver/src/cleanse/parsers.rs | grep -cE '^\s+[A-Z][A-Za-z0-9]+,$'
# The dispatch arms: dedicated modules, the 17 JSON kinds, the Generic arm
sed -n '162,199p' crates/driver/src/cleanse/parsers.rs
# The five per-kind branches outside the dispatch match
grep -n 'kind == ParserKind\|matches!(kind' crates/driver/src/cleanse/parsers.rs
# 36 built-in families and the parser each one names
grep -o 'parser:     ParserKind::[A-Za-z0-9]*' crates/driver/src/cleanse/checkers.rs | sort | uniq -c
# Recorded fixtures per tool
ls fixtures
# Production lines of the dedicated modules: test modules start at 225, 101 and 79
grep -n '^#\[cfg(test)\]' crates/driver/src/cleanse/parsers/{cargo,stylelint,terraform}.rs
```

## Sources

- Commit `230efc0df8` (#516): the eleven defects, the typed parse outcome, and the 32 variants in one
  exhaustive match.
- Pull request [#518](https://github.com/blyzer/oh-my-pi/pull/518), open: clean-run recordings for
  ten JSON checkers and the SwiftLint and Credo defects.
- [ADR 0040, "Phase 0: out-of-core host feasibility"](../adr/0040-remote-build-and-test-execution.md):
  the trusted-route timing, the cancel and deadline results, and the four required changes.
- [The manifest tool declaration defect](../audits/manifest-tool-row-defect.md): the `omp.toml`
  identity mismatch.
- [ADR 0036](../adr/0036-python-for-extensions.md): Python is the only extension language, and
  extension hosts are killable children.
- [The output pipeline ledger](../adr/TASKS.md): rows N1 to N3 and the open admission decision.
- `fixtures/sarif/PROVENANCE.txt` and `fixtures/tap/PROVENANCE.txt`: the recorded standard-format
  runs and their commands.