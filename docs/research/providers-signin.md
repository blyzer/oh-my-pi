# Providers and sign-in: upstream parity on `omp2` (Gap 15, `providers-signin`)

Status: research complete. No code changed.
ADR: [`0059-provider-catalog-additions`](../adr/0059-provider-catalog-additions.md) (Proposed) records the decisions in this note.
Date: 2026-11-13
Upstream pin: `c308970263` (v18.20.0), `/Users/enverfrancisco/repositories/omp-work/upstream`. Every cited upstream line is re-checked at this pin. The provider list is the same at the pin and at `upstream/main` `84f5adc3bb` (94 ids).
omp2 snapshot: worktree `omp-work/gaps` at `ed95a14a41`. Citations resolve at this commit.
Sources compared: `fixtures/llm-oracle/catalog/{providers.toml,oauth.toml,models.json.zst}` (the checked-in catalog source that `crates/catalog/data/catalog.postcard` is compiled from, per `crates/catalog/README.md`), `crates/catalog/compat/providers/*.kdl`, `crates/ai/src/{codec,auth}`.

## Bottom line

1. **Part of the brief is already present.** DeepInfra, SiliconFlow (and `siliconflow-cn`), LiteLLM, GitLab Duo agent (`gitlab-duo-agent`) and Qwen portal are shared ids. They need no work.
2. **Exactly 14 of upstream's 94 `KnownProvider` ids have no `[providers."<id>"]` entry in `omp2`.** Three of the 14 are not gaps (`apple`, `web`, `local`; section 1). **11 are genuinely missing.**
3. **Only the judgment providers need a new operation.** `cloudflare-workers-ai`, `liquid`, `nace` and `typesafe` are System One *judgment* models, not chat. `omp2` has no judgment operation: `OperationKind` (`crates/catalog/src/capability.rs:95`) has no such variant. Five are plain catalog data. Snowflake is data plus a per-account base URL that `omp2` lacks as a generic mechanism. `factory-droid` is a routing layer over four wires `omp2` already has, not a new wire.
4. **Sign-in gaps:** 10 flows across 9 provider ids are missing. Eight are in scope and decided in ADR 0059 (D4). The Cloudflare Workers AI flow is deferred with the judgment decision (D5). The Devin CLI flow is not adopted (D6). None needs a new core protocol: each fits an existing `oauth.toml` kind or a `custom/*.rs` module (section 3).

## 1. Providers: exact diff

Method: upstream's generated `packages/catalog/src/compat/provider-ids.ts` (`KnownProvider`, 94 ids, "every chat-model provider with a catalog entry") against the `[providers."…"]` headers in `omp2`'s `providers.toml` (119 entries). Both lists were extracted by script and compared with `comm` in both directions. 94 = 80 shared + 14 upstream-only; 119 = 80 shared + 39 omp2-only.

### The 14 upstream ids with no `omp2` entry

