# 0059. Provider catalog additions and sign-in flows

Status: Proposed 2026-11-13
Date: 2026-11-13
Area: ai

## Context

Research: [`docs/research/providers-signin.md`](../research/providers-signin.md) (Gap 15). Upstream is pinned at `c308970263` (v18.20.0). Paths prefixed `upstream:` are in that tree. Shorthand `registry/…` means upstream `packages/ai/src/registry/…`, and `rules/…` means upstream `packages/catalog/src/compat/rules/…`. Other paths are in omp2 at `ed95a14a41`.

- **The provider diff is exact.** Upstream's generated `KnownProvider` list has 94 ids (`upstream:packages/catalog/src/compat/provider-ids.ts`). omp2's `fixtures/llm-oracle/catalog/providers.toml` has 119 `[providers."…"]` entries. 80 ids are shared, 14 exist only upstream, and 39 exist only in omp2 (search engines, aliases and additions, not reviewed here).
- **Five entries in the brief are already present.** DeepInfra, SiliconFlow (and `siliconflow-cn`), LiteLLM, GitLab Duo agent and Qwen portal are shared ids. They need no work.
- **Of the 14 upstream-only ids, 11 are real gaps and 3 are not.**
  - `apple` is omp2's `apple-intelligence`, kept on purpose by ADR 0042 (`docs/adr/0042-apple-pcc-integration.md:18`).
  - `web` is upstream's group id for search engines. Engines are modelled one per provider in omp2, and ADR 0055 covers the missing ones.
  - `local` is an open question. omp2 has an on-device runtime (`crates/ai/src/local/`) but no `local` catalog provider, and no ADR rules on it.
- **Wire protocols.** Five of the eleven real gaps are catalog data on an existing codec. Snowflake needs a per-account base URL that omp2 lacks as a generic mechanism. Factory Droid routes to four existing codecs but needs a provider hook. Four are System One *judgment* providers, a different operation from chat (see D5).
- **Per-account base URLs are per-provider code today.** Azure is hardcoded in `canonical_provider_base` (`crates/catalog/src/compile.rs:3136`) and in `azure_effective_route` (`crates/ai/src/provider/builtin.rs:1504`). Vertex and Bedrock fill `{location}` and `{region}` inside their own codecs (`crates/ai/src/codec/bedrock.rs:1355-1356`, `bedrock_mantle.rs:23`, `anthropic.rs:2316,2362`). `cloudflare-ai-gateway` has `{account}` and `{gateway}` placeholders in `providers.toml` that no code resolves.
- **The stored credential has no slot for an account value.** `CredentialMetadata` (`crates/ai/src/auth/store.rs:55-66`) holds `account_id`, `principal_id`, `kind`, a generation counter and timestamps. The account host or account name for Snowflake and Cloudflare therefore cannot come from the stored credential without a new field.
- **The catalog schema has no env-based base-URL override.** No `base_url_env`-style key exists in `crates/catalog/src`. `auth` takes `env` names only.
- **Sign-in is data plus Rust modules.** Login flows are rows in `fixtures/llm-oracle/catalog/oauth.toml` with kinds `pkce`, `device-code` and `custom-exchange`, plus Rust modules in `crates/ai/src/auth/oauth/custom/` (`api_key.rs`, `devin.rs`, `gitlab.rs`, `openrouter.rs`, `perplexity.rs`, `zai.rs`, `cursor.rs`, `anthropic.rs`, `google_antigravity.rs`), `crates/ai/src/auth/alibaba_token_plan.rs` and `crates/ai/src/auth/github_copilot.rs`. Shared PKCE is `begin_pkce` (`crates/ai/src/auth/oauth.rs:265`).
- **Upstream sign-in flows.** Upstream has 106 files under `rules/auth/` (105 excluding the `_order.kdl` index), of which 14 are custom hooks. The flows below are missing from omp2: nine in scope, across eight provider ids (Devin has two flows, Windsurf and CLI). A tenth, the Cloudflare Workers AI flow, is deferred with the judgment decision.
- **ADR 0049 classes the work.** Provider catalog data and OAuth flows are class B ("providers are catalog data … codec code only for genuinely different wire protocols"). Nothing here acts on a vendor's repository or account service in the class A sense, so no vendor code enters core crates.

