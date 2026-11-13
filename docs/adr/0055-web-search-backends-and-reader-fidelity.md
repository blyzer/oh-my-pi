# 0055. Web search backends and reader fidelity

Status: Proposed 2026-11-13
Date: 2026-11-13
Area: tools

## Context

Research: [`docs/research/web-readers.md`](../research/web-readers.md) (Gap 8). Upstream is pinned at `c308970263` (v18.20.0), at `/Users/enverfrancisco/repositories/omp-work/upstream`. Paths prefixed `upstream:` are in that tree. Others are in omp2 at `ed95a14a41`.

- **Placement follows ADR 0049's class table, not the default.** ADR 0049 says "New capabilities default to T2" (`docs/adr/0049-tiered-integration-architecture.md:54`). The class table (`0049` lines 127-133) overrides that default for two classes. Class B (inference and search transports, "providers as data") and class C (content shaping and safety data, "no vendor API is called", naming `tools/src/read/web/scrapers/*`) stay in core. Neither is class A. The default does not apply: readers and search backends are catalog data and page extraction, not vendor-account integrations, so no extension boundary is needed.
- **All 75 upstream scrapers already have an omp2 home.** Sixteen dedicated modules (`crates/tools/src/read/web/scrapers/mod.rs`, `Scraper::ALL: [Self; 17]`) plus a 59-row table (`crates/tools/src/read/web/scrapers/catalog.rs`, `ALL: [Site; 59]` at `:79`). The table covers 59 of the 75 sites. This ADR's characterisation tests (S1) cover the 59 table rows only. A row matches by host (`site_matches`, `:196`), builds one GET URL from the **last path segment** (`api_url`, `:275`), and flattens the reply with `render_json` (`:343`; depth ≤ 5, 80 keys per object, 40 array items, 450 000 bytes).
- **The seven named readers mostly misbehave.**
  - CISA KEV (`catalog.rs:254`, `:328`) fetches the whole feed and prints its first 40 entries, not the requested CVE. Upstream extracts the CVE id (`upstream:packages/coding-agent/src/web/scrapers/cisa-kev.ts:24`, `:43-46`) and selects the matching entry (`:60`).
  - SEC EDGAR (`catalog.rs:261`, `:333`) has no API URL, so it always falls through. Upstream reads `data.sec.gov/submissions/CIK<10 digits>.json` (`sec-edgar.ts:177`) after extracting the CIK from several URL shapes.
  - VS Code Marketplace (`catalog.rs:297`) has no API URL. Upstream POSTs to `marketplace.visualstudio.com/_apis/public/gallery/extensionquery` (`vscode-marketplace.ts:116`, `method: "POST"` at `:129`).
  - JetBrains (`catalog.rs:310`) sends `1234-name` as the plugin id. Upstream takes the leading digits and also calls `/updates?size=1` (`jetbrains-marketplace.ts:92-93`).
  - pub.dev (`catalog.rs:308`) takes the last segment, which is wrong on `/versions` and `/changelog`. Upstream matches `^/packages/([^/]+)` and reads the README (`pub-dev.ts:10`, `:119`).
  - Semantic Scholar (`catalog.rs:325`) works for `/paper/<slug>/<sha>` but loses TLDR, `externalIds` and the open-access PDF (`semantic-scholar.ts:76`).
  - Firefox add-ons (`catalog.rs:296`) works for `/addon/<slug>/` and breaks on `/addon/<slug>/reviews/`.
