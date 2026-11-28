# OMP2 extension infrastructure audit (stage 1)

> Part of the [OMP2 gateway-capabilities research](../README.md). Evidence file for deliverable 7 (ABI-independent extension assessment). Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Scope: how OMP2 loads, admits, supervises and distributes extensions today, which of the owner's section 12 properties (dimension IDs X01 to X11) already exist, and whether any C ABI plugin mechanism exists anywhere in the workspace. Structured rows are in [`omp2-extensions.tsv`](omp2-extensions.tsv). Maturity labels follow [`01-method-and-dimension-ids.md`](01-method-and-dimension-ids.md). Paths are relative to the repository root at `85d219ae7f`.

## 1. Binding constraints found before reading code

| Constraint | Source | Consequence for this research |
|---|---|---|
| "Extensions/eval: embedded free-threaded CPython only, no JS/TS plugin runtime, no multi-language eval" is a Locked Deviation from pi | `AGENTS.md:526-532` | A Wasm component host, a native or Go executable extension protocol, or any non-Python extension runtime contradicts it unless framed as an external integration behind a `Delegated` or MCP-like boundary, or as `Requires ADR reconsideration`. |
| ADR 0036 decision 2: "Adding a second extension language requires superseding this record"; decision 5: every extension host is a killable child process | `docs/adr/0036-python-for-extensions.md:40-41`, `:64-65` | Python stays the only extension language. The 2026-11-09 amendment repeats it ("Python stays the only extension language"). `0036:91` |
| ADR 0046 "not recovered": extension-implemented stream functions or codecs, dynamic tool activation, an in-process inter-extension event bus, any JS/TS runtime, multi-language eval | `docs/adr/0046-recover-v1-extensibility-without-centralization.md:57-63` | An extension cannot be a provider codec. A provider-side extension point must be data (catalog, KDL) or an external process behind a contract. |
| ADR 0049 D1 four tiers; vendor integrations are T2 extensions in `extensions/`, not Cargo members; T3 third-party are "Python extensions, MCP servers" | `docs/adr/0049-tiered-integration-architecture.md:41-54` | `extensions/` has zero Cargo weight by design; new capabilities default to T2. |
| ADR 0049 D5 "rescue the extensions deliberately, outside the Cargo workspace": conformance CI runs every extension under `extensions/` against the frozen `omp.*` API | `docs/adr/0049-tiered-integration-architecture.md:113-122` | Any extension-contract change must keep `extensions/` passing (`just check-extensions`, see section 6). |
| ADR 0025: bounded operation sets are schemas reached through `dyn`; open-ended sets are code surfaces; neither may change the latched roster | `docs/adr/0025-long-tail-behind-stable-surfaces.md:58-61` | Extension tools are mounted through `dyn`; there is no per-extension permanent tool. |

## 2. ADR and ledger status at the audited commit

| ADR | Title (short) | Status in `docs/adr/STATUS.md` | Row line |
|---|---|---|---|
| 0025 | `dyn` and code surfaces carry the long tail | Partially implemented (no Eval `dyn` binding) | `STATUS.md:43` |
| 0036 | Embedded Python for extensions, `@remote`, `Eval` | Partially implemented (`@remote` placement beyond host unproved) | `STATUS.md:54` |
| 0046 | Recover v1 extension properties without routing through envd | Partially implemented | `STATUS.md:64` |
| 0049 | Tiered integration architecture | Partially implemented; Phases 0-5 landed, Phase 1 closure and `DelegatedBackend` remainder open | `STATUS.md:67` |
| 0050 | Extension-declared URL schemes | Accepted 2026-11-11 with conditions; S1-S5 complete | `STATUS.md:68` |
| 0075 | Extension HTTP credentials placed by the host | Partially implemented; S-amend, S0-S5 in tree | `STATUS.md:93` |
| 0076 | Middle level for extension permissions | Proposed 2026-11-27, machine-reviewed, not owner-approved, not started | `STATUS.md:94` |

