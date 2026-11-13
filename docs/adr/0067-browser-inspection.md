# 0067. Browser inspection: React helpers and an accessibility audit through `evaluate`; ABKit is not adopted

Status: proposed
Date: 2026-11-13
Area: tools

Research: [docs/research/browser-inspection.md](../research/browser-inspection.md) (Gap 13 of the upstream gap closure). This record states decisions; the note holds the evidence.

## Context

### What OMPv1 does

- React inspection: `reactEnable`, `reactTree`, `reactInspect`, `reactRenders`, `reactSuspense`; `reactEnable` must run first because it installs the hook and reloads the page (`upstream:docs/tools/browser.md:95`). The hook is upstream's own minimal page script standing in for the React DevTools global hook, not React DevTools code (`upstream:packages/coding-agent/src/tools/browser/react/devtools-hook.ts:21-24`). Results are bounded: a tree of `{id, name, type, key, props, children}` and, per fiber, props, hook state, source location and a DOM selector (`upstream:packages/coding-agent/src/tools/browser/react/tree.ts:7-44`). About 770 lines of TypeScript.
- Accessibility audit: axe-core 4.13.0 injected into the page, with a bounded report of violations, each with rule id, impact, help, tags and failing nodes (`upstream:packages/coding-agent/src/tools/browser/a11y/audit.ts:16-56`, `upstream:packages/coding-agent/package.json:557`). The npm registry lists axe-core 4.13.0 as MPL-2.0 (fetched 2026-11-13).
- ABKit: omp's own WebKit engine, "a patched WPE WebKit", started as the child process `ABHost` and driven over Tern's JSON script protocol on stdin and stdout, not CDP (`upstream:docs/tools/browser.md:154`, `upstream:packages/coding-agent/src/tools/browser/tern/wire.ts:1-19`). The protocol is specified in Stencil's private repository. omp does not bundle it: it downloads the newest stable build from `https://build.stencil.so` into `~/.o2/lib/abkit` (`upstream:packages/coding-agent/src/tools/browser/webkit/install.ts:1-12`, `upstream:packages/coding-agent/src/cli/build-service.ts:48`).

### What omp2 has

- `omp-webview` has a pluggable engine: system WKWebView, Chromium over CDP, Firefox over WebDriver BiDi (`crates/webview/README.md:15-17`). `Engine` is a closed enum dispatched without `dyn` (`crates/webview/src/lib.rs:129`, `crates/webview/README.md:42-43`).
- The browser tool runs in a supervised daemon in `omp-envd` behind `cap-browser` (`crates/envd/Cargo.toml:19`). It dispatches `observe`, `ariaSnapshot`, `screenshot`, `click`, `evaluate` and more (`crates/envd/src/browser_daemon.rs:1137`, `crates/envd/src/browser_daemon.rs:1168`, `crates/envd/src/browser_daemon.rs:1375`). `evaluate` runs arbitrary page script on every engine; that is the seam for a hook and an audit. `ariaSnapshot` is the only accessibility tool; there are no React helpers and no audit (searched: `react`, `a11y`, `axe`, `vitals`).
- `deny.toml` allows MPL-2.0 per named crate only (`deny.toml:30-35`); a vendored JavaScript file is not a crate.

### ABKit's terms, as far as they could be found

- Upstream's own `LICENSE` is MIT and does not mention ABKit (`upstream:LICENSE:1-5`).
- Stencil's Terms of Service (effective 2026-09-17, `https://stencil.so/legal/terms`, fetched 2026-11-13): section 1.3 says open-source portions are governed by their own licences; section 9 says "no rights in the Services ... are granted to you, by implication or otherwise" except by the Terms or an open-source licence. ABKit is not named.
- No ABKit source repository was found (`github.com/stencil-labs`, `github.com/stencilhq`, `github.com/can1357/abkit` all 404). The public build service lists ABKit 0.1.0 with eight archives and no licence field.
- The Windows archive was downloaded once, listed and deleted. It has 83 entries, including `WPEWebKit-2.0.dll` and third-party libraries such as `libglib-2.0-0.dll`, `libgio-2.0-0.dll` and `libcrypto-3-x64.dll`, and no LICENSE, NOTICE or source offer. Which licences those components carry was not read.

### Rules that bind

- ADR 0049: vendor integrations live in extensions; core stays vendor-neutral (`docs/adr/0049-tiered-integration-architecture.md:17`).
- Additive only: new browser ops, no change to an existing op's name or schema.
- No new language runtimes: the injected scripts run inside the browser, not in an omp runtime. ADR 0036 is not touched.

## Decision

**D1. React helpers through `evaluate`.** Port upstream's hook and the tree, inspect, renders and Suspense sources as embedded page scripts, with thin Rust ops `reactEnable`, `reactTree`, `reactInspect`, `reactRenders`, `reactSuspense`. Every result is bounded in depth, nodes and bytes. A call without the hook returns a fixed error telling the agent to call `reactEnable` first.

