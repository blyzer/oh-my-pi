# 11 Catalog census of the embedded snapshot

> Part of the [OMP2 gateway-capabilities research](../../README.md). Audited commit `85d219ae7f`. Measurement, owner: provider runtime (W1).

Command: `CARGO_TARGET_DIR=/private/tmp/omp-research-target-providerruntime cargo build --offline --release` then `catcount` in `/private/tmp/omp-research-bench-providerruntime/catcount` (a 40-line program outside the OMP2 tree that depends on `omp-catalog` by path and calls `Catalog::embedded()`, then counts `providers()`, `routes()`, `models()`, `aliases()`, route codecs, transports, codec profiles and discovery kinds).
Toolchain: nightly-2026-08-08 (pinned by the repo). Environment: `uname -m` arm64, 17179869184 bytes RAM, 10 cores, `uptime` 23:00 load averages 7.24 14.75 16.10 (shared machine). Runs: 1 (deterministic count, no timing).

## Raw output

```
providers	125
routes	171
models	5289
aliases	230
auth_specs	133
oauth_specs	20
header_profiles	8
discovery_specs	92
distinct_codecs	33
codec	anthropic	28
codec	bedrock-converse	2
codec	bedrock-mantle	1
codec	cursor	1
codec	devin	1
codec	gitlab-duo	1
codec	google-cca	3
codec	google-genai	2
codec	google-vertex	2
codec	local	2
codec	ollama	1
codec	openai-chat	81
codec	openai-codex	10
codec	openai-responses	14
codec	parallel-extract	1
codec	search-brave	1
codec	search-duckduckgo	1
codec	search-ecosia	1
codec	search-exa	1
codec	search-firecrawl	1
codec	search-google	1
codec	search-jina	1
codec	search-kagi	1
codec	search-kimi	1
codec	search-mojeek	1
codec	search-parallel	1
codec	search-perplexity	4
codec	search-searxng	1
codec	search-startpage	1
codec	search-synthetic	1
codec	search-tavily	1
codec	search-tinyfish	1
codec	search-zai	1
transport	AwsEventStream	2
transport	Connect	2
transport	Http	165
transport	Local	1
transport	Websocket	1
codec_profile	AppleFm	1
codec_profile	GoogleCcaAntigravity	2
codec_profile	GoogleCcaGeminiCli	1
codec_profile	Standard	167
model_route_pairs_by_codec	anthropic	561
model_route_pairs_by_codec	bedrock-converse	197
model_route_pairs_by_codec	bedrock-mantle	5
model_route_pairs_by_codec	cursor	66
model_route_pairs_by_codec	devin	2
model_route_pairs_by_codec	gitlab-duo	1
model_route_pairs_by_codec	google-cca	45
model_route_pairs_by_codec	google-genai	51
model_route_pairs_by_codec	google-vertex	14
model_route_pairs_by_codec	ollama	51
model_route_pairs_by_codec	openai-chat	4082
model_route_pairs_by_codec	openai-codex	8
model_route_pairs_by_codec	openai-responses	225
discovery_kind	GoogleModels	1
discovery_kind	OllamaTags	1
discovery_kind	OpenAiModels	80
discovery_kind	Specialized	10
```

## What it shows

Exact counts of the compiled catalog the runtime loads (`crates/catalog/data/catalog.postcard`). It does not show which codecs are reachable per route at runtime, only what the catalog declares. The cross-check against the source fixtures is in `evidence/omp2-provider-runtime.md` section 1.