## 3. The extension boundary today

### 3.1 Process model (Implemented)

| Question | Answer | Evidence |
|---|---|---|
| In-process or child process? | Child process. Every Python extension runs in its own host child. The engine never runs extension Python on its own thread (ADR 0036 decision 5). | `docs/adr/0036-python-for-extensions.md:64-65`; `crates/envd/src/exthost/spawn.rs:543 fn spawn` |
| What executable is the child? | The same `omp` binary re-entered with a hidden first argument. There is no separate Python install. The child boots the embedded free-threaded CPython and runs `omp._host.bootstrap().run_forever()`. | `crates/app/src/main.rs:119-130`; `crates/envd/src/exthost/spawn.rs:46 EXT_HOST_ARG`, `:746 fn run_ext_host_entry`, `:763-773` |
| A second child class? | Yes, named Python workers, re-entered with `__omp-py-worker`. | `crates/envd/src/worker_child.rs:19 PY_WORKER_ARG`; `crates/app/src/main.rs:107-118`; `crates/envd/src/worker_process.rs:39 PythonWorkerProcesses` |
| CONTROL channel | Inherited Unix socketpair on descriptor 3 (`OMP_EXT_CONTROL_FD`), never stdio. Frames are a 4-byte big-endian length then JSON `{kind, correlation, body}`, limit 64 MiB. Both ends confirmed: Rust writer and Python `struct "!I"`. | `spawn.rs:545-546, 589, 627-654`; `crates/envd/src/exthost/control.rs:1045 MAX_CONTROL_FRAME_BYTES`, `:2549 struct JsonControlFrame`, `:3814-3834 write_json_control_frame`; `crates/py/python/omp/_host.py:224, 240` |
| DATA channel | Extension-scoped Unix socket to the Environment (`OMP_EXT_ENV_SOCKET`); protobuf `omp.env.v1`, varint-length-delimited frames, `ClientHello{client, schema_rev, capabilities}` handshake, evolution rules "receivers skip unknown fields". | `spawn.rs:50, 591`; `crates/proto/proto/omp/env/v1/env.proto:12-23, 33-40`; `crates/env/src/client.rs:195-232` (hello capability names) |
| Worker protocol | Protobuf `omp.toolhost.v1`, varint-delimited, `WorkerHello{schema_rev, python_rev, api_level, host_version, host_generation, session_generation}`; worker must send `WorkerHello` within 5 s. | `crates/proto/proto/omp/toolhost/v1/toolhost.proto:15-36`; `crates/envd/src/worker_process.rs:30-31, 114` |
| Wire-format summary | Two formats on two channels: JSON frames for extension-host CONTROL, protobuf for DATA and worker CONTROL. Both are message-based; neither exposes memory layout or a C ABI. | rows above |
| Versioning | `omp_api` is an exact integer (`API_LEVELS = {1}`), checked in Python; `Manifest.__post_init__` raises `ApiLevelError`. Each declaration row also carries `api: u32`. Protobuf carries `schema_rev`. Lock format `LOCK_VERSION = 2`, index `INDEX_VERSION = 1`. | `crates/py/python/omp/limits.py:12-15`; `crates/py/python/omp/__init__.py:463-466`; `crates/ext/src/config.rs:1501-1503`; `crates/ext/src/lock.rs:19`; `crates/ext/src/index.rs:17` |
| Platform | Host spawn uses `UnixStream::pair`, `std::os::fd`, `setpgid` and `dup2`; it has no Windows branch. ADR 0076 additionally states that the sandboxed worker cannot start on Windows (`crates/sandbox/src/runner.rs:1136-1142`, not re-read here). | `spawn.rs:5, 545, 629-654` |

### 3.2 Other boundaries that already carry third-party code (Implemented unless noted)

