# 0067. Browser inspection: React helpers in the `run` runtime, an axe-core audit pending a licence ruling; ABKit is not adopted in this pass

Status: Proposed 2026-11-14; EGM round 2 resolved, round 3 pending
Date: 2026-11-14
Area: tools

Research: [docs/research/browser-inspection.md](../research/browser-inspection.md), gap 13 of the upstream gap closure (`docs/research/upstream-gap-closure.md`). Upstream pin `c308970263` (v18.20.0); omp2 base `origin/omp2 9cb6fd6389`. Citations: `upstream:` is the OMPv1 pin; bare paths are omp2. The licence evidence for axe-core and ABKit (archive listings, hashes, the Terms of Service passages) is in the research note, sections 1 and 5; this record cites it and states decisions. Nothing was built or run.

Terms used here. A **tab** is one named browser view the daemon keeps (`TabSession`, `crates/envd/src/browser_daemon.rs:227-230`). An **owned** tab is one the daemon created (`headless`, `window`, `spawned`); an **attached** tab is a user page reached through `app.cdp_url` (`cdp`) or the relay (`relay`) (`crates/envd/src/browser_daemon.rs:859-897`, `crates/tools/src/browser.rs:299-302`). The **run runtime** is the throwaway webview in which `run` code executes. A **document-start script** runs in every new document before the page's own scripts. **UNVERIFIED** marks a claim no source or run confirmed. A **journal lift** maps calls recorded under an older tool revision to the current one (ADR 0026). **P4** and **P10** are the e2e proofs for live tool schemas and for lifts (`crates/e2e/README.md`). **ABKit** is Stencil's WebKit engine, a fork of WebKit's WPE port; **ABHost** is its stdio host process.

## Context

### What OMPv1 does

- **React inspection.** Tab helpers `reactEnable`, `reactTree`, `reactInspect`, `reactRenders`, `reactSuspense`, methods of the tab object in the agent's script runtime (`upstream:packages/coding-agent/src/tools/browser/prelude.js:231`, `upstream:packages/coding-agent/src/tools/browser/tab-call.ts:108`). `reactEnable` "installs the hook and reloads" (`upstream:docs/tools/browser.md:95`); the hook stays "for the tab's life" (`:159`). The hook is upstream's own minimal stand-in for the React DevTools global hook (`upstream:packages/coding-agent/src/tools/browser/react/devtools-hook.ts:21-24`) with `inject`, `onCommitFiberRoot`, `onCommitFiberUnmount` (`:122-150`). It skips only if a hook carrying its own `__ompReact` marker exists, otherwise it defines `__REACT_DEVTOOLS_GLOBAL_HOOK__` over whatever is there (`:22-23`, `:163-170`). On every commit it walks the whole fiber tree and stores every fiber in a strong `fibersById` map, recording or not (`:85-117`, `:140-143`), and it writes `__ompBrowserVitals.hydration` for the separate `vitals` helper (`:144-145`). `enableReact` registers the hook for future documents, evaluates it live, reloads and waits for `load`, then fails with "Unable to install the React DevTools hook in this document" if it did not take (`:213-219`). Reads go to the main frame (`:191`, `:199`). A helper without the hook fails with "React DevTools hook not installed — call tab.reactEnable() first" (`:12`). `reactTree({maxDepth, includeHost})` clamps depth to 0..100, default 20, keeps 20 props and 160-character strings (`upstream:packages/coding-agent/src/tools/browser/react/tree.ts:7-10`, `:53-75`); `reactInspect(id)` returns `{name, props, state, source, domSelector}` or a not-found envelope (`tree.ts:38-44`, `:113`); `reactRenders({action})` takes `start | stop | status` (`upstream:packages/coding-agent/src/tools/browser/react/renders.ts:6-19`); `reactSuspense({onlyDynamic})` lists boundaries (`upstream:packages/coding-agent/src/tools/browser/react/suspense.ts:6-17`).
- **Accessibility audit.** `a11y` evaluates axe-core 4.13.0 inside a function scope that saves and restores `window.axe` (`upstream:packages/coding-agent/src/tools/browser/a11y/audit.ts:2`, `:119-141`), with options `tags`, `rules`, `selector`, `includeIncomplete` (`:5-14`), and returns `BrowserA11yResult` (`:16-56`), at most ten nodes of 300 characters per rule (`:116-117`). It runs axe per frame through Puppeteer frame handles and merges partial results (`:264-287`), and pushes a text summary into the tool output (`upstream:packages/coding-agent/src/tools/browser/tab-worker.ts:3120-3124`).
- **ABKit.** The default browser when installed, started as `ABHost` and driven over length-prefixed JSON on stdio (`upstream:docs/tools/browser.md:154`, `upstream:packages/coding-agent/src/tools/browser/tern/wire.ts:1-19`), downloaded from `https://build.stencil.so` into `~/.o2/lib/abkit` (`upstream:packages/coding-agent/src/tools/browser/webkit/install.ts:1-12`, `upstream:packages/coding-agent/src/cli/build-service.ts:48`).

### What omp2 has

