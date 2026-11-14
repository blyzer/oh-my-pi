# 0067. Browser inspection: React and accessibility helpers in the `run` runtime; ABKit is not adopted in this pass

Status: Proposed 2026-11-14; EGM round 1 resolved, rounds 2-3 pending
Date: 2026-11-14
Area: tools

Research: [docs/research/browser-inspection.md](../research/browser-inspection.md) (Gap 13 of the upstream gap closure). Upstream pin `c308970263` (v18.20.0); omp2 base `origin/omp2 9cb6fd6389`. Citations: `upstream:` is the OMPv1 pin; bare paths are omp2. Read: the browser tool, the browser daemon and relay, `omp-webview`'s engines, upstream's React, a11y and WebKit sources, the npm `axe-core@4.13.0` tarball, the public `stencil-hq/abkit` repository and one ABKit 0.1.1 release archive (listed, then deleted). Nothing was built or run. This record states decisions; the note holds the evidence.

## Context

### What OMPv1 does

- **React inspection.** Five tab helpers, `reactEnable`, `reactTree`, `reactInspect`, `reactRenders`, `reactSuspense`; `reactEnable` must run first because "it installs the hook and reloads" (`upstream:docs/tools/browser.md:95`), and the hook stays "for the tab's life" (`upstream:docs/tools/browser.md:159`). They are methods of the tab object in the agent's script runtime, not tool operations (`upstream:packages/coding-agent/src/tools/browser/prelude.js:231`, `upstream:packages/coding-agent/src/tools/browser/tab-call.ts:108`). The hook is upstream's own minimal stand-in for the React DevTools global hook (`upstream:packages/coding-agent/src/tools/browser/react/devtools-hook.ts:21-24`), implementing `inject`, `onCommitFiberRoot` and `onCommitFiberUnmount` (`devtools-hook.ts:124-147`). `enableReact` registers the hook for every future document, evaluates it in the live one, then reloads unconditionally and checks it took (`devtools-hook.ts:213-219`); all reads go to "the main frame's page world" (`devtools-hook.ts:191`, `:199`). A helper without the hook fails with a fixed message (`devtools-hook.ts:12`). Results are a bounded tree of `{id, name, type, key, props, children}` and per-fiber props, hooks, source and DOM selector (`upstream:packages/coding-agent/src/tools/browser/react/tree.ts:7-44`).
- **Accessibility audit.** `a11y` injects axe-core 4.13.0 (`upstream:packages/coding-agent/src/tools/browser/a11y/audit.ts:2`, `upstream:packages/coding-agent/package.json:557`) and returns `BrowserA11yResult`: URL, engine name and version, counts, and violations with rule id, impact, help, tags and at most ten nodes of at most 300 characters of HTML (`audit.ts:16-56`, `audit.ts:116-117`). It runs axe in each frame through Puppeteer frame handles and merges the partial results (`audit.ts:264-287`).
- **ABKit.** "ABKit (a patched WPE WebKit)": a child process `ABHost` driven over length-prefixed JSON on stdio, default when installed (`upstream:docs/tools/browser.md:154`, `upstream:packages/coding-agent/src/tools/browser/tern/wire.ts:1-19`). omp downloads the newest stable build from Stencil's build service into `~/.o2/lib/abkit` (`upstream:packages/coding-agent/src/tools/browser/webkit/install.ts:1-12`; origin `https://build.stencil.so`, `upstream:packages/coding-agent/src/cli/build-service.ts:48`).

### What omp2 has

