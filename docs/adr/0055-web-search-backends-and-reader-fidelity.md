# 0055. Web search backends and reader fidelity

Status: Proposed 2026-11-13
Date: 2026-11-13
Area: tools

## Context

Research: [`docs/research/web-readers.md`](../research/web-readers.md) (Gap 8). Upstream is pinned at `c308970263` (v18.20.0). Paths prefixed `upstream:` are in that tree; others are in omp2 at `ed95a14a41`.

- **All 75 upstream scrapers already have an omp2 home.** Sixteen dedicated modules (`crates/tools/src/read/web/scrapers/mod.rs`, `Scraper::ALL: [Self; 17]`) plus a 59-row table (`crates/tools/src/read/web/scrapers/catalog.rs`, `ALL: [Site; 59]`). A row matches by host (`site_matches`), builds one GET URL from the **last path segment** (`api_url`), and flattens the reply with `render_json` (depth ≤ 5, 80 keys per object, 40 array items, 450 000 bytes).
- **The seven named readers mostly misbehave.**
  - CISA KEV (`catalog.rs:254`, `:328`) fetches the whole feed and prints its first 40 entries, not the requested CVE. Upstream extracts the CVE id (`upstream:packages/coding-agent/src/web/scrapers/cisa-kev.ts:24`, `:43-46`) and selects the matching entry (`:60`).
  - SEC EDGAR (`catalog.rs:261`, `:333`) has no API URL, so it always falls through. Upstream reads `data.sec.gov/submissions/CIK<10 digits>.json` (`sec-edgar.ts:177`) after extracting the CIK from several URL shapes.
  - VS Code Marketplace (`catalog.rs:297`) has no API URL. Upstream POSTs to `marketplace.visualstudio.com/_apis/public/gallery/extensionquery` (`vscode-marketplace.ts:116`, `method: "POST"` at `:129`).
  - JetBrains (`catalog.rs:310`) sends `1234-name` as the plugin id. Upstream takes the leading digits and also calls `/updates?size=1` (`jetbrains-marketplace.ts:92-93`).
  - pub.dev (`catalog.rs:308`) takes the last segment, which is wrong on `/versions` and `/changelog`. Upstream matches `^/packages/([^/]+)` and reads the README (`pub-dev.ts:10`, `:119`).
  - Semantic Scholar (`catalog.rs:325`) works for `/paper/<slug>/<sha>` but loses TLDR, `externalIds` and the open-access PDF (`semantic-scholar.ts:76`).
  - Firefox add-ons (`catalog.rs:296`) works for `/addon/<slug>/` and breaks on `/addon/<slug>/reviews/`.
- **`catalog.rs` has no tests.** The `#[test]` count is 0, and `crates/tools/tests/it/web.rs` does not exercise these sites.
- **`HttpClient` is GET-only.** `crates/tools/src/read/web/types.rs:214` declares `get(HttpRequest)`. `HttpRequest` (`types.rs:26`) has `url`, `headers` and `max_bytes`, with no method or body. Implementors: `SystemHttpClient` (`crates/envd/src/tool_read_sources.rs:228`), `ReadSourceAdapter` (`:570`, which also implements `ReadSources` at `:709`), test doubles `CannedHttp` and `WebSources` (`crates/tools/tests/it/web.rs:60,93`), `CannedHttpClient` (`crates/envd/src/tool_search.rs:2277`), and one `FakeClient` per dedicated scraper (for example `scrapers/arxiv.rs:583`).
- **Two search backends are missing.** omp2 has a Hacker News *reader* (`scrapers/hackernews.rs`) and no HN or Keenable search provider. Search backends are catalog rows plus a codec: `fixtures/llm-oracle/catalog/providers.toml:611-613` (`[providers."brave"]`), `crates/ai/src/codec/search_*.rs` (module list `codec/mod.rs:72`, URL join `:1399`), and registration at `crates/ai/src/provider/builtin.rs:82` and `:1083`. Thin JSON codecs share `search_json.rs` through `JsonSearchStyle`.
  - Upstream Hacker News (`upstream:packages/coding-agent/src/web/search/providers/hackernews.ts:19`): `GET` on `hn.algolia.com/api/v1/search` with `tags=<kind>` (kinds documented at `:26`), one request per kind, no key.
  - Upstream Keenable (`upstream:packages/coding-agent/src/web/search/providers/keenable.ts:27-28`): `POST /v1/search` with `X-API-Key` when a key is set, otherwise `POST /v1/search/public`. The automatic chain admits it only with a key (`isAvailable`, `:186`); an explicit selection works keyless (`isExplicitlyAvailable`, `:191`).
  - Auth kinds in `providers.toml`: `bearer` 95, `none` 7, `optional-bearer` 7, `header` 4, `basic` 1, `query` 1, `oauth` 4, `google-adc` 1. None is an optional header.