- **`browser@3` is `open`, `run`, `close`**, with automation as a JavaScript `code` body for `run` (`crates/tools/src/browser.rs:26-36`, `:111-112`). Its description lists the `tab` vocabulary (`:293-304`); its revision is pinned by a test (`:571`). A fault reaches the model as its `message` only (`:440`).
- **The run runtime.** Each `run` builds a frames webview, evaluates `RUN_RUNTIME` and starts the code there (`crates/envd/src/browser_daemon.rs:899-947`). `RUN_RUNTIME` defines `tab`, `page`, `browser`; each `tab.*` call is queued as `{id, op, args}` (`:1850-1896`; the `tab` literal ends with `download:url=>rpc('download',[url])`, `:1872`) and served by `dispatch_helper(&WebView, ...)` (`:957-975`, `:1104-1458`). An unknown op is `unsupported browser helper` (`:1450-1455`); a helper fault fails the whole run (`?` at `:970`). Replies travel back by evaluating `__ompReply(id, <json>)` in the runtime with a 1-second timeout (`:1460-1472`). `mutates_page` is `goto | click | press | drag` (`:1612-1614`). A test asserts `RUN_RUNTIME` contains its helper names (`:2003-2019`).
- **Tab lifetime.** `run_tab` looks the session up by shared reference (`:808`). A `TabSession` is dropped on `close` (`:437-491`), `close` with `all` (`:448-460`), `open` over an existing name (`tabs.insert`, `:424`), owner release (`:313-315`), restart for a mode change (`:316-321`), daemon exit (`:325`) and a cancelled run (`:850-855`); a released `spawned` tab is kept alive instead (`:456-458`, `:471-473`).
- **Document-start scripts exist only at construction.** `WebViewBuilder::init_script` (`crates/webview/src/lib.rs:303-310`) is applied when a view is wired: Chromium `Page.addScriptToEvaluateOnNewDocument` (`crates/webview/src/remote/chromium.rs:643-661`; attached and relay views too, `:539`), Firefox `script.addPreloadScript` (`crates/webview/src/remote/firefox.rs:270-298`), system WebKit (`crates/webview/src/wk/mod.rs:166-187`). The daemon's `open` sets none (`crates/envd/src/browser_daemon.rs:372-381`); a built `WebView` has no method to add one (`crates/webview/src/lib.rs:618-807`). Remote drivers take `Command`s whose replies are callbacks or `flume` channels, and `RemoteView::send` returns `Result<()>` (`crates/webview/src/remote/mod.rs:48-103`, `:148-151`). `IPC_SHIM` is registered on every Chromium view, attached ones included, and never removed (`chromium.rs:647`; no `removeScriptToEvaluateOnNewDocument` in `crates/webview/src`).
- **Evaluation is main-frame only** (`chromium.rs:980-985`, `firefox.rs:580-590`).
- **Engines.** `Engine::find` returns the system engine for a remote surface only when `OMP_WEBVIEW_BROWSER=system`; otherwise it prefers an installed Chromium (`crates/webview/src/lib.rs:208-236`). The browser tool's tabs are therefore Chromium (or Firefox) in practice.
- **The relay.** One `chrome.debugger` attachment per tab shared by downstream CDP clients (`crates/envd/src/browser_relay.rs:1-8`); `Page.*` commands are forwarded verbatim (`:1866-1896`). When a client disconnects, the relay detaches every tab no other client still holds (`cdp_closed`, `:1470-1489`; `detach_unheld`, `:1997-2029`).
- **Settings.** Browser convars `sv_browser_enabled`, `sv_browser_headless` (`crates/envd/src/browser_daemon.rs:63-85`), `sv_browser_relay*` (`crates/envd/src/lib.rs:312-330`) feed `BrowserSettings` (`browser_daemon.rs:88-122`), read when the environment session is built (`crates/envd/src/server/session.rs:328`, `:633`) and passed to the daemon at tool install (`crates/envd/src/tools/roster_host.rs:779-782`); a restart changes only `headless` (`browser_daemon.rs:316-321`). A test compares a full `BrowserSettings` literal (`:175-181`). `cap-browser` is on by default in `omp-app` (`crates/envd/Cargo.toml:19`, `crates/app/Cargo.toml:25-33`).
- **Tests and CI.** `crates/webview` has no `tests/` directory; `BrowserDaemon` is `pub(crate)` (`browser_daemon.rs:234`); `browser_relay_contract.rs` drives the relay with raw CDP and a fake extension (`crates/envd/tests/browser_relay_contract.rs:222-300`). No CI workflow installs or names a browser (`.github/workflows/ci.yml`, searched `chrom`, `browser`); workspace tests run in `Rust workspace and acceptance proofs` (macOS, `:427`) and `Remote Linux verification (BuildBuddy)` (`:995`). No `OMP_BROWSER_TESTS` variable exists.
- **Licence policy.** `cargo deny` admits MPL-2.0 per named crate (`deny.toml:28-35`); `just license-check` runs only `cargo deny` (`justfile:476-477`). `THIRD-PARTY-NOTICES.txt` is tracked by hand: MPL-2.0 components with source archives (`:25-41`), package `NOTICE` files concatenated (`:66-72`; fixture precedent `fixtures/llm-oracle/vendor-schemas/NOTICE.txt`, `:1631`). It is embedded in the binary (`crates/app/src/cli.rs:27`).
- **axe-core 4.13.0** is MPL-2.0 and bundles MIT and ISC components; `axe.min.js` is 580,491 bytes (research note, section 5).
- **ABKit** has a public source repository under LGPL 2 and BSD per file, a public host protocol, and release archives with no notices; Stencil's Terms do not address third-party download clients (research note, section 1).

### Rules that bind

- Additive only: no change to an existing tool's name, schema or revision. ADR 0026: a revision bumps when "the argument contract or the model-facing projection changes" (`docs/adr/0026-intent-and-versioned-tools.md:30-32`).
- ADR 0024: no new roster tool. [ADR 0049](0049-tiered-integration-architecture.md): vendor integration lives outside core (`:14`, owner ruling `:17`). ADR 0036: extensions are Python. [ADR 0035](0035-rust-for-the-engine.md) and AGENTS.md: typed `thiserror` errors, no string payloads (`docs/adr/0035-rust-for-the-engine.md:55-56`).
- Page scripts run in the browser's own JavaScript engine, as `RUN_RUNTIME` does; no new omp runtime.