- **The browser tool is `browser@3`: `open`, `run`, `close`.** Automation is a JavaScript body passed as `code` to `run` (`crates/tools/src/browser.rs:26-36`, `:111-112`). Its description lists the `tab` helper vocabulary (`crates/tools/src/browser.rs:293-304`), and its revision is pinned by a test (`crates/tools/src/browser.rs:571`).
- **`run` executes in a separate runtime webview.** For each call the daemon builds a throwaway frames webview, evaluates `RUN_RUNTIME` in it and starts the agent's code there (`crates/envd/src/browser_daemon.rs:899-947`). `RUN_RUNTIME` defines `tab`, `page` and `browser`; every `tab.*` call queues `{id, op, args}` (`crates/envd/src/browser_daemon.rs:1850-1896`), which the daemon drains and serves in `dispatch_helper` against the tab's own view (`crates/envd/src/browser_daemon.rs:957-975`, `:1104-1458`). An unknown op is refused as `unsupported browser helper` (`:1450-1455`), and a helper fault fails the whole run (`?` at `:970`). This runtime is the seam for new helpers.
- **Document-start scripts exist only at construction.** `WebViewBuilder::init_script` (`crates/webview/src/lib.rs:303-310`) is applied when a view is wired: Chromium `Page.addScriptToEvaluateOnNewDocument` (`crates/webview/src/remote/chromium.rs:643-661`, also for attached and relay views, `:539`), Firefox `script.addPreloadScript` (`crates/webview/src/remote/firefox.rs:270-298`), system WebKit `WKUserScript` at document start (`crates/webview/src/wk/mod.rs:166-187`). The daemon's `open` sets none (`crates/envd/src/browser_daemon.rs:372-381`), and a built `WebView` has no method to add one (`crates/webview/src/lib.rs:618-807`: navigate, eval, reload and so on). The daemon cannot inject a hook before page scripts today.
- **Evaluation is main-frame only.** Chromium calls `Runtime.evaluate` with no context id (`crates/webview/src/remote/chromium.rs:980-985`); Firefox targets the top-level context (`crates/webview/src/remote/firefox.rs:580-590`). There is no per-frame evaluate.
- **Backends.** A tab is `cdp` (attached; "attaches without owning the page"), `relay` (the user's logged-in browser), `spawned`, or `headless`/`window` (owned) (`crates/envd/src/browser_daemon.rs:859-897`, `crates/tools/src/browser.rs:204-210`, `:299-302`). The relay multiplexes downstream CDP clients over one `chrome.debugger` attachment per tab (`crates/envd/src/browser_relay.rs:1-8`), so a document-start script registered through it lives on that shared attachment until removed.
- **Convars and features.** Browser policy convars `sv_browser_enabled`, `sv_browser_headless` (`crates/envd/src/browser_daemon.rs:63-85`) and `sv_browser_relay*` (`crates/envd/src/lib.rs:312-330`) feed `BrowserSettings` (`crates/envd/src/browser_daemon.rs:88-122`). `cap-browser` gates `omp-webview` and is on by default in `omp-app` (`crates/envd/Cargo.toml:19`, `crates/app/Cargo.toml:25-33`).
- **Nothing to inspect React or audit a page.** `ariaSnapshot` is the only accessibility helper (`crates/envd/src/browser_daemon.rs:1168-1180`); no `react`, `a11y` or `axe` code exists in `crates/envd`, `crates/tools` or `crates/webview` (searched).
- **Licence policy.** `cargo deny` allows MPL-2.0 per named crate only (`deny.toml:28-35`) and cannot see a JavaScript file. `THIRD-PARTY-NOTICES.txt` lists MPL-2.0 components with their exact source archives and states the section 3.1/3.2 obligations (`THIRD-PARTY-NOTICES.txt:25-41`); vendored non-crate material keeps a package-local `NOTICE` that is concatenated into it (`THIRD-PARTY-NOTICES.txt:66-72`; example `crates/tui/src/markdown/syntaxes/NOTICE` at `:1557`). The file is embedded in the binary (`crates/app/src/cli.rs:27`).

### axe-core 4.13.0, as published (npm tarball, fetched 2026-11-14)

- `https://registry.npmjs.org/axe-core/-/axe-core-4.13.0.tgz`, licence field `MPL-2.0`. `axe.min.js` is 580,491 bytes, SHA-256 `c24f097bd2f451d4f933e8bc7d8d539f8672a2ebcb5cc9f9f3eec8ca9470a0c1`. Its header: "Copyright (c) 2015 - 2026 Deque Systems, Inc. ... subject to the terms of the Mozilla Public License, v. 2.0 ... This entire copyright notice must appear in every copy of this file you distribute".
- The tarball also ships `LICENSE` (MPL-2.0 text) and `LICENSE-3RD-PARTY.txt`: the minified file bundles MIT components (colorjs.io, core-js-pure, css-selector-parser, doT.js, emoji-regex and others) and ISC components (es5-ext, memoizee and others) whose notices must travel with it. The unminified `axe.js` is in the same tarball.

### ABKit's terms (researched 2026-11-14; corrects the first draft)

- **Source and licence are public.** `https://github.com/stencil-hq/abkit` is a public fork of `WebKit/WebKit`. Its README: "ABKit (AgentBrowser Kit) is a browser engine for agents: a fork of WebKit's WPE port ... plus `ABHost`, a headless browser that a parent process drives over its standard streams", and "its licenses (LGPL 2 and BSD, per file) apply to ABKit unchanged". The host's own files (`Source/WebKit/AgentHost/Host.cpp`, `main.cpp`, `Transport.cpp`, `BlockScripts.cpp`) carry a BSD 2-clause header, "Copyright (C) 2026 Stencil". WebCore's `LICENSE-LGPL-2`, `LICENSE-LGPL-2.1` and `LICENSE-APPLE` are present.
- **The protocol is public.** `docs/protocol.md` is "the contract between the host and its clients": a 4-byte little-endian length plus UTF-8 JSON, `hello`/`welcome`, ops on numbered blocks (`open`, `goto`, `events`, `eval` and more). `WebKitBuild/smoke-engine.py` is a reference client.
- **Releases.** `.github/workflows/release.yml` builds every target and publishes to `build.stencil.so`. The service lists ABKit 0.1.1 (published 2026-11-13, commit `ce3a57ab47dd`, which resolves in the public repository) for eight targets, 69 MB to 185 MB each (`https://build.stencil.so/api/products`, fetched 2026-11-14). The metadata has no licence field.
- **The archives carry no notices.** `abkit-0.1.1-windows-x64.zip` (73,158,567 bytes, SHA-256 `79616510b784...` matching the service) has 83 entries and no LICENSE, NOTICE or COPYING file; the only README is fontconfig's `etc/fonts/conf.d/README`. It bundles WebKit (BSD and LGPL files) and LGPL libraries such as GLib and GIO. BSD clause 2 and LGPL 2.1 section 6 require the notices or licence text to accompany a binary redistribution. The other seven archives were not listed (UNVERIFIED).
- **Stencil's Terms of Service** (effective 2026-09-17, `https://stencil.so/legal/terms`, fetched 2026-11-14): section 1.3 says software published under an open-source licence in its repository is governed by that licence when not provided on a hosted basis; section 1.2 grants a limited, non-sublicensable right to use the hosted Services for internal purposes; section 5(g) bars providing the Services to third parties. The terms do not name ABKit or `build.stencil.so`.

### Rules that bind

- Additive only: no change to an existing tool's name, schema or revision (brief, Hard constraints). Under ADR 0026, a revision bumps when "the argument contract or the model-facing projection changes" (`docs/adr/0026-intent-and-versioned-tools.md:30-32`).
- ADR 0024: no new roster tool. ADR 0049: vendor integrations live in extensions; core stays vendor-neutral (`docs/adr/0049-tiered-integration-architecture.md:17`). ADR 0036: extensions are Python. ADR 0035 and AGENTS.md error discipline: typed errors, no string payloads.
- No new language runtime in omp: the page scripts run in the browser's own JavaScript engine, as `RUN_RUNTIME` already does.

## Decision

**D1. The helpers are `tab.*` methods of the `run` runtime, not tool operations and not Rust ops.** With the gate on (D2), `tab` gains `reactEnable(opts?)`, `reactTree(opts?)`, `reactInspect(id)`, `reactRenders({action})`, `reactSuspense(opts?)` and `a11y(opts?)`. Each is an RPC through the existing queue to a new `dispatch_helper` arm, which evaluates an embedded page script on the tab's view with `automation().evaluate` and bounds the result: depth, node count and serialized bytes, with a truncation marker. Page scripts are `.js` files under `crates/envd/src/browser_inspect/`, included with `include_str!`, ported from upstream with its MIT notice. No new tool, no change to the `browser@3` schema, no new journal kind.

**D2. Gate: a default-off convar `sv_browser_inspect`.** Declared with `omp_con::var!` beside `SV_BROWSER_HEADLESS` (`crates/envd/src/browser_daemon.rs:63-85`): `bool`, default `false`, flags `archive` like its neighbours, no `PROJECT` flag (a repository must not be able to switch on script injection into the user's pages), no `legacy.path` (upstream has no such setting). `BrowserSettings` carries it. Gate off: the runtime source the daemon evaluates is byte-identical to today's `RUN_RUNTIME`, and the inspect ops are not served. Gate on: the daemon evaluates the runtime with the inspect methods inserted into the `tab` literal. `RUN_RUNTIME` is split into a prefix and suffix around that insertion point; a test pins the SHA-256 of the gate-off string to today's value. No `cap-*` feature for D1 to D6: they add no dependency, only about 30 KB of text, and `cap-*` features gate build weight, not behaviour. Implementation lands only behind this convar; the design is machine-reviewed at most and not owner-approved.

**D3. `omp-webview` gains post-construction document-start scripts.** New `WebView::add_init_script(&self, source) -> Result<InitScriptId>` and `WebView::remove_init_script(&self, id) -> Result<()>`, the runtime counterparts of `WebViewBuilder::init_script`: Chromium, CDP and relay views use `Page.addScriptToEvaluateOnNewDocument` and `Page.removeScriptToEvaluateOnNewDocument`; Firefox uses `script.addPreloadScript` and `script.removePreloadScript`; the system engine adds a `WKUserScript` at document start, and its removal returns `Error::Unsupported`, which is acceptable because system views are owned and die with their tab. `InitScriptId` is a typed newtype over the engine's identifier. Errors use existing `omp_webview::Error` variants. The builder API is unchanged. This is how the daemon reaches document start; it is an additive crate API, not a change to a tool, convar or scheme.

**D4. `reactEnable` per backend.** It (a) registers the hook with D3 once per tab and keeps the `InitScriptId` in the tab's session, (b) evaluates the hook in the live document, (c) reloads, and (d) reads back `{installed, active, reloaded, reactVersion?}`. Step (c) runs by default only on owned tabs (`headless`, `window`, `spawned`). On `cdp` and `relay` tabs, which are the user's pages, it reloads only with `reactEnable({reload: true})`; without it the result has `reloaded: false` and, unless a renderer registered late, `active: false`, and the hook still takes effect on the next navigation or reload because it is registered at document start. The registration survives navigation and reload for the tab's life, as upstream documents (`upstream:docs/tools/browser.md:159`). Closing a `cdp` or `relay` tab removes the registration before detaching, so the user's browser keeps no omp script, which matters most on the relay's shared attachment (`crates/envd/src/browser_relay.rs:7-8`). A repeated `reactEnable` does not register twice. If the engine refuses D3, `reactEnable` fails with the existing `browser_unsupported` fault (`crates/envd/src/browser_daemon.rs:1793-1795`). Any other `react*` helper on a document without the hook fails the run, like every helper fault today (`:970`), with the new fault code `browser_react_hook_missing` and the message "React DevTools hook not installed: call tab.reactEnable() first", as upstream (`devtools-hook.ts:12`).

**D5. Main frame only.** The React helpers read the main frame's hook only; React roots inside iframes are not reported, as upstream (`devtools-hook.ts:191`). `a11y` audits the main document and same-origin iframes reached through `contentDocument`, recursively, to a fixed depth; cross-origin frames are not audited and are counted in the report as `frames: {audited, skipped}`. A node target in a same-origin iframe is a nested selector array, like upstream's shadow-root boundaries (`audit.ts:18-19`). Per-frame evaluation in `omp-webview` is outside this record.

**D6. `a11y` runs omp's own deterministic rules first.** One page script, the same on every engine, over the DOM: `image-alt` (images without alternative text), `control-name` (form controls and buttons without an accessible name), `duplicate-id`, `html-lang` (missing `lang`), `heading-order` (heading-level skips), `landmark-main` (no `main` landmark), `link-name` (empty links). The result has upstream's `BrowserA11yResult` shape (`audit.ts:44-56`) with `engine: {name: "omp", version: <omp-envd version>}`, the same node and HTML bounds as upstream (`audit.ts:116-117`), and the frame counts of D5. Options: `rules`, `tags` (each rule carries WCAG tags), `selector` (subtree), `includeIncomplete`. The engine name tells the reader it has fewer rules than axe-core.

**D7. axe-core is optional, last, and blocked on O3.** If the owner accepts it: `crates/envd/assets/axe-core/axe.min.js` from the 4.13.0 tarball, unmodified, header kept, embedded with `include_str!`, behind a new envd feature `cap-browser-axe` (580 KB of text is build weight; forwarded by `omp-app`, not in its defaults) and still behind `sv_browser_inspect`; `tab.a11y({engine: "axe"})` selects it. Licence handling, all in the same PR: a package-local `crates/envd/assets/axe-core/NOTICE` with the copyright line, the MPL-2.0 notice, the exact tarball URL and SHA-256, the source repository tag, and the verbatim `LICENSE-3RD-PARTY.txt` for the bundled MIT and ISC components; an `axe-core 4.13.0` line with the tarball URL in the MPL-2.0 list of `THIRD-PARTY-NOTICES.txt` (`:25-41`), the `NOTICE` concatenated into its notice section, and the full MPL-2.0 text kept present in that file. Because the minified file is arguably MPL's Executable Form, the notice names where the Source Code Form is (the unminified `axe.js` in the same tarball), meeting section 3.2. No `deny.toml` change: the file is not a crate.

**D8. ABKit is not adopted in this pass.** Corrected reason: ABKit's licence and protocol are public (LGPL 2 and BSD per file; the host BSD 2-clause; `docs/protocol.md`), so the first draft's "no licence" and "private protocol" do not hold. What still stops it: (1) the published archives carry none of the notices their BSD and LGPL components require for binary redistribution, so omp2 must not bundle or mirror them, and a downloader would put an un-noticed binary on users' machines on omp2's behalf; (2) acquisition through `build.stencil.so` is a vendor service, so under ADR 0049 the downloader belongs in an extension, and Stencil's terms (sections 1.2 and 5(g)) do not say whether a third-party client may fetch builds for its users; (3) `omp-webview`'s `Engine` is a closed enum dispatched without `dyn` (`crates/webview/src/lib.rs:129`, `crates/webview/README.md:44`), so no extension can supply an engine, and a vendor-neutral stdio-host engine in core is a design of its own (O5). No engine variant, download step, bundling or protocol client is added. Revisit when O1, O2 and O5 are answered.

**D9. The `browser@3` description is not changed here.** With the convar on, the helpers can be called but the tool description (`crates/tools/src/browser.rs:293-304`) does not list them. Advertising them changes the model-facing projection, which under ADR 0026 bumps the revision; that is a change to an existing tool, so it is O6 and slice S5 waits on it.

## Rejected options

| Option | Why not |
|---|---|
| New tool operations (`action: "react_tree"` and the like) | Changes the `browser@3` argument contract; the tool is `open`/`run`/`close` with a code surface (`crates/tools/src/browser.rs:26-36`). |
| Install the hook through `tab.evaluate` only | Dies on every reload and navigation, and React registers with the hook at module load, so a live install sees no renderer. |
| Install the hook in every tab at `open` when the convar is on | Injects into the user's relay and attached pages even when nobody asked; D4 registers only on `reactEnable`. |
| Reload attached and relay tabs like upstream (`devtools-hook.ts:216`) | Reloads the user's own page and loses its state. |
| Use CDP's React DevTools backend | Needs the DevTools frontend bundle; Chromium only. |
| Audit every frame through the engine | `omp-webview` has no per-frame evaluate (`chromium.rs:980-985`, `firefox.rs:580-590`). |
| Bundle axe-core first | Licence decision and 580 KB before any evidence the own rules fall short. |
| A `cap-browser-inspect` feature for D1 to D6 | No dependency weight to gate; the convar gates behaviour. |
| Add `Engine::AbKit` and a downloader in core | Vendor acquisition in core (ADR 0049); archives lack notices. |
| Bundle or mirror ABKit archives | The archives carry no licence notices; omp2 would be redistributing un-noticed LGPL and BSD binaries. |

## Slices

Each slice is one PR; tests are written first and not edited to pass. Browser-backed tests run when `Engine::find` finds an engine and print why they skip otherwise; `OMP_BROWSER_TESTS=require` turns a missing engine into a failure. Whether CI runners have Chromium is UNVERIFIED.

**S1. Post-construction init scripts in `omp-webview` (D3).**
- Scope: `crates/webview/src/lib.rs` (`WebView::add_init_script`, `remove_init_script`, `InitScriptId`), the Chromium, Firefox and system-engine drivers.
- Gate: none; no caller until S2.
- Tests (crate, `crates/webview`): `added_init_script_runs_on_reload_and_navigation`, `removed_init_script_does_not_run_on_next_document`, `init_script_on_attached_view_is_scoped_to_its_session`; on macOS `system_engine_added_script_runs_on_next_document`, `system_engine_remove_is_unsupported`.
- Acceptance: existing `omp-webview` tests pass unchanged; `just test-pkg omp-webview`.

**S2. Convar, runtime composition and React helpers (D1, D2, D4, D5).**
- Scope: `sv_browser_inspect`, a `BrowserSettings` field (its test constructor already uses `..BrowserSettings::default()`, `crates/envd/tests/browser_relay_contract.rs:313-314`), the `RUN_RUNTIME` split, `crates/envd/src/browser_inspect/` with the hook and the tree, inspect, renders and Suspense scripts from `upstream:packages/coding-agent/src/tools/browser/react/`, five `dispatch_helper` arms, the `InitScriptId` in `TabSession` and its removal in `close`.
- Gate: `sv_browser_inspect`, default off.
- Tests: unit `gate_off_runtime_is_byte_identical` (pinned SHA-256), `missing_hook_envelope_maps_to_react_hook_missing_fault`, `oversized_tree_is_truncated_with_marker`. Integration `crates/envd/tests/browser_inspect.rs` (`required-features = ["cap-browser"]`) on `fixtures/browser/react/`, a page whose synthetic renderer drives the hook through `inject` and `onCommitFiberRoot` as React does: `react_helpers_are_undefined_with_gate_off`, `tree_lists_fixture_components`, `inspect_returns_props_and_selector`, `renders_counts_commits_after_a_click`, `hook_survives_navigation_within_the_tab`, `page_without_react_returns_an_empty_tree`, `enable_on_attached_tab_does_not_reload_without_opt_in` (a marker on `window` survives), `enable_with_reload_on_attached_tab_activates_the_hook`. Relay, in `crates/envd/tests/browser_relay_contract.rs` with its fake extension: `relay_react_enable_registers_and_close_removes_the_init_script` (the extension sees `Page.addScriptToEvaluateOnNewDocument`, then `Page.removeScriptToEvaluateOnNewDocument` with the same identifier).
- Acceptance: gate off, existing envd and tools tests pass unchanged and `browser@3`'s schema and revision tests (`crates/tools/src/browser.rs:506-623`) pass untouched. Real React is not in CI (UNVERIFIED against React 18 and 19 builds; recorded in `Status in omp` when run by hand). `just test-pkg omp-envd`.
- Depends on: S1.

**S3. `a11y` with omp's rules (D5, D6).**
- Scope: the audit page script, the `a11y` arm, the report type and a text summary in the shape of upstream's `formatA11ySummary` (`audit.ts:309`).
- Gate: `sv_browser_inspect`.
- Tests on fixture pages under `fixtures/browser/a11y/`: `one_known_violation_per_rule_yields_exactly_those_violations`, `clean_page_yields_none`, `selector_limits_the_audit_to_a_subtree`, `results_are_bounded_in_nodes_and_html`, `same_origin_iframe_violation_has_a_nested_target`, `cross_origin_frame_is_counted_as_skipped`, `rules_and_tags_filter_the_report`.
- Acceptance: as S2 for gate off. `just test-pkg omp-envd`.
- Depends on: S2 (convar and runtime composition).

**S4 (optional). axe-core (D7).**
- Scope: the vendored file, `NOTICE`, `THIRD-PARTY-NOTICES.txt` entries, `cap-browser-axe` in envd and `omp-app`.
- Gate: `cap-browser-axe`, off by default, plus `sv_browser_inspect`.
- Tests: `vendored_axe_sha256_is_pinned`, `vendored_axe_header_notice_is_intact`, `notices_file_lists_axe_core_with_its_source_archive` (reads `THIRD-PARTY-NOTICES.txt`, the shipped artefact), and the S3 fixtures run with `engine: "axe"` report axe rule ids. `just check-minimal-caps` passes.
- Depends on: S3. Blocked on O3.

**S5. Advertise the helpers (D9).**
- Scope: the `browser` description and whatever revision the owner rules in O6, with a journal lift if the revision moves.
- Tests: the tool's schema and revision tests; P4 (`crates/e2e/tests/p4_schema_isolation.rs`, live schema and recorded `rev`) and P10 (`crates/e2e/tests/p10_lift_idempotence.rs`) if the revision moves.
- Depends on: S2. Blocked on O6.

No e2e proof is added by S1 to S4: the joined path, tool to envd to daemon, is unchanged, and the behaviour is proven at its owning seam, `crates/envd` (AGENTS.md, Testing & QA). No golden journal: no journal kind or payload field changes.

## Upstream origin and license

S2 draws from `upstream:packages/coding-agent/src/tools/browser/react/` (`devtools-hook.ts`, `tree.ts`, `renders.ts`, `suspense.ts`); S3 from `upstream:packages/coding-agent/src/tools/browser/a11y/audit.ts` for the report shape, bounds and summary. Upstream is MIT (`upstream:LICENSE:1-5`; copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.); the embedded scripts keep that notice. axe-core 4.13.0 is MPL-2.0 with bundled MIT and ISC components (tarball `LICENSE`, `LICENSE-3RD-PARTY.txt`); D7 states its handling. ABKit: nothing reused, not its binaries, not its protocol text.

## Consequences

- With the convar on, agents can inspect React trees and audit pages on every engine omp2 drives, from the same `run` code they already write.
- `reactEnable` never reloads the user's page unless asked, and leaves no script behind in it.
- iframe React roots and cross-origin frames are out of reach; the report says what it skipped.
- The own-rule audit has seven rules, not axe-core's set; its engine name says so.
- The helpers stay unadvertised until O6 is answered.
- omp2 gains no WebKit-over-stdio engine; macOS keeps the system WKWebView.

## Open questions for the owner

- **O1. ABKit binary notices.** The 0.1.1 archives ship BSD and LGPL components with no licence texts or notices. Will Stencil add them, or may omp2 rely on the public repository at the release commit as the notice and source? Recommendation: ask Stencil to ship notices in the archives; until then do not fetch them for users.
- **O2. ABKit acquisition.** May a third-party client download `build.stencil.so` releases for its users under the Terms (sections 1.2, 1.3, 5(g))? Recommendation: ask in writing; if yes, the downloader is an extension (ADR 0049).
- **O3. axe-core.** Is an unmodified, vendored MPL-2.0 JavaScript file with bundled MIT and ISC parts acceptable, handled as D7 says, given that `deny.toml` admits MPL-2.0 per crate only (`deny.toml:28-35`)? Recommendation: yes, with D7's notice entries and pinned hash; otherwise drop S4.
- **O4. Real React coverage.** CI proves the hook against a synthetic renderer only. Vendor a React development build (MIT) as a test fixture, or keep the by-hand check? Recommendation: vendor one pinned React 19 development bundle under `fixtures/` with its notice.
- **O5. An engine seam.** ABKit, or any stdio browser host, needs either a vendor-neutral `Engine` variant for a host process at a user-supplied path or an extension seam for engines. Recommendation: a separate decision record; not this one.
- **O6. Advertising the helpers.** Adding the helpers to the `browser` description changes `browser@3`'s model-facing projection, which ADR 0026 says bumps the revision, and the additive-only rule forbids changing an existing tool's revision. Options: (a) a `browser@4` with a journal lift, used only when `sv_browser_inspect` is on; (b) leave the description unchanged and document the helpers in a skill shipped with the convar; (c) leave them unadvertised. Recommendation: (a), owner to rule; S5 waits.

## What the review changed

### Round 1 (EGM; findings as recorded in `docs/adr/STATUS.md` row 0067)

- **The helpers are JavaScript run-runtime helpers, not Rust ops.** Confirmed: the tool is `open`/`run`/`close` with a code surface (`crates/tools/src/browser.rs:26-36`), and helpers are `tab.*` methods in `RUN_RUNTIME` served by `dispatch_helper` (`crates/envd/src/browser_daemon.rs:1850-1896`, `:1104-1458`), as upstream's are tab methods (`upstream:packages/coding-agent/src/tools/browser/prelude.js:231`). Changed: D1 rewritten as runtime methods; D2 names the runtime composition and the byte-identical gate-off test; D9 and O6 added, because advertising the helpers moves `browser@3`'s projection (`docs/adr/0026-intent-and-versioned-tools.md:30-32`, `crates/tools/src/browser.rs:293-304`).
- **Reload on attached and relay tabs; the injection must survive navigation and reload.** Confirmed: upstream reloads unconditionally (`devtools-hook.ts:213-219`); `cdp` and `relay` tabs are the user's pages (`crates/tools/src/browser.rs:299-302`); the relay shares one `chrome.debugger` attachment per tab (`crates/envd/src/browser_relay.rs:7-8`). Changed: D4 (no reload by default on `cdp`/`relay`, document-start registration that survives navigation, removal on close), the rejected-options rows, and S2's attached and relay tests.
- **Init scripts are unreachable from the daemon.** Confirmed: `init_script` exists only on the builder (`crates/webview/src/lib.rs:303-310`), `open` sets none (`crates/envd/src/browser_daemon.rs:372-381`), and `WebView` has no post-construction method (`crates/webview/src/lib.rs:618-807`). Changed: D3 adds `add_init_script`/`remove_init_script` as an additive `omp-webview` API, and S1 delivers it first; the old O4 ("verify in S1") is resolved and removed.
- **Main frame only.** Confirmed: no per-frame evaluate (`crates/webview/src/remote/chromium.rs:980-985`, `crates/webview/src/remote/firefox.rs:580-590`), and upstream's React reads are main-frame too (`devtools-hook.ts:191`) while its audit walks frames through Puppeteer (`audit.ts:264-287`). Changed: D5 states the limit and the same-origin iframe walk; S2's `frames_are_audited_and_merged` replaced by `same_origin_iframe_violation_has_a_nested_target` and `cross_origin_frame_is_counted_as_skipped`.
- **The axe-core MPL-2.0 notice (O3).** Confirmed and widened: the npm tarball is MPL-2.0 and also bundles MIT and ISC components (`LICENSE-3RD-PARTY.txt`); the repository's notice practice is a package-local `NOTICE` plus an MPL-2.0 list entry with the source archive (`THIRD-PARTY-NOTICES.txt:25-41`, `:66-72`). Changed: D7 spells out the files, the Source Code Form pointer and the pinned hash; S4's tests check the shipped notices file; O3 restated.
- **ABKit "not adopted", checked against its published terms.** Corrected against source: the first draft said no licence and no repository were found and the protocol was private. The public repository `stencil-hq/abkit` states LGPL 2 and BSD per file, the host is BSD 2-clause, and `docs/protocol.md` is public; the 0.1.1 archive still ships no notices. Changed: Context section on ABKit's terms, D8's reasons, O1 and O2 restated, O5 kept. The verdict stays "not adopted in this pass".
- Also corrected while verifying: a `cap-browser-inspect` feature was dropped in favour of the convar alone, because D1 to D6 add no dependency weight.

Not machine-reviewed after this revision: rounds 2 and 3 are pending.

## Acceptance tests

1. S1: a script added after build runs on reload and on navigation, and no longer runs after removal, on Chromium; on macOS the system engine runs it and refuses removal.
2. S2: with `sv_browser_inspect` off, the evaluated runtime is byte-identical to today's and `typeof tab.reactTree` is `"undefined"` in `run` code.
3. S2: with it on, the fixture page yields the expected tree, props and selector, and render counts after a click; the hook survives navigation within the tab.
4. S2: `reactEnable` on an attached tab does not reload without `{reload: true}`; closing a relay tab removes its registration.
5. S3: each own rule reports exactly its fixture violation; a clean page reports none; cross-origin frames are counted as skipped.
6. S4: the vendored axe-core matches its pinned SHA-256 and the notices file lists it with its source archive.

## Status in omp

**Status: Not started.** Reusable today: the `run` runtime and `dispatch_helper` (`crates/envd/src/browser_daemon.rs:899-1458`), `WebViewBuilder::init_script` (`crates/webview/src/lib.rs:303-310`), `ariaSnapshot` and `accessibility_snapshot` (`crates/webview/src/automation.rs:481-499`). Not yet implemented: everything in D1 to D7.

## References

[docs/research/browser-inspection.md](../research/browser-inspection.md); ADR 0024 (fixed roster); ADR 0026 (versioned tools); ADR 0035 (errors); ADR 0036 (Python-only extensions and eval); ADR 0049 (vendors as extensions); `https://github.com/stencil-hq/abkit` (`ReadMe.md`, `docs/protocol.md`, `docs/installing.md`).