- **ADR 0049 already classifies this work.** Readers are class C ("content shaping … no vendor API is called", naming `tools/src/read/web/scrapers/*`). Search transports are class B (providers as data; codecs only for different wire protocols). Neither is class A (acting on a vendor account), so the vendor-extension rule does not apply.

Out of scope: upstream's `sofya`, `deepseek` and `ollama` search providers, which omp2 also lacks. They are listed in the research note and need their own decision.

## Decision

**D1. Everything stays in core.** Readers under ADR 0049 class C, search transports under class B. A reader extension would add an IPC hop per URL for data omp2 already ships for 75 sites.

**D2. The POST path is additive.** `HttpRequest` gains an optional method and body, defaulting to GET with no body, through builder methods rather than new public fields. A new public field would break every in-tree struct literal, so it is not additive. Before the change, every `HttpRequest { … }` literal must be found and either kept valid or moved to `HttpRequest::new`. `HttpClient` gains a defaulted `post` that returns a typed "unsupported" `WebError`. `get`, existing implementors and existing tests are untouched. Only `SystemHttpClient` and `ReadSourceAdapter` implement `post`. Error handling follows ADR 0035.

**D3. Reader fixes are deterministic per-site extractors.** Each affected site gets an `extract(url)` that yields an endpoint, plus a renderer. It lives beside its row in `catalog.rs` or in a dedicated module. No model is involved. A site that cannot extract returns `Ok(None)` and falls through to the generic fetch, as today.

**D4. Fixes that change existing output ship behind a default-off convar.** Proposed name: `sv_read_site_extractors`, a bool declared in `crates/tools/src/settings.rs` in the same form as `SV_TOOLS_OUTPUT_MAX_COLUMNS` (`:220`), with default `0` and the `archive` flag. With it off, the current `catalog.rs` path runs unchanged. This keeps the additive-only rule until the owner decides the default (O1).

**D5. Two search backends, as catalog rows plus codecs.**
- `hackernews`: a new `JsonSearchStyle::HackerNews` in `search_json.rs`. GET, `auth = { type = "none" }`, `base_url = "https://hn.algolia.com"`, `facets = ["web_search"]`, modelled on `duckduckgo`. The codec does the per-kind merge itself, either with two requests or with one request using Algolia's `tags` OR syntax.
- `keenable` and `keenable-public`: two catalog rows sharing one codec, `search-keenable`, which POSTs JSON. `keenable` uses `auth = { type = "header", name = "X-API-Key", env = ["OMP_KEENABLE_API_KEY"] }`; `keenable-public` uses `auth = { type = "none" }` and the public path. No auth-kind change is needed. The `X-Keenable-Title` header value carries an omp name, not upstream's.
- Both are added to the default `WebSearchSettings.exclusions` (`crates/ai/src/search_settings.rs:31`). Automatic search is therefore unchanged until a user selects one of them or removes the exclusion.

**D6. `catalog.rs` is characterised before it is changed.** A table-driven suite pins current match and endpoint behaviour for every row. It lands first, so every later change is visible.