| Boundary | Language-neutral? | Evidence |
|---|---|---|
| MCP servers: stdio, HTTP and legacy SSE transports with JSON-RPC; one Environment-scoped manager; mounted behind `dyn` | Yes. Any executable that speaks MCP | `crates/envd/src/mcp/mod.rs:1-40`; `crates/envd/src/mcp/transport/{stdio,http,legacy_sse,json_rpc}.rs`; ADR 0025 `:58-61` |
| `Delegated` adapters: ACP clients for third-party agent CLIs (dsh, Devin, Grok), the Codex app-server client, and the forge and CI `Dyn` adapters | Yes. Child processes the operator names; disabled until a program convar is set | `crates/driver/src/delegated_acp.rs:1-33`; `crates/envd/src/acp_client/`; `crates/envd/src/codex_app_server/`; `crates/envd/src/forge_adapter/`; ADR 0049 D2 |
| Claude-compatible plugins: marketplace catalogs, `installed_plugins.json`, plugin MCP, LSP and DAP servers and hooks. Each launched command needs an operator approval keyed by a digest that includes the plugin files it names | Yes (commands); data-only for skills and rules | `crates/ext/README.md:65-87`; `crates/ext/src/plugin_command.rs`; `crates/ext/src/claude_plugin.rs` (README read, module code not read) |
| Static content rows in a native extension manifest: skills, rules, context-files, prompts, agents, lsp-servers, dap-adapters, themes | Declarative data, no code | `crates/ext/src/config.rs:1963-1972, 1993-1997` |
| Provider catalog entries (`kind = "provider"`), shipped as pure data by `extensions/provider-pack`, and a Python declaration vocabulary that mirrors `crates/catalog` | Declarative. The Python module "performs no credential, network, filesystem, CONTROL, or DATA access" | `extensions/provider-pack/omp.toml`; `crates/py/python/omp/provider.py:1-7`; `crates/ext/src/config.rs:2149 StaticDeclarationClass::Provider` |
| Hook surface for plugin hooks: Claude events mapped onto omp lifecycle seams (`session_start`, `tool_call`, `tool_result`, `agent_settled`, `compaction`, others); unsupported events never run | Command hooks, any executable | `crates/ext/README.md:152-191`; `crates/driver/src/plugin_hooks.rs` (file exists, 52 KB, not read; `crates/agent` hook code not read) |

### 3.3 What ADR 0046 and the Locked Deviations forbid on this boundary

ADR 0046 lists as not recovered: extension-implemented stream functions or codecs, dynamic tool activation, an in-process inter-extension event bus, any JS/TS runtime, multi-language eval (`docs/adr/0046-recover-v1-extensibility-without-centralization.md:57-63`). `crates/driver/src/discovery/native.rs:1-6` states that JavaScript and TypeScript files "are never inspected or inferred as extensions". A skills-discovery unit test forbids `libloading`, `dlopen`, `dlsym` and process spawning in that module (`crates/driver/src/discovery/skills.rs:2128-2139`).

## 4. Manifest and capability declarations

