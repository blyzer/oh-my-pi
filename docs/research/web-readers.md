# Web readers and search backends: upstream parity on `omp2` (Gap 8, `web-readers`)

Status: research complete. No code changed.
Date: 2026-11-13
Upstream pin: `c308970263` (v18.20.0), detached at `/Users/enverfrancisco/repositories/omp-work/upstream`. Line citations are re-verified at this pin. The first scan ran against `upstream/main` `84f5adc3bb`; every provider id, scraper count and cited line matched at both.
omp2 snapshot: worktree `omp-work/gaps` at `ed95a14a41` (branch `docs/upstream-gap-research`). Citations resolve at this commit.

## Bottom line

1. **All seven named readers already exist in `omp2` as matchers, so none is missing. Six of them are low fidelity or broken.** Upstream has 75 scraper modules (`packages/coding-agent/src/web/scrapers/*.ts`, excluding `index`, `types` and `utils`; 78 files in total at the pin). `omp2` covers them with 16 dedicated Rust modules plus one 59-row table, `crates/tools/src/read/web/scrapers/catalog.rs` (`Site` enum, `ALL: [Site; 59]`). The normalised-name diff is empty both ways. Only Firefox add-ons reliably works.
2. **Keenable and Hacker News as *search backends* are genuinely missing.** `omp2` has a Hacker News *reader* (`scrapers/hackernews.rs`) but no HN or Keenable search codec or catalog row. A repo-wide grep for `keenable` finds nothing outside this note.
3. **Placement: core, not extensions.** The readers are ADR 0049 class C ("content shaping … stays as data or tables. No vendor API is called"; the ADR names `tools/src/read/web/scrapers/*`). The search backends are class B ("inference and search transports … providers as data; codec code exists only for genuinely different wire protocols"; `ai/src/codec/*`, including `search_*`). Neither is class A (acts on a vendor account or service API), so the vendor-extension rule does not apply.

## 1. Readers

### Where the code lives

| Side | Location | Mechanism |
|---|---|---|
| Upstream | `packages/coding-agent/src/web/scrapers/<site>.ts`, registered in `scrapers/index.ts` | One `SpecialHandler` per site: parse URL, call the site's API, build bespoke markdown |
| `omp2` | `crates/tools/src/read/web/scrapers/mod.rs` (`Scraper::ALL: [Self; 17]`, concrete match dispatch, `mod.rs:72` onward) | 16 dedicated modules (`arxiv, crates_io, docs_rs, github, github_gist, gitlab, hackernews, huggingface, mdn, npm, pypi, reddit, stackoverflow, twitter, wikipedia, youtube`) plus `Scraper::LongTail` → `catalog.rs` |

`catalog.rs` is a *generic* path. `find(url)` picks the first `Site` whose `site_matches` host test passes (`catalog.rs:254-333` for the seven). `api_url(site, url)` builds one GET endpoint from the **last path segment**. `render` does a GET with `User-Agent: omp/<ver>` and dumps the response through `render_json` (depth ≤ 5, 80 keys per object, 40 array items, 450 000-byte cap). A site with no API URL returns `Ok(None)` and falls through to the generic bounded fetch. The module has no tests (`#[test]` count in `catalog.rs` is 0; `crates/tools/tests/it/web.rs` mentions none of the seven).

`HttpClient` (`crates/tools/src/read/web/types.rs:214`) exposes only `get(HttpRequest)`. `HttpRequest` (`types.rs:26`) has `url`, `headers` and `max_bytes` only, so POST is not expressible today.

### Per-reader comparison