**Constraints checked.** Additive only: new method, new convar, new catalog rows; the one changed interface (`HttpClient`) gains a defaulted method, and `HttpRequest` gains builder methods, not public fields. Vendor code stays out of core (ADR 0049); no vendor SDK or account API is added. Deterministic first: no model is used. No `pi` identity: names use `omp-*`, `sv_*` and the omp name for the Keenable header. ADR 0024 (latched roster): no new roster family; new providers are catalog rows. ADR 0036 (Python-only extensions): unaffected, because no extension code is added. ADR 0006 (sandbox policy): unaffected; no sandbox or file-read surface changes.

## Alternatives rejected

| Alternative | Why rejected |
|---|---|
| Reader and search backends as extensions | They are class C and class B, not vendor-account integrations. Extensions would add IPC and a second registry for 75 sites that already work this way. |
| Change `HttpClient::get` to take a method | Breaks every implementor and test double. A defaulted `post` is additive. |
| Public `method` and `body` fields on `HttpRequest` | Breaks every in-tree struct literal, so not additive. Builder methods avoid it. |
| New `optional-header` auth kind for Keenable | A schema change for one provider. Two rows need none. Revisit if a second optional-header provider appears. |
| Reuse `search_aggregate.rs` for the Hacker News kind fan-out | It merges engines for Public Web, not tags of one engine. |
| Fix readers in place, on by default | Changes output for existing URLs without an owner ruling, which the additive-only rule forbids. |
| Port upstream's TypeScript scrapers | omp2 is Rust-only and adds no runtimes. The Rust catalog already covers the 75 sites. |
| Fix only the `last`-segment heuristic and keep the generic dump | Cheaper, but CISA, EDGAR and JetBrains still lose their structured output. Acceptable only as a fallback inside S3. |

## Consequences

- Six existing readers return correct content once the convar is on. Nothing changes while it is off.
- `HttpClient` gains one defaulted method. Implementors that never POST need no change.
- Two new search providers exist but stay out of automatic search until selected.
- There are no recorded responses for any of the seven sites. Fixtures are new work.
- The same flattening affects the other ~50 catalog rows. This ADR does not audit them (O2).

## Slices

Each slice is one PR. Gates: `just check-pkg <pkg>` and `just test-pkg <pkg>`, then `just check-minimal-caps` where a crate's features change.

| # | Slice | Ships behind | Acceptance and tests |
|---|---|---|---|
| S1 | Characterisation tests in `omp-tools` for every `catalog.rs` row: `site_matches` and `api_url` on canned URLs | Nothing (tests only) | `just test-pkg omp-tools` passes against unchanged code. Any later behaviour change in a row fails it. |
| S2 | Additive POST seam: builder methods for method and body on `HttpRequest`, defaulted `HttpClient::post`, implemented in `SystemHttpClient` and `ReadSourceAdapter` | Dormant: no caller | Existing `crates/tools/tests/it/web.rs` and `omp-envd` tests pass unchanged. New: the default `post` returns the typed unsupported error. New: `ReadSourceAdapter` forwards method and body. |
| S3 | Extractors for CISA KEV, pub.dev, Firefox add-ons and JetBrains | `sv_read_site_extractors`, default `0` | Per-site `FakeClient` render tests, following `scrapers/arxiv.rs:583`. CISA returns the named CVE and `Ok(None)` when absent. JetBrains calls `/api/plugins/<digits>` and `/updates?size=1`. pub.dev reads `/packages/<name>` on sub-pages and the README. Firefox takes the slug after `addon`. Convar off: S1 suite unchanged. |
| S4 | Extractors for SEC EDGAR (CIK from `?CIK=`, `/cik/<n>`, `/submissions/CIK<n>.json` and `/Archives/edgar/data/<n>/`) and Semantic Scholar (40-hex id, TLDR, `externalIds`, open-access PDF) | `sv_read_site_extractors` | Render test per EDGAR URL shape. A Semantic Scholar URL without a paper id returns `Ok(None)`. Convar off: unchanged. |
| S5 | VS Code Marketplace reader on the S2 POST path, `filterType: 7` | `sv_read_site_extractors`; needs S2 | Canned POST fixture. The request body is asserted. A failed request returns `Ok(None)`. |
| S6 | Hacker News search codec and catalog row, plus default exclusion | Excluded from automatic search by default | Encode and decode unit tests in the style of `crates/ai/src/codec/search_kagi.rs`. Story and comment merge is deterministic and bounded. `just catalog-snapshot` regenerates the snapshot; `just test-pkg omp-catalog` passes. |
| S7 | Keenable codec and its two catalog rows | Same as S6 | Encode test: a key sets `X-API-Key` and uses `/v1/search`; no key uses `/v1/search/public`. Automatic admission of `keenable` needs a stored key only if omp2's chain has a per-provider availability check; S7 must confirm this before relying on it. Otherwise the default exclusion alone holds. Snapshot regenerated. |