- **Discovery.** An extension is a Python distribution root with `omp.toml`, a wheel `*.dist-info/omp.toml`, or `pyproject.toml` containing `[tool.omp]` (`crates/driver/src/discovery/native.rs:607-645`). Roots: invocation-explicit, the user extension root and `<project>/.omp/extensions` (`native.rs:49-64`, `crates/ext/src/workspace_trust/inventory.rs` constants).
- **Rust manifest struct.** `DeploymentManifest` has `id`, `entry`, `settings`, `requires`, `capabilities`, `features`, `binaries`, `declarations`, `schemes` (`crates/ext/src/config.rs:1360-1389`). It has no `version` or `omp_api` field and no `deny_unknown_fields` (the only `deny_unknown_fields` in `crates/ext/src` is `scheme.rs:175`), so those keys are ignored by the Rust parser and enforced only by the Python `Manifest` (`crates/py/python/omp/__init__.py:440-466`).
- **Declarations.** `StaticDeclaration{feature, id, kind, module, trigger, key, api, failure, grants, filter, path, metadata, properties}` (`config.rs:1482-1522`). Classes: tool, hook, service, provider, UI command, shortcut, renderers, completion, telemetry subscription and export, prompt slot, credential, secret, worker, placement (`config.rs:2142-2175`). "Permission is granted by membership in the containing declaration table, never by a runtime callback" (`config.rs:1478-1480`).
- **Digests.** `declaration_digest`, `manifest_capability_digest` (BLAKE3 over canonical JSON, `config.rs:1861-1911`), and a per-grant `capability_digest`; a changed manifest voids a grant (ADR 0075 status, `docs/adr/0075-extension-credential-host-placement.md:160`).
- **Capability vocabulary.** The DATA vocabulary is a closed list of 15 names (`crates/envd/src/policy.rs:37-53`) plus 3 CONTROL names reachable only through an operator grant (`policy.rs:57-58`). `validate_capability_name` in `omp-ext` checks only the character set (`config.rs:1918-1935`). `ECapUnknown` and `ECapExecOpen` are declared (`lib.rs:82-84`) but I found no producer in `crates/{ext,app,driver,envd,py}`. Names that shipped manifests declare, such as `env.net.fetch`, `ui.slots`, `place.worker`, `sessions.read`, `schedules:project`, are not in `CAPABILITIES` (read with `extensions/*/omp.toml`); ADR 0076 reports that `env.net.fetch` grants nothing, and I confirmed only the absence from the list, not the runtime effect.
- **Features.** Optional named features carry their own `entry`, `requires`, `capabilities` (`config.rs:1160-1175`).
- **Settings.** Typed non-secret settings; a `secret` setting is refused (`ESettingSecret`, `config.rs:1707-1712`).
- **URL schemes.** `[[schemes]]` claims (ADR 0050), `crates/ext/src/scheme.rs`.
- **Scaffold.** `omp ext new` writes `omp.toml` with `omp_api = 1`, a hard tool and a lazy `extension_activate/observe` hook (`crates/app/src/ext_cli/mod.rs:1362-1391`).

## 5. Registry, integrity, signatures, dependencies, upgrade