- **`catalog.rs` has no tests.** The `#[test]` count is 0, and `crates/tools/tests/it/web.rs` does not exercise these sites. `site_matches`, `api_url` and `find` are private (`fn`, not `pub`), so S1 tests live in a `#[cfg(test)]` module inside `catalog.rs`.
- **`HttpClient` is GET-only, and its request type is public.** `crates/tools/src/read/web/types.rs:214` declares `get(HttpRequest)`. `HttpRequest` (`types.rs:26-33`) has public fields `url`, `headers` and `max_bytes`, and an in-tree struct literal depends on them: `crates/tools/src/read/web/scrapers/reddit.rs:38` builds `HttpRequest { url, headers, max_bytes }` directly. Adding a public field breaks that literal, so the POST seam cannot extend `HttpRequest`.
  - Implementors: `SystemHttpClient` (`crates/envd/src/tool_read_sources.rs:228`), whose GET path goes through `request_with_retries` (`:161`, `.get(...)` at `:175`); `ReadSourceAdapter` (`:570`, which forwards `get` at `:575` and also implements `ReadSources` at `:709`); test doubles `CannedHttp` and `WebSources` (`crates/tools/tests/it/web.rs:60,93`); `CannedHttpClient` (`crates/envd/src/tool_search.rs:2277`); and one `FakeClient` per dedicated scraper (for example `scrapers/arxiv.rs:583`).
  - `WebError` (`types.rs:120`) has no variant for an unsupported method. Its nearest is `Request(Str)` (`:126`), a string payload. ADR 0035 rules out string-payload variants (`docs/adr/0035-*.md:55-56`, enforced by `tools/lintx` `error-str-payload`).
- **Two search backends are missing.** omp2 has a Hacker News *reader* (`scrapers/hackernews.rs`) and no HN or Keenable search provider. Search backends are catalog rows plus a codec: `fixtures/llm-oracle/catalog/providers.toml:611-613` (`[providers."brave"]`), `crates/ai/src/codec/search_*.rs` (module list `codec/mod.rs:72`, URL join `:1399`), and registration at `crates/ai/src/provider/builtin.rs:82` and `:1083`. The search codec layer already has a POST method (`RequestMethod::Post`, `crates/ai/src/codec/mod.rs:94-98`) and inline bodies (`BodySource::Bytes`, `crates/ai/src/body.rs:512-514`). Thin JSON codecs share `search_json.rs` through `JsonSearchStyle`.
  - Upstream Hacker News (`upstream:packages/coding-agent/src/web/search/providers/hackernews.ts:19`): `GET` on `hn.algolia.com/api/v1/search` with `tags=<kind>` (kinds documented at `:26`), one request per kind, no key.
  - Upstream Keenable (`upstream:packages/coding-agent/src/web/search/providers/keenable.ts:27-28`): `POST /v1/search` with `X-API-Key` when a key is set, otherwise `POST /v1/search/public`. The automatic chain admits it only with a key (`isAvailable`, `:186`); an explicit selection works keyless (`isExplicitlyAvailable`, `:191`).
  - Auth kinds in `providers.toml`: `bearer` 95, `none` 7, `optional-bearer` 7, `header` 4, `basic` 1, `query` 1, `oauth` 4, `google-adc` 1. None is an optional header.
- **The model-facing `web_search` schema is versioned.** `Provider` (`crates/tools/src/web_search.rs:100`) is part of `web_search@2`, the args struct documented at `:18`. ADR 0026 records revision cutovers for `web_search@2` (`docs/adr/0026-intent-and-versioned-tools.md:52`) and says durable calls record `ToolIdentity { name, rev }` (`:57`). A change to the model-facing schema is therefore a new revision, not an additive edit.
- **The default search exclusions are two places, not one.** `WebSearchSettings::default` sets `exclusions: Vec::new()` (`crates/ai/src/search_settings.rs:52-56`). The effective value comes from `AI_SEARCH_EXCLUSIONS` (`:225-226`, `default: Vec::new()`), read at `:75`. The `exclusions` field is at `:31` and `order` at `:29`.

## Decision

**D1. Everything stays in core.** Readers under ADR 0049 class C, search transports under class B. A reader extension would add an IPC hop per URL for data omp2 already ships for 75 sites.

