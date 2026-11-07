# BuildBuddy out of core: the crate line, the one in-core adapter, and repository pins

Status: design proposal for ADR 0040 dimension 2. Nothing here is implemented or authorized.
Date: 2026-11-05
Baseline: `origin/omp2` at `230efc0df8`.
Decided input: the owner's dimension-1 ruling, recorded in
[ADR 0040's 2026-11-05 amendment](../adr/0040-remote-build-and-test-execution.md#amendment-2026-11-05-vendor-integration-leaves-core).

Evidence keys:

- **OBSERVED** means read in code or docs at the baseline. Citations use `path:line` or
  `path:symbol`.
- **[INFERENCE]** means the author's reasoning. Every size estimate is an inference.
- **UNVERIFIED** means no run or source confirmed it.

No build, test, remote run, or measurement was made for this document.

## The ruling and the questions it leaves

ADR 0040's owner question has three dimensions. The owner ruled on two of them:

- Dimension 1, code location, is decided. BuildBuddy does not stay in `omp2`. Core keeps only the
  generic, extensible ground. Vendor integration moves outside as an extension.
- Dimension 2, what ships in the binary, is to be built as needed. Its shape is an engineering
  call.

Dimension 3, how an out-of-core backend is reached, was answered by Phase 0 (PR #496). No existing
host carries such a backend as it stands.

This document answers three questions that follow from the ruling:

1. Which execution crates are generic ground, and which are vendor integration?
2. What must the binary carry so that a backend outside it can run?
3. What generic ground replaces `repo_pins`, which makes remote runs work only in this repository?

The amendments to ADR 0040 and ADR 0047 record the ruling itself.

**Scope.** "BuildBuddy" here means BuildBuddy code in omp's crates. The repository also uses
BuildBuddy as CI infrastructure through the `bb` CLI:

- the required job "Remote Linux verification (BuildBuddy)" (`.github/workflows/ci.yml:783`);
- `.github/actions/buildbuddy-client`;
- the `remote` recipes in `justfile` (lines 122 to 296);
- `scripts/remote-verify.sh` and `scripts/remote-verify-runner.sh`.

None of that links into `omp`. ADR 0040 already calls `bb remote` "an operator tool, not omp
product code". This document does not move that tooling. It lists it so the owner can rule on it
separately.

## The line falls inside `omp-exec-reapi`, not between crates

Four of the five crates sort cleanly. The fifth, `omp-exec-reapi`, is generic ground with a vendor
seam inside it.

| Crate | Verdict | Evidence (OBSERVED) |
|---|---|---|
| `omp-exec` | Generic ground, with one adapter that is specific to this repository | Depends only on `omp-adw`, `omp-core`, `serde`, `strum`, and `thiserror`, and names no vendor. `crates/exec/src/cargo_just.rs:1` describes `CargoJust` as "the adapter for this repository's `just` recipes". `RepoPins` (`adapter.rs:13`) holds this repository's four pins, and `BuildSystemAdapter::plan` (`adapter.rs:61`) takes that struct. |
| `omp-exec-local` | Generic ground | Wraps `omp_envd::exec::ExecHost` (`crates/exec-local/src/lib.rs:12`). ADW and cleanse compose it (`crates/driver/src/adw/production.rs:272`, `crates/driver/src/cleanse/production.rs:294`). |
| `omp-exec-ssh` | Generic ground with no composer | SSH is an open protocol, reached through `omp_envd::ssh::SshService` (`crates/exec-ssh/src/lib.rs:7`). No workspace crate depends on it. PR #486 (`f796e0b78f`) removed it from `crates/driver/Cargo.toml` after ADR 0040's status was last verified at `1578b1204c`. |
| `omp-exec-reapi` | Generic ground with a vendor seam | REAPI v2 and the googleapis packages it imports are open, published protocols (`crates/exec-reapi/README.md`). Three BuildBuddy items sit in the crate. The next section lists them. |
| `omp-exec-buildbuddy` | Vendor integration, the whole crate | A client for BuildBuddy's REST Run API that sends `x-buildbuddy-api-key` (`crates/exec-buildbuddy/README.md`). No workspace crate depends on it. Outside its own directory, only the root `Cargo.toml:140` names it. Its only `RunInputSource` is `PreparedInputSource` (`lib.rs:66`), so no product path builds its input. |

The dependency facts that ADR 0040 records hold at the baseline. `omp-exec-reapi` depends on
`omp-exec`, `omp-adw`, `omp-core`, and `omp-observability`. `omp-exec-buildbuddy` depends on
`omp-exec`, `omp-adw`, `omp-core`, and optionally `omp-exec-reapi`. Neither depends on an envd,
driver, session, journal, or agent crate. `omp-exec-local` and `omp-exec-ssh` reach into envd.
That fact decides where their code can live, not which side of the vendor line they are on. Local
and SSH execution are generic, and they stay.

### Three BuildBuddy items keep `omp-exec-reapi` from splitting cleanly

The protocol line is clean. REAPI is published, and the Run API is BuildBuddy's own. The code line
is not clean, because the REAPI crate carries three BuildBuddy items:

1. A named vendor profile. `Profile::buildbuddy()` (`crates/exec-reapi/src/profile.rs:87`)
   hard-codes the `x-buildbuddy-api-key` header, the `Arch` and `amd64` spelling
   (`ArchSpelling::Short`, documented as "BuildBuddy's vocabulary" at `profile.rs:48`), and the
   fixed property `workload-isolation-type=oci`. The crate README says "Nothing that names one
   service lives in the client". The profile is the exception.
2. The vendor cancel RPC. The `buildbuddy-cancel` feature (`crates/exec-reapi/Cargo.toml:16`)
   compiles four vendored protos under `proto-buildbuddy/` (`build.rs:30-31`),
   `src/buildbuddy_cancel.rs`, and a `buildbuddy_canceller` field on `Config`
   (`execute.rs:69-72`) and on the backend's inner state. `ReapiRun::cancel` tries that client
   before the standard `Operations.CancelOperation`.
3. A dependency back into the crate. `omp-exec-buildbuddy` uses `omp-exec-reapi` only for the same
   canceller (`crates/exec-buildbuddy/Cargo.toml:15`).

Without those three items the crate is REAPI v2 plus googleapis. [INFERENCE] The split is then
clean. At the baseline, the only other file in the crate that contains `buildbuddy` is the README,
which describes these items.

### Vendor code also lives outside the five crates

- `crates/driver/Cargo.toml:51` enables `buildbuddy-cancel` for every `omp-driver` build, and
  `omp-app` depends on `omp-driver` (`crates/app/Cargo.toml:68`). [INFERENCE] Every `omp` binary
  therefore compiles the BuildBuddy cancel client. No binary was inspected.
- `crates/driver/src/adw/remote.rs` branches on the vendor name three times. `remote_profile` maps
  the string `"buildbuddy"` to `Profile::buildbuddy()` (`:182`). `validate_cache_policy` refuses
  the action cache for that string (`:199`). `run_remote` connects `BuildBuddyCanceller` when the
  profile name is `"buildbuddy"` (`:487-491`). ADR 0040 Part A.3 says "No other code branches on a
  vendor name".
- `REMOTE_POSTURE` (`remote.rs:110`) claims `NetworkScope::Disabled` for every remote backend. Its
  doc comment grounds the claim in "BuildBuddy's Firecracker worker" (`:109`), and the generic
  `reapi` profile inherits it. ADR 0039 B5 asks for what a backend enforces, not what it intends.
- `crates/e2e/tests/p13_remote_exec.rs:235` sets `buildbuddy_canceller: None`. [INFERENCE] It
  compiles because `omp-e2e` also depends on `omp-driver` (`crates/e2e/Cargo.toml:42`), and
  feature unification turns the feature on.

### What stays and what moves

These stay in core as generic ground:

- `omp-exec`, with `CargoJust` and `RepoPins` reworked as "What replaces `repo_pins`" describes.
- `omp-exec-local`.
- `omp-exec-reapi` without the three BuildBuddy items. `Profile` stays as a data shape. Its values
  come from operator cfg or from an extension, never from a named constant.
- `omp-exec-ssh`. It is generic, but nothing composes it. Whether to wire it or delete it is a
  separate decision that this ruling does not make.

These move into the BuildBuddy extension:

- `omp-exec-buildbuddy`, the Run API backend.
- `buildbuddy_cancel.rs` and `proto-buildbuddy/`.
- The BuildBuddy profile values, as data.

The vendor-name branches in `crates/driver/src/adw/remote.rs` are deleted, not moved. Profile data
replaces them.

## A static trait means the binary carries one concrete adapter

`ExecutionBackend` (`crates/exec/src/backend.rs:125`) has a `Clone` supertrait, an associated
`type Run`, and `impl Future` returns. It is not dyn-compatible. A backend that is not compiled
into the binary cannot implement it. So the binary must carry one concrete type that implements
`ExecutionBackend` and delegates each operation outward. This document calls that type
`DelegatedBackend`. Vendor code sits behind it.

The same constraint shapes reattach. The module documentation says "the composing crate holds one
enum over its concrete backends and matches on it" (`backend.rs:3`). No such enum exists. The
driver builds `LocalBackend` and `ReapiBackend` where it uses them (`adw/production.rs:272`,
`cleanse/production.rs:294`, `adw/remote.rs:523`). `reattach_execution`
(`crates/agent/src/jobs.rs:1247`) waits for "`ExecutionBackend` trait objects", which the trait's
design rules out. The enum is the answer the design already names. ADR 0040's gap item 2 needs it
whether or not anything moves out, and `DelegatedBackend` becomes one more arm.

## Both vendor backends already work in short calls

The cost of the adapter depends on how long one delegated call must live. Phase 0 measured the
embedded-Python host on both sides of that line (ADR 0040, "Phase 0"). Short request and response
calls worked. Structured JSON came back in 41 ms, and the payload arrived intact. Long invocations
failed. Closing the connection cancelled the body, and re-invoking the same ID started it again.

Neither vendor backend needs a long call. Each one keeps the run on the service and gives it a
durable identity:

- The Run API backend posts `/api/v1/Run` and receives an invocation ID. It then polls
  `GetInvocation` every 500 ms and pages `GetLog` (`crates/exec-buildbuddy/src/run.rs:71`, `:94`,
  `:182`).
- REAPI returns an operation name, and `WaitExecution` resumes it. `ReapiBackend` already reports
  `ResumeSupport::Reattach` (`crates/exec-reapi/src/execute.rs:192`).

So the delegate contract can be four short operations. Core owns the loop, the journal record, and
the reattach handle.

| Operation | Returns | Safe to repeat | Accepted-action rule |
|---|---|---|---|
| `describe` | `BackendCapabilities` | Yes | Not involved |
| `start(request, inputs)` | An opaque operation identity, once the service has accepted the run | No | If the reply is lost after the call left core, record EffectsUnknown. Never send a second `start`. |
| `poll(operation, cursor)` | The state, a bounded output page, the next cursor, and the result once settled | Yes | Not involved |
| `cancel(operation)` | A `CancelOutcome` | Yes | Not involved |

Reattach is `poll` on the stored operation, so nothing on the delegate side has to survive an omp
restart. The operations carry typed serde structs with a revision, following the AGENTS.md rule for
typed wire structs. `inputs` refers to inputs that envd captured. The delegate never gets
filesystem authority, which matches the Run API backend's contract today ("The backend never reads
the checkout", `crates/exec-buildbuddy/README.md`).

Two facts limit the Run API whatever carries it:

- Its reattach still waits on facts that the 2026-10-31 amendment lists as UNVERIFIED:
  client-supplied idempotency and invocation lookup.
- No product code builds its input. `RunInput` (`crates/exec-buildbuddy/src/lib.rs:43`) needs a
  repository URL, a base commit, and git patches. envd's manifest diff
  (`crates/envd/src/workspace/operations.rs:1426`) is an internal format for worktree recovery,
  not a git patch. ADR 0047 decision 3 already plans a git-semantics `CaptureInputs` in envd.

## Three shapes for the adapter, costed

Every shape ships one concrete in-core adapter. The shapes differ in where vendor code runs and in
what core must change first. Phase 0 numbered the core changes that its pivot routes need, and this
section reuses the numbers:

1. Make `native.rs:lower_manifest` and the Python validator agree on the `omp.toml` tool row.
2. Add invocation reattach that outlives the connection.
3. Send `CancelDispatch` on deadline expiry and expose the deadline to `ctx`.
4. Add an effect class that maps to `env.process`.

### Shape A: an erased in-process bridge keeps the vendor in the binary

An object-safe mirror of the trait, with boxed `start`, `reattach`, and `cancel` futures, sits
behind one adapter that implements `ExecutionBackend`. AGENTS.md forbids boxing per message, so
events cross a channel and `next_event` does not box. The bridge needs none of the Phase 0
changes. [INFERENCE] It adds one trait and one adapter, near the size of
`crates/exec-ssh/src/lib.rs` (243 lines), plus a build-time seam that injects backends into the
driver.

Shape A does not satisfy the ruling. The bridge erases types, not linkage. omp has no Rust plugin
loader, and Rust has no stable ABI to build one on. ADR 0036 puts every extension on one embedded
CPython runtime, so a native plugin runtime would need a record that supersedes it. The vendor
crate must still be linked, so it ships in every binary that links it.

### Shape B: a long-lived program under envd pays for state the service already holds

Core starts an operator-installed vendor program as a named process (`StartProcess`,
`crates/proto/proto/omp/env/v1/env.proto:515`). Core writes typed frames to the program's stdin
(`SendInput`, `:616`) and reads typed frames from its output (`AttachOutput` with
`after_sequence`, `:583`). Cancel is a cancel frame, then `StopProcess` with a grace period
(`:633`).

Phase 0 named this supervisor as the candidate carrier. A new connection attached by name and
replayed 20 ticks and `Exited(0)`, and `stop_process` delivered TERM. Survival across an omp
restart was not tested. Extensions cannot reach the supervisor, because
`Grants::from_effect_envelope` (`crates/envd/src/policy.rs:110`) never emits `env.process`. Core
must therefore launch the program, or Phase 0 change 4 must land first.

The cost is a framed protocol, a reattach handle that names the process generation, and a rule
that `Accepted` reaches the journal before a rotated output log (`OutputAttached.rotated`) can drop
it. The program holds the run for its whole life, although the service already holds it under a
durable ID. [INFERENCE] Shape B is the most core work for the least gain. The program is also an
external executable, like an MCP stdio server, and not an extension in ADR 0036's sense.

### Shape C: a poll-shaped delegate behind one adapter

`DelegatedBackend` implements `ExecutionBackend`. Its run is a loop in core over the four short
operations. The delegate keeps no state between calls. Two carriers can serve the operations. They
share the schema, the adapter, and the journal path.

#### C1: a Python extension carries the operations

The operations are extension declarations, called through the extension host. Phase 0 found this
call shape working. C1 needs no invocation reattach (change 2) and no `env.process` class (change
4), because no call outlives its reply. It still needs this core work:

1. A supported admission route, which is Phase 0 change 1. Registration worked only through the
   hidden `--trusted-extension` flag. The documented `omp.toml` route fails at the tool identity
   gate, because `ToolManifestEntry` (`crates/ext/src/config.rs:1405-1420`) has no `family`,
   `name`, or `rev` field.
2. Cancellation on deadline for `start`, which is Phase 0 change 3. Phase 0 saw a body finish at
   4.006 s after the caller's 1500 ms deadline had aborted the call. `DispatchRouter::expire`
   (`crates/envd/src/exthost/dispatch.rs:1500`) answers the waiter without telling the host. For
   `poll` and `cancel` that is harmless. For `start` it can submit a run after core gave up on the
   call. Core records EffectsUnknown, as the accepted-action rule requires, but the run is real.
3. A callable route that the model cannot reach. `is_model_callable`
   (`crates/tool/src/registry.rs:112`) returns `true` for all three `ToolRoute` variants. A backend
   operation carries a credential and spends money, so it needs a new route or a new declaration
   kind that core can call and the model cannot.
4. An entry point that the driver can call. `PyCallbackRoute::dispatch`
   (`crates/envd/src/exthost/extensions.rs:1715`) is `pub(crate)`. envd must expose one call for
   the adapter.
5. A caller that has an extension host. ADW and cleanse attach with `py_eval: false` and no trusted
   extensions (`crates/driver/src/adw/production.rs:165-176`,
   `crates/driver/src/cleanse/production.rs:183-189`). The headless kernel attaches its configured
   extensions (`crates/driver/src/headless/kernel.rs:2687-2697`). ADR 0040's gap item 1 already
   moves the execution producer to a place that a kernel can build, so this work is shared.
6. Credential and egress paths. The extension needs the vendor key and network access to one host.
   `docs/py/00-overview.md:121` specifies credential requests on the CONTROL channel, and
   `Grants::from_effect_envelope` can grant `env.net` (`policy.rs:132`). Neither path was
   exercised for this use (UNVERIFIED).

The vendor side stays small. [INFERENCE] The Run API is REST and JSON, so a Python client needs no
third-party package once item 6 gives it an egress path. Invocation cancel is a gRPC call
(`BuildBuddyService.CancelExecutions`). From Python it needs a native wheel, which ADR 0036
decision 4 permits. Without one, the extension reports `CancelSupport::None`.

#### C2: a one-shot program carries the operations

The adapter runs an operator-installed program once per operation through envd's exec authority,
the same `ExecHost` path that `omp-exec-local` uses. C2 needs no extension-host change. It has two
gaps. `ExecRequest` (`env.proto:340`) has no stdin field, and the credential must not travel in
`Script.text`, so the request needs a stdin field or the named-process `SendInput` path. The
program is also an external executable, not an extension in ADR 0036's sense.

### Rejected: a local REAPI shim

An extension could run a local server that speaks REAPI to core and the Run API to BuildBuddy.
`docs/py/13-inference.md` uses a local proxy the same way for inference protocols that omp does not
speak. Core would then need no new adapter. The shim is rejected for three reasons. REAPI uploads a
content-addressed tree, while the Run API takes a commit and git patches, so the shim would have to
rebuild patches from a tree. Starting its server needs Phase 0 change 4. And the extension would
implement a REAPI server (CAS, ByteStream, `Execute`, and `WaitExecution`) to save core one small
adapter.

### The shapes side by side

| | A | B | C1 | C2 |
|---|---|---|---|---|
| Vendor code runs in | the `omp` process | a long-lived program | the extension host | a program run once per operation |
| Vendor code ships in the binary | Yes, if linked | No | No | No |
| Phase 0 changes needed | None | 4, if an extension launches the program | 1 and 3 | None |
| Other new core work | A build-time injection seam | A framed protocol, generation-aware handles, log-rotation handling | A non-model route and an envd entry point | A stdin field on `ExecRequest` |
| An extension in ADR 0036's sense | No | No | Yes | No |

## Recommendation: build shape C1

The ruling names an extension, and ADR 0036, which is accepted, defines an extension as Python on
the embedded runtime. C1 uses the one extension-host behavior that Phase 0 found working. Its two
Phase 0 prerequisites, changes 1 and 3, are fixes that any extension-provided capability needs, so
they are not costs unique to execution. [INFERENCE] The cleanse format registry's open carrier
question (`docs/adr/TASKS.md:93`) depends on the same short-call route.

The pattern is not new. `docs/py/13-inference.md` ("Providers are data; code is the cold path
only") sorts provider extensions into three classes. Class (a) is pure data. Class (b) is data plus
cold-path Python that core calls over CONTROL, and no Python runs on the token path. Class (c) is a
proxy process for a foreign wire protocol. The BuildBuddy REAPI profile is class (a). The Run API
delegate is class (b), because core owns the run loop and calls Python only for short operations.
The rejected shim is class (c).

### What the binary carries

This list is the dimension-2 answer:

1. `omp-exec`, `omp-exec-local`, `omp-exec-reapi` without the vendor items, and `omp-exec-ssh` if
   someone wires it.
2. `DelegatedBackend` and the four-operation schema, in `omp-driver`, which already composes
   network services (ADR 0040 Part A.3).
3. One envd entry point that calls an extension operation for the adapter.
4. The composition enum over `Local`, `Reapi`, and `Delegated` arms, keyed by `BackendId` from
   operator cfg. Placement, the execution-job producer, and `reattach_execution` all use it.
5. Operator cfg for REAPI profile values: the credential header, the platform property spellings,
   and the fixed properties. These values replace the two names that `sv_exec_remote_profile`
   accepts today (`crates/driver/src/adw/remote.rs:81`).

### What the BuildBuddy extension carries

1. A REAPI profile as data for core's generic backend. It holds the header, the property
   vocabulary, `workload-isolation-type=oci`, and the action-cache restriction that
   `validate_cache_policy` hard-codes today.
2. The Run API backend as the four operations, moved from `omp-exec-buildbuddy`.
3. Optionally, invocation cancel as a native wheel.

### What it costs

The sizes are inferences from files of the same shape at the baseline.

| Item | Size [INFERENCE] | Needed even if nothing moved out? |
|---|---|---|
| Composition enum | 100 to 150 lines in `omp-driver` | Yes, ADR 0040 gap item 2 |
| Execution-job producer | Not sized here | Yes, ADR 0040 gap item 1 |
| `DelegatedBackend` and its schema | Near `crates/exec-ssh/src/lib.rs` (243 lines) | No |
| envd entry point | Small, because it exposes the existing `pub(crate)` call | No |
| Phase 0 changes 1 and 3 | Not sized here | No, but every extension capability needs them |
| Non-model route | Unknown until a route is designed | No |
| Profile values from operator cfg | Small. It replaces two constants and three branches. | No |
| Removal of the vendor items | Deletion only | No |
| Git-patch capture in envd | Not sized here | Yes, for any Run API path |

### What gets worse, and how to fix it

REAPI on BuildBuddy loses invocation cancel when the vendor client leaves `omp-exec-reapi`.
Without the vendor client, `ReapiRun::cancel` sends only the standard `CancelOperation`. ADR 0040's
"BuildBuddy coverage of REAPI" table found no `CancelOperation` on BuildBuddy, and the backend maps
`UNIMPLEMENTED` to `CancelOutcome::Unsupported`. The action timeout then bounds the leaked remote
work. That timeout is 30 minutes by default (`crates/exec/src/cargo_just.rs:89`).

To keep cancel, the `Reapi` arm of the composition enum can fall back to the extension's `cancel`
with the run's `ExecutionId`. That ID is already the standard `RequestMetadata.tool_invocation_id`
(`crates/exec-reapi/src/execute.rs:add_request_metadata`), and BuildBuddy keys invocation cancel on
it. The fallback keeps `omp-exec-reapi` free of vendor code. [INFERENCE] It is a few lines in one
match arm.

### Decisions left to the owner

No experiment settles these product calls:

1. Does "outside as an extension" mean ADR 0036's Python extension (C1), or does it also admit an
   operator-installed program (C2)? The adapter and the schema are the same either way. The carrier
   decides which core work comes first.
2. Should REAPI on BuildBuddy keep invocation cancel through the enum fallback, or accept a cancel
   that the action timeout bounds?
3. Does the ruling also cover the repository's CI use of BuildBuddy through `bb`? This document
   assumes that it does not.

## What replaces `repo_pins`

### `repo_pins` binds remote runs to this repository

`repo_pins` (`crates/driver/src/adw/remote.rs:205`) reads three files and needs four values:

- `channel` from `rust-toolchain.toml`;
- the `tool: nextest@` line from `.github/workflows/ci.yml`;
- `TAG=` and `VER=` from `crates/py/scripts/fetch-python.sh`.

In any other repository a file read fails, or parsing fails with "toolchain pins missing". The
values fill `RepoPins`
(`crates/exec/src/adapter.rs:13`). `CargoJust::toolchain` (`cargo_just.rs:107`) turns them into
`rust:<channel>+nextest-<version>+cpython-<version>-<tag>`.

The pins do not detect drift. `run_remote` keys its one-entry toolchain map with
`CargoJust::toolchain(&pins)`, computed from the same `pins` that planned the request
(`remote.rs:510`). The lookup in `ReapiBackend` (`execute.rs:490`) therefore always hits, whatever
image the operator pinned. The real binding between pins and image is the image build.
`scripts/exec-image/Dockerfile` reads `rust-toolchain.toml` and runs `fetch-python.sh`, and
`scripts/exec-image/README.md` says to rebuild the image when those pins change.

Remote placement depends on this repository in three more places:

- `CARGO_JUST_RECIPES` (`cargo_just.rs:55`) classifies this `justfile`'s recipes. Its unit test
  reads the `justfile` at compile time (`cargo_just.rs:207`).
- `run_remote` rewrites `argv[0]` to `/usr/local/bin/just` and sets a `PATH` that match one image
  (`remote.rs:444`, `:516`).
- `run_remote` targets `Platform::LINUX_X86_64` (`remote.rs:434`, `:506`) and asserts
  `REMOTE_POSTURE`.

### Generic ground that replaces it

None of the following is BuildBuddy-specific, so under the ruling it is core's generic ground:

1. Pins owned by each adapter. `BuildSystemAdapter::plan` takes the adapter's own pin type instead
   of one struct with this repository's fields. The Cargo adapter reads `rust-toolchain.toml`,
   which rustup defines for any Rust project. envd reads the file and the adapter parses it, so
   planning stays pure.
2. Declared toolchain files instead of scraped lines. A pin that no build system standardizes,
   here the nextest version and the CPython bundle, enters the toolchain ID as the content digest
   of a file that the project declares. Core then parses no CI YAML and no shell variable. A
   changed file yields a new ID.
3. An operator map from toolchain ID to a digest-pinned image. `ExecuteConfig::toolchains` already
   has that shape. The map must come from operator cfg, not from the request's own pins.
   `ReapiBackend` must also refuse, before acceptance, a request whose toolchain has no image.
   Today a miss sends no image property (`execute.rs:514`).
4. Recipe classes as project data. The `just` adapter reads its recipe classes from a project file
   instead of `CARGO_JUST_RECIPES`. A recipe with no class stays `LocalPreferred`, as
   `CargoJust::plan` already does. Eligibility stays advisory, because `sv_exec_policy` remains
   the operator's decision.
5. Image facts in the image entry. The program path and `PATH` belong in the operator's
   `Toolchain` entry, which already carries `env`.
6. Platform and posture from the backend. The target platform and the enforced posture come from
   the backend's operator cfg and its `BackendCapabilities`, not from constants that describe one
   vendor's workers.

## Order of work if this is built

Each step ends in a check and leaves core coherent. Steps 1 to 3 remove vendor code. Steps 4 to 6
build the generic ground.

1. Make REAPI profile values operator data. Delete `Profile::buildbuddy`, the three `"buildbuddy"`
   branches in `remote.rs`, and the vendor-named `RemoteError` variants. BuildBuddy's REAPI
   endpoint keeps working, configured as data. Check with the `omp-exec-reapi` and `omp-driver`
   tests.
2. Delete the `buildbuddy-cancel` feature: `buildbuddy_cancel.rs`, `proto-buildbuddy/`, the
   `build.rs` branch, the cfg-gated fields, the feature request at `crates/driver/Cargo.toml:51`,
   and the P13 field. Check that no `Cargo.toml` outside `crates/exec-buildbuddy` names a
   BuildBuddy feature.
3. Move `omp-exec-buildbuddy` out of the workspace into the extension's repository. Nothing
   depends on it, so no product path breaks. Check that the root `Cargo.toml` names no BuildBuddy
   crate.
4. Add the composition enum, the execution-job producer, and real reattach. These are ADR 0040's
   gap items 1 and 2, needed in any case.
5. Land the extension-host prerequisites: Phase 0 changes 1 and 3, the non-model route, and the
   envd entry point. Then add `DelegatedBackend` as an enum arm, with the cancel fallback.
6. Replace `repo_pins` with the generic pins above.

Step 2 leaves REAPI cancel on BuildBuddy bounded by the action timeout until step 5 adds the
fallback. If that window matters, run step 2 after step 5.

## References

- ADR 0040: "Owner question", "Phase 0", Part A, "Status in omp", "BuildBuddy coverage of
  REAPI", and the amendments of 2026-10-10, 2026-10-31, and 2026-11-05.
- ADR 0047, "Current dispatch and extension constraints" and its 2026-11-05 amendment.
- ADR 0036 (embedded Python for extensions), ADR 0039 B5 (enforced, not intended), ADR 0046
  (extensibility without centralization).
- PR #496 (`fd68bd20f2`), the Phase 0 verdict. PR #486 (`f796e0b78f`), which removed
  `omp-exec-ssh` from `crates/driver/Cargo.toml`.
- `docs/adr/TASKS.md`, `docs/py/00-overview.md`, `docs/py/13-inference.md`,
  `scripts/exec-image/README.md`.
