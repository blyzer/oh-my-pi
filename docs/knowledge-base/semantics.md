# Semantic domain → symbol map

Maps **meaning** (what the system is doing) to **owning crates and anchor symbols**.
Use [`lookup.json`](lookup.json) for exact `name → path:line` resolution; this file is the
human/agent orientation layer.

Regenerate the machine indexes with:

```sh
python3 scripts/knowledge-base/gen-symbols.py
```

Method note: indexes are a **column-0 `pub` textual scan** (module-level API surface), not a
full rustc AST. Prefer rust-analyzer / LSP when you need precise resolution, impls, or
private items.

---

## Cross-crate import anchors

| Symbol | Kind | Owning path | Semantics |
| --- | --- | --- | --- |
| `Journal` | struct | [`crates/journal/src/lib.rs`](../../crates/journal/src/lib.rs) | Append-only `.oms` authority + blob CAS |
| `Dom` | struct | [`crates/dom/src/txn.rs`](../../crates/dom/src/txn.rs) | Materialized session tree |
| `Session` | struct | [`crates/session/src/session.rs`](../../crates/session/src/session.rs) | Sole journal+DOM owner; fold/subscribe/rewind |
| `Kernel` | struct | [`crates/agent/src/loop.rs`](../../crates/agent/src/loop.rs) | Journal-first turn loop |
| `Inference` | trait | [`crates/agent/src/loop.rs`](../../crates/agent/src/loop.rs) | Capability the kernel calls for chat streams |
| `Up` | enum | [`crates/agent/src/steering.rs`](../../crates/agent/src/steering.rs) | Mailbox messages into the kernel |
| `Director` | trait | [`crates/agent/src/director.rs`](../../crates/agent/src/director.rs) | Cross-turn control plane (slots/yields) |
| `Dispatcher` | struct | [`crates/agent/src/dispatch.rs`](../../crates/agent/src/dispatch.rs) | Tool batch execution over the session |
| `compose_kernel` | fn | [`crates/driver/src/headless/kernel.rs`](../../crates/driver/src/headless/kernel.rs) | Production DI composition boundary |
| `Tool` | trait | [`crates/tool/src/lib.rs`](../../crates/tool/src/lib.rs) | Versioned tool contract (`name@rev`) |
| `Registry` (tools) | struct | [`crates/tool/src/registry.rs`](../../crates/tool/src/registry.rs) | Tool identity registry + advertise |
| `EnvClient` | struct | [`crates/env/src/client.rs`](../../crates/env/src/client.rs) | Typed env protocol client (no host resources) |
| `ProjectEnvironment` | struct | [`crates/envd/src/lib.rs`](../../crates/envd/src/lib.rs) | Attach/start project envd host |
| `EnvServer` | struct | [`crates/envd/src/server.rs`](../../crates/envd/src/server.rs) | Live environment authority |
| `Client` (ai) | struct | [`crates/ai/src/client.rs`](../../crates/ai/src/client.rs) | Tower inference facade |
| `ChatEvent` | enum | [`crates/ai/src/event.rs`](../../crates/ai/src/event.rs) | Canonical provider-decoded stream events |
| `Catalog` | struct | [`crates/catalog/src/snapshot.rs`](../../crates/catalog/src/snapshot.rs) | Providers-as-data snapshot |
| `Ctx` | struct | [`crates/con/src/ctx.rs`](../../crates/con/src/ctx.rs) | Convar/command-stream context |
| `Host` (chat) | struct | [`crates/chat/src/host.rs`](../../crates/chat/src/host.rs) | Projection-only TUI chat actor |
| `run` (app) | fn | [`crates/app/src/lib.rs`](../../crates/app/src/lib.rs) | Process presentation entry |

Ambiguous names (same identifier, different crates) — always disambiguate via `lookup.json`:

| Name | Typical owners |
| --- | --- |
| `Session` | `session` (authoritative), also `edit`, `tools` |
| `Registry` | `tool` (tools), `ai` (inference providers), others |
| `Client` | `ai`, plus transport/oauth clients |
| `Error` / `*Error` | nearly every crate — use path |

---

## Domain: state authority

**Owns:** durable truth and the derived tree.