**D2. The POST seam is a separate request type, not an extension of `HttpRequest`.**
- A new public type `HttpPostRequest` (in `crates/tools/src/read/web/types.rs`) holds `url`, `headers`, `body: Bytes`, `content_type` and `max_bytes`, with `HttpPostRequest::new(url, body)` and builder methods. `HttpRequest` and its literal at `reddit.rs:38` are unchanged.
- `HttpClient` gains `fn post(&self, request: HttpPostRequest) -> impl Future<Output = Result<HttpResponse, WebError>> + Send + '_`, with a default body that returns `Err(WebError::Unsupported { method: HttpMethod::Post })`.
- A new `HttpMethod` enum (`Get`, `Post`) is typed, with no string payload (ADR 0035). `WebError::Unsupported { method: HttpMethod }` is added. Every exhaustive `match` on `WebError` gets the new arm. S2 finds them with the compiler, not by grep.
- `ReadSourceAdapter::post` forwards explicitly, through the same host allowlist and cache policy as `get` (`:575`). It does not rely on the default, so the adapter cannot bypass its checks.
- `SystemHttpClient::post` is a separate path. `request_with_retries` (`tool_read_sources.rs:161`) stays GET-only. The POST path issues a single request with no retry, sends the fixed `USER_AGENT` (`crates/core/src/lib.rs:14`), and does not follow redirects. A 3xx response is returned as `WebError::HttpStatus`. A redirect would change a POST into an unintended request, so it is not followed.
- **UA rotation.** The GET path rotates through `USER_AGENTS` (`types.rs:17`). POST does not, because the rotation is a scraping measure for GET pages, while POST endpoints identify their client.
- **Caching.** POST responses are never cached by URL. The reader cache (`read/web`, `HttpClient` cache lookup) keys on the URL alone, which is wrong for POST. S5 does not cache. If caching is added later, the key must include method and a hash of the body, and only for readers whose contract declares the POST idempotent.

**D3. Reader fixes are deterministic per-site extractors.** Each affected site gets an `extract(url)` that yields an endpoint, plus a renderer. It lives beside its row in `catalog.rs` or in a dedicated module. No model is involved. A site that cannot extract returns `Ok(None)` and falls through to the generic fetch, as today.

**D4. Fixes that change existing output ship behind a default-off convar.** Proposed name: `sv_read_site_extractors`, a bool declared in `crates/tools/src/settings.rs` in the same form as `SV_TOOLS_OUTPUT_MAX_COLUMNS` (`:220`), with default `0` and the `archive` flag. With it off, the current `catalog.rs` path runs unchanged.

**D5. Two search backends, as catalog rows plus codecs, through the existing search codec path.**
- `hackernews`: a new `JsonSearchStyle::HackerNews` in `search_json.rs`. GET, `auth = { type = "none" }`, `base_url = "https://hn.algolia.com"`, `facets = ["web_search"]`, modelled on `duckduckgo`. The codec does the per-kind merge itself, with two requests or one request using Algolia's `tags` OR syntax.
- `keenable` and `keenable-public`: two catalog rows sharing one codec, `search-keenable`, which uses the existing `RequestMethod::Post` and `BodySource::Bytes` (`codec/mod.rs:94-98`, `body.rs:512-514`). **This path does not use the reader POST seam (D2), so S6 and S7 do not depend on S2.**
- Keyed `keenable` uses `auth = { type = "api-key", sources = [{ type = "environment", ordered_names = ["OMP_KEENABLE_API_KEY"] }], placement = { type = "header", name = "X-API-Key" } }`. The `AuthSpec::ApiKey` variant (`crates/ai/src/auth/spec.rs:12-13`, `ApiKey` with `sources` and a `KeyPlacement`, `KeyPlacement::Header` at `:473`) is the valid form; the earlier `type = "header"` was not an `AuthSpec` tag. The exact serde casing of `CredentialSourceSpec` (`spec.rs:439-440`) is confirmed in S7.
- Keyless `keenable-public` uses `auth = { type = "none" }` (`AuthSpec::None`).
- **Default exclusion.** Both new providers are added to the effective default, `AI_SEARCH_EXCLUSIONS` (`search_settings.rs:225-226`), and to `WebSearchSettings::default` (`:52-56`). Automatic search is therefore unchanged until a user removes the exclusion. Both locations change together, and a test asserts it.