| Upstream id | Real gap? | What it is (upstream file) | Wire, and what `omp2` needs |
|---|---|---|---|
| `sambanova` | yes | OpenAI-compatible, `https://api.sambanova.ai/v1`, `SAMBANOVA_API_KEY` (`packages/catalog/src/compat/rules/providers/sambanova.kdl`) | **Data only.** `openai-completions` exists |
| `coralbricks` | yes | OpenAI-compatible, `https://inference.coralbricks.ai/v1`, `CORAL_API_KEY` or `CORALBRICKS_API_KEY` (`providers/coralbricks.kdl`) | **Data only** |
| `helmcode` | yes | OpenAI-compatible, `https://api.helmcode.com/v1`, `HELMCODE_API_KEY` (`providers/helmcode.kdl`) | **Data only** |
| `modular` | yes | OpenAI-compatible, `https://api.modular.com/v1`, `MODULAR_API_KEY`; catalog-only roster from models.dev; `thinking-format "openrouter"` (`providers/modular.kdl`) | **Data only.** `openrouter` is a documented thinking-format value (`crates/catalog/compat/README.md:169`) |
| `otari` | yes | OpenAI-compatible gateway, `https://api.otari.ai/api/v1`, `OTARI_API_KEY`; credential-scoped authoritative discovery; `supports-developer-role #false` (`providers/otari.kdl`) | **Data only.** Shipped with its fixed URL. Self-hosted gateways use the existing `models.toml` `baseUrl` overlay, with no schema change (ADR 0059 D1) |
| `snowflake` | yes | Cortex REST. Default model `claude-opus-5-5`, `SNOWFLAKE_PAT` (`providers/snowflake.kdl:13-14`). Per-model `api`: `anthropic-messages` at `<account>/api/v2/cortex` (`:18`) or `openai-completions` at `<account>/api/v2/cortex/v1` (`:79`). 15 seeded models | **Data plus a missing mechanism.** The roster supports per-model `api` and `baseUrl`, but the account host is per-user and `omp2` has no generic resolver (section 2) |
| `factory-droid` | yes | `packages/catalog/src/wire/factory-droid.ts` routes each model to one of four wires (`:16-24`, `:75-84`), `api = "factory-droid-agent"`, 77 seeded models (`providers/factory-droid.kdl`); hosts `api.factory.ai` and `api.eu.factory.ai`; the runtime is `packages/ai/src/providers/factory-droid.ts` (814 lines) plus `factory-droid/gemini.ts` and `tool-names.ts` | **No new wire.** `omp2` has all four codecs (`openai-completions`, `openai-responses`, `anthropic-messages`, google-generate). Needs a provider hook (`crates/ai/src/codec/provider_hooks.rs`) for per-model wire choice, regional host, client headers and tool-name mapping, plus device-code sign-in. Largest item here |
| `cloudflare-workers-ai` | yes | Judge models `@cf/cloudflare/clef` and `clef-flash`; `api = "workers-ai-systemone"`; `POST …/accounts/<account>/ai/run/@cf/cloudflare/<model>` (`providers/cloudflare-workers-ai.kdl`) | **New operation and codec** (judgment) |
| `liquid` | yes | Judge models `d1.1` and `d1-ultra`; `api = "liquid-systemone"`; `POST /decisions/v1/systemone`; `LIQUID_API_KEY` (`providers/liquid.kdl`) | **New operation and codec** (judgment) |
| `nace` | yes | Drex judge, `api = "nace-systemone"`, `POST /v1/systemone`; rejects TypeSafe's `assets` map and any base64 media data URL inside text (`providers/nace.kdl`) | **New operation and codec** (judgment) |
| `typesafe` | yes | `api = "typesafe"`, `POST {baseUrl}/v1/systemone`; the same body serves OpenJEV, Kev, self-hosted Clef, OpenCode Zen and Command Code (`packages/ai/src/judgment/systemone.ts` header comment) | **New operation and codec** (judgment) |
| `apple` | **no, alias** | On-device Apple Foundation Models. `omp2` has `apple-intelligence` (`base_url = "local://apple-intelligence"`, `auth none`) and `crates/ai/src/local/applefm*` | **Deliberate.** ADR 0042 (`docs/adr/0042-apple-pcc-integration.md:18`): "The existing `apple-intelligence` on-device identifier and behavior remain unchanged." Keep the id; at most add an import alias |
| `web` | **no, structural** | Upstream's group id for standalone web-search engines. `omp2` models each engine as its own provider (`brave`, `exa`, `tavily`, …) | None here. Missing engines are covered by ADR 0055 |
| `local` | **no, settled** | On-device inference workers as catalog models (upstream `providers/local.kdl:7`, `api = "local-inference"`, 14 models). The on-device route is live under the `local` codec (`crates/ai/src/provider/builtin.rs:893`), and the catalog row that serves it is `apple-intelligence`. | Keep `apple-intelligence` as the id; upstream's `local` catalog row is not adopted (ADR 0059 D7) |

Count check: data-only 5 + snowflake 1 + factory-droid 1 + judgment 4 = 11 genuinely missing; apple, web, local = 3; total 14. ✓

