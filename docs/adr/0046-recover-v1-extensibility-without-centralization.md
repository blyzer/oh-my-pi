# 0046. Recover v1 extension properties without routing every feature through envd

Status: proposed; requires owner review
Date: 2026-10-11
Area: extensions/runtime

No implementation is authorized by this ADR.

## Context

OMP² (`omp2` at `bc1e12d21b`) is a ground-up Rust rewrite with a history disjoint from v1 (`upstream/main` at `80577ebff5`). Its instruction is "port observable behavior, not TS shape" (AGENTS.md:5-6). The owner deliberately narrowed third-party reach:
- Python-only killable extensions (ADR 0036).
- A fixed wire roster with a `dyn` long tail (ADR 0024).
- Providers as data.
- Typed components (ADR 0030/0031).
- The "Locked Deviations from pi" in AGENTS.md:489.

The comparison in `docs/research/omp-v1-vs-omp2-architecture.md` found that v1 keeps two properties OMP² lost *by accident*, not by decision:

1. **Locality of first-party additions.**
   - A v1 built-in tool is one file plus one `BUILTIN_TOOLS` row (`tools/index.ts:461`); tts `bde0114f87` touched 3 files.
   - In OMP² the same addition must also edit `crates/envd/src/tools.rs`: the `prepare_registry` name list at :3671 and `register_session_base` at :3736. That happens even though `omp-tools` already owns a static identity table (`BUILTIN_TOOL_IDENTITIES`, `crates/tools/src/lib.rs:21`).
2. **Additive events and UI.**
   - A new hook event touched 29 files across agent, proto, envd, four app adapters, chat and py (`f7dd7a8414`), against 11 files in v1 (`6872a73977`).
   - Extension UI slots and dynamic command mounts have frozen Python APIs (`omp.ui.mount`, `omp.ui.set_status`, `omp.ui.dynamic_mount`), but the chat host returns `unsupported_operation` for `DynamicMount` and overlays (`crates/app/src/chat_services/extension_ui.rs:499-510`).

Two structural facts make envd the place where these costs land:
- envd depends upward on `omp-agent` for approval and hook vocabulary, while driver imports envd-internal policy and eval-bridge types.
- envd composes the native roster even though AGENTS.md:15-17 names driver as the composer.

See also the companion proposed ADR 0045 (envd responsibilities).

## Decision

The decision is proposed. Recover the following v1 *properties* in idiomatic Rust, at build or construction time only, without reopening any Locked Deviation:

1. **Table-driven built-in roster.**
   - Extend the existing `omp-tools` identity table into a static construction table: identity, constructor, and required host bridges.
   - `envd` iterates the table instead of enumerating names. `driver` owns roster selection through `compose_kernel`.
   - The roster stays closed at compile time. Erasure stays at the existing single `Arc<dyn ErasedTool>` boundary. There is no runtime discovery, `loadMode` or schema mutation.
2. **Neutral home for shared vocabulary.**
   - Move the approval, posture, hook-gate and eval-bridge types shared by `omp-agent`, `omp-envd` and `omp-driver` into a contract-level crate below all three.
   - envd stops depending on the kernel crate; driver stops importing envd internals.
3. **Additive typed events.**
   - Describe hook events in a catalog (identity, schema revision, payload type) that adapters consume through a default arm.
   - Only adapters that render an event specially change when an event is added.
   - Payloads remain typed prost/serde structs; there is no untyped envelope.
4. **Extension UI host arms.**
   - Implement the chat-host side of `omp.ui.mount`/`set_status` slots, overlays and `dynamic_mount` using TML and the existing slot registry, within ADR 0031's typed model.
5. **Data-only discovery providers.**
   - Express foreign-config discovery (`driver/src/discovery/*`) as static provider slices per resource kind.
   - Foreign executables remain unloaded (ADR 0036).
6. **Envd periphery outside envd.**
   - New first-party integrations that are not project-resource authorities enter through `RegistryBridges`/`DynamicToolFactory`/`ContentResolver` from their own crates, not as envd modules.
   - Moving the existing periphery out of envd is governed by ADR 0045.

Explicitly **not** recovered, per the Locked Deviations:
- extension-implemented stream functions/codecs;
- dynamic tool activation;
- `renderCall`/`renderResult` callbacks;
- an in-process inter-extension event bus;
- any JS/TS runtime;
- multi-language eval.

## Alternatives considered

- **Keep the status quo:** leaves per-tool envd edits and the 29-file event fan-out. This remains the default until review.
- **Port v1's registry and extension API shape:** conflicts with ADR 0024/0030/0035/0036 and the Locked Deviations. Rejected.
- **Split envd into more processes:** does not address composition or vocabulary placement and breaks single ownership (ADR 0006/0011/0036). Rejected.
- **Runtime plugin discovery in Rust (`inventory`/`linkme`):** acceptable only as a build-time collection mechanism. A plain static table is preferred unless it causes merge contention.

## Consequences

- **Expected:** adding a built-in tool that needs no new host verb no longer edits envd; adding a hook event no longer edits every adapter; envd loses its upward dependency on `omp-agent`.
  - [INFERENCE] These effects are unmeasured; acceptance requires the measurements below.
- **Unchanged by design:**
  - New host verbs still cross `env.proto` + envd + `omp-env`, because that is the typed process boundary.
  - New codecs remain ai/catalog edits.
  - New vocabulary tags remain `crates/vocab` edits.
- **Cost:**
  - One new contract crate, which adds one node to the crate graph.
  - Moving types breaks downstream imports once (clean cutover, no re-exports).
  - The event catalog adds a descriptor per event.
- **Reversibility:** every item is a build-graph or composition change with no wire or journal format change, except item 4, which only implements already-frozen Python APIs. Each item can ship and revert independently.
- **Compatibility:** no change to Python extension ABI, `toolhost/v1`, `env.proto` frames or journal schema is implied. Item 3 must keep existing event IDs and schema revisions.

## Required evidence before acceptance

- An owner ruling on the open questions:
  1. Should driver own roster composition?
  2. Is envd's `omp-agent` dependency intentional?
  3. Is a catalog-driven default adapter arm compatible with "typed wire structs"?
- Baseline and after measurements:
  - `cargo build --timings` for `omp-envd` and `omp`: cold, and after a one-line edit in a leaf crate used by envd.
  - envd transitive crate count.
  - Co-change share of tools, ai and ext commits that touch envd/driver/app/proto/agent, re-measured excluding `de7a1c71a9` and port bulk commits.
- No regression in the cold `cargo run --bin omp` build gate and no new runtime IPC hop on the tool-call path.

## Implementation gate

This ADR does not authorize production changes. Each numbered decision item becomes a separate, reviewable task, and may begin only after owner review accepts this ADR and the baseline measurements above are recorded.
