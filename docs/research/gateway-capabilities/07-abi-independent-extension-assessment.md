# ABI-independent extension assessment

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 7. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Evidence base: [`evidence/omp2-extensions.md`](evidence/omp2-extensions.md) (code audit, rows X01 to X11 in [`omp2-extensions.tsv`](evidence/omp2-extensions.tsv)) and [`evidence/ref-extension-mgmt.md`](evidence/ref-extension-mgmt.md) (reference repositories; section 9 below). Diagram: [`diagrams/11-abi-independent-extension-boundaries.md`](diagrams/11-abi-independent-extension-boundaries.md). Nothing in this file was measured. Where a cost or size would matter, the card carries a measurement plan, not a figure. The Python runtime comparison is deliverable 8 (another owner) and the serialization benchmarks belong to the transport deliverable; this file only cross-references them.

## 1. Answer in brief

1. OMP2 already has an ABI-independent extension boundary. It is a killable child process re-entered from the same `omp` binary, speaking length-prefixed JSON on CONTROL and protobuf on DATA, with a signed index, a lock file, trust pins and an upgrade and rollback path. No OMP-defined C ABI, `extern "C"` plugin export, `libloading`, `abi_stable` or `savefile-abi` use exists (`evidence/omp2-extensions.md` section 7).
2. The extension language is locked to embedded free-threaded CPython (`AGENTS.md:531-532`; `docs/adr/0036-python-for-extensions.md:40-41`). A Wasm component host, a native executable extension protocol, or any non-Python extension runtime therefore needs either superseding ADR 0036 (`Requires ADR reconsideration`) or framing as an external integration behind a boundary that already exists (MCP, `Delegated`, a configured gateway `base_url`).
3. The most valuable work is smaller than a new plugin system: make the existing manifest and diagnostic vocabulary honest. Eight names are declared but never produced, and `[[binaries]]` is parsed but never materialized (cards EXT-02 to EXT-05).
4. Gateway-style capabilities that are "extension-shaped" (more providers, a provider registry, per-provider pre/post hooks) map onto existing shapes: declarative provider rows (`extensions/provider-pack`), data-only catalog entries, and MCP or `Delegated` processes. A second native plugin ABI is not needed and is rejected (EXT-09).

## 2. Binding constraints

| Constraint | Source |
|---|---|
| Embedded free-threaded CPython is the only extension runtime; no JS/TS plugin runtime, no multi-language eval | `AGENTS.md:531-532` |
| "Adding a second extension language requires superseding this record" | `docs/adr/0036-python-for-extensions.md:40-41` |
| Every extension host is a killable child; the engine never runs extension Python on its own thread | `0036:64-65` |
| No extension-implemented stream functions or codecs, no dynamic tool activation, no in-process inter-extension event bus | `docs/adr/0046-recover-v1-extensibility-without-centralization.md:57-63` |
| Vendor integrations are T2 extensions in `extensions/` (zero Cargo weight); third-party is T3 (Python, MCP) | `docs/adr/0049-tiered-integration-architecture.md:41-54` |
| Providers are data; model-name conditionals in `.rs` are reviewer-reject; typed wire structs only | `AGENTS.md:548-554` |
| Tools: no dynamic schema mutation; long tail rides `dyn` | `AGENTS.md:539-547`; `docs/adr/0025-long-tail-behind-stable-surfaces.md:58-61` |
| Owner brief: avoid native ABI dependencies between independently distributed extensions; do not propose replacing ADR 0036 without concrete evidence | `evidence/00-owner-brief-verbatim.md` sections 6 and 18 |

## 3. Terms: what "ABI" means in each option