**D6. No `web_search` schema change.** New providers are not added to the `Provider` enum (`web_search.rs:100`). Adding variants changes the model-facing `web_search@2` schema, which is a revision cutover under ADR 0026 (`0026:52,57`), not an additive edit. Instead, a user selects a new provider through the existing settings `order` (`search_settings.rs:29`), which is a `Vec<Str>` of provider ids. The `web_search` tool's explicit `provider` argument cannot name `hackernews` or `keenable` under `@2`. That limit is accepted for now. Whether `order` accepts ids that are not in the enum is unverified and is checked in S6. If it does not, the choice is a `web_search@3` cutover, which is a separate owner decision (O4).

**D7. Schema test.** A snapshot test asserts the serialised `web_search@2` schema is byte-identical before and after this ADR's slices. It is the mechanical check for D6.

## Alternatives rejected

| Alternative | Why rejected |
|---|---|
| Reader and search backends as extensions | They are class C and class B under ADR 0049, not vendor-account integrations. Extensions would add IPC and a second registry for 75 sites that already work this way. |
| Add `method` and `body` fields to `HttpRequest` | Breaks the in-tree struct literal at `reddit.rs:38`, so it is not additive. A separate `HttpPostRequest` avoids it (D2). |
| Change `HttpClient::get` to take a method | Breaks every implementor and test double. A defaulted `post` is additive. |
| Add `Hackernews` and `Keenable` to the `Provider` enum | Changes the model-facing `web_search@2` schema, which ADR 0026 treats as a revision cutover (D6). |
| Let `ReadSourceAdapter::post` use the trait default | The adapter's allowlist and cache checks would be bypassed. It forwards explicitly (D2). |
| Retry and follow redirects for POST, as for GET | A retried POST can repeat a side effect, and a followed redirect changes the method. POST is single-shot (D2). |
| New `optional-header` auth kind for Keenable | A schema change for one provider. The two-row design needs none (D5). |
| Reuse `search_aggregate.rs` for the Hacker News kind fan-out | It merges engines for Public Web, not tags of one engine. |
| Fix readers in place, on by default | Changes output for existing URLs without an owner ruling, which the additive-only rule forbids (D4). |
| Port upstream's TypeScript scrapers | omp2 is Rust-only and adds no runtimes. The Rust catalog already covers the 75 sites. |
| Fix only the `last`-segment heuristic and keep the generic dump | Cheaper, but CISA, EDGAR and JetBrains still lose their structured output. Acceptable only as a fallback inside S3. |

## Consequences

- Six existing readers return correct content once the convar is on. Nothing changes while it is off.
- `HttpClient` gains one defaulted method, and `WebError` gains one typed variant. Implementors that never POST need no change, but every exhaustive `WebError` match gains an arm.
- Two new search providers exist, reachable through settings and excluded from automatic search by default. The `web_search@2` schema is unchanged (D7).
- There are no recorded responses for any of the seven sites. Fixtures are new work.
- The same flattening affects the other ~50 catalog rows. This ADR does not audit them (O2).

## Review (EGM, 2026-11-13)

A fresh-context critic and a readiness reviewer checked the draft against the code. Each finding and its resolution:

1. **POST seam was not additive.** Builder methods on `HttpRequest` still needed stored fields, which breaks `reddit.rs:38`. Resolved: a separate `HttpPostRequest` type, with its semantics stated (D2).
2. **`WebError` had no unsupported variant.** Resolved: a typed `WebError::Unsupported { method: HttpMethod }`, with no string payload per ADR 0035. The default `post` returns it (D2).
3. **Keenable depended on the reader POST slice unnecessarily.** Resolved: Keenable uses the search codec's existing POST (`RequestMethod::Post`, `BodySource::Bytes`). S6 and S7 no longer depend on S2 (D5, Slices).
4. **The `web_search` `Provider` enum is a model-facing schema.** Adding variants breaks additivity, and ADR 0026 makes a schema change a revision cutover. Resolved: no enum change; selection through settings `order`, with a schema snapshot test (D6, D7). Whether `order` accepts unlisted ids is verified in S6.
5. **Default exclusion location was wrong.** The effective default is `AI_SEARCH_EXCLUSIONS` (`search_settings.rs:225-226`), with `WebSearchSettings::default` at `:52-56`. Both change (D5).
6. **The auth example was not a valid `AuthSpec` tag.** Resolved: `api-key` with an environment source and a header placement (D5).
7. **The catalog count was wrong.** `catalog.rs` has 59 rows (`ALL: [Site; 59]`, `:79`), and the 16 dedicated modules are outside S1. Site functions are private, so S1 tests are in-module (Context).