**Constraints checked.** Additive only: new catalog rows, new `oauth.toml` rows and new `custom/*.rs` modules; no existing provider id, schema key or flow changes. Vendor code stays out of core per ADR 0049. Deterministic first: no model is involved in sign-in or routing. ADR 0024 (latched roster): no new roster family; the judgment `OperationKind` (D5) would change the wire vocabulary, so it is deferred. ADR 0036: unaffected, no extension code. ADR 0006 (sandbox policy): applies to the Devin CLI read (O4).

## Decision

**D1. Five data-only providers.** Add `sambanova`, `coralbricks`, `helmcode`, `modular` and `otari` as `providers.toml` rows on existing codecs, then run `just catalog-snapshot`. `modular` uses `thinking-format "openrouter"`, a value documented in the compat README (`crates/catalog/compat/README.md:169`), so it needs no code. `otari` ships with its fixed URL: the schema has no base-URL env override, so self-hosted Otari waits on O1. Each row is unselectable until its key is set. The roster is refreshed with `just catalog-import-v1`, which also fills `scaleway`'s empty seeded roster.

**D2. A generic account-scoped base URL, then the providers that need it.** Add one mechanism that fills a declared `{account}` (and `{gateway}`) placeholder from a configured value, then fails with a typed error when the value is missing. The value comes from an env var first (O2). It replaces the Azure-only hardcoding as the route for new providers. Then:
- `cloudflare-ai-gateway` starts working, since its placeholders are already in `providers.toml`.
- `snowflake` is added as data: default model `claude-opus-5-5`, per-model `api` of `anthropic-messages` at `<account>/api/v2/cortex` or `openai-completions` at `<account>/api/v2/cortex/v1` (`upstream:packages/catalog/src/compat/rules/providers/snowflake.kdl:13-18`, `:79`). The roster already carries per-model `api` and `baseUrl`.

**D3. Factory Droid as a provider hook over existing wires.** Upstream routes each model to `openai-completions` at `/api/llm/o/v1`, `openai-responses`, `anthropic-messages` at `/api/llm/a`, or google-generate at `/api/llm/g/v1` (`upstream:packages/catalog/src/wire/factory-droid.ts:16-24`, `:75-84`). omp2 has all four codecs. The hook goes in `crates/ai/src/codec/provider_hooks.rs`, which already exists. It covers per-model wire choice, the regional host (`api.factory.ai` or `api.eu.factory.ai`), client headers and tool-name mapping. No new codec is added. The provider stays out of the catalog snapshot until recorded fixtures exist (O3).

**D4. Nine sign-in flows across eight provider ids, each on an existing mechanism.** Each is a `custom` module or an `oauth.toml` row, with no new core protocol.

| Provider | Upstream flow (file) | omp2 mechanism |
|---|---|---|
| Mistral Vibe | Browser sign-in: `POST /api/vibe/sign-in` with a PKCE challenge, then poll, then `POST …/exchange` for a plain API key (`upstream:…/registry/oauth/mistral-vibe.ts:4-6`, `:21-26`) | New `custom/mistral_vibe.rs`, reusing `begin_pkce` |
| Snowflake | Account prompt, Snowflake OAuth with `LOCAL_APPLICATION`, PKCE, loopback port 54551, refresh (`registry/oauth/snowflake.ts:9`; `rules/auth/snowflake.kdl`) | New `custom/snowflake.rs` on the callback server (`crates/ai/src/auth/oauth/callback.rs`). Needs D2 |
| Kilo | Device authorisation (`registry/oauth/kilo.ts`) | `oauth.toml` `device-code` row |
| Muse Code | Device login, then a key minted at `https://api.meta.ai/muse-code/key` (`registry/oauth/muse-code.ts:9`) | New `custom/muse_code.rs` |
| Factory Droid | WorkOS device login, `/api/cli/whoami` and `/api/cli/org` (`registry/oauth/factory-droid.ts:8-10`, `:34-37`) | New `custom/factory_droid.rs`. Needs D3 |
| Cloudflare AI Gateway | Collects gateway credential and account and gateway ids (`registry/oauth/cloudflare-ai-gateway.ts:5`) | Existing `custom/api_key.rs` with two extra fields. Needs D2 |
| Alibaba Coding Plan | Region choice (international or China base URL), key paste, optional proxy (`registry/oauth/alibaba-coding-plan.ts:5-8`) | Existing `auth/alibaba_token_plan.rs` pattern |
| Devin, Windsurf | Windsurf authorise page with PKCE and loopback, stored as a `devin` key (`registry/oauth/devin-windsurf.ts:1-5`) | Existing `custom/devin.rs` |
| Devin, CLI (second Devin flow) | Adopts the seat key the Devin CLI stores in `credentials.toml`. Never prompts, never touches the network (`registry/oauth/devin-cli.ts:1-4`) | New `custom/devin_cli.rs`, read-only |