| Term | Meaning | Relevance |
|---|---|---|
| C ABI | The platform calling convention and data layout (`extern "C"`, `#[no_mangle]`). A shared library exports symbols a host resolves with `dlopen`/`dlsym` or `libloading`. Both sides run in one address space with the host's privileges. | The mechanism the owner excludes. OMP code uses it only to bind operating-system frameworks (`crates/ai/src/local/applefm/platform.rs:124-144`) and to let CPython native wheels resolve the CPython C-API (`crates/py/src/lib.rs:25-31`). |
| Rust ABI | Rust has no stable ABI across compiler versions. Crates such as `abi_stable` or `savefile-abi` layer one on top of the C ABI. | Not used and not needed when everything is compiled together or crosses a message boundary. |
| CPython ABI (`cp314t`, `abi3t`) | The binary interface native wheels target. OMP accepts these tags only (`crates/ext/src/resolver.rs:19`). | A third party's native code can enter an extension host child through this interface. OMP does not own or version it. |
| WebAssembly Component Model Canonical ABI | A specified lowering and lifting of interface-typed values (WIT types) to core Wasm values and linear memory, used between a component and its host or between components. It is neither the C calling convention nor a native pointer ABI; a component sees only its own linear memory and imports it was given. Section 9 cites `wit-bindgen` and `wasmtime` for what the tools generate and enforce. | The only "plugin ABI" that is both language-neutral and memory-isolated. Absent in OMP2 and outside the locked extension language. |
| Message contract | A serialized, versioned schema carried over a pipe, socket or stdio (JSON frames, protobuf, JSON-RPC, and so on). Only bytes cross. | The existing boundary. Independent compilation and distribution need nothing else. |

## 4. Option comparison

"Foothold" is what exists on `85d219ae7f`. Status uses the method's maturity labels. "Locked?" says whether the option fits the Locked Deviation without an ADR change.