Placement: the class-table override of the T2 default is stated in Context (ADR 0049 `:54` versus `:127-133`).

**Readiness.** S1 is buildable now from this ADR and the code. S2 needs the decisions in D2, which are now stated. S6 and S7 can proceed independently of S2. S5 remains gated on S2.

## Slices

Each slice is one PR. Gates: `just check-pkg <pkg>` and `just test-pkg <pkg>`, then `just check-minimal-caps` where a crate's features change.

| # | Slice | Ships behind | Acceptance and tests | Depends on |
|---|---|---|---|---|
| S1 | Characterisation tests for the 59 `catalog.rs` rows, in a `#[cfg(test)]` module inside `catalog.rs`: `find`, `site_matches` and `api_url` on canned URLs | Nothing (tests only) | `just test-pkg omp-tools` passes against unchanged code. Any later behaviour change in a row fails it. | none |
| S2 | POST seam: `HttpMethod`, `HttpPostRequest`, `WebError::Unsupported`, defaulted `HttpClient::post`, explicit `ReadSourceAdapter::post` forward, `SystemHttpClient` single-shot POST path (no retry, fixed UA, no redirects) | Dormant: no caller | Existing `crates/tools/tests/it/web.rs` and `omp-envd` tests pass unchanged, and `reddit.rs` still compiles. New: the default `post` returns `Unsupported`. New: `ReadSourceAdapter` forwards method, body and headers. New: a POST 3xx returns `HttpStatus`, not a followed redirect. New: a POST response is never cached by URL. | none |
| S3 | Extractors for CISA KEV, pub.dev, Firefox add-ons and JetBrains | `sv_read_site_extractors`, default `0` | Per-site `FakeClient` render tests, following `scrapers/arxiv.rs:583`. CISA returns the named CVE and `Ok(None)` when absent. JetBrains calls `/api/plugins/<digits>` and `/updates?size=1`. pub.dev reads `/packages/<name>` on sub-pages and the README. Firefox takes the slug after `addon`. Convar off: S1 suite unchanged. | S1 |
| S4 | Extractors for SEC EDGAR (CIK from `?CIK=`, `/cik/<n>`, `/submissions/CIK<n>.json`, `/Archives/edgar/data/<n>/`) and Semantic Scholar (40-hex id, TLDR, `externalIds`, open-access PDF) | `sv_read_site_extractors` | Render test per EDGAR URL shape. A Semantic Scholar URL without a paper id returns `Ok(None)`. Convar off: unchanged. | S1 |
| S5 | VS Code Marketplace reader on the S2 POST path, `filterType: 7` | `sv_read_site_extractors` | Canned POST fixture. The request body is asserted. A failed request returns `Ok(None)`. | S1, S2 |
| S6 | Hacker News search codec (`search_json.rs`), catalog row, and the two-place default exclusion (D5). Verify that settings `order` accepts an unlisted id (D6) | Excluded from automatic search by default | Encode and decode unit tests in the style of `crates/ai/src/codec/search_kagi.rs`. Story and comment merge is deterministic and bounded. A test asserts both exclusion locations. `just catalog-snapshot` regenerates the snapshot; `just test-pkg omp-catalog` passes. | none |
| S7 | Keenable codec using the existing search POST path, and its two catalog rows. Confirm `CredentialSourceSpec` serde casing (`spec.rs:439-440`) | Same as S6 | Encode test: a key sets `X-API-Key` and uses `/v1/search`; no key uses `/v1/search/public`. The `web_search@2` schema snapshot (D7) is unchanged. Automatic admission of `keenable` needs a stored key only if omp2's chain has a per-provider availability check; S7 confirms this. Snapshot regenerated. | none |