**D2. An accessibility audit with omp's own deterministic rules first.** An `a11y` op returns upstream's report shape with engine name `omp`: images without alternative text, controls without an accessible name, duplicate ids, missing `lang`, heading-level skips, landmark gaps, empty links, run over the DOM and the accessibility snapshot omp2 already captures. A text summary accompanies it.

**D3. axe-core is optional and last.** Vendor axe-core unmodified behind a separate feature, with its notice, only if the owner accepts an unmodified MPL-2.0 JavaScript file under the licence policy (O3).

**D4. ABKit is not adopted.** No engine variant, no download step, no bundling, no protocol implementation. Reason: no licence for ABKit was found, the archive ships no notices for the third-party and likely copyleft components it contains, the terms grant no redistribution rights, and the protocol is specified in a private repository. Under ADR 0049 it could only ever be an extension, and omp2's engine enum is closed, so even that needs a separate core decision. Revisit only when the items in O1 and O2 are answered in writing by the owner of ABKit.

**D5. Gates.** D1 and D2 ship behind feature `cap-browser-inspect` (depends on `cap-browser`) and default-off convar `sv_browser_inspect`. D3 ships behind `cap-browser-axe`, off by default.

## Rejected options

- **Use CDP's React DevTools backend.** Needs the DevTools frontend bundle and works on Chromium only.
- **Bundle axe-core first.** Licence question and size before any evidence the own rules fall short.
- **Add an `Engine::AbKit` variant.** Blocked on licence, protocol and ADR 0049.
- **Bundle or mirror ABKit archives in releases.** No redistribution right found.
- **Download ABKit at runtime like upstream.** Even without bundling, omp2 would depend on a third party's service and on terms that grant it nothing.

## Slices

Each slice is one PR; tests are written first and not edited to pass.

**S1. React helpers.** Embedded scripts and the five ops. `reactEnable` needs a script that runs before page scripts; whether every engine offers one is UNVERIFIED and is a prerequisite check.
- Tests: unit tests on envelope handling and byte caps with canned JSON: `missing_hook_returns_the_fixed_error`, `result_over_the_byte_cap_is_truncated_with_a_marker`. Integration on a fixture page with a React development bundle, run only when a Chromium is discovered: `tree_lists_expected_components`, `inspect_returns_props_and_dom_selector`, `renders_counts_rerenders_after_a_click`, `page_without_react_returns_an_empty_tree`.
- Gate: `cap-browser-inspect` and `sv_browser_inspect`.

**S2. Audit with own rules.** The `a11y` op.
- Tests on fixture pages: `one_known_violation_per_rule_yields_exactly_those_violations`, `clean_page_yields_none`, `selector_limits_the_audit_to_a_subtree`, `results_are_bounded_in_nodes_and_bytes`, `frames_are_audited_and_merged`.

**S3 (optional). axe-core.** Vendored, unmodified, embedded as text, with the notice added to `THIRD-PARTY-NOTICES.txt`.
- Tests: `vendored_file_sha256_is_pinned`, the S2 fixtures report axe rule ids.
- Gate: `cap-browser-axe`. Blocked on O3.

## Upstream origin and license

S1 draws from `upstream:packages/coding-agent/src/tools/browser/react/` (`devtools-hook.ts`, `tree.ts`, `renders.ts`, `suspense.ts`); S2 from `upstream:packages/coding-agent/src/tools/browser/a11y/audit.ts` for the report shape and summary. TypeScript and embedded JavaScript under MIT (`upstream:LICENSE:1-5`; copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.); the notice stays. axe-core 4.13.0 is MPL-2.0 per the npm registry and must stay unmodified with its notice if vendored. ABKit: nothing reused.

## Consequences

- Agents can inspect React trees and audit pages on any engine omp2 already drives.
- An own-rule audit has fewer rules than axe-core; its report says so through the engine name.
- omp2 does not gain a WebKit-over-stdio engine; macOS keeps the system WKWebView.

## Open questions for the owner

- **O1.** ABKit licence: who owns it, what licence the patched WPE WebKit ships under, where the source and patches are published. Recommendation: ask Stencil Labs in writing; do not proceed without an answer.
- **O2.** ABKit redistribution and use of `build.stencil.so` by a third party. Recommendation: treat as not permitted until stated.
- **O3.** Is an unmodified, vendored MPL-2.0 JavaScript file acceptable when `deny.toml` allows MPL-2.0 per crate only (`deny.toml:30-35`)? Recommendation: yes, with a notice entry and a pinned hash, if the owner agrees; otherwise drop S3.
- **O4.** Document-start scripts on every engine. Recommendation: verify in S1 and add the call to `omp-webview` if missing.
- **O5.** If ABKit is ever wanted, an extension seam for engines is needed first, since `Engine` is closed. Recommendation: separate decision.

## Status in omp

**Status: Not started.** Reusable today: `evaluate`, `ariaSnapshot`, `accessibility_snapshot` (`crates/webview/src/automation.rs:481-499`), the browser daemon's op dispatch.

## References

[docs/research/browser-inspection.md](../research/browser-inspection.md); ADR 0049 (vendors as extensions); ADR 0036 (Python-only extensions and eval).