| Option | Foothold in OMP2 | Independent build and distribution | Isolation from the engine | Native ABI between parties | Locked? | Verdict |
|---|---|---|---|---|---|---|
| A. Statically compiled Rust provider crates | Static roster table (`omp_tools::roster`, ADR 0046 item 1, status in `docs/adr/STATUS.md:64`), `cap-*` Cargo features, `omp-forge`, `omp-ci` crates (ADR 0049 D1 T1). Implemented. | No. Ships in lock step with the binary | None (same process) | None (one compilation) | Yes | Right home for first-party generic capabilities that need a hot path or raw credentials (T1 test). Not an extension mechanism. |
| B. Declarative providers | Catalog data and KDL; `kind = "provider"` rows in `extensions/provider-pack/omp.toml`; Python declaration vocabulary `crates/py/python/omp/provider.py:1-7`; `StaticDeclarationClass::Provider` (`crates/ext/src/config.rs:2149`). Implemented. | Yes (signed wheel via the index) | High (data only; the module "performs no credential, network, filesystem, CONTROL, or DATA access") | None | Yes | Preferred for any provider whose wire protocol OMP2 already speaks. |
| C. Independently executable native providers | Child-process integrations exist for other purposes: MCP (`crates/envd/src/mcp/transport/stdio.rs`), `Delegated` ACP and Codex adapters (`crates/driver/src/delegated_acp.rs:1-33`). `[[binaries]]` is parsed only (`config.rs:1179, 1381`). Prototype. | Yes, if a per-target artifact format existed; it does not (wheels only) | OS process boundary; sandbox only for Python hosts | None (messages) | Only as an external integration | Fits as an external process the operator configures (MCP server, gateway at a `base_url`, `Delegated` CLI). A native executable that registers into the extension host protocol would be a second extension runtime. |
| D1. Protobuf over local IPC | DATA (`omp.env.v1`, varint-length-delimited, `ClientHello`/`ServerHello`), worker CONTROL (`omp.toolhost.v1`), `prost 0.14.4` and `tonic 0.14.6` in `Cargo.lock`. Implemented. | Yes (schema files plus `schema_rev` and capability strings) | Process boundary | None | Yes (host side) | Already the contract for the Environment. Reuse. |
| D2. JSON frames and JSON-RPC | Extension-host CONTROL: u32-BE length plus JSON `{kind, correlation, body}` (`crates/envd/src/exthost/control.rs:2549, 3814-3834`); MCP and ACP are JSON-RPC. Implemented. | Yes | Process boundary | None | Yes | Already the contract for extension hosts and external agents. Reuse. |
| D3. Cap'n Proto, FlatBuffers | Absent: no `capnp*` or `flatbuffers` crate in `Cargo.lock` (exact-name search). | Yes | n/a | None | Not proposed | Requires benchmarking before any adoption; owner of the serialization deliverable measures. |
| D4. MessagePack, CBOR | Absent: no `rmp*`, `rmpv` or `ciborium` in `Cargo.lock`. `postcard 1.1.3` is used by `omp-ai`, `omp-catalog`, `omp-core`. | Yes | n/a | None | Not proposed | Unnecessary unless JSON frame cost is measured as a problem. |
| D5. `rkyv` | `rkyv 0.8.18` in `Cargo.lock`, reachable only through `dtls`. | Fragile: zero-copy archives tie readers to a layout version | n/a | Layout, not C ABI | Not proposed | Not an extension boundary. |
| E. Shared memory and mmap | `memmap2` is a direct dependency only of `omp-shell-builtins`; the worker tunnel already moves out-of-band buffers (`crates/envd/src/worker_pool.rs:35-40`, limits 64 buffers of 256 KiB); blobs live in a content-addressed store. Prototype. | Yes, if the layout is a message schema | Weaker (shared pages) | Layout contract | Not for Python hosts | Requires benchmarking; only for large tensors and embeddings (see deliverables 10 and 11), not for provider extensions. |
| F. Native worker pools | `WorkerSupervisor` supervises named Python workers with a per-layer ceiling of 8 and 4 concurrent spawns (`worker_pool.rs:41-44`); same-binary re-entry. Implemented for Python. | Same binary | Process boundary | None | Python only | A native (non-Python) pool for inference or indexing is a separate, in-tree design (deliverables 9 to 11), not an extension runtime. |
| G. WebAssembly components | Absent: no `wasmtime`, `wasmer`, `wasmi`, `wit-parser` or `wit-component` in `Cargo.lock`. `wit-bindgen 0.57.1` appears only under `wasip2 > getrandom`. | Yes (components are portable, signed artifacts) | Strong (linear-memory sandbox, capability imports) | Canonical ABI, not C ABI | No: a second extension runtime | `Requires ADR reconsideration` and an owner decision; optional only. |
| H. Shared libraries (`libloading`, `abi_stable`, `savefile-abi`, `cdylib`) | Absent in OMP code; `libloading` is transitive through GPU, windowing and ML crates only. | No (must match compiler and layout, or a C ABI) | None (arbitrary native code in the engine process) | C ABI or Rust ABI shim | No (contradicts ADR 0036 decision 5) | Rejected (EXT-09). |

## 5. Recommended boundary

The recommendation is not a new mechanism. It names the existing boundary, makes it honest, and routes every new "extension-shaped" need to the lowest option that satisfies it, following ADR 0049 D1 ("the lowest tier that satisfies its requirements").

