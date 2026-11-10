# search-provider — Brave web search (ADR 0049 D5 rescue)

Declares a Brave `ProviderSpec` and a soft `web_search` device that pages and
parses ranked results in Python (class-b response interpretation).

Historical source: `git show 00611047c1^:examples/search-provider/`.

## Surface

- Provider `brave-search@0`
- Device `web_search@1` (family `ws`, place `env`)
- Settings: optional `credential_id`
- Pure helpers: `omp_search_provider.parse`

## Gaps vs frozen API

- Route uses `Api.SEARCH_HTTP` (generic HTTP search selector). Brave is not a
  dedicated closed `Api` enum member; spine auto-dispatch must not treat this as
  Exa-wire-compatible.
- No extension-visible SEARCH parser-registration seam yet — the soft device
  owns parsing explicitly via `urllib` (same v1 no-sandbox ruling as the
  historical example). `omp.creds.reveal` exists on the frozen surface now.

## How to test

```sh
PYTHONPATH=extensions/search-provider/src python3 -m unittest discover -s extensions/search-provider/tests -v
```