| Crate | Role | Key symbols |
| --- | --- | --- |
| `journal` | `.oms` + blob CAS | `Journal`, `Entry`, `EntryId`, `BlobStore`, `Kind` |
| `dom` | Materialized tree | `Dom`, `Txn`, `Handle`, `Op`, `Snapshot`, `Event`, `Patch` |
| `vocab` | Closed tag/prop vocabulary | `KnownTag`, `Tag`, `PropId` |
| `session` | Journal-first API | `Session`, `Component`, `ComponentRegistry`, `project_thread` |

Semantics: append journal first → fold exact entry into DOM → actors subscribe. Never a parallel
mutable turn-state store.

---

## Domain: control plane

**Owns:** turn policy, settings, composition.

| Crate | Role | Key symbols |
| --- | --- | --- |
| `agent` | Kernel loop | `Kernel`, `Director`, `Dispatcher`, `JobBoard`, `CancelTree`, `ApprovalDesk`, `HookGate` |
| `driver` | Production composition | `compose_kernel`, `KernelOptions`, `SessionRegistry`, `ProductionInference` |
| `con` | Typed settings stream | `Ctx`, `CVar`, `VarSpec`, `CmdSpec`, `Replica` |

Built-in directors (under `crates/agent/src/directors/`): `advisor`, `compaction`, `force_tool`,
`goal`, `loop_mode`, `plan`, `prewalk`, `stream_rules`, `todo_reminder`, `vibe`, …

---

## Domain: inference

**Owns:** model routing and canonical stream events — not session state.

| Crate | Role | Key symbols |
| --- | --- | --- |
| `ai` | Tower spine | `Client`, `ChatRequest`, `ChatStream`, `ChatEvent`, `Planner`, `Registry` |
| `catalog` | Compatibility data | `Catalog`, `ModelSpec`, `ProviderId`, `ModelKey`, `Availability` |
| `scribe` | Banded prompts | template/props types in `crates/scribe` |

Semantics: providers-as-data; no `model_id.contains` in Rust; vendor frames decode to `ChatEvent`.

---

## Domain: tools & environment

**Owns:** tool contracts, executors, host resources.

| Crate | Role | Key symbols |
| --- | --- | --- |
| `tool` | Contracts only | `Tool`, `ToolSpec`, `ToolIdentity`, `IncomingParams`, `Registry` |
| `tools` | Resource-owning builtins | per-tool modules (`bash`, `edit`, `read`, …) |
| `env` | Client protocol | `EnvClient`, `Admitter`, `SCHEMA_REV` |
| `envd` | Host daemon | `EnvServer`, `ProjectEnvironment`, `ExtHostSupervisor`, `WorkspaceHost` |
| `shell` / `shell-builtins` | In-process bash | parser/runtime + coreutils |

Semantics: `ArgsCommitted` is the effect gate. Envd owns FS/process/document/tool authorities;
`omp-env` never owns host resources.

---

## Domain: presentation

**Owns:** projections only — never controller state.

| Crate | Role | Key symbols |
| --- | --- | --- |
| `chat` | Session actor UI | `Host`, `HostCommand`, `HostAction` |
| `tui` | Retained engine | components, terminal, debug protocol |
| `app` | CLI/ACP/RPC adapters | `run`, `cli::run`, `Command` |
| `macros` | `dom!` / `view!` | proc macros |

---

## Domain: extensions

| Crate | Role | Key symbols |
| --- | --- | --- |
| `ext` | Manifests, locks, trust | deployment/lock/trust types in `crates/ext` |
| `py` | Embedded CPython 3.14t | `Engine`, frozen stdlib |
| `envd` exthost/worker | Supervised hosts | `ExtHostSupervisor`, `WorkerSupervisor` |

---

## Domain: transport & embed

| Crate | Role | Key symbols |
| --- | --- | --- |
| `proto` | Wire schemas | generated `omp.*.v1` packages |
| `rpc` | Framing / health / UDS | handshake, NDJSON RPC helpers |
| `serve` | gRPC service projections | inference/auth/blob servers |
| `sdk` | Native embedding facade | session/callback facades |

---

## How to find “any class”

1. **Know the name** → open [`lookup.json`](lookup.json) and search the key (exact Rust ident).
2. **Know the domain** → use the tables above, then open [`by-crate/<crate>.jsonl`](by-crate/).
3. **Need impls / private items / trait resolutions** → rust-analyzer LSP (`definition`,
   `references`, `implementations`) — the index is a memory aid, not a compiler.
4. **Need architecture “why”** → [`../knowledge-base.md`](../knowledge-base.md) and
   [`../adr/`](../adr/).