1. **Decision ladder for a new capability.** (a) Data: a catalog or provider row, a static content row (skills, rules, prompts, agents, `lsp-servers`, `dap-adapters`, themes). (b) Python extension in `extensions/` or a third-party wheel, through the signed index and lock. (c) External process behind MCP, `Delegated`, or a configured gateway endpoint. (d) First-party Rust crate behind a `cap-*` feature, only if it passes the ADR 0049 T1 test. Anything that needs a second extension language stops at an ADR.
2. **One wire discipline across all of them.** Messages only; versioned by explicit integers and capability strings (`schema_rev`, `api_level`, hello `capabilities`), receivers skip unknown fields (`env.proto:17-23`; `toolhost.proto:15-23`). The gateway hello already states the rule that a client aborts when `schema_rev` is lower than requested (`crates/proto/proto/omp/gateway/v1/gateway.proto:9-17`).
3. **One distribution discipline.** Signed index, per-artifact hashes, TOFU key pins, signed revocations, lock file, generations (`evidence/omp2-extensions.md` section 5). Reuse it for provider-catalog packs. The same `SignedIndex` and `verify_artifact_signature` already serve binary self-update (`crates/app/src/update_cmd.rs:731, 765`).
4. **Make the declared surface match the produced surface.** Implement or delete the declared-but-unproduced names (EXT-02 to EXT-05) so a third party can trust the manifest vocabulary.
5. **Keep native code out of the engine process.** CPython native wheels stay confined to the sandboxed host child; add the missing `EVendorNative` and `EAbiExport` checks (EXT-03) so that confinement is enforced, not only documented.

## 6. Declared-but-unproduced manifest fields and codes, as cards

Cards use the capability-card template from [`evidence/01-method-and-dimension-ids.md`](evidence/01-method-and-dimension-ids.md) section 4, with one extra last line, `Draft ADR needed?`.

### EXT-01 Message-based extension host as the boundary of record
**Decision class:** Already implemented: reuse
**Existing implementation evidence:** `crates/envd/src/exthost/spawn.rs:46 EXT_HOST_ARG`, `:543 fn spawn`, `:746 fn run_ext_host_entry`; `crates/app/src/main.rs:119-130`; CONTROL frame `crates/envd/src/exthost/control.rs:2549 JsonControlFrame`, `:3814 write_json_control_frame`; Python end `crates/py/python/omp/_host.py:224, 240`; DATA `crates/proto/proto/omp/env/v1/env.proto:12-40`. Implemented.
**Actual missing capability:** None for the boundary itself. The frame `kind` vocabulary (`UiEffect`, `IntentEffect`, `DispatchStarted`, `DispatchResponse`, and others in `_host.py`) is documented only by code.
**Alternatives considered:** Options A to H in section 4.
**Expected benefit:** Reliability and security (killable, sandboxed, independent of compiler version); no new code.
**Implementation complexity:** low (documentation only if the owner wants a published contract).
**Security implications:** Sandboxed tier has no network and scoped filesystem (`spawn.rs:553-577`); trusted tier has no sandbox (`spawn.rs:578`). ADR 0076 (Proposed) covers refinement.
**Maintenance cost:** Existing.
**Performance measurement strategy:** not applicable for this card. Spawn-to-ready latency and RSS per child are not measured; plan in `evidence/omp2-extensions.md` section 10.
**Acceptance criteria:** The boundary contract (frame kinds, fields, limits, handshake, versioning) is stated in one place and tested against the Python end by an existing integration test.
**Follow-up:** Combine with EXT-04 for the version rule.
**Draft ADR needed?** No.

### EXT-02 Materialize or remove the `[[binaries]]` manifest field
**Decision class:** Requires ADR reconsideration
**Existing implementation evidence:** `crates/ext/src/config.rs:1179 BinaryManifest` ("Logical executable name used by static LSP and DAP configuration"), `:1379-1381 binaries` ("Logical binaries materialized by the lock"); no other reader in `crates/{ext,app,driver,envd}/src` (`rg`); `EBinPlatform` declared at `lib.rs:132` with no producer. Prototype.
**Actual missing capability:** A per-target executable artifact in the index and lock, with the same hash and signature checks as wheels, resolved to a path the LSP and DAP loaders may use. Index artifacts are wheels only (`index.rs:76-94`).
**Alternatives considered:** (1) Delete the field and the code. (2) Ship executables inside platform wheels (the usual Python route; keeps the existing signature path; `ETargetMissing` already enforces a wheel per target). (3) A new signed executable artifact kind.
**Expected benefit:** Reliability and security for extension-supplied language servers and debug adapters (verified binary, not `$PATH`).
**Implementation complexity:** medium for (3); low for (1) or (2).
**Security implications:** Adds executable code distribution. ADR 0046 item 5 states "Foreign executables remain unloaded (ADR 0036)"; any materializer must run under the workspace-trust and `plugin_command` approval model.
**Maintenance cost:** Medium for (3): per-target build and signing pipeline.
**Performance measurement strategy:** not applicable.
**Acceptance criteria:** Either the field is removed and `EBinPlatform` deleted, or a manifest `[[binaries]]` row resolves to a hash-verified, signed, target-specific path, a missing target yields `EBinPlatform`, and an unapproved launch is refused.
**Follow-up:** Option (2) is the smallest step and needs no ADR; take it first if a need appears.
**Draft ADR needed?** Only if option (3) is chosen.

