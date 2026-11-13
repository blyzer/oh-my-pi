# Gap 13: React inspection, accessibility audit and ABKit as a browser engine

Status: research only. Nothing here is implemented or authorized.
Date: 2026-11-13
Baseline: omp2 f4186b783b; upstream c308970263 (pinned checkout).

Evidence keys:

- **OBSERVED** means read in code or docs at the baseline, or fetched from a named URL on 2026-11-13. Code citations use `path:line`. Upstream paths carry the `upstream:` prefix.
- **[INFERENCE]** means the author's reasoning. Every size estimate is an inference.
- **UNVERIFIED** means no run or source confirmed it.

No build, test or run of omp2 was made for this document. The ABKit archive was downloaded once, listed and deleted.

## Question

Upstream's browser tool can inspect a React component tree, run an accessibility audit, and drive its own WebKit engine, ABKit. What does each do, what does `omp-webview` offer today, and may omp2 adopt ABKit? ABKit is research only unless its distribution terms are clear. They are not (section 1).

Out of scope: Web Vitals (`vitals`), the Tern terminal's own browser blocks, and the Chromium relay.

## 1. Upstream

### React inspection

- The helpers are `reactEnable`, `reactTree`, `reactInspect`, `reactRenders` and `reactSuspense`; `reactEnable` must run first because it installs the hook and reloads the page (`upstream:docs/tools/browser.md:95`). The hook stays installed for the tab's life (`upstream:docs/tools/browser.md:159`).
- The hook is upstream's own page script, a minimal stand-in for the React DevTools global hook, not React DevTools code (`upstream:packages/coding-agent/src/tools/browser/react/devtools-hook.ts:21-24`). It records renderers, roots and fibers and tracks Suspense ids and render commits.
- `reactTree` returns a bounded tree of `{id, name, type, key, props, children}` with `maxDepth` and an `includeHost` option (`upstream:packages/coding-agent/src/tools/browser/react/tree.ts:7-20`). `reactInspect(id)` returns props, hook state, source file and line, and a DOM selector for one fiber (`upstream:packages/coding-agent/src/tools/browser/react/tree.ts:38-44`). `reactRenders` starts, stops or reports render counts per component (`upstream:packages/coding-agent/src/tools/browser/react/renders.ts:6-16`). `reactSuspense` lists boundaries and their state (`upstream:packages/coding-agent/src/tools/browser/react/suspense.ts:6-12`).
- A call without the hook returns a fixed error telling the agent to call `reactEnable` (`upstream:packages/coding-agent/src/tools/browser/react/devtools-hook.ts:12`).
- Size: about 770 lines of TypeScript across the five files (226 + 247 + 63 + 88, plus vitals) on the pinned checkout.

### Accessibility audit

- `a11y` runs axe-core in the page and returns a bounded report (`upstream:docs/tools/browser.md:83`). Options are axe tags, rule ids, a CSS selector and whether to include rules needing manual review (`upstream:packages/coding-agent/src/tools/browser/a11y/audit.ts:5-14`).
- The result carries the page URL, the engine name and version, counts of violations, incomplete and passes, and for each violation its rule id, impact, help text and URL, tags, node count and failing nodes with a selector path (shadow-root boundaries kept) and truncated HTML (`upstream:packages/coding-agent/src/tools/browser/a11y/audit.ts:16-56`). `formatA11ySummary` renders a short agent-readable text form (`upstream:packages/coding-agent/src/tools/browser/a11y/audit.ts:309`).
- axe-core is a pinned dependency, version 4.13.0, imported as text and injected (`upstream:packages/coding-agent/src/tools/browser/a11y/audit.ts:2`, `upstream:packages/coding-agent/package.json:557`). The npm registry lists axe-core 4.13.0 under **MPL-2.0** (fetched 2026-11-13 from `https://registry.npmjs.org/axe-core/4.13.0`).
- It audits frames one by one and merges partial results (`upstream:packages/coding-agent/src/tools/browser/a11y/audit.ts:58-71`).

### ABKit