## Decision

**D1. The helpers are `tab.*` methods of the run runtime.** With the gate on (D2), `tab` gains `reactEnable()`, `reactDisable()`, `reactTree(opts?)`, `reactInspect(id)`, `reactRenders({action})`, `reactSuspense(opts?)` and, only with D7, `a11y(opts?)`. Each is an RPC through the existing queue to a `dispatch_helper` arm that evaluates an embedded page script on the tab's view and returns one bounded JSON value. Page scripts are `.js` files in `crates/envd/src/browser_inspect/`, loaded with `include_str!` and ported from upstream with its MIT notice, minus the `vitals` coupling (`devtools-hook.ts:144-145`) and minus `getFiberRoots`/`supportsFiber` beyond what React calls. No tool, schema, journal kind or revision changes.

Signatures and results (TypeScript notation for the page-side values):
- `reactEnable(): {installed: true, renderers: number, reloaded: boolean, reactVersion?: string}`. `renderers` is the number of React renderers registered with the hook.
- `reactDisable(): {removed: boolean}`. Removes the document-start registration; the hook stays in the current document until it is replaced.
- `reactTree({maxDepth?, includeHost?}): {roots: ReactTreeNode[], truncated: boolean, omitted: number}`, `ReactTreeNode` as upstream (`tree.ts:13-20`).
- `reactInspect(id: number): ReactInspectResult` as upstream (`tree.ts:38-44`); an unknown or collected id fails with `browser_react_fiber_not_found`.
- `reactRenders({action: "start" | "stop" | "status"}): {active?: boolean, commits: number, components: {name, renders, totalMs}[]}` as upstream (`renders.ts:6-19`).
- `reactSuspense({onlyDynamic?}): ReactSuspenseBoundary[]` as upstream (`suspense.ts:6-17`), with the classification caveat in D4.

**D2. Gate: default-off convar `sv_browser_inspect`.** Declared with `omp_con::var!` beside `SV_BROWSER_HEADLESS`: `bool`, default `false`, flags `archive`, meta `ui.tab = "tools"`, `ui.group = "Grep & Browser"`, `ui.label = "Browser Inspection"`, no `legacy.path` (upstream has no such setting), no `PROJECT` flag. What it protects: a persistent document-start registration and a hook that observes every React commit on the user's attached pages, which `tab.evaluate` alone does not create. `BrowserSettings` gains `inspect: bool`, read with the other browser convars when the environment session is built; a change takes effect in the next session, not on a mode restart. Two enforcement points:
- Runtime composition. `RUN_RUNTIME` stays as it is and is what the daemon evaluates with the gate off. With the gate on, the daemon evaluates `RUN_RUNTIME_INSPECT`, a `LazyLock<String>` built once by inserting `INSPECT_TAB_METHODS` after the anchor `download:url=>rpc('download',[url])`, which must occur exactly once in `RUN_RUNTIME`. With the gate off, `typeof tab.reactTree` is `"undefined"`, as today.
- Dispatch. `dispatch_helper` takes the flag and answers the inspect op names with the existing `unsupported browser helper` fault when it is off.

**D3. `omp-webview` gains post-construction document-start scripts, on Chromium only.** `WebView::add_init_script(&self, source: &str) -> Result<InitScriptId>` and `WebView::remove_init_script(&self, id: &InitScriptId<str>) -> Result<()>`. `InitScriptId` is declared with `omp_core::string_id!` over the CDP identifier. Plumbing: two `Command` variants, `AddInitScript { source, reply: flume::Sender<Result<Str>> }` and `RemoveInitScript { id, reply: flume::Sender<Result<()>> }`, served in the Chromium driver with `Page.addScriptToEvaluateOnNewDocument` and `Page.removeScriptToEvaluateOnNewDocument` on the view's own CDP session (owned, CDP-attached and relay views alike). Scope: a registration applies to the documents of that view's target and lasts until removed or until that CDP session detaches. The Firefox driver and the system engine return the existing `Error::Unsupported { operation }`; no `MainThreadMarker` work and no BiDi path is added. The builder API is unchanged; no new error variants.

