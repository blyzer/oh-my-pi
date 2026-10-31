# OMP codebase knowledge base

Navigable map of the omp² workspace: architecture, crates, ADRs, process model, and developer
entry points. Prefer the linked source docs when a detail here conflicts with them.

**One-sentence product bet** ([PHILOSOPHY.md](../PHILOSOPHY.md)): build a coding harness so good
that the model is the only replaceable part.

### Symbol / semantic memory

For “where is this type?” and domain ownership, use the machine index under
[`knowledge-base/`](knowledge-base/):

| File | Use |
| --- | --- |
| [`knowledge-base/semantics.md`](knowledge-base/semantics.md) | Domain → crate → anchor symbols |
| [`knowledge-base/lookup.json`](knowledge-base/lookup.json) | Exact name → path:line (all module-level `pub` items) |
| [`knowledge-base/by-crate/`](knowledge-base/by-crate/) | Per-crate JSONL API dumps |
| [`knowledge-base/summary.json`](knowledge-base/summary.json) | Counts + `lib.rs` module trees |

Regenerate: `python3 scripts/knowledge-base/gen-symbols.py`

| Index | Jump |
| --- | --- |
| Architecture | [Production spine](#production-spine) · [Five layers](#five-layer-topology) · [One turn](#one-turn-kernel) |
| Decisions | [Locked deviations](#locked-deviations-from-pi) · [ADR ledger](#adr-ledger) |
| Catalog | [Crates](#crate-catalog) · [Docs map](#canonical-docs-map) |
| Runtime | [Processes](#process-topology) · [IPC](#key-ipc-boundaries) · [Config](#config--identity) |
| Dev | [Commands](#commands) · [E2E proofs](#e2e-proofs) · [House rules](#house-rules-that-reject-prs) |

---

## Production spine

A live turn has one composition and one state authority
([docs/architecture/crates.md](architecture/crates.md)):

```text
omp-app command adapter
  → omp-driver::compose_kernel
    → omp-agent::Kernel
    → omp-session::Session
        → append to omp-journal (.oms)
        → fold the exact entry into omp-dom
    → omp-chat actor over Session::subscribe()
```

### Ownership rules

1. **Driver composes; app presents.** Libraries never build a second production stack.
2. **Session appends first**, then folds the exact returned journal entry; replay uses the same fold.
3. **Actors project only** — snapshots + events in; `Up` mailbox out. They never hold session authority.
4. **Envd owns host resources**; `omp-env` is the typed client only.
5. **Catalog owns compatibility**; inference owns translation and correction.
6. **Tools are versioned**; presentation consumes typed payloads.
7. **Transport projects** canonical state; `rpc`/`serve` invent no second dialect or store.

---

## Five-layer topology

Strictly acyclic. Lower layers never import upper ones. Only layer 5 assembles the production stack.

| Layer | Owns | Example crates |
| --- | --- | --- |
| 1 Substrate | Primitives, wire, infra | `core`, `vocab`, `proto`, `http`, `walker`, `macros` |
| 2 Domain | State + editing models | `journal`, `dom`, `con`, `catalog`, `edit`, `scribe` |
| 3 Engines | Execution interpreters | `ai`, `shell`, `tool`/`tools`, `py`, `env`, `tui`, `exec*` |
| 4 Session | Kernel + daemons | `session`, `agent`, `envd`, `rpc`, `serve`, `sdk` |
| 5 Apps | Composition + proofs | `driver`, `chat`, `app`, `e2e` |

---

## One turn (Kernel)

`Kernel<C: Inference>` in `crates/agent` — transport-neutral; composed by driver; presented by
app/chat. Full mechanics: [architecture/agent-loop.md](architecture/agent-loop.md).

| Step | What happens |
| --- | --- |
| 1 Admission | Cancel/deadline; pause gate; drain `Up::Pause` / `Up::Cancel` |
| 2 Housekeeping | Director handoff; settled jobs; consume steering |
| 3 Projection | Prompt bands + thread + volatile trailing items + roster |
| 4 Preflight | Director `before_inference` / `prepare_inference` / `watch_stream` |
| 5 Inference | `omp-ai` `ChatEvent` stream (vendor shapes never escape) |
| 6 Fold | Journal deltas → DOM; `tool.call` streaming → `PreparedCall` |
| 7 Tool batch | Dispatcher multiplex; read-only concurrent; mutate exclusive |
| 8 Boundary | Mailbox drain; Directors observe / may hand off |
| 9 Continue? | Tools/steering/pause → resample; else `before_yield` → Yield |

**Authorization boundary:** `ArgsCommitted` is the effect gate. `InvokeTool` and `ArgText` may
prepare work; effects wait for committed canonical arguments journaled on the call.

### Directors vs Components

- **Director** — behavior that keeps control across turns; a stack in the session DOM the loop only
  walks; arbitrates exclusive `Slot` claims (`mode`, `loop`, `tool_choice`, `worktree`) and convar
  binds. Stream rules (TTSR-like) are a Director (ADR 0038).
- **Component** — journal-derived durable state reducing to `<meta>`. Extensions use
  `@omp.director` / `@omp.component`. The old `@omp.regime` design was removed — do not reintroduce it.

---

## Locked deviations from pi

“pi does X” is never an argument for these. Enforcement summary: [AGENTS.md](../AGENTS.md)
“Locked Deviations from pi”. Reasoning: [docs/adr/](adr/).

| Area | Rule |
| --- | --- |
| Extensions | Embedded free-threaded CPython only — no JS plugin runtime |
| Shell | In-process bash + builtins; never shell out to `/bin/bash` |
| Edits | Through envd document authority (CAS + fuzzy 3-way), never raw disk I/O |
| Tools | Fixed wire roster; long tail via `dyn` devices; `name@rev` identities |
| Inference | Providers-as-data (catalog); no `model_id.contains` in Rust |
| Prompts | Scribe banded slots; volatile facts never in stable prefix |
| Control | Directors + Components — no regimes / campaign arbiter |
| Runtime | tokio + rayon only; candle for local ML — no C++ binding graphs |

---

## Crate catalog

Package names are `omp-` prefixed; directory names under `crates/` are not. Descriptions from each
crate's `Cargo.toml`.

### Layer 1 — Core substrate

| Dir | Package | Role |
| --- | --- | --- |
| `core` | `omp-core` | Compact `Str`/bytes, sparse collections, encodings, `slopjson` |
| `vocab` | `omp-vocab` | Closed DOM/TUI structural vocabulary |
| `proto` | `omp-proto` | Generated Protobuf + optional gRPC bindings |
| `http` | `omp-http` | Outbound HTTP pools and TLS policy |
| `secrets` | `omp-secrets` | Secret rules, placeholders, redaction |
| `oauth` | `omp-oauth` | Bounded OAuth discovery/PKCE primitives |
| `sandbox` | `omp-sandbox` | Fail-closed native process confinement |
| `audio` | `omp-audio` | Native audio capture/playback/metering |
| `desktop` | `omp-desktop` | Desktop capture, input, accessibility |
| `webview` | `omp-webview` | Embedded browser surfaces |
| `walker` | `omp-walker` | Filesystem traversal and discovery |
| `ar` | `omp-ar` | Lazy multi-format archive read/write |
| `vcs` | `omp-vcs` | gitoxide + jj behind one interface |
| `macros` | `omp-macros` | `dom!` / `view!` / `cached` proc macros |
| `cache` | `omp-cache` | Document, GitHub, MCP, secret caches |
| `snapcompact` | `omp-snapcompact` | Bitmap archive context compaction |

### Layer 2 — Domain & state

| Dir | Package | Role |
| --- | --- | --- |
| `journal` | `omp-journal` | Authoritative `.oms` journal + blob CAS |
| `dom` | `omp-dom` | Materialized session tree + patches |
| `con` | `omp-con` | Convars, cfg, binds, aliases, commands |
| `catalog` | `omp-catalog` | Provider/route/model capability catalog |
| `ast` | `omp-ast` | Tree-sitter analysis and AST edits |
| `edit` | `omp-edit` | replace/patch/hashline/sloppy edits |
| `scribe` | `omp-scribe` | Banded prompt template engine |
| `collab` | `omp-collab` | Encrypted room relay + replication |
| `memory` | `omp-memory` | Mnemopi banks, recall, retention |
| `adw` | `omp-adw` | ADW phase graphs and acceptance |

### Layer 3 — Engines

| Dir | Package | Role |
| --- | --- | --- |
| `ai` | `omp-ai` | Typed requests → Tower → `ChatEvent` |
| `shell` | `omp-shell` | In-process Bash parser/runtime |
| `shell-builtins` | `omp-shell-builtins` | In-process coreutils builtins |
| `tool` | `omp-tool` | Versioned tool contracts + registry |
| `tools` | `omp-tools` | Resource-owning built-in executors |
| `py` | `omp-py` | Embedded free-threaded CPython 3.14t |
| `py-link` | `omp-py-link` | Final-link flags for omp-py binaries |
| `env` | `omp-env` | Typed environment protocol client |
| `ext` | `omp-ext` | Extension config, locks, trust |
| `tui` | `omp-tui` | Retained terminal UI engine |
| `gui` | `omp-gui` | Native GPU window host |
| `exec` | `omp-exec` | Build/test placement + adapters |
| `exec-local` | `omp-exec-local` | Local ExecHost backend |
| `exec-reapi` | `omp-exec-reapi` | REAPI v2 remote execution client |
| `exec-buildbuddy` | `omp-exec-buildbuddy` | BuildBuddy Run API backend |
| `exec-ssh` | `omp-exec-ssh` | SSH execution backend |

### Layer 4 — Session & daemons

| Dir | Package | Role |
| --- | --- | --- |
| `session` | `omp-session` | Journal-first session API + fold |
| `agent` | `omp-agent` | Kernel, Directors, dispatch, jobs |
| `envd` | `omp-envd` | Project environment daemon/host |
| `observability` | `omp-observability` | OTel metrics, export, redaction |
| `rpc` | `omp-rpc` | gRPC framing, health, UDS, NDJSON RPC |
| `serve` | `omp-serve` | Inference/auth/blob gRPC projections |
| `sdk` | `omp-sdk` | Stable native embedding facade |

### Layer 5 — Apps & composition

| Dir | Package | Role |
| --- | --- | --- |
| `driver` | `omp-driver` | `compose_kernel` + discovery/cfg |
| `chat` | `omp-chat` | Session-subscriber chat actor |
| `app` | `omp-app` | CLI, TUI, ACP, RPC, daemon adapters |
| `e2e` | `omp-e2e` | Joined-system proofs P1–P14 |

---

## ADR ledger

Owner decisions live in [docs/adr/](adr/). Status table: [adr/STATUS.md](adr/STATUS.md)
(25 implemented, 19 partially implemented, 4 not started — as of that ledger).

### Foundations / State

| ADR | Title | Status |
| --- | --- | --- |
| [0001](adr/0001-design-envelope.md) | Four operating modes are the architecture tests | Partial |
| [0002](adr/0002-complexity-has-one-owner.md) | Complexity has one owner | Implemented |
| [0003](adr/0003-one-authoritative-session-tree.md) | One authoritative session tree from the journal | Implemented |
| [0004](adr/0004-lifecycle-derives-from-the-tree.md) | Lifecycle derives from the tree | Implemented |
| [0005](adr/0005-controller-actor-separation.md) | Controller / actor separation | Implemented |

### Runtime / Control

| ADR | Title | Status |
| --- | --- | --- |
| [0006](adr/0006-host-policy-sandbox-stub.md) | Host policy; bounded sandbox stub | Partial |
| [0007](adr/0007-subagent-filesystem-isolation.md) | Subagent filesystem isolation | Partial |
| [0008](adr/0008-tool-execution-is-a-state-stream.md) | Tool execution is a state stream | Implemented |
| [0009](adr/0009-bound-output-once.md) | Bound output once; spill to artifacts | Implemented |
| [0010](adr/0010-one-job-primitive.md) | One job primitive | Implemented |
| [0011](adr/0011-cancellation-needs-a-kill-boundary.md) | Cancellation needs a kill boundary | Implemented |
| [0012](adr/0012-convars.md) | Settings are convars | Implemented |
| [0013](adr/0013-inheritance-by-seeding-and-cfg.md) | Inheritance by seeding and cfg | Implemented |
| [0014](adr/0014-command-stream-binds-and-aliases.md) | Command stream binds and aliases | Partial |
| [0015](adr/0015-directors.md) | Directors own candidate yields | Implemented |
| [0016](adr/0016-semantic-requests-cross-layers.md) | Semantic requests cross layers | Implemented |
| [0038](adr/0038-stream-rules-as-a-director.md) | Stream rules as a Director | Partial |
| [0039](adr/0039-remote-control-and-factory-modes.md) | Remote control and factory modes | Partial |
| [0040](adr/0040-remote-build-and-test-execution.md) | Remote build and test execution | Partial |
| [0045](adr/0045-envd-responsibility-boundaries.md) | Envd responsibility boundaries | Partial |
| [0047](adr/0047-agent-build-execution-model.md) | Agent build execution model | Partial |

### Inference / Tools

| ADR | Title | Status |
| --- | --- | --- |
| [0017](adr/0017-compatibility-as-structured-knowledge.md) | Compatibility as structured knowledge | Implemented |
| [0018](adr/0018-provider-is-more-than-stream.md) | Provider is more than stream | Implemented |
| [0019](adr/0019-forced-tool-call-escalation.md) | Forced tool-call escalation | Implemented |
| [0020](adr/0020-charitable-argument-repair.md) | Charitable argument repair | Implemented |
| [0021](adr/0021-constrained-sampling-ownership.md) | Constrained sampling ownership | Partial |
| [0022](adr/0022-corrective-inference.md) | Corrective inference | Implemented |
| [0023](adr/0023-tiny-local-model.md) | Tiny local model role | Partial |
| [0024](adr/0024-small-permanent-roster.md) | Small permanent tool roster | Implemented |
| [0025](adr/0025-long-tail-behind-stable-surfaces.md) | Long tail behind stable surfaces | Partial |
| [0026](adr/0026-intent-and-versioned-tools.md) | Intent + versioned tools | Implemented |
| [0027](adr/0027-read-materializes-resources.md) | Read materializes resources | Implemented |
| [0028](adr/0028-bash-is-an-in-process-interpreter.md) | Bash is an in-process interpreter | Implemented |
| [0029](adr/0029-autoqa-report-issue.md) | AutoQA `report_issue` path | Partial |

### Interface / Stack

| ADR | Title | Status |
| --- | --- | --- |
| [0030](adr/0030-one-pass-rendering-pipeline.md) | One-pass rendering pipeline | Implemented |
| [0031](adr/0031-typed-component-model.md) | Typed component model | Partial |
| [0032](adr/0032-presentation-policy-in-the-renderer.md) | Presentation policy in the renderer | Implemented |
| [0033](adr/0033-verification-is-part-of-the-interface.md) | Verification is part of the interface | Implemented |
| [0034](adr/0034-transcript-is-a-protocol.md) | Transcript is a protocol | Partial |
| [0035](adr/0035-rust-for-the-engine.md) | Rust for the engine | Partial |
| [0036](adr/0036-python-for-extensions.md) | Python for extensions | Partial |
| [0037](adr/0037-acp-editor-io.md) | ACP editor I/O | Implemented |
| [0041](adr/0041-native-surfaces-over-tsp.md) | Native surfaces over TSP | Partial |
| [0042](adr/0042-apple-pcc-integration.md) | Apple PCC integration | Not started |
| [0043](adr/0043-native-cli-integration.md) | Native CLI integration | Partial |
| [0044](adr/0044-presentation-surface-and-agent-boundaries.md) | Presentation / surface / agent boundaries | Not started |
| [0046](adr/0046-recover-v1-extensibility-without-centralization.md) | Recover v1 extensibility without centralization | Not started |
| [0048](adr/0048-schema-and-serialization-boundaries.md) | Schema and serialization boundaries | Partial |

### Active gaps worth knowing

- **0001 / 0039** — remote-driver and factory modes incomplete
- **0042** — Apple PCC blocked on entitlement
- **0044 / 0046** — proposed; production changes gated on owner review
- **0035** — error-formatting lintx ratchet still migrating legacy sites
- **0040 / 0047** — `omp-exec` backends exist; some remote proofs still open

---

## Process topology

Full detail: [architecture/processes.md](architecture/processes.md). Presentation + agent live in the
main `omp` process. Project-scoped resources live in `omp envd`. Same binary re-enters hidden child roles.

| Process | Role | Spawned by |
| --- | --- | --- |
| `omp` (main) | app + driver + agent + TUI/GUI | User / SDK / shell |
| `omp envd` | Project environment owner | `ProjectEnvironment::connect_or_start` |
| `__omp-ext-host` | Python extension host | envd `ExtHostSupervisor` |
| `__omp-py-worker` | Placed Python worker | envd `WorkerProcess` |
| `__omp-eval-child` | Eval kernel child | envd eval |
| MCP / LSP / DAP | Foreign adapters | envd / docserver |
| `omp serve` | gRPC gateway (optional) | User daemon commands |

### Key IPC boundaries

| Boundary | Framing | Contract |
| --- | --- | --- |
| app ↔ envd | UDS varint protobuf | `omp.env.v1` ClientFrame/ServerFrame |
| envd ↔ docserver | local UDS / duplex | `omp.document.v1` |
| envd ↔ ext host CONTROL | FD3 length-prefixed JSON | CONTROL Request/Response |
| ext/worker ↔ envd DATA | capability-reduced env client | `omp.env.v1` + `InvocationScope` |
| Python worker | piped stdio WorkerFrame | `omp.toolhost.v1` family |
| Client ↔ serve | Tonic gRPC (UDS or bearer TCP) | gateway / inference / auth / blob |
| Embedding ↔ `omp rpc` | NDJSON stdio | ReadyFrame / RpcRequest (not gRPC) |

### Config & identity

- User config lives in `~/.o2` (override: `OMP_CONFIG_DIR`). Not `~/.omp`, not XDG.
- Workspace env vars are `OMP_*` only — never `PI_*`. Notable: `OMP_TUI_DEBUG`, `OMP_TTY`,
  `OMP_PY_SITE`, `PYO3_CONFIG_FILE`.

Extensions lifecycle: [architecture/extensions.md](architecture/extensions.md).

---

## Commands

`justfile` is the source of truth — use `just`, not raw cargo.

| Recipe | Purpose |
| --- | --- |
| `just setup-python` | One-time before linking `omp-py` |
| `just check-pkg <pkg>` | Targeted compile of one crate |
| `just test-pkg <pkg>` | nextest + doctests for one crate |
| `just check` / `just test` | Workspace compile / tests (minus e2e) |
| `just lint` | fmt + clippy + proto + lintx ratchets |
| `just e2e` | P1–P7, P9–P12, P14, `tool_sources`, then P8 |
| `just ci` | Local approx of CI format+rust gate |
| `just remote-test-pkg <pkg>` | BuildBuddy Linux runner over current checkout |

### Toolchain

| Item | Value |
| --- | --- |
| Rust | nightly pinned in `rust-toolchain.toml` |
| Edition | 2024 |
| Fmt | hard tabs, 3-col, width 100 |
| Tests | `cargo nextest` + `cargo test --doc` |
| Dev codegen | Cranelift (no panic landing pads) |
| Test codegen | LLVM via `[profile.test]` |
| Python | CPython 3.14t free-threaded (`just setup-python`) |
| Config dir | `~/.o2` |

---

## E2E proofs

Authority: [crates/e2e/README.md](../crates/e2e/README.md). CI gates P1–P12, P14, `tool_sources`.
P8 metrics schema gates; timings do not.

| Proof | Contract |
| --- | --- |
| P1 | Document lease races + rebase |
| P2 | CancelTree / interrupt journal consistency |
| P3 | One job primitive for detached work |
| P4 | Live schema + `tool.call` rev recording |
| P5 | Prompt-band prefix cache stability |
| P6 | Crash/replay torn-tail recovery |
| P7 | Real-PTY chat lifecycle (+ Seatbelt cases) |
| P8 | Non-gating performance recorder |
| P9 | Worktrees + extension Directors/Components |
| P10 | Idempotent historical tool lifts |
| P11 | Collab spectator over real relay |
| P12 | Daemon approval relay (macOS Seatbelt) |
| P14 | Session attach / observer convergence |
| `tool_sources` | Environment tool-source routing |

---

## House rules that reject PRs

Full policy: [AGENTS.md](../AGENTS.md).

- **Allocation** — Prefer `&T`. Use `omp-core` `Str`, SmallVec 2.0-alpha syntax, `FastHashMap`,
  `AppendVec`, `im::Vector` where the clone pattern pays. No hasher crates for discretionary keys.
- **Errors** — `thiserror` with `#[error]`; carry typed sources — never string-payload variants or
  `format!` of errors. App boundary: `miette`.
- **Async** — Unboxed async traits (RPITIT or TAIT). No `BoxFuture` on hot paths. Tower stacks
  allocate at construction. `flume` mailboxes; `parking_lot` locks.
- **Editing discipline** — Edit tracked files with editing tools — never sed/python rewrite scripts.
  Sonic for mechanical renames. `just`, not raw cargo.
- **Unicode/ANSI** — `xutf` only; no `unicode-*` / `ansi_*` utility crates.
- **Enum↔string** — derived strum; no hand-written match tables.

---

## Canonical docs map

| Path | Use when |
| --- | --- |
| [AGENTS.md](../AGENTS.md) | Mandatory conventions: allocation, async, TUI, locked deviations |
| [PHILOSOPHY.md](../PHILOSOPHY.md) | Two bets + eight subsystem lessons |
| [architecture/crates.md](architecture/crates.md) | Production spine ownership rules |
| [architecture/agent-loop.md](architecture/agent-loop.md) | Kernel turn mechanics |
| [architecture/processes.md](architecture/processes.md) | Process topology + IPC contracts |
| [architecture/extensions.md](architecture/extensions.md) | Signed Python extensions lifecycle |
| [adr/](adr/) | Owner decisions 0001–0048 + STATUS.md |
| [py/](py/) | Python extension SDK guides |
| [pyx/](pyx/) | pyx API reference |
| [crates/e2e/README.md](../crates/e2e/README.md) | P1–P14 proof contracts |
| [justfile](../justfile) | Source of truth for build/test/e2e recipes |
| [knowledge-base/](knowledge-base/) | Symbol lookup + semantic domain memory |