- What it is: omp's own browser engine, "ABKit (a patched WPE WebKit)", started as a child process, `ABHost`, one per omp process, with one view per tab (`upstream:docs/tools/browser.md:154`; `upstream:packages/coding-agent/src/tools/browser/webkit/kind.ts:1-11`). It is the default browser when installed, with Chromium as the fallback (`upstream:packages/coding-agent/src/tools/browser/webkit/kind.ts:6-8`).
- Protocol: not CDP. The host answers Tern's JSON script protocol over stdin and stdout, as frames of a `u32` little-endian length and one JSON object, after a `hello` and `welcome` handshake. The comments say the protocol lives in `crates/tern/src/daemon/json.rs` "in the stencil repository" (`upstream:packages/coding-agent/src/tools/browser/tern/wire.ts:1-19`; `upstream:packages/coding-agent/src/tools/browser/webkit/host.ts:1-13`). That repository is not part of the pinned checkout, and the `tern` product is listed as not public (build-service product list, below).
- Acquisition: the engine is not bundled in omp. omp downloads it from Stencil's build service, product `abkit`, newest stable build, into `~/.o2/lib/abkit`, checks size and SHA-256 while streaming, unpacks beside the target and renames it into place under a lock (`upstream:packages/coding-agent/src/tools/browser/webkit/install.ts:1-12`, `upstream:packages/coding-agent/src/tools/browser/webkit/install.ts:29-33`). The service origin is `https://build.stencil.so`, overridable by `PI_BUILD_URL` (`upstream:packages/coding-agent/src/cli/build-service.ts:48`).
- Platforms: the build service lists 8 archives for ABKit 0.1.0 (stable, published 2026-11-12): macOS arm64 and x64, Linux glibc and musl on x64 and arm64, Windows x64 and arm64, from 69 MB to 183 MB each (fetched 2026-11-13 from `https://build.stencil.so/api/products`). A window viewer, `ABView`, ships only for macOS (`upstream:packages/coding-agent/src/tools/browser/webkit/kind.ts:21-25`).
- A page cannot tell it is driven, and no CDP is exposed; the upstream docs list what engine features differ (`upstream:docs/tools/browser.md:154`).

### ABKit distribution terms: what exists and what does not

What I could find, and where:

1. **omp's own licence** is MIT, with copyright lines for Mario Zechner, Can Bölük and Stencil Labs, Inc. (`upstream:LICENSE:1-5`). It covers the omp repository. It does not mention ABKit. A search of the pinned checkout's `README.md`, `THIRD-PARTY*`, `about.toml`, `deny.toml` and `docs/*.md` for ABKit with a licence, redistribution or terms word found nothing.
2. **Stencil's Terms of Service** (effective 2026-09-17, `https://stencil.so/legal/terms`, fetched 2026-11-13). Three passages matter. Section 1.3: "Portions of the Services are published under open-source licenses in their respective repositories. Your use of that software, when not provided on a hosted basis, is governed exclusively by the applicable open-source license, not these Terms." Section 5(c) bars you from "reverse engineer, decompile, disassemble, or otherwise attempt to derive source code or underlying models of the Services, except software licensed to you under Section 1.3". Section 9: "Except as expressly granted in these Terms or an applicable open-source license, no rights in the Services or Third-Party Models are granted to you, by implication or otherwise." The terms do not name ABKit. Whether ABKit is "published under an open-source licence in its repository" depends on a repository I could not find.
3. **A source repository**: not found. Searched: `https://github.com/stencil-labs`, `https://github.com/stencilhq`, `https://github.com/can1357/abkit` (all HTTP 404), a GitHub repository search for `abkit` (results were unrelated projects), and a GitHub code search for `ABHost` (unrelated hits). The `omp` README links only `https://github.com/can1357/oh-my-pi` (`upstream:README.md:713-722`).
4. **The archive itself.** I downloaded `abkit-0.1.0-windows-x64.zip` (73,132,655 bytes) from the build service and listed it. It has 83 entries. It holds `ABHost.exe`, `ABNetworkProcess.exe`, `ABWebProcess.exe`, `WPEWebKit-2.0.dll`, `WPEInjectedBundle.dll` and many third-party libraries such as `libglib-2.0-0.dll`, `libgobject-2.0-0.dll`, `libgio-2.0-0.dll`, `libffi-8.dll`, `libharfbuzz-0.dll`, `libfreetype-6.dll`, `libcrypto-3-x64.dll` and `libicudt78.dll`. It contains **no LICENSE, NOTICE or COPYING file** and no source offer; the only text file matching a licence or readme pattern is `abkit/etc/fonts/conf.d/README`. I did not unpack or run any binary, and I deleted the archive afterwards. The other seven platform archives were not inspected (UNVERIFIED that they match).
5. **The build service's metadata** has no licence field: the product entry has only `id`, `name`, `public` and `access`, and each file has `name`, `platform`, `arch`, `kind`, `size`, `sha256` and `url` (fetched 2026-11-13).