### EXT-03 Enforce the native-code policy codes `EVendorNative` and `EAbiExport`
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** Wheel ABI policy is enforced (`resolver.rs:19, 609-620 validate_abi`, `EAbiRejected`); `EVendorNative` ("A vendored tree contained native code", `lib.rs:136`) and `EAbiExport` ("A host binary does not export the `CPython` C API", `lib.rs:148`) have no producer in `crates/{ext,app,driver,envd,py}` (`rg -w`). `omp ext doctor` runs no export check (`crates/ext/src/doctor.rs:110-185`).
**Actual missing capability:** A deterministic scan of a vendored tree for `.so`, `.dylib`, `.pyd` or ELF/Mach-O/PE content at install, `bundle` or `publish`, and a doctor check that the running executable exports the CPython C-API symbol set.
**Alternatives considered:** Leave as documentation; scan only at `publish`.
**Expected benefit:** Security (confines native code to declared wheels), reliability (fails early instead of at import).
**Implementation complexity:** low (file-type scan plus a symbol probe).
**Security implications:** Strengthens the isolation claim in section 5.
**Maintenance cost:** Low.
**Performance measurement strategy:** not applicable.
**Acceptance criteria:** A vendored tree containing a native object yields `EVendorNative` and exit code 4 or the documented class; `omp ext doctor` reports `EAbiExport` when the symbol set is missing; both covered by tests.
**Follow-up:** None.
**Draft ADR needed?** No.

### EXT-04 Admit `omp_api` and per-row `api` in Rust before spawning a child
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** `omp_api` is checked only in the child (`crates/py/python/omp/__init__.py:463-466`, `API_LEVELS = {1}` at `limits.py:15`); `DeploymentManifest` has no `omp_api` or `version` field and the parser accepts unknown keys (`config.rs:1360-1389`); no Rust comparison of `StaticDeclaration.api` (`config.rs:1501-1503`) with a supported level was found in `config.rs`, `native.rs`, `lifecycle.rs` (`rg`); the worker host does compare `api_level` (`crates/envd/src/worker_process.rs:118`). `WApiSkew` has no producer (`lib.rs:166`).
**Actual missing capability:** A pure admission check that refuses an unsupported level before a child is spawned, and the documented client/workspace skew warning.
**Alternatives considered:** Keep the Python-only check (error appears after a child boots, within the 10 s `ACTIVATION_TIMEOUT`, `limits.py:9`); accept version ranges (docs say "No range", `docs/py/00-overview.md:459`).
**Expected benefit:** Reliability and latency (no wasted spawn), clearer diagnostics.
**Implementation complexity:** low.
**Security implications:** None adverse.
**Maintenance cost:** Low; one more place that must track `API_LEVELS`.
**Performance measurement strategy:** not measured. Plan: time admission to first error for an unsupported level, before and after.
**Acceptance criteria:** A manifest with `omp_api = 2` is refused at admission with an `ExtensionCode`, no child is spawned, and `W-API-SKEW` fires when client and workspace sets differ.
**Follow-up:** Owner decides ranges versus exact integers (O-EXT-2).
**Draft ADR needed?** No.