**D4. `reactEnable` and the hook's lifetime.**
- **Install.** `reactEnable` registers the hook once per tab through D3 (a second call reuses the stored `InitScriptId`) and evaluates it in the live document. If D3 is unsupported, it fails with the existing `browser_unsupported` fault (`crates/envd/src/browser_daemon.rs:1793-1795`).
- **Reload.** On an owned tab it then calls `automation().reload(remaining)` and `wait_for_condition(view, Some(Load), remaining)` within the run's deadline (`crates/webview/src/automation.rs:297-300`, `browser_daemon.rs:1552`), and `reactEnable` joins `mutates_page`, so pending navigation waits see it. On an attached tab it never reloads (`reloaded: false`); the agent reloads with `tab.goto(tab.url())` when it chooses.
- **Late install is detected, not guessed.** The hook records whether it was created by the document-start registration or by the live evaluation (`late`). A read helper on a hook with zero renderers fails with `browser_react_reload_required` when `late` is true ("React hook was installed after this page loaded; reload the tab to inspect React"), and returns an empty tree when it is false (no React on the page). After `reactEnable` on an owned tab, a page whose React still did not register fails with upstream's "Unable to install the React DevTools hook in this document" as `browser_react_hook_failed`.
- **No overwrite.** If `__REACT_DEVTOOLS_GLOBAL_HOOK__` exists without the omp marker (the user's React DevTools, typically on the relay), the hook does not replace it and `reactEnable` fails with `browser_react_hook_conflict` ("another React DevTools hook is installed in this page"). The registration is removed again in that case.
- **Cost.** `onCommitFiberRoot` only records the root; fiber ids are assigned when a read helper visits a fiber, kept in a `WeakMap` from fiber to id and a map from id to `WeakRef`, so the hook retains no unmounted fiber. The whole-tree walk on commit runs only while `reactRenders` is recording. Consequence: Suspense `classification: "dynamic"` covers boundaries pending at read time or seen pending during a recorded commit, narrower than upstream's every-commit tracking (`devtools-hook.ts:108`). `reactDisable` ends the registration.
- **Missing hook.** A read helper on a document with no omp hook fails with `browser_react_hook_missing` and upstream's message (`devtools-hook.ts:12`).
- **Faults.** New `Fault` codes, built in `browser_daemon.rs` beside `not_found`: `browser_react_hook_missing`, `browser_react_reload_required`, `browser_react_hook_failed`, `browser_react_hook_conflict`, `browser_react_fiber_not_found`. Like every helper fault they fail the run (`:970`) and reach the model as their message (`crates/tools/src/browser.rs:440`).
- **Teardown on every path.** `TabSession` gains `react_hook: Option<InitScriptId>` and `impl Drop`, which on an attached tab calls `remove_init_script` best-effort and logs a failure with `tracing::warn!` (teardown cannot fail the caller). This covers every drop site listed in Context. A daemon crash runs no `Drop`: for relay tabs the relay detaches tabs no client holds when the client disconnects (`browser_relay.rs:1470-1489`), and for CDP-attached tabs the CDP session ends with the connection; that Chromium drops session-scoped scripts on detach is UNVERIFIED and tested in S1. `IPC_SHIM`'s registration (`chromium.rs:647`) is existing behaviour and out of scope here.
- **Plumbing.** `run_tab` takes the session with `tabs.get_mut`, and `run_code` and `dispatch_helper` take `&mut TabSession` and the gate flag instead of `&WebView`.

**D5. Main frame only.** React helpers read the main frame's hook; React roots in iframes are not reported. `a11y` (D7) audits the main document with axe's `iframes: false` and reports how many `iframe` and `frame` elements it did not enter as `frames: {skipped}`. Per-frame evaluation is outside this record.

**D6. Result bounds are page-side.** Every page script bounds its own output before returning, so nothing large crosses `returnByValue` or the 1-second reply (`browser_daemon.rs:1460-1472`): `reactTree` as upstream (depth 0..100, default 20; 20 props; 160-character strings) plus at most 2,000 nodes and 256 KiB of serialized JSON, after which nodes are omitted and counted in `omitted` with `truncated: true`; `reactInspect` as upstream (500-character strings, 20 items, `tree.ts:116-130`) plus 64 KiB; `reactRenders` and `reactSuspense` at most 500 entries; `a11y` as upstream (10 nodes, 300 characters) plus 256 KiB.

**D7. The audit is axe-core only, and waits on O3.** The first draft's seven-rule own engine is dropped: its rules, accessible-name algorithm and selector generation would be a second audit engine to specify and own, and a short rule list risks reading as a pass. If the owner accepts axe-core (O3):
- `crates/envd/assets/axe-core/axe.min.js` from the 4.13.0 npm tarball, unmodified with its header, embedded with `include_str!` behind a new envd feature `cap-browser-axe` (580 KB of build weight; forwarded by `omp-app`, not in its defaults), and still behind `sv_browser_inspect`.
- `tab.a11y({tags?, rules?, selector?, includeIncomplete?})` evaluates axe inside a function scope that saves and restores `window.axe`, ported from `audit.ts:119-141`, then `axe.run(context, {resultTypes: ["violations", "incomplete", "passes"], iframes: false, runOnly})` with upstream's option mapping and selector errors (`audit.ts:175-190`; upstream's standalone path omits `iframes: false`, which D5 adds). It returns `BrowserA11yResult` as upstream (`audit.ts:44-56`) plus `frames: {skipped}`. It returns the object only; the text summary upstream pushes into its output (`tab-worker.ts:3123`) is not produced, and the agent can `display()` what it needs.
- Licence handling in the same PR: a package `crates/envd/assets/axe-core/NOTICE` with the copyright line, the MPL-2.0 notice, the tarball URL and SHA-256, the source tag `v4.13.0` of `github.com/dequelabs/axe-core`, the unminified `axe.js` in the same tarball named as the Source Code Form (MPL section 3.2), and the verbatim `LICENSE-3RD-PARTY.txt`; an `axe-core 4.13.0` line in the MPL-2.0 list of `THIRD-PARTY-NOTICES.txt` (`:25-41`); the `NOTICE` concatenated into its package-notice section (`:66-72`), edited by hand as the file is today. No `deny.toml` change.

**D8. ABKit is not adopted in this pass.** Its licence (LGPL 2 and BSD per file; the host BSD 2-clause) and its host protocol are public, so that is not the obstacle. What is: (1) the one release archive inspected (0.1.1, Windows x64) carries none of the notices its BSD and LGPL components require for binary redistribution, and the other seven were not inspected, so omp2 must not bundle or mirror the archives; (2) acquisition through `build.stencil.so` is a vendor service, which under ADR 0049 belongs in an extension, and Stencil's Terms do not say whether a third-party client may fetch builds for its users; (3) `Engine` is a closed enum dispatched without `dyn` (`crates/webview/src/lib.rs:129`, `crates/webview/README.md:44`), so no extension can supply an engine. No engine variant, download, bundling or protocol client is added. Revisit with O1, O2 and O5.