What that means. ABKit is a public download with no published licence. Its contents include components whose usual licences require notices and, for LGPL parts, a way to obtain or relink the source. That is [INFERENCE]: I did not read those components' licence texts, and I do not know which licence the patched WebKit ships under or where its patches are published. I could not find any statement that lets a third party redistribute or bundle ABKit.

## 2. omp2 inventory

- `omp-webview` is "modular embedded-browser surfaces", with a pluggable engine: `system` (WKWebView, macOS, in process), `chromium` (CDP) and `firefox` (WebDriver BiDi) (`crates/webview/README.md:15-17`). The `Engine` enum also has CDP-endpoint and relay variants (`crates/webview/src/lib.rs:129`). Backends are dispatched by enum, not `dyn` (`crates/webview/README.md:42-43`). `remote` runs one driver thread per view, with `chromium` and `firefox` on a shared websocket layer, and `discover` finds installed browsers (`crates/webview/README.md:47-51`).
- Automation is engine neutral. Chromium uses CDP for accessibility and file input, and the system and Gecko engines use injected DOM helpers (`crates/webview/src/automation.rs:1-4`). `AutomationCapabilities` reports `dom`, `native_accessibility` and `file_upload` (`crates/webview/src/automation.rs:155-162`). `accessibility_snapshot` returns the native Chromium tree, or injected ARIA YAML on other engines, capped at 4 MiB (`crates/webview/src/automation.rs:481-499`, `crates/webview/src/automation.rs:23`).
- The browser tool lives in `omp-tools` (`crates/tools/src/browser.rs:1`) and runs in a supervised daemon in `omp-envd` behind `cap-browser`, which pulls in `omp-webview` (`crates/envd/Cargo.toml:19`). The daemon dispatches helper ops: `observe`, `ariaSnapshot`, `screenshot`, `extract`, `click`, `type`, `fill`, `press`, `scroll`, `drag`, `select`, `uploadFile`, `evaluate`, the `waitFor*` family, `download` and request interception (`crates/envd/src/browser_daemon.rs:1137`, `crates/envd/src/browser_daemon.rs:1168`, `crates/envd/src/browser_daemon.rs:1375`, `crates/envd/src/browser_daemon.rs:1408`, `crates/envd/src/browser_daemon.rs:1438`).
- `evaluate` runs arbitrary page script on every engine (`crates/envd/src/browser_daemon.rs:1375-1387`). That is the seam through which upstream's React hook and the audit script would be injected.
- Not found (searched: `react`, `a11y`, `axe`, `vitals`, `reactTree` in `crates/envd/src/browser_daemon.rs`, `crates/tools/src/browser.rs` and `crates/webview/src`): no React helpers, no audit, no vitals. `ariaSnapshot` is the only accessibility tool.
- No ABKit and no WebKit-over-stdio engine: not found (searched: `abkit`, `ABHost`, `wpe` across `crates/`).
- Licence policy: `deny.toml` allows permissive licences only, with MPL-2.0 allowed per package for five named crates (`deny.toml:13-35`). A vendored JavaScript asset is not a crate, so `cargo deny` would not see it; a notice would go in `THIRD-PARTY-NOTICES.txt`.
- ADR 0049 puts vendor integrations in extensions and keeps core vendor-neutral. The owner ruling of 2026-11-09 extends the rule to every vendor (`docs/adr/0049-tiered-integration-architecture.md:17`).