| Reader | Upstream (file; endpoint) | `omp2` (`catalog.rs`) | Verdict |
|---|---|---|---|
| CISA KEV | `cisa-kev.ts`. Matches `cisa.gov` with `known-exploited-vulnerabilities` in the path **and a CVE id** in path or query (`CVE_PATTERN` at `:24`, match at `:43-44`); fetches the KEV feed, finds the entry whose `cveID` equals that id (`:46`, `:60`); returns `null` if no CVE or no entry | `site_matches` (`:254`) needs only host and path, no CVE. `api_url` (`:328`) fetches the whole feed. `render_json` then prints the **first 40** array items, so a lookup for a named CVE returns unrelated entries, or only the head of a ~1 MB feed | **Wrong output.** Needs CVE extraction and an entry filter |
| SEC EDGAR | `sec-edgar.ts`. `extractCik` handles `?CIK=`, `/cik/<n>`, `/submissions/CIK<n>.json`, `/Archives/edgar/data/<cik>/`. Fetches `https://data.sec.gov/submissions/CIK<10-digit>.json` (`:177`) with `User-Agent` (`:182`); renders company and filings markdown | Matches any `sec.gov` host (`:261`); `api_url` returns `None` (`:333`) | **No-op.** Always falls through to the generic fetch |
| Semantic Scholar | `semantic-scholar.ts`. Requires a 40-hex paper id, calls `api.semanticscholar.org/graph/v1/paper/<id>?fields=…` (`:76`), builds title, authors, venue, TLDR, open-access and `externalIds` link sections | Host `semanticscholar.org` (`:251`); `api_url` (`:325`) uses the last path segment with `fields=title,abstract,authors,year,citationCount,url` | **Works for `/paper/<slug>/<sha>`.** The generic dump loses TLDR, `externalIds` and the open-access PDF. A URL without a paper id gets a 404 and falls through, where upstream returns an explanatory result |
| pub.dev | `pub-dev.ts`. Matches `pub.dev` (`:10`) and `^/packages/([^/]+)`; calls `pub.dev/api/packages/<name>`, then the version README at `/packages/<name>/versions/<v>/readme` (`:119`) | `api_url` (`:308`) uses the **last** segment | **Wrong for sub-pages** (`/packages/foo/versions` and `/changelog` resolve to `versions` or `changelog`). No README |
| Firefox add-ons | `firefox-addons.ts`. Slug is the segment after `addon`; `addons.mozilla.org/api/v5/addons/addon/<slug>/` | `api_url` (`:296`) uses the same endpoint with the last segment | **Works** for `/addon/<slug>/`. Wrong on `/addon/<slug>/reviews/`. Generic dump, no structured sections |
| JetBrains Marketplace | `jetbrains-marketplace.ts`. `^/plugin/(\d+)(?:-[^/]+)?`, then `/api/plugins/<id>` (`:92`) **and** `/api/plugins/<id>/updates?size=1` (`:93`) | `api_url` (`:310`) uses `/api/plugins/{last}`, where `last` is `1234-name` | **Broken.** The id is `1234-name`, so the call fails and falls through. No update or compatibility data |
| VS Code Marketplace | `vscode-marketplace.ts`. **POST** to `marketplace.visualstudio.com/_apis/public/gallery/extensionquery` (`:116`, `method: "POST"` at `:129`), with `criteria: [{ filterType: 7, value: itemName }]` | `api_url` returns `None` (`:297`) | **No-op**, and not fixable in the table alone: `HttpClient` is GET-only |

Both sides send the same user agent: `omp/<VERSION>` at `packages/utils/src/dirs.ts:38` upstream, and `crates/core/src/lib.rs:14` in `omp2`. Upstream's comment at `sec-edgar.ts:176` says SEC requires contact information in the UA, and neither side sends any. That is a shared, unverified risk of SEC rate-limiting or refusal, not a regression.

### Recommendation (readers)

Core. Keep the ADR 0049 class C classification. Do **not** open a reader extension for any of these. In priority order:

1. **Add a bounded POST to the reader HTTP seam** (method and body on `HttpRequest`, and a `post` on `HttpClient`) so VS Code Marketplace can be a real reader. Make it additive: a defaulted `post`, and builder methods rather than new public fields, because a field added to a `pub` struct breaks every in-tree struct literal. Implementors are listed under Verification below.
2. **Promote the seven from catalog rows to dedicated modules, or give `catalog.rs` a per-site `extract(url) -> Option<Endpoint>` plus a renderer.** CISA needs a feed-entry filter, SEC needs CIK extraction and a different host, JetBrains needs numeric-id extraction and two calls, and pub.dev needs a README call. The `last`-segment heuristic cannot express any of these.
3. **Fix the `last`-segment heuristics even for sites that stay in the table**: pub.dev (`/packages/<name>`), Firefox (the segment after `addon`) and JetBrains (leading digits). These are correctness bugs independent of rendering fidelity.
4. **Tests.** None exist for `catalog.rs`. Add table-driven match and endpoint tests per site, then recorded-response render tests for the seven, following the `FakeClient` pattern the dedicated scrapers already use (`scrapers/arxiv.rs:583`, `gitlab.rs:467`, `hackernews.rs:453`, `huggingface.rs:540`, `reddit.rs:292`, `stackoverflow.rs:281`) and `crates/tools/tests/it/web.rs`.
5. Out of scope here, but the same `render_json` flattening affects the other ~50 catalog rows. Audit them separately before claiming parity for them.

## 2. Search backends

### Upstream

Registry: `packages/coding-agent/src/web/search/provider.ts` (`PROVIDER_LOADERS`; `keenable` at `:23`, `hackernews` at `:42`).

- **Keenable** (`providers/keenable.ts`, 198 lines). With a key: `POST https://api.keenable.ai/v1/search` (`:27`) with `X-API-Key`. Without one: `POST https://api.keenable.ai/v1/search/public` (`:28`, per-IP rate limited) with `X-Keenable-Title: oh-my-pi`. `isAvailable` (`:186`, used by the auto chain) requires a configured `keenable` key. `isExplicitlyAvailable` (`:191`) is always true, so an explicit `web/keenable` selection works keyless.
- **Hacker News** (`providers/hackernews.ts`, 184 lines). `GET https://hn.algolia.com/api/v1/search` (`:19`), with `query`, `advancedSyntax=true`, `hitsPerPage` and `tags=<kind>`, one request per kind (`:26` documents the kinds). Hits map to `https://news.ycombinator.com/item?id=<id>`. No key; `isAvailable()` returns true. `search/index.ts:194-205` (`isHackerNewsEngine`, `preferHackerNews`) moves it to the front of the role chain when selected.