**D9. The `browser@3` description is not changed here.** With the gate on, the helpers work but the description (`crates/tools/src/browser.rs:293-304`) does not list them, so a model learns them only from the user or a skill until S5. Advertising them is O6.

## Rejected options

| Option | Why not |
|---|---|
| New tool actions (`action: "react_tree"`) | Changes `browser@3`'s argument contract (`crates/tools/src/browser.rs:26-36`). |
| Install the hook with `tab.evaluate` only | Gone after every reload; React registers with the hook at module load, so a live install sees no renderer. |
| Register the hook in every tab at `open` | Injects into attached pages nobody asked to inspect. |
| Reload attached tabs, or a `{reload: true}` option | Reloads the user's page; `tab.goto(tab.url())` already does it when the agent decides to. |
| Define the methods with the gate off and refuse them in dispatch only | Changes gate-off behaviour: `typeof tab.reactTree` would stop being `"undefined"`. |
| A SHA-256 pin of the gate-off runtime | Breaks on unrelated `RUN_RUNTIME` edits; the gate-off path evaluates the unchanged constant itself. |
| Init scripts on Firefox and the system engine | No caller needs them: the tool's tabs are Chromium in practice (`lib.rs:208-236`); the system engine needs main-thread work and cannot remove one script. |
| Overwrite an existing DevTools hook | Breaks the user's React DevTools on relay tabs. |
| omp's own seven-rule audit | A second audit engine with its own accessible-name and selector algorithms; see D7. |
| Audit every frame | No per-frame evaluate in `omp-webview` (`chromium.rs:980-985`). |
| CDP's React DevTools backend | Needs the DevTools frontend bundle; Chromium only. |
| `Engine::AbKit` and a downloader in core; bundling or mirroring ABKit | ADR 0049; archives without notices. |

## Slices

Each slice is one PR; tests are written first and not edited to pass.

Browser test support (S1, used by S2 to S4): browser-backed tests look for an engine with `Engine::find`; without one they print the reason and return, unless the new variable `OMP_BROWSER_TESTS=require` is set, which makes a missing engine a failure. S1 sets that variable in the CI job that runs the `omp-webview` and `omp-envd` tests on Linux and adds a Chromium install step there if the image has none; which job that is (`Rust workspace and acceptance proofs` is macOS, `.github/workflows/ci.yml:427`; the Linux workspace run is `Remote Linux verification (BuildBuddy)`, `:995`) and whether its image has Chromium is UNVERIFIED and is S1's first task. Without that step the browser acceptance tests do not gate.