| Property | What exists | Evidence |
|---|---|---|
| Sources | Index distribution, PyPI, commit-pinned Git, local path, hash-addressed HTTPS archive | `crates/ext/src/config.rs:381-412 SourceSpec` |
| Signed index | JSON snapshot `{version, name, issued_at, valid_until, extensions[], signature}`; Ed25519 detached signature over canonical JSON; rejected outside the validity window; ids unique and sorted; per-release artifacts must have unique targets, `b3:` and `sha256:` hashes and a signature | `crates/ext/src/index.rs:180-193, 235-351` |
| Artifact | Wheel per target triple or `any`; size, BLAKE3, SHA-256, publisher signature over `blake3 ‖ sha256 ‖ capability digest` | `index.rs:76-94`; `crates/ext/src/trust.rs:1249-1275 verify_artifact_signature`; `ring::signature::ED25519` at `trust.rs:17, 1312` |
| Key continuity | TOFU pin per extension id; a changed key is `EKeyChanged` unless a rotation signed by the pinned key is supplied; operator override `omp ext trust --key` | `trust.rs:914-1058` |
| Revocation | Signed snapshot with validity window; stale is a warning offline, an error under strict offline | `trust.rs:1060-1180`; `crates/app/src/ext_cli/mod.rs:2739-2775, 3711-3730` |
| Lock | `LOCK_VERSION = 2`; per extension id, version, tier, features, source, manifest and capability digests, publisher, signature, wheel; package closure with per-target wheels; `requires_python`, `abi`, `targets`, `index_strategy`; `exclude_newer` clamp | `crates/ext/src/lock.rs:19-134` |
| Resolver | External `uv` (`OMP_EXT_UV` or `PATH`) driven through `std::process::Command`; policy R3 accepts wheel ABIs `cp314t`, `abi3t`, `none`; R4 needs a wheel per materializing target | `crates/ext/src/resolver.rs:1-19, 30-40, 609-632` |
| Install CLI | `omp ext` list, info, install, new, uninstall, link, unlink, enable, disable, features, config, lock, resolve, sync, upgrade, pin, unpin, gc, doctor, trust, grant, verify, bundle, publish, search, index, where | `crates/app/src/ext_cli/mod.rs:279-340` |
| Air gap | `omp ext bundle` and `omp_env::AirgapBundle` (`pack_bundle`, `unpack_bundle`) | `crates/env/src/lib.rs:19-22`; `ext_cli/mod.rs:279-340 ExtCommand::Bundle` |
| Upgrade and rollback | `commit_generation` retains a restorable prior generation; `omp ext upgrade --rollback <generation>` calls `load_generation` then `commit_generation`; pins; `gc` with `--keep-generations`; capability widening needs consent (`--allow-capability-widening`) | `crates/ext/src/upgrade.rs:726-782`; `ext_cli/mod.rs:561-575, 1865-1877`; startup auto-update policy `crates/driver/src/ext_updates.rs` |
| Operator grants | `<data>/ext/grants.toml`: extension id, publisher, layer, workspace, scope, capability digest, tier, ship level, duration; locked read-modify-write | `trust.rs:36-110`; `crates/ext/README.md:24-30` |
| Credential placement | Host-held `[[credential_grant]]`, key never reaches the extension | `crates/ext/src/credential_grant.rs:223, 355`; ADR 0075 status (`:151-163`) |
| Workspace trust | Digest over every project-sourced input incl. `.omp/extensions` and the plugin registry | `crates/ext/README.md:31-64, 89-150` |
| Diagnostics | Closed code vocabulary (`ExtensionCode`); `omp ext doctor` checks lock, site tree, integrity, signatures, revocation, ungranted extensions, ambient `OMP_PY_SITE`, Environment reachability, credential health | `crates/ext/src/lib.rs:74-205`; `crates/ext/src/doctor.rs:1, 88-110, 156-185` |

Self-update of the `omp` binary reuses `SignedIndex` and `verify_artifact_signature` (`crates/app/src/update_cmd.rs:19, 731, 765, 832`). This shows the signed-index machinery is already shared by something that is not an extension.

### 5.1 Declared but unproduced diagnostics (verified by grep, a gap between vocabulary and code)

Searched `crates/{ext,app,driver,envd,py}` for each name with `rg -w`; the only hit for each is the declaration in `crates/ext/src/lib.rs`:

`EBinPlatform` (`lib.rs:132`), `EVendorNative` (`:136`), `EAbiExport` (`:148`), `ECapUnknown` (`:82`), `ECapExecOpen` (`:84`), `WPoolCount` (`:164`), `WApiSkew` (`:166`). `ESig`, `EIntegrity`, `EKeyChanged`, `ERevoked`, `ETargetMissing`, `EAbiRejected`, `EOffline`, `EUpdatePolicy` and `ETrustedLoad` do have producers (`index.rs:349`, `trust.rs:1022`, `resolver.rs:398, 616`, `config.rs:63`, `envd/src/site.rs:157`). The docs in `docs/py/14-deploy.md:2323, 2629` describe `W-API-SKEW`, which this commit does not emit.

## 6. Shipped extensions (`extensions/`, 17 first-party, none a Cargo member)

All 17 are pure Python 3.11+ distributions (`pyproject.toml` with `hatchling`, `omp.toml`, `src/<pkg>/*.py`, `tests/*.py`); the only non-Python, non-doc files are JSON test fixtures under `extensions/{github,gitlab}/tests/fixtures/`. Each is launched the same way: admitted by native discovery, then a lazily spawned `__omp-ext-host` child. `omp_api = 1` and version `0.1.0` in every manifest. Counts below are read from each `omp.toml` and `pyproject.toml`.