### Judgment is one capability, not four providers

Upstream `packages/ai/src/judgment/{systemone,native,types,wire}.ts` implements a typed-question operation: body `{ model, state, questions }`, returning calibrated choice, bool or score answers. Per-vendor variants: `typesafe` (`POST /v1/systemone`, images as references into a top-level `assets` map), `openrouter-decisions` (`POST /decisions`), `workers-ai-systemone` (Cloudflare `{ result, success, errors }` envelope, account id in the URL), `liquid-systemone`, and `nace-systemone`. In `omp2` the word "judgment" appears only in prose, and `OperationKind` (`crates/catalog/src/capability.rs:95`) is `Chat, CountTokens, Tokenize, Detokenize, Embed, GenerateImage, GenerateVideo, Speak, Transcribe, Realtime, Search, Usage, DiscoverModels, Auth, Native, Extract`. So this is a new `OperationKind` plus codecs: an ADR-sized decision. Recommendation: do **not** add the four as chat entries. Open a separate decision on whether `omp2` wants the judge operation. The existing `typesafe`-served providers `opencode-zen` and `commandcode` are already in `omp2` as chat routes, so any judge models they carry are presumably unreachable; I did not verify this.

### Present in `omp2` but not in upstream's `KnownProvider` (39)

`agnes agnes-plan apple-intelligence brave cohere crofai duckduckgo ecosia exa firecrawl friendli gigachat google-search jina kagi kimi-search llama.cpp minimax-cn mojeek openai-codex-device opencode ovhai parallel parallel-extract perplexity perplexity-anonymous perplexity-cookie perplexity-openrouter poolside sarvam searxng startpage stepfun-plan synthetic-search tavily tinyfish yandex zai-coding-plan zai-search`. These are search engines, aliases or `omp2` additions. No action.

### Secondary data finding

For the 80 shared ids, upstream seeds models in the provider `.kdl` that `omp2`'s roster (`models.json.zst`, 70 providers) lacks for two: `scaleway` (upstream seeds 12, `omp2` 0; discovery is `authoritative = false`, so the roster stays empty until a key is present) and `inception` (upstream 3, `omp2` 0; discovery is authoritative, so the gap is intentional). Only `scaleway` is a plausible data gap. The roster is imported from v1's generated `models.json` (`crates/catalog/README.md`), so refresh it with `just catalog-import-v1`, not by hand. `deepinfra` has 101 roster models in `omp2`.

## 2. Per-account base URLs: the one real design gap

`omp2` has no generic account-scoped base-URL mechanism. What exists:

- **Azure** is hardcoded. `canonical_provider_base` (`crates/catalog/src/compile.rs:3136`) rewrites Azure's `{region}`/`{deployment}` base, and `azure_effective_route` (`crates/ai/src/provider/builtin.rs:1504`, provider check at `:1510`) builds the deployment URL from `AzureEndpointConfig`.
- **Vertex and Bedrock** fill `{location}` and `{region}` inside their own codecs (`crates/ai/src/codec/bedrock.rs:1355-1356`, `bedrock_mantle.rs:23`, `anthropic.rs:2316` and `:2362`), not through a shared mechanism.
- **`cloudflare-ai-gateway`** has `{account}` and `{gateway}` placeholders in `fixtures/llm-oracle/catalog/providers.toml`, but no code resolves them. A search of `crates/ai/src/{provider,auth}` and `crates/catalog/src` finds none.
- **The stored credential has no slot for an account value.** `CredentialMetadata` (`crates/ai/src/auth/store.rs:55-66`) holds `account_id`, `principal_id`, `kind`, a generation counter and timestamps. The secret payload is opaque to this metadata. So a Snowflake account URL or a Cloudflare account id cannot come from the stored credential without a new field.