**S1. Chromium post-construction init scripts (D3).**
- Scope: `crates/webview/src/lib.rs` (`WebView::add_init_script`, `remove_init_script`, `InitScriptId`), `crates/webview/src/remote/mod.rs` (two `Command` variants), the Chromium driver's command arm (`chromium.rs:955` area); Firefox and system return `Error::Unsupported`. The test-support variable and the CI step above.
- Gate: none; no caller until S2.
- Tests, new `crates/webview/tests/init_scripts.rs` with a support module that spawns a headless Chromium on a loopback debugging port and also attaches a second view to it with `Engine::chromium_cdp`: `added_init_script_runs_on_reload_and_navigation`, `removed_init_script_does_not_run_on_next_document`, `init_script_is_scoped_to_its_view_session` (the attached view's registration does not run for the owning view, and vice versa), `init_script_does_not_survive_session_detach`. Unit: `firefox_add_init_script_is_unsupported`.
- Acceptance: existing `omp-webview` tests pass unchanged. `just test-pkg omp-webview`.

**S2. Gate, runtime composition and the core React helpers (D1, D2, D4, D5, D6).**
- Scope: `sv_browser_inspect`; `BrowserSettings.inspect` (the literal test at `browser_daemon.rs:175-181` gains the field); `RUN_RUNTIME_INSPECT`; `TabSession.react_hook` with `Drop`; `get_mut` and `&mut TabSession` plumbing; `reactEnable`, `reactDisable`, `reactTree`, `reactInspect` and their page scripts; `reactEnable` in `mutates_page`.
- Gate: `sv_browser_inspect`, default off.
- Fixture: `fixtures/browser/react/` holds a page built on pinned React 18.3.1 and ReactDOM 18.3.1 UMD development builds (MIT), with a `NOTICE` concatenated into `THIRD-PARTY-NOTICES.txt` like `fixtures/llm-oracle/vendor-schemas/NOTICE.txt`. It renders a known component tree with props, state and a button that re-renders one child. React 19 is UNVERIFIED and stated as such in `Status in omp`.
- Tests, unit in `crates/envd/src/browser_daemon.rs` (the seam is `pub(crate)`, so the daemon tests live in the crate): `inspect_anchor_occurs_once_in_run_runtime`, `gate_off_dispatch_refuses_inspect_ops`, `missing_hook_envelope_maps_to_hook_missing_fault`, `late_hook_with_no_renderer_maps_to_reload_required`. Browser-backed, in the same module through `BrowserDaemon::start`: `react_helpers_are_undefined_with_gate_off`, `tree_lists_fixture_components`, `inspect_returns_props_state_and_selector`, `hook_survives_navigation_within_the_tab`, `page_without_react_returns_an_empty_tree`, `tree_over_the_node_cap_is_truncated_with_a_count`, `largest_bounded_tree_round_trips_within_the_reply_timeout`, `enable_on_attached_tab_does_not_reload` (a marker on `window` survives) `and_reports_reload_required`, `existing_devtools_hook_is_not_replaced`, `dropping_an_attached_tab_removes_its_registration` (covers `close`, `close` with `all`, replacing `open` and owner release). Relay: `crates/envd/tests/browser_relay_contract.rs`, `relay_detaches_unheld_tab_when_the_client_disconnects` (the fake extension sees `Detach` after a client that registered a script drops its socket).
- Acceptance: gate off, all existing `omp-envd` and `omp-tools` tests pass, the `RUN_RUNTIME` helper test (`:2003-2019`) and `browser@3`'s schema and revision tests (`crates/tools/src/browser.rs:506-623`) unchanged; the only edited existing test is the `BrowserSettings` literal. `just test-pkg omp-envd`.
- Depends on: S1.

**S3. Render recording and Suspense (D1, D4, D6).**
- Scope: `reactRenders`, `reactSuspense`, the recording-only commit walk.
- Gate: `sv_browser_inspect`.
- Tests on the S2 fixture: `renders_counts_commits_after_a_click`, `renders_stop_freezes_counts`, `commits_outside_recording_do_not_walk_the_tree` (a counter in the hook), `suspense_lists_fixture_boundaries`, `results_are_capped_at_500_entries`.
- Depends on: S2.

**S4 (optional). axe-core audit (D5, D7).**
- Scope: the vendored file, `NOTICE`, `THIRD-PARTY-NOTICES.txt` entries, `cap-browser-axe` in envd and `omp-app`, the `a11y` arm and page script.
- Gate: `cap-browser-axe`, off by default, plus `sv_browser_inspect`.
- Fixtures `fixtures/browser/a11y/`: one page with an image without `alt`, an unlabeled input and an empty link; one clean page; one page with an iframe.
- Tests: `vendored_axe_sha256_is_pinned`, `vendored_axe_header_notice_is_intact`, `notices_file_lists_axe_core_with_its_source_archive` (reads the shipped `THIRD-PARTY-NOTICES.txt`), `a11y_reports_axe_rule_ids_for_fixture_violations` (`image-alt`, `label`, `link-name`), `clean_page_reports_no_violations`, `selector_limits_the_audit`, `iframes_are_counted_as_skipped`, `existing_window_axe_is_restored`. `just check-minimal-caps` passes.
- Depends on: S2. Blocked on O3.

**S5. Advertise the helpers (D9).**
- Scope: the `browser` description and whatever revision O6 settles, with a journal lift if it moves.
- Tests: the tool's schema and revision tests; P4 (`crates/e2e/tests/p4_schema_isolation.rs`) and P10 (`crates/e2e/tests/p10_lift_idempotence.rs`) if the revision moves.
- Depends on: S2. Blocked on O6.

S1 to S4 add no e2e proof: the joined path from tool to envd to daemon is unchanged and the behaviour is proven at its owning seams, `crates/webview` and `crates/envd` (AGENTS.md, Testing & QA). No golden journal: no journal kind or payload field changes.

## Upstream origin and license

S2 and S3 draw from `upstream:packages/coding-agent/src/tools/browser/react/` (`devtools-hook.ts`, `tree.ts`, `renders.ts`, `suspense.ts`), with the changes D4 names (no overwrite, lazy ids with weak references, recording-only walks, `late` flag, no `vitals` coupling). S4 draws from `upstream:packages/coding-agent/src/tools/browser/a11y/audit.ts` for the injection scope, options and report shape. Upstream is MIT (`upstream:LICENSE:1-5`; copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.); the embedded scripts keep that notice. React 18.3.1 (test fixture only) is MIT. axe-core 4.13.0 is MPL-2.0 with bundled MIT and ISC parts; D7 states its handling. ABKit: nothing reused.

## Consequences

- With the gate on, an agent can enable, read and inspect React trees on Chromium tabs from the `run` code it already writes; render counts and Suspense follow in S3.
- `reactEnable` never reloads a user's page, never replaces the user's React DevTools hook, and removes its registration when the tab is dropped; after a daemon crash, removal relies on the relay's detach or on the CDP session ending.
- Firefox and system-engine tabs get `browser_unsupported` from `reactEnable`.
- iframe React roots and iframe accessibility are out of reach; the audit says how many frames it skipped.
- There is no accessibility audit unless the owner accepts axe-core (O3).
- The helpers stay unadvertised until O6 is answered.
- omp2 gains no WebKit-over-stdio engine.

## Open questions for the owner

- **O1. ABKit binary notices.** The inspected 0.1.1 archive ships BSD and LGPL components with no licence texts or notices. Recommendation: ask Stencil to ship notices in the archives; until then do not fetch them for users.
- **O2. ABKit acquisition.** May a third-party client download `build.stencil.so` releases for its users under the Terms (sections 1.2, 1.3, 5(g))? Recommendation: ask in writing; if yes, the downloader is an extension (ADR 0049).
- **O3. axe-core.** Is an unmodified vendored MPL-2.0 JavaScript file with bundled MIT and ISC parts, handled as D7 says, acceptable when `deny.toml` admits MPL-2.0 per crate only (`deny.toml:28-35`)? Recommendation: yes; otherwise omp2 ships no audit in this pass.
- **O4.** Resolved in round 2: S2 requires a pinned React 18.3.1 fixture.
- **O5. An engine seam.** ABKit, or any stdio browser host, needs a vendor-neutral `Engine` variant for a host at a user-supplied path, or an extension seam for engines. Recommendation: a separate record.
- **O6. Advertising the helpers.** Listing them in the description changes `browser@3`'s model-facing projection, which ADR 0026 says bumps the revision; the additive-only rule forbids changing an existing tool's revision. Options: (a) a `browser@4` with a lift from `@3`, chosen when the roster host builds the tool (`crates/envd/src/tools/roster_host.rs:779-782`) from the session's `sv_browser_inspect`, which is fixed for the session so the tool set stays stable; (b) leave the description unchanged and ship a skill that documents the helpers with the gate; (c) leave them unadvertised. Recommendation: (a), owner to rule; S5 waits.

## What the review changed

D-numbers in each round refer to the record as it stood after that round.

### Round 1 (EGM; findings as recorded in `docs/adr/STATUS.md` row 0067)

- **The helpers are JavaScript run-runtime helpers, not Rust ops.** Confirmed: `crates/tools/src/browser.rs:26-36`; `crates/envd/src/browser_daemon.rs:1850-1896`, `:1104-1458`; upstream `prelude.js:231`. Changed: helpers became `tab.*` runtime methods behind a default-off convar; D9 and O6 added because advertising them moves `browser@3`'s projection (`docs/adr/0026-intent-and-versioned-tools.md:30-32`).
- **Reload on attached and relay tabs; injection must survive navigation and reload.** Confirmed: upstream reloads unconditionally (`devtools-hook.ts:213-219`); attached tabs are the user's pages (`crates/tools/src/browser.rs:299-302`); the relay shares one attachment per tab (`crates/envd/src/browser_relay.rs:7-8`). Changed: no reload on attached tabs, document-start registration, removal on close.
- **Init scripts are unreachable from the daemon.** Confirmed: `crates/webview/src/lib.rs:303-310`, `:618-807`; `crates/envd/src/browser_daemon.rs:372-381`. Changed: a post-construction `omp-webview` API and slice S1.
- **Main frame only.** Confirmed: `chromium.rs:980-985`, `firefox.rs:580-590`; upstream React reads are main-frame (`devtools-hook.ts:191`). Changed: the limit stated; frame tests replaced.
- **axe-core MPL-2.0 notice (O3).** Confirmed and widened: the tarball bundles MIT and ISC components. Changed: notice files, Source Code Form pointer, pinned hash.
- **ABKit "not adopted", checked against published terms.** Corrected: the public `stencil-hq/abkit` repository states LGPL 2 and BSD per file and publishes the protocol; the archive still has no notices. Changed: D8's reasons, O1, O2. Verdict kept.
- Also: a `cap-browser-inspect` feature was dropped; the convar alone gates behaviour.

### Round 2 (EGM, four fresh reviewers and a synthesis: 1 blocker, 19 majors, 11 minors)

- **B1. The own-rule audit was unspecified.** Confirmed: the round-1 record named seven rules in one line each, with no predicates, accessible-name algorithm, selector generation or result type, while upstream's type requires `helpUrl` and `failureSummary` and names the engine `axe-core` (`upstream:.../a11y/audit.ts:26-56`). Resolved by a scope cut: the own engine is dropped and the audit is axe-core only (D7), so the rules are axe's. S3's untestable tests are gone. No blocker remains.
- **M1. Teardown only on graceful close.** Confirmed: sessions drop at `browser_daemon.rs:313-325`, `:424`, `:448-460`, `:850-855`. Changed: `impl Drop for TabSession` removes the registration on every path, best-effort with a logged failure; the crash path rests on the relay's detach (`browser_relay.rs:1470-1489`, `:1997-2029`) and the CDP session ending, the latter tested in S1. Consequences no longer claim "no script behind" unconditionally.
- **M2. `IPC_SHIM` already persists on attached views.** Confirmed: `chromium.rs:647`, no removal in `crates/webview/src`. Changed: the claim is narrowed to the React hook. Rejected in part: shim cleanup is not added; it is existing behaviour outside this gap.
- **M3. `reactEnable` result and fault text.** Confirmed: upstream's result is `{installed, reactVersion?}` and its message uses an em dash (`devtools-hook.ts:7-12`, `:218`). Changed: D1 defines the result (`renderers`, `reloaded`), D4 uses upstream's messages verbatim and names the post-reload failure.
- **M4. "No React" and "not hooked" were indistinguishable.** Confirmed from the record's own D4. Changed: the hook's `late` flag and `browser_react_reload_required`.
- **M5. Collision with a user's DevTools hook.** Confirmed: upstream overwrites (`devtools-hook.ts:22-23`, `:163-170`). Changed: no overwrite, `browser_react_hook_conflict`, registration removed.
- **M6. Permanent cost on the page.** Confirmed: whole-tree walk and strong `fibersById` on every commit (`devtools-hook.ts:85-117`, `:140-143`). Changed: lazy ids with `WeakRef`, recording-only walks, `reactDisable`; the Suspense classification narrowing is stated.
- **M7. Unquantified bounds.** Confirmed: upstream bounds depth, props and strings only (`tree.ts:53-75`); replies go through a 1-second evaluation (`browser_daemon.rs:1460-1472`). Changed: D6 gives page-side caps and S2 tests the largest bounded reply.
- **M8. Incomplete signatures; `vitals` coupling.** Confirmed: `renders.ts:6-19`, `suspense.ts:6-17`, `tree.ts:7-10`, `:113`, `devtools-hook.ts:144-145`. Changed: D1 lists every signature and result; the coupling is stripped.
- **M9. Gate and session plumbing.** Confirmed: `dispatch_helper` takes `&WebView` (`browser_daemon.rs:1104-1112`), `run_tab` borrows immutably (`:808`), settings are read at session build (`crates/envd/src/server/session.rs:328`) and a restart changes only `headless` (`browser_daemon.rs:316-321`), the literal test at `:175-181`, the contains test at `:2003-2019`. Changed: D2 and D4 name the plumbing, the read point and the insertion anchor; `RUN_RUNTIME` stays a constant; S2 states the one edited existing test.
- **M10. `add_init_script` plumbing.** Confirmed: `crates/webview/src/remote/mod.rs:48-103`, `:148-151`; system engine needs the main thread (`wk/mod.rs:171-187`) and is never found by default (`lib.rs:208-236`). Changed: D3 names the `Command` variants, the id type and the scope rule, and drops Firefox and system (with M18).
- **M11. Reload semantics and activity.** Confirmed: `automation.rs:297-300`, `mutates_page` at `browser_daemon.rs:1612-1614`. Changed: D4 names the reload and ready wait; `reactEnable` joins `mutates_page`; the `{reload: true}` option is gone.
- **M12. No channel for the summary text.** Confirmed: helpers return one value (`browser_daemon.rs:1460-1472`); upstream pushes text (`tab-worker.ts:3123`). Changed: D7 returns the object only.
- **M13. Tests may never run; harnesses missing.** Confirmed: no browser in `.github/workflows/ci.yml`, no `OMP_BROWSER_TESTS`, `BrowserDaemon` is `pub(crate)` (`browser_daemon.rs:234`), no `crates/webview/tests`. Changed: the variable is declared as new, S1 owns the CI step and names the candidate jobs (which one is UNVERIFIED), daemon tests live in the crate, the relay test uses the existing fake-extension harness, fixtures are named.
- **M14. Real React unproven.** Confirmed: tag numbers are hard-coded (`devtools-hook.ts:36-79`). Changed: a pinned React 18.3.1 fixture is a precondition of S2; React 19 is UNVERIFIED; O4 resolved.
- **M15. Undiscoverable deliverable; option (a) layering.** Confirmed: `crates/tools/src/browser.rs:287-306` is a static spec. Changed: D9 says plainly how a model learns the helpers before S5; O6(a) now says where the spec would be chosen and why it is session-stable (`crates/envd/src/tools/roster_host.rs:779-782`). Rejected in part: Z9's claim that a skill makes the gate redundant; the gate controls page-side registration, not discovery.
- **M16. Gate rationale and mechanisms.** Confirmed: `tab.evaluate` already runs arbitrary page JavaScript ungated (`browser_daemon.rs:1375-1387`). Changed: the rationale now names what the gate protects; the SHA-256 pin is dropped; dispatch refuses the ops when off. Rejected in part: defining the methods unconditionally, because it changes gate-off behaviour.
- **M17. Own rules before axe was an inversion.** Accepted: D7 is axe-only (with B1).
- **M18. Three-engine init scripts were speculative.** Accepted: Chromium only (D3).
- **M19. Altitude.** Accepted in part: licence evidence moved to the research note, terms defined at the top. Rejected in part: "What the review changed" stays, because the gap-closure brief requires it in every reviewed ADR.
- **m1.** Accepted: render recording and Suspense moved to S3.
- **m2.** Accepted: `{reload: true}` removed (M11).
- **m3.** Superseded: same-origin iframe walking went with the own engine.
- **m4.** Accepted: D4 says where fault codes are built and how they reach the model (`crates/tools/src/browser.rs:440`); D3 adds no error variants.
- **m5.** Accepted: D7 names the `window.axe` save and restore and the `axe.run` options (`audit.ts:119-141`, `:175-190`).
- **m6.** Accepted: D7 names the tag and the hand edit; no generator exists (`justfile:476-477`). The section 3.2 reading stays under O3.
- **m7.** Accepted: D8 says "the one archive inspected".
- **m8.** Accepted: the review status left D2.
- **m9.** Accepted: ADR 0049 cited at `:14` and `:17` and linked; ADR 0035 cited at `:55-56`.
- **m10.** Accepted: terms defined under the research line.
- **m11.** Accepted for the convar meta (D2). Rejected for `docs/adr/STATUS.md`: the gap-closure orchestrator owns that file; the proposed row goes in its report.

Machine-reviewed (EGM, two rounds so far), not owner-approved. Round 3 is pending.

## Acceptance tests

1. S1: on Chromium, a script added after build runs on reload and navigation, stops after removal, is scoped to its view's session and does not survive a session detach; Firefox reports `Error::Unsupported`.
2. S2: gate off, `typeof tab.reactTree` is `"undefined"` and dispatch refuses the inspect ops.
3. S2: gate on, the React 18.3.1 fixture yields the expected tree, props, state and selector; the hook survives navigation; an oversized tree is truncated with a count.
4. S2: on an attached tab, `reactEnable` does not reload and reads report `browser_react_reload_required`; an existing DevTools hook is not replaced; dropping the tab removes the registration.
5. S3: render counts follow clicks only while recording; Suspense boundaries are listed.
6. S4: axe reports its rule ids on the fixture, nothing on the clean page, counts the iframe as skipped, restores `window.axe`; the notices file lists axe-core with its source archive.

## Status in omp

**Status: Not started.** Reusable today: the run runtime and `dispatch_helper` (`crates/envd/src/browser_daemon.rs:899-1458`), `WebViewBuilder::init_script` (`crates/webview/src/lib.rs:303-310`), `ariaSnapshot` (`browser_daemon.rs:1168-1180`). Not yet implemented: D1 to D7. Unverified: React 19 compatibility; Chromium dropping session-scoped scripts on detach; which CI job can host browser tests.

## References

[docs/research/browser-inspection.md](../research/browser-inspection.md); [ADR 0024](0024-small-permanent-roster.md) (fixed roster); [ADR 0026](0026-intent-and-versioned-tools.md) (versioned tools); [ADR 0035](0035-rust-for-the-engine.md) (errors); [ADR 0036](0036-python-for-extensions.md) (Python-only extensions and eval); [ADR 0049](0049-tiered-integration-architecture.md) (vendors outside core); `https://github.com/stencil-hq/abkit` (`ReadMe.md`, `docs/protocol.md`, `docs/installing.md`).