| Extension | Declaration kinds | Capabilities | Python dependencies |
|---|---|---|---|
| buildbuddy | soft, hook | env.net, env.net.fetch | none |
| ci | soft, hook | env.net.fetch | omp-github, omp-gitlab |
| forge | soft, hook | env.net.fetch | omp-github, omp-gitlab |
| fuzzy-index | soft x2, worker | env.doc.read, env.search, place.worker | none |
| github | none (library) | env.net.fetch | none |
| gitlab | none (library) | env.net.fetch | none |
| green-loop | command | env.exec, schedules:project | none |
| hover-cards | soft | ui.dialogs | none |
| pr-review | soft | env.exec, env.blob | none |
| provider-pack | provider x4 | none | none |
| remote-approve | hook x2 | env.net, env.net.fetch, schedules:project | none |
| search-provider | provider, soft | place.env, env.net.fetch | none |
| sidebar | none | ui.slots | none |
| spec-flow | soft | env.doc.read, env.doc.write, env.blob | none |
| vault | soft | env.doc.read | none |
| welcome-chrome | none | sessions.read, ui.dialogs, ui.slots, ui.title | none |
| workflows | soft | none | none |

`extensions/README.md`, `RESCUE.md` and `WAVE1.md` describe the rescue wave (ADR 0049 D5, `docs/adr/0049-tiered-integration-architecture.md:113-122`); they were not read in full. ADR 0076 reports that the native route rejected the in-tree manifests (`[requires]` table) until PR #754 and that three still do not parse (`docs/adr/STATUS.md:94`); I did not re-run that and label it "claim, not verified".

### 6.1 The "existing Go executable adapters": none exist at `85d219ae7f`

- `git ls-tree -r origin/omp2` lists exactly one Go-related pair: `fixtures/sarif/sources/golangci/go.mod` and `outcomes.go`, a SARIF parser fixture for `golangci-lint` (`crates/driver/src/cleanse/checkers.rs:239` names the binary). No `.go` file, `go.mod`, `go.work` or Go build step exists elsewhere in the tracked tree.
- `docs/research/rpc-clients.md:61` records the same result: "There is no Go code in the workspace today".
- The nearest real artefacts are outside this commit: upstream v1's `sdk/go/omp-rpc`, a Go client for `omp --mode rpc` (`docs/research/rpc-clients.md:23-29`; the Go module is `github.com/can1357/oh-my-pi/sdk/go/omp-rpc`), and the external Go daemon `ovnfs`, used by v1 isolated subagents and rejected for embedding (`docs/adr/0071-subagent-isolation.md:24, 103`).
- In omp2 the executable-adapter shape that does exist is the Rust-side `Delegated` ACP and Codex adapters (section 3.2); their target CLIs' implementation languages were not checked.
- The owner's brief item may refer to CLIProxyAPI, which is Go (stage 2 reference reading, not this file). Open question O-EXT-1 below.

## 7. C ABI, `extern "C"` exports, `libloading`, `abi_stable`, `savefile-abi`

Method: `rg` for `extern "C"`, `extern "C-unwind"`, `no_mangle`, `export_name`, `libloading`, `abi_stable`, `savefile`, `dlopen`, `LoadLibrary`, `cdylib`, `crate-type` over `crates/` (`*.rs`, `Cargo.toml`); `rg` for the same crate names and for wasm and script engines over `Cargo.toml` and `Cargo.lock`; and a parse of `Cargo.lock` (`tomllib`) to compute reverse dependencies and the shortest path from each workspace crate.