### EXT-05 Close the capability-vocabulary gap (`ECapUnknown`, `ECapExecOpen`, undeclared names)
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** `validate_capability_name` checks the character set only (`config.rs:1918-1935`); DATA vocabulary is 15 names (`crates/envd/src/policy.rs:37-53`); shipped manifests declare names outside it (`env.net.fetch`, `ui.slots`, `place.worker`, `sessions.read`, `schedules:project`, `ui.dialogs`, `ui.title`, `place.env`; from `extensions/*/omp.toml`); `ECapUnknown` and `ECapExecOpen` have no producer (`lib.rs:82-84`). ADR 0076 is Proposed.
**Actual missing capability:** One closed, versioned vocabulary shared by `omp-ext` and the host, with unknown names refused at install and shown in `omp ext info --capabilities`.
**Alternatives considered:** Leave names free-form (current); split DATA, CONTROL and UI vocabularies per owner.
**Expected benefit:** Security (a declared name always means something) and reliability.
**Implementation complexity:** medium: crosses `omp-ext` and `omp-envd`; touches 7 shipped manifests (count from `extensions/*/omp.toml`).
**Security implications:** Directly supports ADR 0076 scoped grants.
**Maintenance cost:** Low once centralized.
**Performance measurement strategy:** not applicable.
**Acceptance criteria:** Installing a manifest that declares an unknown capability fails with `ECapUnknown`; all 17 first-party manifests pass; `just check-extensions` stays green.
**Follow-up:** Fold into ADR 0076 slices.
**Draft ADR needed?** No (ADR 0076 already proposes the surrounding design).

### EXT-06 Runtime host status in the management surface
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** Restart breaker state is in memory only (`crates/envd/src/worker.rs:3718-3755 ControlRestartBreaker`); `omp ext doctor` checks static state and Environment reachability (`crates/ext/src/doctor.rs:88-110`); host-down is published as availability (`worker.rs:1619 publish_host_down`); no periodic liveness probe was found in `worker.rs`, `lifecycle.rs`, `spawn.rs`, `worker_pool.rs`, `worker_process.rs`.
**Actual missing capability:** A deterministic status record per host (generation, last failure, breaker state, forced-kill count) visible to `omp ext list` or the management surface (dimension O01/O06).
**Alternatives considered:** A heartbeat ping (not justified: CONTROL EOF reveals a crash and the cancel ladder bounds a hung call, `cancel.rs:13-107`); logs only.
**Expected benefit:** Reliability and operability (diagnosis without log reading).
**Implementation complexity:** low to medium.
**Security implications:** None; must not expose credentials.
**Maintenance cost:** Low.
**Performance measurement strategy:** not applicable.
**Acceptance criteria:** After two forced kills the extension appears as disabled with its journal fact (`CancellationJournal`) in the status output; test through the e2e extension isolation suite.
**Follow-up:** Coordinate with the observability owner.
**Draft ADR needed?** No.

### EXT-07 Gateway-style providers as declarative provider packs through the signed index
**Decision class:** Already implemented: reuse
**Existing implementation evidence:** `extensions/provider-pack` (4 provider rows: moonshot, zai-glm, alibaba-dashscope, ovhai); `StaticDeclarationClass::Provider` (`config.rs:2149`); the declaration vocabulary mirrors `crates/catalog` (`provider.py:1-7`); signed index and lock (`index.rs:180`, `lock.rs:103`). Implemented as an in-tree extension. Whether an installed provider row reaches the live model catalog end to end was not traced here (provider-runtime owner).
**Actual missing capability:** Not an extension-mechanism gap. Any gap is in the provider runtime and catalog synchronization (deliverable 5).
**Alternatives considered:** Executable gateway plugins (EXT-08); static crates (option A).
**Expected benefit:** Reduced code and a distribution path with signatures; no ABI.
**Implementation complexity:** low (reuse).
**Security implications:** Data-only module; credentials stay host-placed (ADR 0075).
**Maintenance cost:** Existing.
**Performance measurement strategy:** not applicable.
**Acceptance criteria:** A third-party provider pack installed from a signed index appears in the catalog with its declared capabilities, and an undeclared capability is refused.
**Follow-up:** Provider-runtime owner confirms the catalog path.
**Draft ADR needed?** No.