### Gap table

| Capability | Upstream | omp2 | Gap |
|---|---|---|---|
| React tree, inspect, renders, Suspense | page hook plus 4 helpers, about 770 lines | none | port; page script via `evaluate` |
| Accessibility audit | axe-core 4.13.0 injected, bounded report | `ariaSnapshot` only | new op; axe is MPL-2.0 |
| ABKit engine | WebKit host over Tern JSON on stdio | none | no public protocol spec or licence; research only |
| Engine abstraction | process kinds | `Engine` enum | a new engine needs a new variant and a driver |

## 3. Class

**Class c (port)** for React inspection and the accessibility audit. Both are TypeScript wrappers around page-side scripts, and omp2 already has the seam to run them (`evaluate`). They add a Rust wrapper plus an embedded script, with no new language runtime in the host: the script runs in the browser's own engine.

**ABKit: not adopted.** It is a vendor product with no published licence and a protocol specified in a private repository. Under ADR 0049 it could only ever be an extension. Verdict for ABKit: **research only; do not adopt or bundle** until the items in section 6 are cleared. It gets no slice.

## 4. Options

### React inspection

1. **Build on existing omp2 code.** Use `evaluate` to inject a hook script and read it back. Cost: 3 to 5 days. Fit: good. No engine changes.
2. **Port upstream's hook and helpers as the page script plus thin Rust helpers.** This is option 1 with upstream's logic as the starting point. Cost: the same, with less design risk, because the hook is already worked out for fibers, Suspense and render counts.
3. **Use CDP's React DevTools backend.** Cost: high; it needs the React DevTools frontend bundle and works on Chromium only. Fit: poor.

Choice: option 2, which is option 1 seeded from upstream. [INFERENCE]

### Accessibility audit

1. **Bundle axe-core** as a vendored asset, as upstream does. Cost: 3 to 4 days. Fit: broad coverage, about 90 rules maintained elsewhere. Costs: about 3 MB unpacked (the minified file is smaller, size not measured: UNVERIFIED), a pinned third-party file, and **MPL-2.0**, a file-level copyleft licence. Shipping the file unmodified with its notice and a source pointer is the usual compliance path [INFERENCE], and the owner should confirm.
2. **A small deterministic rule set** over the DOM and the accessibility snapshot omp2 already captures: images without alternative text, controls without an accessible name, duplicate ids, missing `lang`, heading-level skips, landmark gaps, empty links. Cost: 3 to 5 days. Fit: good with AGENTS.md's "deterministic rules first". Fewer rules, no licence question.
3. **Both**: own rules first, axe-core as an opt-in extra.

Choice: option 3, starting with option 2. [INFERENCE]

### ABKit

1. **Add an `Engine::AbKit` variant and a stdio driver** speaking Tern's JSON protocol, plus the download step. Cost: weeks. Blocked: no licence, no protocol spec, vendor product.
2. **Ship it as an extension** that registers the engine. omp2's `Engine` enum is closed and dispatched without `dyn`, so an extension cannot add an engine today (`crates/webview/README.md:42-43`). That is a core change first.
3. **Do nothing.** Chromium and Firefox cover automation, the system webview covers macOS. Cost: zero.

Choice: option 3. [INFERENCE]

## 5. Plan

Both slices ship behind `cap-browser-inspect`, a feature of `omp-envd` that depends on `cap-browser`, and a default-off convar `sv_browser_inspect`. Nothing changes for an existing browser op, name or schema: the new ops are additive.

### Slice 1: React helpers

- Add the hook script, the tree, inspect, renders and Suspense sources as embedded scripts, and ops `reactEnable`, `reactTree`, `reactInspect`, `reactRenders`, `reactSuspense` to the daemon, run through `evaluate`. Bound every result: depth, node count, bytes. Return the fixed "call `reactEnable` first" error when the hook is absent.
- `reactEnable` has to install the hook before page scripts run, and reload. Whether the webview layer offers a "script at document start" call on every engine was not traced (UNVERIFIED); if not, that is a prerequisite change in `omp-webview`.
- Acceptance: on a local fixture page built with a React production-free development bundle, `reactTree` lists the expected components, `reactInspect` returns props and a DOM selector, `reactRenders` counts re-renders after a click, and a result larger than the byte cap is truncated with a marker. A page with no React returns an empty tree, not an error.
- Tests: Rust unit tests for the envelope handling and the byte caps with canned JSON; an integration test using a discovered Chromium, skipped when none is found. How existing webview tests skip is UNVERIFIED.