No `cap-*` feature is added. In this workspace `cap-*` gates optional heavy dependencies (`crates/tools/Cargo.toml:20-24`), and these slices add none. Default-off behaviour comes from the convar (S3 to S5) or from the default exclusion (S6 and S7).

## Upstream origin and license

Files a port draws behaviour from, all `upstream:` paths at `c308970263`, re-verified on 2026-11-13: `packages/coding-agent/src/web/scrapers/{cisa-kev,sec-edgar,semantic-scholar,pub-dev,firefox-addons,jetbrains-marketplace,vscode-marketplace}.ts` and `packages/coding-agent/src/web/search/providers/{hackernews,keenable}.ts`.

License: MIT (`upstream:LICENSE`), "Copyright (c) 2025 Mario Zechner" and "Copyright (c) 2025-2026 Can Bölük". The omp2 code is a Rust re-implementation from observed behaviour. If any upstream code is copied rather than re-implemented, its MIT attribution goes into the root `THIRD-PARTY-NOTICES.txt`, which already exists. Upstream's `X-Keenable-Title: oh-my-pi` header value is not copied; omp2 sends an omp name.

## Open questions for the owner

- **O1.** Turn `sv_read_site_extractors` on by default once S3 to S5 land? Recommendation: yes, after one release with it off. Today's output for CISA, EDGAR and VS Code is wrong, so little existing behaviour is worth protecting. Code and ADRs cannot settle this, because the change alters outputs.
- **O2.** Audit the other ~50 catalog rows for the same flattening defect? Recommendation: a separate follow-up ADR. Sample five rows first.
- **O3.** SEC asks for a contact address in the `User-Agent` (`upstream:…/sec-edgar.ts:176`). omp2 sends `omp/<version>` (`crates/core/src/lib.rs:14`), and upstream sends the same (`packages/utils/src/dirs.ts:38`). Recommendation: do not change it now, and act only if SEC answers with a refusal. SEC's policy is outside this repository, so no code or ADR settles it.
- **O4.** Only if S6 finds that settings `order` rejects unlisted ids (D6): add `hackernews` and `keenable` to the `Provider` enum under a `web_search@3` cutover, with the ADR 0026 lift, or leave them settings-only. Recommendation: settings-only, so no cutover is needed.

## Acceptance tests

1. With `sv_read_site_extractors` at `0`, the S1 suite passes unchanged against the pre-change output.
2. With it at `1`, a CISA KEV URL naming one CVE returns that entry only.
3. Calling `HttpClient::post` on a client that does not override it returns `WebError::Unsupported`, and does not panic.
4. `hackernews`, `keenable` and `keenable-public` are absent from automatic search by default, through both exclusion locations, and present when selected.
5. The serialised `web_search@2` schema is byte-identical before and after the slices (D7).
6. `just check-minimal-caps` still passes, with no new `cap-*` feature and no new heavy dependency.

## Status in omp

**Status: Not started.** The reader table, the dedicated scrapers and the search codec machinery exist. None of S1 to S7 is implemented.

## References

ADR 0026 (versioned tools, revision cutovers); ADR 0035 (typed errors, no string payloads); ADR 0049 (default T2, classes B and C, class table); ADR 0050 (extension-declared URL schemes, unaffected); `docs/research/web-readers.md`; `crates/tools/src/read/web/scrapers/catalog.rs`; `crates/tools/src/read/web/types.rs`; `crates/tools/src/web_search.rs`; `crates/ai/src/codec/search_json.rs`; `crates/ai/src/search_settings.rs`; `crates/ai/src/auth/spec.rs`.