So Snowflake and both Cloudflare providers need a generic account-scoped base-URL mechanism, not a per-provider resolver. ADR 0059 D2 fixes its inputs to environment variables, validated as `production_azure_endpoint` validates its own (`crates/driver/src/registry.rs:1786`): `OMP_CLOUDFLARE_ACCOUNT_ID`, `OMP_CLOUDFLARE_AI_GATEWAY_NAME` and `OMP_SNOWFLAKE_ACCOUNT`. The stored credential is not used for the account value. This is more than catalog data.

## 3. Sign-in

Upstream non-API-key login flows (`packages/catalog/src/compat/rules/auth/*.kdl`: 106 files, 105 excluding the `_order.kdl` index): 14 `custom` hooks, 10 `oauth-code`, 4 `device-code`; the rest are `api-key` prompts or none. Implementations: `packages/ai/src/registry/oauth/*.ts`.

`omp2` has 19 `[[provider]]` rows in `fixtures/llm-oracle/catalog/oauth.toml` (kinds `pkce`, `device-code` and `custom-exchange`) plus Rust modules in `crates/ai/src/auth/oauth/custom/` (`anthropic`, `api_key`, `cursor`, `devin`, `gitlab`, `google_antigravity`, `openrouter`, `perplexity`, `zai`), `crates/ai/src/auth/github_copilot.rs` and `crates/ai/src/auth/alibaba_token_plan.rs`. Shared PKCE is `begin_pkce` (`crates/ai/src/auth/oauth.rs:265`).

| Upstream flow | `omp2` | Upstream shape (file) |
|---|---|---|
| anthropic, openai-codex, google-gemini-cli, google-antigravity, github-copilot, gitlab-duo, gitlab-duo-agent, devin, cursor, openrouter, perplexity, xiaomi, zai (and zai-coding-plan), kimi-code, xai-oauth | present, by id or alias | n/a |
| **Mistral Vibe** (`mistral`) | **missing**; `mistral` is bearer-env only | `registry/oauth/mistral-vibe.ts:4-6`, `:21-26`: `POST console.mistral.ai/api/vibe/sign-in` with a PKCE challenge (S256) → `{process_id, sign_in_url, poll_url, expires_at}`; poll every 3 s (410 expired, 429 too fast); `POST …/sign-in/{process_id}/exchange` → `{api_key}`. The result is a plain Mistral API key billed to the plan's Vibe Code quota |
| **Snowflake** | **missing**, with no `snowflake` provider either | `registry/oauth/snowflake.ts:9` (client `LOCAL_APPLICATION`), `auth/snowflake.kdl`: account prompt, Snowflake OAuth with PKCE on loopback port 54551, refresh hook, `api-key-format "structured"`, paste-code fallback. Alternative: `SNOWFLAKE_PAT` env |
| **Kilo** | missing; bearer-env only | `registry/oauth/kilo.ts`: device authorisation at `https://api.kilo.ai/api/device-auth`, 5 s poll |
| **Muse Code** | missing; bearer-env only (`api.meta.ai/v1`) | `registry/oauth/muse-code.ts:9`: device login, then a key minted at `https://api.meta.ai/muse-code/key` |
| **Factory Droid** | missing | `registry/oauth/factory-droid.ts:8-10`, `:34-37`: WorkOS device login, `/api/cli/whoami`, `/api/cli/org`, an org-scoped refresh grant |
| **Cloudflare Workers AI** | missing (with the provider) | `registry/oauth/cloudflare-workers-ai.ts`: collects an API token and account id. **Deferred** with the judgment decision |
| **Cloudflare AI Gateway** | provider present, no guided login | `registry/oauth/cloudflare-ai-gateway.ts:5`: collects the gateway credential and account and gateway ids |
| **Alibaba Coding Plan** | provider present; only `alibaba_token_plan.rs` exists | `registry/oauth/alibaba-coding-plan.ts:5-8`: region choice (international or China base URL), key paste, optional proxy |
| **Devin, Windsurf** | `devin` exists; this variant not found | `registry/oauth/devin-windsurf.ts:1-5`: Windsurf authorise page with PKCE and loopback, stored as a `devin` key |
| **Devin, CLI** | not adopted | `registry/oauth/devin-cli.ts:1-4` adopts the seat key the Devin CLI keeps in `credentials.toml`. omp2 does not: ADR 0059 D6 gives five reasons. The existing delegated ACP login notice tells the user to run the Devin CLI login (`crates/driver/src/delegated_acp.rs:175-183`) |
| coreweave | present as bearer; login helper absent | `registry/oauth/coreweave.ts`: requires `COREWEAVE_PROJECT=<team>/<project>` (OpenAI-Project header) before `/login` |
| stencil | not a model provider | `auth/stencil.kdl`: credential for omp-hosted Stencil services, OIDC code flow. Out of scope for the catalog |