| Item | Result |
|---|---|
| `abi_stable`, `savefile`, `savefile-abi`, `wasmtime`, `wasmer`, `wasmi`, `extism`, `rquickjs`, `boa_engine`, `deno_core`, `mlua`, `rhai`, `libffi` | Absent from `Cargo.lock` (exact `name = "..."` search). |
| `cdylib` or `crate-type` in any workspace `Cargo.toml` | Absent. The only `cdylib` text is `pyo3_introspection::introspect_cdylib` in `crates/py/src/bin/stubgen.rs:5, 14` (a stub generator). |
| `libloading 0.8.9` in `Cargo.lock` | Present as a transitive dependency of `ash`, `clang-sys`, `dlib`, `khronos-egl`, `ug`, `wgpu-hal`, `x11rb` only. Shortest chains from the workspace: `omp-ai > candle-core > candle-ug > ug`, `omp-gui > wgpu > wgpu-hal`, `omp-envd > arboard > x11rb`, `omp-envd > omp-desktop > pipewire > libspa-sys > bindgen > clang-sys`, `omp-gui > winit > xkbcommon-dl > dlib`. No workspace crate lists `libloading` in its own `Cargo.toml`. These are GPU, windowing and ML stacks, not an extension loader. |
| `wit-bindgen 0.57.1` in `Cargo.lock` | Present only under `wasip2`, which only `getrandom` depends on (a WASI-target backend). There is no component host. |
| Project-owned `dlopen` and `dlsym` | One place: `crates/ai/src/local/applefm/platform.rs:124-144` loads the Apple FoundationModels framework with `libc::dlopen(FRAMEWORK_PATH, RTLD_NOW | RTLD_GLOBAL)` and resolves symbols with `dlsym`. It also exports two `#[unsafe(no_mangle)]` symbols for that bridge (`platform.rs:223-224 APPLE_FM_ACTIVE_REQUEST`, `:1245-1246 apple_fm_next_completed`). It binds an operating-system framework; it is not a third-party plugin interface. |
| Other `extern "C"` | `unsafe extern "C" { fn ... }` imports of operating-system and CPython symbols: `crates/py/src/interrupt.rs:10` (`PyThread_get_thread_ident`), `crates/envd/src/exthost/spawn.rs:476` (`_dyld_image_count`), `crates/app/src/live_path.rs:87,101`, `crates/shell-builtins/src/proc_snapshot.rs:406`, `crates/sandbox/src/runtime/macos.rs:186`, `watchdog_macos.rs:255`, plus audio, desktop, gui, webview, tui bindings (file list from the pattern count; those files were not each read). One `extern "C" fn` callback: the SIGINT recorder `crates/tools/src/eval/kernel.rs:1458`. |
| Embedded CPython C ABI | The `omp` executable exports the CPython C-API through an export list (`--dynamic-list` or `-exported_symbols_list`) so user-installed native wheels in `$OMP_PY_SITE` resolve it at `dlopen` (`crates/py/src/lib.rs:11-12, 25-31`; `crates/py-link/src/lib.rs:14-17`). Accepted wheel ABIs are `cp314t`, `abi3t`, `none` (`crates/ext/src/resolver.rs:19`). This is CPython's ABI, loaded inside a Python extension host child by the Python import system. OMP defines no ABI of its own. |
| Native code in vendored extension trees | Policy code `EVendorNative` and checker `EAbiExport` exist in the vocabulary only (section 5.1). |

Conclusion: at `85d219ae7f` there is **no OMP-defined C ABI plugin interface, no `extern "C"` plugin export, no `libloading`/`abi_stable`/`savefile-abi` use by OMP code, and no dynamically loaded provider or extension shared library**. The only third-party native code that can enter a process is a CPython native wheel, inside a killable, sandboxed-by-default extension host child, under a CPython ABI that OMP does not own. [INFERENCE] Because that CPython ABI dependency is a consequence of ADR 0036 decision 4, any move away from native wheels is an ADR 0036 question, not a gateway question.

## 8. Dimension rows X01 to X11 (summary; detail in `omp2-extensions.tsv`)