### EXT-08 Non-Python provider code as an external process behind a configured endpoint
**Decision class:** Suitable as an external integration
**Existing implementation evidence:** Configured endpoints and OmniRoute or CLIProxyAPI examples are treated as ordinary routes (`docs/research/native-resource-architecture/04-providers-context-and-allocation.md:93-99`); MCP stdio and `Delegated` adapters (`delegated_acp.rs:1-33`); `ExecutorId`/`AcpProfile` launch profiles disabled until an operator names a program.
**Actual missing capability:** None in the extension system. A provider with a bespoke wire protocol runs as its own gateway process that OMP2 reaches with a declarative route.
**Alternatives considered:** Executable native extension class (EXT-10); Wasm (EXT-09).
**Expected benefit:** Keeps the locked deviation intact; isolates unreviewed code in a separate OS process.
**Implementation complexity:** low (documentation and an example).
**Security implications:** The operator owns the process and its credentials; Provider terms apply to what that gateway does.
**Maintenance cost:** Low for OMP2.
**Performance measurement strategy:** not measured; extra local hop latency is a plan item for the transport deliverable.
**Acceptance criteria:** A documented recipe adds a locally run gateway as a declarative route with no new code in `crates/`.
**Follow-up:** Provider-runtime owner supplies the route template.
**Draft ADR needed?** No.

### EXT-09 Dynamic shared-library plugins (`libloading`, `abi_stable`, `savefile-abi`, `cdylib`)
**Decision class:** Rejected for security, licensing or operational reasons
**Existing implementation evidence:** Absent (`evidence/omp2-extensions.md` section 7); a unit test forbids `libloading` and `dlopen` in skills discovery (`crates/driver/src/discovery/skills.rs:2128-2139`).
**Actual missing capability:** Not desired.
**Alternatives considered:** Options B, C, D, G.
**Expected benefit:** Only in-process call speed, which extensions do not need ("Extensions do not account for even 1% of a session's run time", `0036:16`).
**Implementation complexity:** high (stable ABI design) with no isolation gain.
**Security implications:** Third-party native code in the engine process; contradicts the killable-child rule (`0036:64-65`) and the Rust ABI is unstable across compiler versions.
**Maintenance cost:** High.
**Performance measurement strategy:** not applicable.
**Acceptance criteria:** No `libloading`, `abi_stable` or `savefile` appears in any workspace `Cargo.toml`; the existing grep-based tests stay.
**Follow-up:** None.
**Draft ADR needed?** No.

### EXT-10 WebAssembly component extensions
**Decision class:** Requires ADR reconsideration
**Existing implementation evidence:** No Wasm runtime in `Cargo.lock`; `docs/adr/0036-python-for-extensions.md:40-41`; `AGENTS.md:531-532`.
**Actual missing capability:** A component host, WIT interface for tools, hooks and providers, a capability-import mapping onto DATA grants, and signing of components through the existing index.
**Alternatives considered:** Python hosts (current), external processes (EXT-08).
**Expected benefit:** Possibly security (linear-memory isolation) and language choice; not measured, so no claim on size or startup. Reference tools are read in section 9.
**Implementation complexity:** high: a second extension runtime, a second sandbox story, a second dependency graph (the workspace gates feature graphs by build weight, `AGENTS.md:577-581`).
**Security implications:** Smaller attack surface per component than a Python host in principle; larger engine dependency surface.
**Maintenance cost:** High.
**Performance measurement strategy:** Plan: build a throwaway host outside the OMP2 tree and record binary-size delta, cold and warm instantiation, and per-call overhead against a Python host, per the owner's section 6 list. Not run.
**Acceptance criteria:** An owner decision to supersede ADR 0036 decision 2, then a measured comparison.
**Follow-up:** Optional extension only; not before EXT-02 to EXT-06.
**Draft ADR needed?** Yes, a supersede proposal, only if the owner opens the question.