**D5. The judgment operation is deferred to its own ADR.** Four providers are System One judgment models, not chat: `cloudflare-workers-ai`, `liquid`, `nace` and `typesafe`. They need an operation omp2 does not have. `OperationKind` (`crates/catalog/src/capability.rs:95`) has `Chat`, `Embed`, `Search`, `Native`, `Extract` and so on, but no judge. Upstream's `packages/ai/src/judgment/systemone.ts` defines the body `{ model, state, questions }` and the per-vendor endpoints. The Cloudflare Workers AI sign-in ships with this decision, not with D4. This ADR does not add an `OperationKind` variant, and it does not add the four providers as chat entries.

## Alternatives rejected

| Alternative | Why rejected |
|---|---|
| Azure-style hardcoded resolver for each account provider | Repeats `canonical_provider_base` (`compile.rs:3136`) once per provider. D2 generalises it instead. |
| Add the judgment providers as chat entries | They would send chat requests to a decisions API, which is wrong on the wire. |
| New codec for Factory Droid | All four wires exist. A codec would duplicate them. |
| Add `local` as a catalog provider now | Its roster would describe on-device runtime artefacts (`crates/ai/src/local/tiny_catalog.rs`, `speech_catalog.rs`), a different thing from upstream's `rules/providers/local.kdl`. That is a design decision, not a data edit. |
| One generic "custom" sign-in hook for all nine | Each flow has its own endpoints, state and failure modes. A shared hook would hide them. |
| Store the account value in the existing credential secret | The secret payload is opaque and the metadata has no field for it (`store.rs:55-66`). Mixing a host into the secret would also make it unreadable without the secret. |

## Consequences

- Five providers can be selected once a key is set. Nothing changes for providers already present.
- D2 adds a mechanism that `cloudflare-ai-gateway` needs today, and that Snowflake and Cloudflare Workers AI will also need.
- Factory Droid and the Devin CLI flow depend on services or files outside this repository (O3, O4 and O5).
- Judgment remains unavailable until its own ADR.

## Slices

Each slice is one PR. Catalog slices end with `just catalog-snapshot`. Gates are `just test-pkg omp-catalog` for data and `just test-pkg omp-ai` for code, then `just check-minimal-caps`.

| # | Slice | Ships behind | Acceptance and tests |
|---|---|---|---|
| S1 | Five data rows (D1) and roster refresh | Unselectable until a key is set | `omp-catalog` tests resolve each provider's env names, base URL, codec and default model. The snapshot regenerates. |
| S2 | Account-scoped base URL (D2) and `cloudflare-ai-gateway` as the first user | No change for other providers. A missing value fails with a typed error | Unit tests: placeholder filled from the configured value; env fallback; missing value returns the typed error, never a literal `{account}` on the wire. |
| S3 | Mistral Vibe sign-in | `login` command only | Fixture test over the exchange: pending, `429` and `410` polls; a plain key is stored. No network in tests. |
| S4 | Kilo and Muse Code sign-ins (`device-code`) | `login` command only | Fixture tests for the device poll and, for Muse Code, the key mint. |
| S5 | Snowflake provider data and sign-in | Needs S2; dormant without a configured account | Loopback callback test on port 54551; refresh test; the paste-code fallback is tested. Per-model `api` routing test. |
| S6 | Alibaba Coding Plan and Devin Windsurf sign-ins | `login` command only | Region-to-base-URL test; Devin key stored as `devin`. |
| S7 | Devin CLI adoption | `login` command only; read-only | Test on fixture credential files for each platform path (Windows, XDG, macOS). No write, no network. |
| S8 | Factory Droid hook and sign-in (D3) | Absent from the snapshot until fixtures exist | Recorded fixtures under `fixtures/llm-oracle/agent-protocols/factory-droid/` for each wire. Replay tests per wire, plus the whoami and org flow. |
| — | Judgment operation, the four judgment providers, and the Cloudflare Workers AI sign-in | Deferred to its own ADR | None in this ADR. |