| ID | Status | One line |
|---|---|---|
| X01 | Implemented | Child process (same-binary re-entry, embedded CPython) plus declarative rows and MCP; no native library, Wasm or static-crate plugin |
| X02 | Partial | Typed manifest and declaration table with digests; capability names loosely validated; Rust ignores `omp_api` |
| X03 | Partial | Signed index, five source kinds, rich CLI; no hosted official index evidenced |
| X04 | Implemented | BLAKE3 + SHA-256, Ed25519, TOFU pins, revocations, validity windows; path and link sources unsigned by design |
| X05 | Partial | Two tiers, sandboxed profile, operator grants, credential grants; coarse capability names; ADR 0076 only Proposed |
| X06 | Implemented | Static, first-reach, before-first-prompt, before-UI-input triggers; cancel ladder to process-group kill; restart breaker |
| X07 | Partial | Static doctor and host-down availability; no periodic liveness probe found |
| X08 | Implemented | Generations, `--rollback`, pins, gc, capability-widening gate |
| X09 | Partial | Per-target wheels only; `[[binaries]]` parsed but no materializer; `EBinPlatform` unproduced |
| X10 | Implemented | `requires`, feature requires, uv resolution, lock with closure and cycle codes |
| X11 | Partial | Exact integer `omp_api`, per-row `api`, ABI tags, `schema_rev`; no ranges; `W-API-SKEW` unproduced |

## 9. Departures from ADR text found

1. ADR 0036 and ADR 0049 describe first-party extensions living "in a repository outside `omp2`" (`0036:87`) while ADR 0049 D5 and the tree place them in `extensions/` (owner decision 2026-11-10, `0049:114`). The 2026-11-09 ADR 0036 amendment line is stale on that point; STATUS row 0036 says "first-party extension repository and conformance CI not started" (`STATUS.md:54`) while ADR 0049's status lists conformance CI landed (`STATUS.md:67`). I did not run the CI job.
2. `crates/ext/src/config.rs:1379` documents `[[binaries]]` as "Logical binaries materialized by the lock", but no code materializes them (section 5.1 and TSV X09).
3. `docs/py/14-deploy.md:2323, 2629` documents `W-API-SKEW`; no producer exists (section 5.1).
4. `omp_api` is documented as a required manifest field (`docs/py/00-overview.md:459`) but Rust `DeploymentManifest` does not model it (section 4).

## 10. Not verified at this stage

- `crates/agent` hook code and `crates/driver/src/plugin_hooks.rs`: hook seams taken from `crates/ext/README.md:152-191`, code not read.
- `crates/sandbox/src/runner.rs` backends (Seatbelt, Bubblewrap): taken from ADR 0076 (`docs/adr/0076-extension-permission-middle-level.md:21`), not re-read.
- Whether a hosted official index exists and how `omp ext publish` uploads: CLI shape read, network code not read.
- `crates/driver/src/ext_updates.rs` and `crates/ext/src/marketplace.rs`: purpose from README and file names only.
- Runtime memory, startup latency and binary-size cost of an extension host: **not measured**. Plan for stage 3 (needs the Python-runtime owner's numbers): cold and warm `omp __omp-ext-host` spawn to `WorkerHello` or first `Registration` frame, RSS per child, with and without `extensions/` loaded, recorded with `uptime` under `evidence/measurements/`. No measurement file was written in this stage.

## 11. Open questions for the owner

- **O-EXT-1.** Which artefacts does the brief's "existing Go executable adapters" name? None exist in the tree (section 6.1). If it means CLIProxyAPI plugin executables, they are reference material only.
- **O-EXT-2.** Is `omp_api` meant to remain an exact integer (no ranges), given that `W-API-SKEW` and per-host admission are documented but unimplemented?
- **O-EXT-3.** Should declared-but-unproduced diagnostics (`EBinPlatform`, `EVendorNative`, `EAbiExport`, `ECapUnknown`, `ECapExecOpen`, `WPoolCount`, `WApiSkew`) be implemented, or removed from the closed vocabulary?

Back to the [index](../README.md).