### EXT-11 Native executable extension class speaking the CONTROL protocol
**Decision class:** Requires ADR reconsideration
**Existing implementation evidence:** CONTROL is JSON frames over an inherited descriptor; the only client is `omp._host` (`_host.py`). The protobuf worker protocol has `api_level` and a hello (`toolhost.proto:24-36`).
**Actual missing capability:** A published CONTROL contract, an executable artifact kind (EXT-02) and a non-Python host.
**Alternatives considered:** MCP or `Delegated` (EXT-08), Python with native wheels.
**Expected benefit:** Reliability and memory for heavy extensions; not measured.
**Implementation complexity:** high.
**Security implications:** Executable distribution plus a sandbox profile for a non-Python process; ADR 0076 limits apply.
**Maintenance cost:** High.
**Performance measurement strategy:** Plan with EXT-10's harness.
**Acceptance criteria:** Owner supersedes ADR 0036 decision 2 and EXT-02 option (3) lands first.
**Follow-up:** Prefer MCP unless callback overhead is shown to matter.
**Draft ADR needed?** Yes, if pursued.

### EXT-12 Alternative encodings and shared memory for extension traffic
**Decision class:** Requires benchmarking
**Existing implementation evidence:** JSON CONTROL (`control.rs:1045` 64 MiB limit, `MAX_DISPATCH_PROGRESS_*`), protobuf DATA, out-of-band worker tunnel buffers (`worker_pool.rs:35-40`); no `capnp`, `flatbuffers`, `rmp` in `Cargo.lock`; `rkyv` only via `dtls`.
**Actual missing capability:** No measurement shows extension traffic is a bottleneck.
**Alternatives considered:** Keep JSON and protobuf.
**Expected benefit:** Latency only if measured.
**Implementation complexity:** medium to high.
**Security implications:** Zero-copy readers need strict validation.
**Maintenance cost:** Medium to high.
**Performance measurement strategy:** Use the serialization owner's harness; record `uptime`, runs, frame sizes; compare CONTROL JSON with protobuf on a dispatch round trip.
**Acceptance criteria:** A stored measurement shows a bounded, repeatable gap that matters at session scale.
**Follow-up:** Close as unnecessary if not shown.
**Draft ADR needed?** No.

## 7. Open questions for the owner

1. **O-EXT-1.** Which artifacts does "existing Go executable adapters" name? None exist at `85d219ae7f` (`evidence/omp2-extensions.md` section 6.1).
2. **O-EXT-2.** Keep `omp_api` as an exact integer, or admit ranges, and should `W-API-SKEW` be implemented (EXT-04)?
3. **O-EXT-3.** Implement or remove the unproduced vocabulary (EXT-02 to EXT-05)?
4. **O-EXT-4.** Is the owner willing to open ADR 0036 decision 2 at all? If not, EXT-10 and EXT-11 close as "not pursued".

## 8. Rejected alternatives

| Alternative | Reason |
|---|---|
| Shared-library plugins (EXT-09) | In-process native code, unstable ABI, contradicts ADR 0036 decision 5 |
| Replace the embedded CPython host | Not proposed; needs concrete evidence and is deliverable 8's scope |
| A second provider runtime or registry inside the extension system | Prohibited by owner constraint 8; providers stay data |
| In-process inter-extension event bus | ADR 0046 "not recovered" |

## 9. Reference comparison

(Written after reading the reference repositories; see [`evidence/ref-extension-mgmt.md`](evidence/ref-extension-mgmt.md).) Pending at the time of the first save of this file.

Back to the [index](README.md).