## Upstream origin and license

Files a port draws behaviour from, all `upstream:` paths at `c308970263`: `packages/catalog/src/compat/rules/providers/{sambanova,coralbricks,helmcode,modular,otari,snowflake,factory-droid}.kdl`, the matching `rules/auth/*.kdl`, `packages/ai/src/registry/oauth/*.ts` for the sign-in flows, `packages/catalog/src/wire/factory-droid.ts`, and `packages/ai/src/judgment/systemone.ts`.

Licences. The root `LICENSE` is MIT, "Copyright (c) 2025 Mario Zechner" and "Copyright (c) 2025-2026 Can Bölük". `packages/catalog/LICENSE` is MIT with two copyright lines, "Copyright (c) 2025-2026 Can Bölük" and "Copyright (c) 2026 Stencil Labs, Inc." The catalog data is derived from upstream's `packages/catalog` rules, so the catalog package's notice is the one to carry. Record it in the root `THIRD-PARTY-NOTICES.txt` in the S1 PR.

## Open questions for the owner

- **O1.** `otari` supports self-hosted gateways through `OTARI_BASE_URL`, but the catalog schema has no env base-URL override. Recommendation: ship `otari` with its fixed URL in S1, and add an override only as a separate schema change, if the owner wants self-hosted Otari. The code cannot settle this, because the schema itself would change.
- **O2.** Where does the account value for D2 come from? The code settles part of it: the stored credential has no slot for it (`crates/ai/src/auth/store.rs:55-66`). Recommendation: an env var first, in S2. A value kept with the stored credential needs a new optional metadata field, which is a schema change and an owner decision.
- **O3.** Who records the Factory Droid fixtures, and from which account? Recommendation: the owner records them. No fixture means no snapshot entry, so the provider stays absent until then. omp2 has agent-protocol fixtures only for cursor, devin, gitlab and ollama.
- **O4.** Devin CLI adoption reads another tool's credential file. ADR 0006 sets the host-side sandbox policy but does not say whether the host may read another tool's credential file outside the workspace, so the code and ADRs leave this open. Recommendation: read-only, only on an explicit `login` command, never write, and never cache the value beyond the stored credential. The owner should rule on it before S7 lands.
- **O5.** Factory Droid's device login and org refresh call services that are not public. Recommendation: ship only after the S8 fixtures prove the flow. Do not infer endpoints from upstream beyond those fixtures.
- **O6.** Judgment operation: add an `OperationKind` variant, and do the four providers become judge routes? This needs its own ADR. ADR 0053 records the judge role as not started and may use it. Check against ADR 0024 (latched roster) before any variant is added, because a new variant changes the wire vocabulary. Whether `opencode-zen` and `commandcode` carry judge models is unverified.
- **O7.** `local`: keep it out of the provider catalog? Recommendation: yes, for now. Changing that needs an ADR, because it defines what a catalog model is for on-device workers.

## Acceptance tests

1. After S1, `omp-catalog` resolves all five providers with the expected env names, base URLs, codecs and default models.
2. After S2, a `cloudflare-ai-gateway` request with no configured account value fails with the typed error and never sends `{account}`.
3. Each sign-in in S3 to S7 completes against recorded or canned responses, with no live network in tests.
4. S8 is absent from the snapshot until its fixtures are committed.
5. `just check-minimal-caps` still passes, with no new `cap-*` feature.

## Status in omp

**Status: Not started.** The catalog rows, the `oauth.toml` kinds, the custom OAuth modules and the account placeholder mechanism exist in part. None of S1 to S8 is implemented.

## References

ADR 0006 (sandbox policy); ADR 0024 (latched roster); ADR 0042 (`apple-intelligence` identifier); ADR 0049 (class B); ADR 0053 (judge role, not started); ADR 0055 (web search backends, a parallel gap); `docs/research/providers-signin.md`; `crates/catalog/src/compile.rs`; `crates/catalog/src/capability.rs`; `crates/ai/src/provider/builtin.rs`; `crates/ai/src/auth/store.rs`; `crates/ai/src/auth/oauth.rs`; `crates/ai/src/auth/oauth/custom/`; `crates/ai/src/codec/provider_hooks.rs`; `fixtures/llm-oracle/catalog/providers.toml`; `fixtures/llm-oracle/catalog/oauth.toml`.