### Slice 2: audit with own rules

- Add an `a11y` op returning the report shape of `BrowserA11yResult` (page URL, engine name, counts, violations with id, impact, help, nodes with selector and truncated HTML), engine name `omp`, and a text summary like `formatA11ySummary`.
- Acceptance: a fixture page with one known violation per rule yields exactly those violations; a clean page yields none; `selector` limits the audit to a subtree; results are bounded in nodes and bytes; frames are audited and merged.
- Tests: fixture pages under the webview test fixtures, run on the system engine where available and on Chromium.

### Slice 3 (optional): axe-core

- Vendor axe-core 4.13.0 unmodified, embedded as text, with an `engine: axe-core` report. Add the notice to `THIRD-PARTY-NOTICES.txt` and record the MPL-2.0 decision.
- Acceptance: the same fixture pages report axe's rule ids; the vendored file's SHA-256 is pinned in a test.
- Gate: `cap-browser-axe`, off by default. Blocked on the owner's licence decision (section 6).

### ABKit

No slice. See section 6 for what must be cleared first.

## 6. Open questions

1. **ABKit licence.** Who owns it, under what licence is the patched WPE WebKit published, and where are the patches and the LGPL-style source obtained? The archive has no notice files. This needs an answer from Stencil Labs, not from this repository.
2. **ABKit redistribution.** May a third party download from `build.stencil.so` programmatically, mirror the archives, or bundle them in releases? Terms section 9 grants no rights beyond an open-source licence, and none was found (section 1).
3. **ABKit protocol.** The engine speaks a protocol specified in a private Stencil repository. omp2 would be writing to a spec it cannot read, only to upstream's client code (MIT).
4. **ABKit scope.** The platform list is eight archives; the viewer is macOS only; `cargo deny` and `about.toml` do not cover a downloaded binary, so a notice process would have to be invented.
5. **ADR 0049.** An ABKit engine is a vendor integration and must live in an extension. omp2's `Engine` enum is closed, so that needs a core change to let an extension register an engine. That is its own gap.
6. **axe-core and MPL-2.0.** Is a vendored, unmodified MPL-2.0 JavaScript file acceptable under the repository's licence policy, which allows MPL-2.0 only per crate (`deny.toml:30-35`)? If not, slice 3 is dropped and slice 2 stands alone.
7. **Document-start scripts.** Does every `omp-webview` engine support running a script before page scripts? React inspection needs it (UNVERIFIED).
8. No locked ADR is changed. ADR 0036 (Python-only extensions and eval) is not touched: the injected scripts run in the browser, not in an omp extension runtime. If the owner reads that ADR more widely, slices 1 and 3 need a ruling.

## 7. Upstream origin and license

A port of slice 1 would draw from `upstream:packages/coding-agent/src/tools/browser/react/devtools-hook.ts`, `tree.ts`, `renders.ts` and `suspense.ts`. A port of slice 2 would draw from `upstream:packages/coding-agent/src/tools/browser/a11y/audit.ts` for the report shape and summary. These are TypeScript under upstream's MIT licence (`upstream:LICENSE:1-5`; copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.). The page scripts are JavaScript strings inside those files and would be embedded in Rust, keeping the notice.

axe-core 4.13.0 is MPL-2.0 per the npm registry (not read from a licence file in this checkout: the checkout has no `node_modules`). A vendored copy keeps its licence and notice and must stay unmodified.

ABKit has no licence that I could find. Nothing from it is proposed for reuse: no file, no binary, no protocol text. The only upstream files touching it (`webkit/host.ts`, `webkit/install.ts`, `webkit/kind.ts`, `tern/wire.ts`) are MIT, but the protocol and the engine they talk to are not covered by that licence as far as I can tell.

