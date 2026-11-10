# ADR 0049 Phase 3 / D3.5 — capability feature plan

Status: PR1 (`cap-computer`, PR #549), PR2 (`cap-browser`, PR #550), and PR3 (`cap-media`) implemented on this branch. Tip at planning: `5173e17090` (ENVD-04 #548).
Pattern source: ENVD-07 `cap-ssh` (PR #544).

## Goal

One reversible PR per capability. Each PR:

1. Adds a `cap-*` Cargo feature (default-on in `omp-app`, off in `omp-envd` defaults).
2. Makes the heavy crate an **optional** envd dependency (or removes a hard edge).
3. Removes the capability from the **construction table** when the feature is off — never a runtime toggle for “compiled out”.
4. Leaves default `omp-app` behavior unchanged (caps on).

Phase 3 exit (ADR 0049): each change removes an envd dependency edge; the **minimal profile** (all `cap-*` off) builds.

---

## 1. Current dependency edges

### Hard edges into `omp-envd` (must become optional)

| Crate | envd modules | Lines (approx) | Call sites outside envd |
|---|---|---|---|
| `omp-webview` | `browser_daemon.rs`, `browser_fetch.rs`, `browser_relay.rs` | ~6.1k | `driver/src/registry.rs` (`BrowserFetchAdapter`); `app/src/browser_relay_cmd.rs` |
| `omp-desktop` | `computer.rs` | ~1.9k | none (host-only) |
| `omp-audio` | `media_devices.rs`, `media_tts.rs` | ~2.5k | `app` (`say_cmd`, `chat_voice`, `audio_coordinator`) and `chat` keep their own `omp-audio` edges — same as `omp-ssh` staying linked in app when `cap-ssh` is off |

`crates/envd/Cargo.toml` today (hard deps):

- `omp-webview.workspace = true`
- `omp-desktop.workspace = true`
- `omp-audio.workspace = true`
- `cap-ssh = ["dep:omp-ssh"]` with `omp-ssh` optional (already landed)

### Related crates (not envd hard edges for these caps)

| Crate | Depends on | Role |
|---|---|---|
| `omp-app` | `omp-envd`, `omp-audio`, `omp-ssh` (always) | Forwards `cap-ssh`; owns voice/say CLI over `omp-audio` |
| `omp-driver` | `omp-envd` (no direct webview/desktop/audio) | `BrowserFetchAdapter` from `omp_envd::browser_fetch`; `SelectedRoster::production()` |
| `omp-tools` | neither webview nor desktop nor audio | Host-free contracts + **`ROSTER` construction table** (`SelectedRoster`). Chat cards import `omp_tools::{browser,computer}` types always |
| `omp-gui` | `omp-webview` | GUI examples only — out of Phase 3 envd path |
| `omp-chat` | `omp-audio` | Voice notices / cards — keep; not an envd edge |

### Construction table note

There is no symbol named `PRODUCTION_BUILTIN_ROSTER`. The production table is:

- `omp_tools::roster::ROSTER` (static `&[BuiltinRosterEntry]`)
- `SelectedRoster::production()` → full mask over `ROSTER`
- envd walks that selection in `tools/roster_host.rs`

Capability rows today (always present):

| Family | Phase | Bridges |
|---|---|---|
| `ImageGen` | Session | SEARCH \| BLOBS |
| `Tts` | Session | SEARCH \| GITHUB \| BLOBS |
| `Browser` | EnvironmentEarly | BROWSER \| BLOBS |
| `Computer` | EnvironmentEarly | COMPUTER \| BLOBS |
| `SecurityScan` | EnvironmentEarly | SECURITY \| GITHUB |

`browser` still has a **runtime** settings gate (`SV_BROWSER_ENABLED` / `browser_enabled()`) on top of the roster row. That gate stays valid **only while `cap-browser` is on**. With the cap off, the row must not exist — settings must not reintroduce it.

---

## 2. `cap-ssh` pattern to mirror

Landed shape (PR #544 / ENVD-07):

| Layer | Mechanism |
|---|---|
| `omp-envd` features | `default = []`; `cap-ssh = ["dep:omp-ssh"]`; `omp-ssh` optional |
| `omp-app` features | `default = ["local", "cap-ssh"]`; `cap-ssh = ["omp-envd/cap-ssh"]` |
| Construction absence | `#[cfg(feature = "cap-ssh")]` on URL resolver / `ResourceMutationServices` / docs SSH paths — scheme missing from the table when off |
| Reject path | `#[cfg(not(feature = "cap-ssh"))]` → `"ssh:// writes are not compiled into this environment"` (not a settings toggle) |
| Proof | `cargo check -p omp-envd` with selected unification (envd defaults → cap off); `rg russh crates/envd/Cargo.toml` empty; tree has no hard `russh` |

SSH is a **URL scheme**, not a `ROSTER` family. Browser/computer/media add one extra seam: **`cfg` the `ROSTER` row** (and install/declare arms that only exist for that row). Keep `omp_tools::{browser,computer,…}` contract modules compiled always so chat cards and journal replay still typecheck — same idea as app keeping `omp-ssh` linked when envd’s `cap-ssh` is off.

Feature-unification warning (from #544): workspace unification can keep a cap “on” when anything in the graph enables it. Minimal-profile and edge proofs must use:

```text
--config 'resolver.feature-unification="selected"'
```

---

## 3. Recommended first PR

**Do not bundle ENVD-08** (browser+computer). ADR D3.5: one capability per change.

**Recommend `cap-computer` as PR1**, then `cap-browser`, then media.

| Option | Verdict |
|---|---|
| Bundle ENVD-08 | Reject — violates “one capability per change”; harder revert |
| `cap-browser` first | Viable, but pulls `browser_fetch` (driver) + `browser_relay_cmd` (app) + three envd modules + relay tests |
| **`cap-computer` first** | **Preferred** — single envd module, sole hard edge `omp-desktop`, no driver/app CLI coupling, closest to a pure optional-dep cut |

If this branch must stay named for browser, either retarget the branch to computer or accept browser as PR1 for stack continuity; on technical merit computer is the smaller first cut.

Cargo feature naming for ADR’s `cap-tts`/media bullet: use **`cap-media`** (gates `tts` + `image_gen` + `omp-audio` in envd). Say so in the PR body so it maps to D3.5.

---

## 4. How roster rows `cfg` out

Put empty marker features on `omp-tools` and forward them from envd/app:

```toml
# crates/tools/Cargo.toml
[features]
cap-computer = []
cap-browser = []
cap-media = []
cap-security = []   # optional Phase 3 hygiene; see §5
```

```toml
# crates/envd/Cargo.toml
cap-computer = ["dep:omp-desktop", "omp-tools/cap-computer"]
cap-browser  = ["dep:omp-webview", "omp-tools/cap-browser"]
cap-media    = ["dep:omp-audio", "omp-tools/cap-media"]
```

```toml
# crates/app/Cargo.toml
default = ["local", "cap-ssh", "cap-computer", "cap-browser", "cap-media", ...]
cap-computer = ["omp-envd/cap-computer"]
# (envd already forwards omp-tools/*)
```

In `crates/tools/src/roster.rs`:

```rust
pub static ROSTER: &[BuiltinRosterEntry] = &[
    // ...
    #[cfg(feature = "cap-media")]
    same(Family::ImageGen, /* ... */),
    #[cfg(feature = "cap-media")]
    same(Family::Tts, /* ... */),
    // ...
    #[cfg(feature = "cap-browser")]
    same(Family::Browser, /* ... */),
    #[cfg(feature = "cap-computer")]
    same(Family::Computer, /* ... */),
    #[cfg(feature = "cap-security")]
    same(Family::SecurityScan, /* ... */),
    // ...
];
```

Also `#[cfg]` the corresponding `match` arms in `install_*` / `declare_*` / bridge-need tables that are only reachable from those rows, and `#[cfg]` envd `install_computer` / `install_browser` / media constructors / module declarations.

**Keep** `Family::*` variants and `omp_tools` contract modules unconditional unless a later PR needs true enum absence — construction-table absence is the ADR requirement; chat depends on the contract types.

**Do not** replace compile-out with “settings default false” or `SelectedRoster` runtime masking for these caps.

`ROSTER_LEN` / `FULL_MASK` / identity consts already derive from `ROSTER.len()`, so they shrink automatically when rows cfg out (still must stay ≤ 64).

---

## 5. Phase 3 vs Phase 4

| Item | Phase | Cap / vehicle | Env d edge removed |
|---|---|---|---|
| computer | **3** | `cap-computer` → optional `omp-desktop` | yes |
| browser (+ fetch/relay host wiring) | **3** | `cap-browser` → optional `omp-webview` | yes |
| tts + image_gen | **3** | `cap-media` (`cap-tts`/media) → optional `omp-audio` in envd | yes (`omp-audio`); `omp-ai` stays (many other users) |
| security_scan | **3 optional hygiene** | `cap-security` — row/module cfg; **no** heavy crate edge today | no Cargo edge; still valid for “minimal roster” |
| report_issue | **3 optional / later** | not named in D3.5; small; no webview/desktop/audio edge | no |
| model_discovery, MCP config/OAuth | **park / later** | still coupled to broader `omp-ai` use in envd | alone will not drop `omp-ai` |
| github / `github_url` / `github@3` | **4** | `omp-forge` + `omp-ci` + `Delegated`; GitHub adapter outside core (ADR 0045 amendment, ADR 0049 Phase 4) | N/A — replace, don’t `cap-github` |
| `cap-forge`, `cap-ci` | **4** | named in D3.5 but sequenced in Phase 4 with forge/CI neutrality | Phase 4 |
| BuildBuddy / exec vendor (D6 class A execution) | **4** | ADR 0040 amendment | Phase 4 |
| chat `github_*` cards / `gitwatch` `gh` | **4** | migrate to forge-neutral data | Phase 4 |

TASKS.md already matches: Phase 3 = computer, browser, tts/media; Phase 4 = forge/ci/github.

---

## 6. PR1 (`cap-computer`) — concrete file list + check command

### Files (expected)

| Path | Change |
|---|---|
| `crates/envd/Cargo.toml` | `omp-desktop` optional; `cap-computer = ["dep:omp-desktop", "omp-tools/cap-computer"]` |
| `crates/envd/src/lib.rs` | `#[cfg(feature = "cap-computer")] mod computer;` |
| `crates/envd/src/computer.rs` | stays; only compiled with feature |
| `crates/envd/src/tools/roster_host.rs` | cfg import + `install_computer` |
| `crates/envd/src/tool_settings.rs` | cfg tests that construct `ComputerSessionHost` |
| `crates/tools/Cargo.toml` | `cap-computer = []` |
| `crates/tools/src/roster.rs` | `#[cfg(feature = "cap-computer")]` on Computer row + install/declare arms as needed; update identity tests that assert `"computer"` always listed |
| `crates/app/Cargo.toml` | `cap-computer` feature; add to `default` beside `cap-ssh` |
| `docs/adr/TASKS.md` | mark Phase 3 PR1 progress |
| `docs/adr/0049-tiered-integration-architecture.md` | status blurb when PR lands |
| `.github/labeler.yml` | only if a new crate appears (none for PR1) |

Do **not** cfg out `crates/tools/src/computer.rs` or chat computer cards.

### Exit proofs (PR1)

```bash
# Default app still has the cap
cargo check -p omp-app --locked

# Minimal: envd defaults compile with no omp-desktop
cargo check -p omp-envd --locked \
  --config 'resolver.feature-unification="selected"'

# Edge gone when cap off (selected unification)
cargo tree -p omp-envd -i omp-desktop -e normal \
  --config 'resolver.feature-unification="selected"'
# expect: package not found / not in graph

# App default still links desktop through the feature
cargo tree -p omp-app -i omp-desktop -e normal \
  --config 'resolver.feature-unification="selected"'
# expect: present via omp-envd feature

# Focused tests
just test-pkg omp-tools
just test-pkg omp-envd
```

Optional CI seed (add once, reuse for later caps):

```bash
cargo check -p omp-app --no-default-features --features local --locked \
  --config 'resolver.feature-unification="selected"'
```

After PR1 this command still enables no `cap-computer`; as more caps join `default`, keep them **out** of this minimal invocation.

---

## Sequenced PRs

### PR1 — `cap-computer` (ENVD-08 half)

- Optional `omp-desktop`; roster Computer row cfg; envd `computer` module cfg.
- **Exit:** `omp-desktop` absent from selected-unification `omp-envd` tree; default `omp-app` unchanged; `just test-pkg omp-envd` + `omp-tools` green.

### PR2 — `cap-browser` (ENVD-08 half)

- Optional `omp-webview`; cfg `browser_daemon` / `browser_fetch` / `browser_relay`; roster Browser row cfg.
- Gate `driver/src/registry.rs` `BrowserFetchAdapter` and `app/src/browser_relay_cmd.rs` behind the same cap (or thin stubs that report “not compiled in”).
- Runtime `SV_BROWSER_ENABLED` remains only under `cap-browser`.
- **Exit:** `omp-webview` absent from selected `omp-envd` tree; browser relay contract tests cfg/skip when off; default app still registers browser.

### PR3 — `cap-media` (ENVD-09 media/TTS; ADR `cap-tts`/media)

- Optional `omp-audio` **on envd only**; cfg `media_devices` / `media_tts`; roster `ImageGen` + `Tts` rows cfg together (one capability).
- App/chat keep their own `omp-audio` for say/voice (mirror app×`omp-ssh`).
- Keep existing `local-tts` feature composition; document interaction (`cap-media` off ⇒ no envd TTS device even if `local-tts` on).
- **Exit:** `omp-audio` absent from selected `omp-envd` tree; default app still exposes `image_gen`/`tts`.
- Implemented on this branch. `omp-ai` also depended on `omp-audio` through `rewrite_for_speech`, which is unconditional in the module list. That function now compiles only with `omp-ai`'s `speech` feature, and `realtime` includes `speech`. envd enables neither, so the selected envd tree has no `omp-audio` edge. Ask playback (`ask_vocalizer`) lives in `media_devices` and compiles out with `cap-media`; ask still registers.

### PR4 — minimal-profile CI + matrix docs (after ≥1 cap, ideally after PR3)

- Formalize minimal check in `justfile` (e.g. `check-minimal-caps`) and a non-gating or required CI job: default `omp-app` **and** all `cap-*` off.
- Document feature matrix in ADR 0049 status / TASKS.
- **Exit:** one command proves minimal profile; default profile still green.

### PR5 (optional Phase 3 hygiene) — `cap-security`

- cfg `security_scan` roster row + envd `security_scan` module.
- **Exit:** row absent when off; may **not** remove a Cargo edge — say so in the PR; do not block Phase 4 on this.

### Not in Phase 3 PRs

- `github*` → Phase 4 (`omp-forge` / `omp-ci` / Delegated / adapters).
- `cap-forge` / `cap-ci` → Phase 4.
- Extracting browser/computer into new workspace crates beyond optional gating — only if a later PR needs a non-envd consumer; D3.5 exit is the **edge**, not a mandatory new crate (ssh needed `omp-ssh` for `omp-exec-ssh`; desktop/webview already are crates).

---

## Minimal-profile check command (canonical)

Until a `just` recipe exists:

```bash
cargo check -p omp-app --no-default-features --features local --locked \
  --config 'resolver.feature-unification="selected"'
```

Per-edge proof template:

```bash
cargo tree -p omp-envd -i <omp-desktop|omp-webview|omp-audio> -e normal \
  --config 'resolver.feature-unification="selected"'
```

Default (caps on) smoke:

```bash
just check-pkg omp-app
# or: cargo check -p omp-app --locked
```