No `cap-*` feature is added. In this workspace `cap-*` gates optional heavy dependencies (`crates/tools/Cargo.toml:20-24`), and these slices add none. Default-off behaviour comes from the convar (S3 to S5) or from the default exclusion (S6 and S7).

## Upstream origin and license

Files a port draws behaviour from, all `upstream:` paths at `c308970263`: `packages/coding-agent/src/web/scrapers/{cisa-kev,sec-edgar,semantic-scholar,pub-dev,firefox-addons,jetbrains-marketplace,vscode-marketplace}.ts` and `packages/coding-agent/src/web/search/providers/{hackernews,keenable}.ts`.

License: MIT (`upstream:LICENSE`), "Copyright (c) 2025 Mario Zechner" and "Copyright (c) 2025-2026 Can Bölük". The omp2 code is a Rust re-implementation from observed behaviour. If any upstream code is copied rather than re-implemented, its MIT attribution goes into the root `THIRD-PARTY-NOTICES.txt`, which already exists.

## Open questions for the owner

- **O1.** Turn `sv_read_site_extractors` on by default once S3 to S5 land? Recommendation: yes, after one release with it off. Today's output for CISA, EDGAR and VS Code is wrong, so little existing behaviour is worth protecting. Code and ADRs cannot settle this, because the change alters outputs.
- **O2.** Audit the other ~50 catalog rows for the same flattening defect? Recommendation: a separate follow-up ADR. Sample five rows first.
- **O3.** SEC asks for a contact address in the `User-Agent` (`upstream:…/sec-edgar.ts:176`). omp2 sends `omp/<version>` (`crates/core/src/lib.rs:14`), and upstream sends the same (`packages/utils/src/dirs.ts:38`). Recommendation: do not change it now, and act only if SEC answers with a refusal. SEC's policy is outside this repository, so no code or ADR settles it.

## Acceptance tests

1. With `sv_read_site_extractors` at `0`, the S1 suite passes unchanged against the pre-change output.
2. With it at `1`, a CISA KEV URL naming one CVE returns that entry only.
3. Calling `HttpClient::post` on a client that does not override it returns the unsupported error and does not panic.
4. `hackernews`, `keenable` and `keenable-public` are absent from automatic search by default and present when selected.
5. `just check-minimal-caps` still passes, with no new `cap-*` feature and no new heavy dependency.

## Status in omp

**Status: Not started.** The reader table, the dedicated scrapers and the search codec machinery exist. None of S1 to S7 is implemented.

## References

ADR 0035 (error handling); ADR 0049 (classes B and C); ADR 0050 (extension-declared URL schemes, unaffected); `docs/research/web-readers.md`; `crates/tools/src/read/web/scrapers/catalog.rs`; `crates/tools/src/read/web/types.rs`; `crates/ai/src/codec/search_json.rs`; `crates/ai/src/search_settings.rs`.
