# 0049. Tiered integration architecture: lean authority core, generic capability crates, neutral provider contracts, vendors as extensions

Status: accepted 2026-11-09. Implementation is gated by Phase 0 (see "Implementation gate").
Date: 2026-11-09
Area: runtime/extensions/integrations

## Context

Accepted and proposed decisions point the same way, but none says **where a new capability belongs**. So the periphery keeps landing in `omp-envd`, and vendor code keeps landing in core crates.
- [ADR 0045](0045-envd-responsibility-boundaries.md) (accepted 2026-10-28, Alternative F) preserves the authority core and extracts the periphery. Vocabulary extraction, modularization and peripheral extraction are open.
- [ADR 0046](0046-recover-v1-extensibility-without-centralization.md) recovers v1 locality: a table-driven roster, a contract crate, additive events, extension UI host arms, and periphery outside envd.
- [ADR 0024](0024-small-permanent-roster.md)/[0025](0025-long-tail-behind-stable-surfaces.md) keep a small, latched wire roster and push the long tail behind `dyn`.
- [ADR 0036](0036-python-for-extensions.md) keeps extensions as embedded Python only.
- [ADR 0040's 2026-11-05 amendment](0040-remote-build-and-test-execution.md#amendment-2026-11-05-vendor-integration-leaves-core) records the owner's ruling: **vendor integration moves outside core as an extension; core keeps only the generic, extensible ground.** It also defines one in-core `Delegated` adapter for a backend outside the binary.
- [ADR 0047's 2026-11-05 amendment](0047-agent-build-execution-model.md#amendment-2026-11-05-vendor-clients-leave-core) extends the vendor-name prohibition to every core crate.

**Owner ruling, 2026-11-09:** the 2026-11-05 vendor rule applies to forge and CI vendors too, not only to execution vendors.

### Evidence (at `omp2` @ `278d636`)

| Fact | Source |
|---|---|
| envd has no optional dependencies; its only feature is `local-tts`. The periphery compiles in every envd build regardless of runtime use | `crates/envd/Cargo.toml` |
| The `omp` binary closure is 1,214 crates; envd's alone is 1,164 | `Cargo.lock` graph |
| `omp-exec-local` uses only `omp_envd::exec::{ExecError, ExecEvent, ExecHost, ExecRun}`: closure 1,165 crates, 284 without envd | `crates/exec-local/src`, `Cargo.lock` |
| `omp-exec-ssh` uses only `omp_envd::ssh::SshService`: 1,165 crates, 200 without envd. It has no composer since PR #486 | `crates/exec-ssh/src`, ADR 0040 amendment |
| `omp-desktop` (`computer`) is used only by envd and adds 67 crates to `omp`, many of them Linux-only | `Cargo.lock` |
| `russh`/`russh-sftp` are used only by envd and add 30 crates | `Cargo.lock` |
| `candle` adds 29 crates through `omp-ai`, enabled by `app`'s `default = ["local"]` | `crates/app/Cargo.toml` |
| `omp-ai/realtime` (WebRTC/DTLS/Opus) is used by `app` voice, so it earns its weight | `crates/app/src/{chat_voice.rs,voice/synth.rs}` |
| GitHub vendor code sits in core: `envd/src/github.rs`, `github_url.rs`, the `github@3` tool contract, and the `Issue`/`Pr` schemes documented as "a cached GitHub issue/pull request" | `crates/{envd,tools}/src`, `crates/tools/src/read/resolver.rs` |
| Built-ins are closed at compile time (`Scheme` + `url-vocab.json`); the unknown-scheme fallback (`install_unknown_fallback` → `HostUriResolver`) already resolves schemes declared by the RPC host, with selectors in core; extensions cannot declare schemes today | `crates/tools/src/read/resolver.rs`, `crates/tools/url-vocab.json`, `crates/py/build.rs`, `crates/envd/src/tool_url.rs`, `omp/urls.py`; [ADR 0050](0050-extension-declared-url-schemes.md) |
| A Python extension can mount an MCP server (`omp.mcp.mount`), whose resources are readable as `mcp://` | `crates/py/python/omp/mcp.py`, ADR 0027 |
| 120 Python ports (113 v1 extensions plus 7 probes) were written 2026-08-20..24 to measure API gaps and deleted 2026-09-03 (`00611047`). 58 have no native equivalent today | `00611047c1^:examples/` |
| The gaps that blocked those ports are closed: `omp.Device.subtool`, `Device.mount` and `omp.creds.mint_scoped` exist | `crates/py/python/omp/{devices,creds}.py` |

Crate counts come from the `Cargo.lock` graph, which unifies platforms and features. They are proxies. Phase 0 replaces them with `cargo build --timings`.

## Decision

### D1. One placement rule, four tiers
Every capability belongs to exactly one tier: **the lowest tier that satisfies its requirements.**

| Tier | What lives here | Admission test | Location | Build cost |
|---|---|---|---|---|
| **T0 Authority core** (`omp-envd`) | Document authority, exec and kill boundary, sandbox and egress, admission decision function, blobs and generations, Python supervision, MCP transports (audit §8 invariants) | Needs a single owner, a kill boundary or the effect gate | `omp2` | Always compiled |
| **T1 Generic capability crates** | Vendor-neutral capabilities and contracts: `browser`, `computer`, `tts`, `omp-forge`, `omp-ci`, the `Delegated` adapters, generic protocols (git, REAPI, SSH, MCP) | Names **no vendor**, **and** needs one of: a first-class scheme with its own selectors or cache; repository-lock or worktree coordination; a hot-path budget; holding raw credentials | `omp2`, one crate each | Behind a Cargo feature `cap-<name>`, default-on in `omp-app` only |
| **T2 First-party extensions** | **All vendor integrations** (GitHub, GitLab, Gitea, Bitbucket, BuildBuddy, Jenkins…), plus maintained long-tail workflows, policy, UI and provider data | Expressible with the frozen `omp.*` API, the T1 contracts and declared effects | **Outside `omp2`**, in a first-party extensions repository | Zero Cargo weight |
| **T3 Third-party** | Python extensions, MCP servers | Declares effects; obeys trust tiers | Anywhere | Zero |

Placement rules:
- **Vendor integration code (D6 class A) never enters T0 or T1.** Inference and search transports follow providers-as-data (D6 class B). Vendor *data* may flow through T1 as operator or extension data: host patterns, env-var names, API base paths, ref conventions. Vendor *code* may not.
- **Tier is not roster.** T1 tools still ride `dyn` unless they pass ADR 0024's permanent-roster bar. No tier mutates the latched wire roster or schemas at runtime (Locked Deviation).
- **New capabilities default to T2.** Promotion to T1 needs an evidenced reason from the T1 test, recorded in the crate README. A capability that no longer meets its reason is demoted.

### D2. Neutral contracts in core; vendors behind one `Delegated` adapter
This follows the execution precedent (ADR 0040 Part A.1 as amended: `Local | Reapi | Ssh | Delegated`).

1. **Pure contract crates** hold types, traits and pure logic, with no I/O and no vendor names:
   - **`omp-forge`**: `RepoRef`, `CommitRef`, `ChangeRequest`, `Issue`, `Check`/`CheckState`, `ForgeChecks`, change-request and issue read/write operations, `required_checks`, and which operations **mutate**.
   - **`omp-ci`**: `Pipeline`, `Job`, `CiStatus`, `CiProvider`, `Capabilities` (`rerun_failed`, `rerun_job`, `cancel`, `trigger_with_inputs`, `artifacts`, `log_stream`, `play_manual`), and a pure `route(details_url)`.
2. **One in-core adapter per contract.** `omp-forge-delegated` and `omp-ci-delegated` (T1) route each request by host to the extension that declared it, and enforce security in core:
   - mutation flags come from the contract, not from vendor code;
   - approvals, egress host checks and result bounding are applied in core.
3. **Vendor adapters are T2 extensions.** They implement the contract through a documented `dyn` device schema (`forge/*`, `ci/*`) over `toolhost.v1`, or through an MCP server they mount. Adding a vendor edits no `omp2` file.
4. **Repository work stays generic and in core.** An adapter returns *data*: clone URL, ref spec, head SHA, base. Core performs fetch, worktree checkout under the repository mutation lock, and push, using `omp-vcs` and the user's git credential helpers. `pr_checkout` and `pr_push` therefore keep their isolation and lease guarantees without vendor code.
5. **Schemes are generalized, not added.**
   - `pr://` and `issue://` keep their names. Their meaning changes from "a cached GitHub pull request/issue" to "a forge change request/issue".
   - They are resolved by the `Delegated` resolver to the adapter owning the host, with core-side caching and selectors (`/diff`, comments).
   - The built-in vocabulary does not grow. A GitLab merge request is `pr://gitlab.example.com/group/project/42`. Extension-declared schemes follow [ADR 0050](0050-extension-declared-url-schemes.md).
6. **Tools become neutral.** `github@3` is replaced by neutral `forge@1` and `ci@1` contracts, served by the `Delegated` adapters. `run_watch`, checks, rerun, cancel, dispatch and artifacts move to `ci@1`. ADR 0024 roster review applies.
7. **Credentials are host-scoped.**
   - The resolution order (stored credential for the canonical host only → host-family env vars → git credential helper) stays in core.
   - Env-var names and canonical hosts are **adapter data**, not core constants.
   - Adapters receive only `omp.creds.mint_scoped` short-TTL, scope-limited credentials for providers listed in `credentials.allow`.
8. **A second adapter proves the contract** before it is declared stable. The GitLab adapter (T2) lands before the GitHub adapter replaces the in-core code.

**Vendor-lock register** (reviewed whenever an adapter is added):

| Area | Today | Target |
|---|---|---|
| Model providers | providers as data | keep |
| Remote execution | generic REAPI, plus BuildBuddy code in core | generic REAPI in core; BuildBuddy is an extension behind `Delegated` (ADR 0040 amendment) |
| Forge | GitHub code in envd and the tools contract | `omp-forge` + `Delegated`; GitHub and GitLab are extensions |
| CI | Actions-only `run_watch` | `omp-ci` + `route()` + `Delegated`; vendors are extensions |
| Tool interop | MCP | keep as the T3 standard and as a T2 transport |

### D3. Build weight: cut edges, gate capabilities
Each item is one reversible change.
1. **`ExecHost` contract → `omp-exec`.** Move `ExecError`, `ExecEvent`, `ExecHost` and `ExecRun` out of envd; `omp-exec-local` drops `omp-envd` (≈284 crates, from 1,165).
2. **`ssh` → `omp-ssh`** (ENVD-07, widened to include the `exec-ssh` edge). `omp-exec-ssh` drops `omp-envd` (≈200 crates), and envd drops `russh`/`russh-sftp` (30).
3. **Vocabulary contract crate** (ENVD-02, ADR 0046 item 2). envd stops depending on `omp-agent`.
4. **Table-driven roster** (ADR 0046 item 1). A T1 crate registers through a construction-table row plus `RegistryBridges`/`DynamicToolFactory`/`ContentResolver`, and edits no envd file.
5. **Capability features.**
   - `cap-computer` (`omp-desktop`), `cap-browser` (`omp-webview`), `cap-tts`/media, `cap-forge`, `cap-ci`.
   - Each is default-on in `omp-app`, and absent from envd, `exec-*` and `driver` tests and from headless builds.
   - A compiled-out capability is absent from the construction table; it never becomes a runtime toggle.
6. **Vendor code leaves the workspace**: `envd/src/github*.rs` (Phase 4) and `omp-exec-buildbuddy` plus the vendor items in `omp-exec-reapi` (ADR 0040 amendment).
7. **`local` (candle) default** is an owner choice, recorded with timings.

CI builds the default `omp` profile and a minimal profile with all `cap-*` off.

### D4. Security is preserved or strengthened, never traded for lightness
- Raw provider tokens stay in core. envd keeps scrubbing provider tokens from children. Extensions get only `mint_scoped` credentials.
- Effects are declared:
  - T1 tools declare precise effects (fetch hosts, mutation).
  - T2/T3 manifests declare `capabilities`, fetch hosts and credentials.
  - Anything undeclared stays `Effects::unknown()`, which means the exec approval tier.
- Every mutation is approval-gated in core: forge writes, CI rerun/cancel/dispatch, pushes. The `Delegated` adapter applies the contract's mutation flags, so a vendor adapter cannot mark a write as a read.
- Sandbox egress per host (ADR 0028), the document authority and the effect gate are unchanged. No tier bypasses them.

### D5. Rescue the extensions deliberately, outside the core repository
- **A first-party extensions repository**, outside `omp2`, holds the T2 extensions as locked and signed wheels (`crates/ext`). That covers vendor adapters and the rescued ports.
- **Conformance CI.**
  - An `omp2` CI job checks out that repository at a pinned revision and runs every extension against the frozen `omp.*` API using the QA fixture harness (`scripts/qa/fixtures/extensions`).
  - An API change that breaks a maintained extension fails `omp2` CI. That missing guard is what made the 2026-08 ports obsolete.
- **Triage the 58 ports without a native equivalent.** Restore a first wave of 6–8 with distinct surfaces and real demand, from `git show 00611047c1^:examples/<name>/`.
  - Candidates: `pr-review`, `green-loop`, `workflows`, `remote-approve`, `search-provider`, `provider-pack`, `fuzzy-index`, `spec-flow`.
  - Each must pass on the current API, declare its effects and have an owner.
- **UI-bound ports** (`sidebar`, `hover-cards`, `welcome-chrome`) wait for ADR 0046 item 4.
- The 34 ports with a confirmed native equivalent are not restored.

### D6. Vendor-code inventory: what leaves, what stays

"Vendor code" means code that names a vendor. It falls into three classes, and only class A leaves `omp2`. Inventory at `278d636`:

| Class | Rule | Code |
|---|---|---|
| **A. Vendor integration** (acts on a vendor's account, repository or service API) | Leaves `omp2`. It is replaced by a neutral contract plus `Delegated` (D2), and the vendor part becomes a T2 extension | Forge: `envd/src/github.rs`, `envd/src/github_url.rs`, `tools/src/github.rs` (`github@3`), `cache/src/github_cache.rs`, `chat/src/cards/github.rs`, `chat/src/autocomplete/github_refs.rs`, `chat/src/gitwatch.rs` (spawns `gh`). Execution: `crates/exec-buildbuddy`, `exec-reapi/src/buildbuddy_cancel.rs`, `exec-reapi/proto-buildbuddy` (ADR 0040 amendment) |
| **B. Inference and search transports** | Stays. Governed by the Locked Deviation "providers-as-data": providers are catalog data, and codec code exists only for genuinely different wire protocols. Many interchangeable providers is not lock-in | `ai/src/codec/*` (model and `search_*` codecs), `ai/src/auth/**` (OAuth flows, including `github_copilot`, `gitlab`, `devin`), `ai/src/operation/usage/*`, `ai/src/provider/*`, `catalog/compat/providers/*.kdl` |
| **C. Content shaping and safety data** | Stays, as data or tables. No vendor API is called | `tools/src/read/web/scrapers/{github,github_gist,gitlab}.rs` (page extraction, like `arxiv`/`crates_io`); `minimizer` `gh` filter (output shaping, like other CLI filters); `envd/src/admission.rs` `gh` check and `envd/src/child_env.rs` `GITHUB_TOKEN` scrub (D4 safety) |
| **Recorded exception** (owner ruling 2026-11-09) | Stays until an exit condition holds | `envd/src/codex_app_server/*` speaks Codex's own agent-session protocol (threads, turns, approvals), which no other vendor implements. It is not an OpenAI-compatible model API; that API is class B. An extension would add a new executor surface and one IPC hop per streamed event, and would save almost no build weight, because spawn, kill, approval mapping and policy stay in core either way. **Exits:** (1) a maintained Codex ACP route with no loss on the gaps ADR 0043 lists (system prompt on `turn/start`, approval and network policy mapping): then delete the module and add Codex as an ACP profile; (2) a delegated-executor extension contract exists for another reason: then move the module behind it |
| **T1 generic protocol** | Stays | `envd/src/acp_client/*`, `app/src/acp_client.rs` and `driver/src/delegated_acp.rs` speak generic ACP, and their CLI profiles are already data (`sv_delegated_<cli>_program`) |

Class A items move in Phase 4. The `chat` items move to the neutral contract: cards and autocomplete render `forge@1` data, and `gitwatch` reads change-request state through `omp-forge-delegated` instead of spawning `gh`.

## Sequencing

| Phase | Work | Backlog | Exit criterion |
|---|---|---|---|
| 0 Baseline | `cargo build --timings` (cold and one-line incremental) for `omp`, `omp-envd`, `omp-exec-local`, `omp-exec-ssh`; per-platform closure counts | ENVD-01 extension | Recorded in P8 |
| 1 Decouple | D3.1, D3.2, D3.3 | ENVD-16 (new), ENVD-07, ENVD-02 | `exec-*` closures ≤ 300 crates with no `omp-envd`; envd without `omp-agent` |
| 2 Locality | D3.4; `server.rs`/`tools.rs`/`mcp` splits | 0046 item 1, ENVD-03/04 | Adding a test capability edits 0 envd files |
| 2S Extension schemes | Extension-declared URL schemes ([ADR 0050](0050-extension-declared-url-schemes.md)): manifest `schemes`, additive `omp.schemes`, served by the existing unknown-scheme fallback. One PR per S1–S5 | ADR 0050 S1–S5 | Blocked on owner acceptance of ADR 0050 |
| 3 Generic capabilities | D3.5, one capability per change | ENVD-08/09 | Each removes an envd dependency edge; the minimal profile builds |
| 4 Neutral forge/CI/exec | `omp-forge`, `omp-ci`, `Delegated` adapters, `forge@1`/`ci@1`, generalized `pr://`/`issue://`; GitLab adapter (T2) proves the contract; GitHub adapter (T2) reaches parity; then remove every D6 class A forge item; BuildBuddy behind the execution `Delegated` adapter, then remove the D6 class A execution items | ENVD-09 (github item), ADR 0040 amendment | Parity suite green on the GitHub adapter; no D6 class A code in any `omp2` crate |
| 5 Extensions | Extensions repository, conformance CI job, manifest effect checks, rescue wave 1; ADR 0046 item 4 for UI ports | 0046 item 4 | Wave-1 extensions green against the frozen API |

Every step is one self-contained, reversible change, with no shims or re-exports and all callers migrated (AGENTS.md). Until Phase 4 completes, the existing in-core GitHub code stays and receives fixes (for example host-scoped API authentication) as ordinary maintenance.

## Non-goals
- No new daemons, no event bus, no thin supervisor (ADR 0045 alternatives C, D and E stay rejected).
- No JS/TS runtime, no dynamic tool schemas, no runtime roster mutation (Locked Deviations).
- No new built-in schemes for vendor concepts. Extension-declared schemes follow [ADR 0050](0050-extension-declared-url-schemes.md).
- Document authority never leaves envd.
- No bulk restore of the 2026-08 ports.

## Alternatives considered
| Alternative | Why not |
|---|---|
| Vendor adapters as first-party Rust crates in core (this ADR's first draft) | Contradicts the 2026-11-05 vendor rule as extended by the owner on 2026-11-09 |
| Keep adding capabilities natively in envd | Repeats the weight and inversion costs ADR 0045 classifies as defects |
| Everything as extensions, including repository work | Loses worktree, lock and precise-effect guarantees; weakens security |
| Bulk-restore all ports | Unowned code against a moving API; repeats the 2026-09-03 deletion |
| Vendor-specific tools and built-in schemes (`gitlab@1`, `mr://`) | N parallel contracts and prompt surfaces; grows the closed built-in vocabulary; lock-in by shape. An adapter may document merge requests as `pr://…` |

## Consequences
**Easier:**
- Adding a forge or CI vendor edits no `omp2` file.
- Adding a generic capability edits no envd file.
- `exec-*`, envd tests and headless profiles build much smaller graphs.
- The extension API gains maintained first-party users and a CI guard.

**Prohibited:** vendor integration code (D6 class A) in any `omp2` crate; new built-in schemes for vendor concepts; extension-held raw provider tokens.

**Costs accepted:**
- Two contract crates and two `Delegated` adapters.
- A new device schema (`forge/*`, `ci/*`) added to the frozen Python surface, additively.
- A `github@3` → `forge@1`/`ci@1` migration with roster review.
- A parity suite for the GitHub adapter.
- An extensions repository with owners.
- A feature matrix to keep green.

**Unchanged:** journal schema and `env.proto` frames. `toolhost/v1` and the Python ABI change only additively.

## Acceptance metrics
- Cold `cargo run --bin omp` build time does not regress; the minimal profile is measurably smaller.
- `omp-exec-local` and `omp-exec-ssh` closures are ≤ 300 crates with no `omp-envd`.
- Adding a T1 capability edits 0 files in `crates/envd/src`.
- Adding a forge or CI vendor edits 0 files in `omp2`.
- None of the D6 class A paths exists; recorded exceptions are on the allowlist only while no exit condition holds. A lintx rule rejects new vendor-integration modules outside the class B and C allowlist.
- No new RTT on the tool-call path (P8 W1–W9 unchanged or better).
- An e2e test proves no raw provider token is visible in an extension process.

## Implementation gate
- The owner accepted this record on 2026-11-09.
- Phase 0 must be recorded before Phase 1 starts.
- Each later phase starts only when the previous exit criterion is met.
- ADR 0046 acceptance (2026-11-09) authorizes its items 1, 2 and 4 under this sequencing.
- ADR 0040 and ADR 0047 keep their own acceptance gates; this record amends them but does not accept them.

## Status in omp

**Not started.** No code implements this record at `278d636`.

## References
- ADRs 0024, 0025, 0027, 0028, 0036, 0040 (2026-11-05 amendment), 0045, 0046, 0047 (2026-11-05 amendment), 0050 (proposed; Phase 2S).
- `docs/research/envd-responsibility-audit.md`; `docs/research/exec-out-of-core.md`; `docs/research/omp-v1-vs-omp2-architecture.md`.
- Deleted ports: `00611047c1^:examples/`.