Count: 10 missing flows across 9 provider ids (Devin has two flows). In scope and decided: 8 flows across 8 provider ids (ADR 0059 D4). Deferred: the Cloudflare Workers AI flow (D5). Not adopted: the Devin CLI flow (D6).

### Recommendation (sign-in)

Every in-scope flow fits an existing core mechanism: a `device-code`, `pkce` or `custom-exchange` row in `oauth.toml` for the pure OAuth ones (Kilo, Muse Code, Factory Droid), and a `custom/*.rs` module for the bespoke ones (Mistral Vibe, Snowflake, Cloudflare AI Gateway, Alibaba Coding Plan, Devin Windsurf). They stay in core under ADR 0049 class B (`ai/src/auth/**` OAuth flows are listed there). No new transport is needed. Order by value: Mistral Vibe (three calls, no per-user host), then Kilo and Muse Code (device code), then Snowflake with its provider data, then Factory Droid with its hook, since its login is useless without the provider.

## 4. Suggested slices

1. **Data PR.** Add `sambanova`, `coralbricks`, `helmcode`, `modular` and `otari` to `providers.toml`, regenerate with `just catalog-snapshot`, and refresh the roster with `just catalog-import-v1` (this also fills `scaleway`).
2. **Account-scoped base URL.** Environment-variable mechanism per ADR 0059 D2, then `snowflake` provider data (per-model `api`) and its sign-in module. The same mechanism serves `cloudflare-ai-gateway`, whose `{account}`/`{gateway}` placeholders may already be a live defect (the S2 test settles it).
3. **Mistral Vibe, Kilo and Muse Code sign-ins.**
4. **Factory Droid:** provider hook and device-code login. Needs its own recorded fixture set first.
5. **Separate ADR:** the judgment operation, covering `typesafe`, `liquid`, `nace` and `cloudflare-workers-ai`, and the Cloudflare Workers AI sign-in with it.
6. **Owner decisions.** `local` is settled by ADR 0059 D7 (keep `apple-intelligence`). `apple` is settled by ADR 0042.

## 5. Verification performed and limits

- **Provider ids:** upstream list parsed from `provider-ids.ts` at the pin (94); `omp2` list parsed from `providers.toml` (119); `comm` both ways; counts reconcile (94 = 80 + 14; 119 = 80 + 39). The same 94 and 14 come out at `upstream/main`.
- **Upstream wire facts** read from the `.kdl`, `auth/*.kdl`, `wire/factory-droid.ts`, `judgment/systemone.ts` and `registry/oauth/*.ts` files named above, all re-checked at the pin.
- **`omp2` roster:** read by decompressing `models.json.zst` (70 providers; per-model `api` and `baseUrl` present).
- **Stored credential fields:** `crates/ai/src/auth/store.rs:55-66`.
- **Not done:** no catalog compiled or run; upstream endpoints are as written in upstream source, not probed live. Remaining open items are below.

## Open items (ADR 0059)

- **O1.** Who records the Factory Droid fixtures, and from which account? Recommendation: the owner records them; no fixture means no snapshot entry.
- **O2.** Factory Droid's device login and org refresh use services that are not public. Recommendation: ship only after the fixtures prove the flow.
- **O3.** Judgment `OperationKind`: a separate ADR. Check against ADR 0024 before any variant is added.