### `omp2`

Search backends are catalog providers plus a codec. Catalog entry: `fixtures/llm-oracle/catalog/providers.toml`, e.g. `[providers."brave"]` at `:611-613` (`codec = "search-brave"`, `facets = ["web_search"]`). Codec: `crates/ai/src/codec/search_*.rs`, module list `codec/mod.rs:72`, URL join `codec/mod.rs:1399`, registration `crates/ai/src/provider/builtin.rs:82` and `:1083`. Thin codecs share `codec/search_json.rs` through `JsonSearchStyle { Firecrawl, Brave, Jina, Tinyfish, Searxng }`. Auth kinds in `providers.toml`: `bearer` 95, `none` 7, `optional-bearer` 7, `header` 4, `basic` 1, `query` 1, `oauth` 4, `google-adc` 1. None is an optional header.

`omp2` standalone web-search providers: zai-search, kimi-search, perplexity (plus cookie, openrouter and anonymous variants), duckduckgo, google-search, startpage, ecosia, mojeek, searxng, firecrawl, brave, jina, tinyfish, synthetic-search, tavily, kagi, exa and parallel.

Upstream standalone ids (`PROVIDER_LOADERS`) that `omp2` lacks: **keenable and hackernews**, plus **sofya, deepseek and ollama** (listed so the next pass need not rediscover them). `public` is upstream's multi-engine Public Web provider and has an `omp2` counterpart, `aggregate_public_web` in `crates/ai/src/operation/search_aggregate.rs:38` and the "Public Web" option in `crates/ai/src/search_settings.rs`. The grounded set (`gemini, anthropic, codex, xai, openrouter, openai`) corresponds to `codec/search_hosted.rs` and was not compared.

### Recommendation (search)

Core, class B. Neither wire shape is covered by an existing style.

- **Hacker News:** add `JsonSearchStyle::HackerNews` (or a `search-hackernews` codec). GET, no auth. Catalog entry: `auth = { type = "none" }`, `base_url = "https://hn.algolia.com"`, `facets = ["web_search"]`, modelled on `duckduckgo`. Upstream issues one request per kind (story, comment) and merges. `search_aggregate.rs` merges results across *engines* for Public Web, not across tags of one engine, so the codec should either send one request with Algolia's `tags` OR syntax or do the merge itself. Selection preference (`preferHackerNews`) is chain policy, to be handled where `omp2` builds chains (`crates/ai/src/operation/search_query.rs`, `crates/ai/src/search_settings.rs`).
- **Keenable:** POST JSON, key in `X-API-Key`, **optional**, with a second path for keyless use. No existing auth kind is an optional header (`optional-bearer` is bearer only; `header` is required). Options: (a) add an `optional-header` auth kind; (b) two catalog rows (`keenable` with `header`, `keenable-public` with `none`) sharing one codec. Option (b) needs no schema change. Auto-chain admission only when a key exists, as upstream does. The `X-Keenable-Title` value must not carry upstream's name; use an omp name.
- **Tests:** no search-codec fixtures exist under `fixtures/llm-oracle` today (the only search-named file there is an unrelated Devin proto). Request and response tests for the new codecs would follow in-crate codec tests, for example the test module in `crates/ai/src/codec/search_kagi.rs`. Not designed here.

## 3. Verification performed

- **Upstream:** file inventory at the pin (`git ls-tree`), the seven reader files and two search providers read, and every cited line re-checked at `c308970263`.
- **Scraper count:** 78 files at the pin, 75 once `index`, `types` and `utils` are excluded, equal to `omp2`'s 16 modules + 59-row table. The normalised-name diff against `upstream/main` was empty both ways; `W3c` is the only row the regex initially missed.
- **Implementors of the read HTTP seam** (for the POST change): production `SystemHttpClient` (`crates/envd/src/tool_read_sources.rs:228`) and `ReadSourceAdapter` (`:570`, which also implements `ReadSources` at `:709`); test doubles `CannedHttp` and `WebSources` (`crates/tools/tests/it/web.rs:60,93`); `CannedHttpClient` (`crates/envd/src/tool_search.rs:2277`); and one `FakeClient` per dedicated scraper.
- **Behaviour** of `catalog.rs` was read directly (`site_matches`, `api_url`, `render`, `render_json`). Nothing was executed against live endpoints, so every "wrong output" claim comes from code reading, not observation.
- **Not verified:** live responses of any API, and whether `omp2`'s chain has a per-provider availability check that `keenable` admission would rely on.
