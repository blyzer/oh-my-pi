# 0018. A provider is shared infrastructure, not a `stream` function

Status: accepted
Date: 2026-09-02
Area: inference

## Context

pi models a provider as `stream` and `streamSimple`, and little else. That is ideal for standing a
provider up in an afternoon and poor for everything built on top of it afterwards. The pressure
showed up twice in the author's own history: a web-search plugin for pi had to reimplement the
provider's transport, and pi's later `image-models.ts` grew a parallel provider surface for image
generation because the chat surface could not host it.

A provider that only streams chat leaves these unowned:

- Anthropic's token-counting endpoint
- Codex's WebRTC voice endpoint and remote compaction
- Anthropic / OpenAI hosted web search
- embeddings
- image and video generation
- tokenization
- usage and quota queries
- model discovery

Every extension that adds one of these must also implement synchronized OAuth refresh and retries
for the same credential, and in practice each one does it partially and differently.

The same gap hides provider-native controls behind an opaque request object: constrained sampling,
OpenAI text verbosity, Google's context filters, forced tool calls, the developer role, mid-session
system prompts. Callers who want them go around the library and re-create every failure mode the
library was supposed to prevent.

## Decision

A provider is a shared piece of inference infrastructure with a typed operation surface. Its
non-chat operations and its cross-cutting concerns MUST live in the inference layer, NEVER in
extensions.

1. Authentication refresh, credential leasing, retries, rate limiting, and account rotation are
   engine-owned and shared across every operation on a route. An extension NEVER holds or refreshes
   a provider credential itself.
2. The operation surface is enumerated, not implied by `stream`: chat, token counting, tokenization,
   embeddings, image and video generation, transcription and speech, realtime voice, web search,
   usage, and discovery are first-class operations a route may declare. Capability is data (0017);
   absence is `unknown` or `unsupported`, never a silent 404.
3. Provider-native controls are exposed as semantic intents on the request (0016): strictness,
   grammar, forced call, service tier, verbosity, context filters, cache retention, developer role.
   The codec decides how — or whether — each intent reaches the wire, and records the adjustment.
4. Extensions contribute providers as data (routes, models, auth spec, discovery spec) plus
   cold-path hooks (`provider_login`, `provider_refresh`, `before_request`, `provider_usage`,
   `search_parse`); they NEVER implement a second transport, retry loop, or refresh protocol.

## Consequences

- One correct OAuth refresh and one retry policy serve chat, search, embeddings, and voice alike.
  A provider that ships usage or discovery gets it through the same credential path as chat.
- Harness features (title generation, search, memory embeddings, voice) can rely on provider
  operations without each feature bundling a client.
- Bleeding-edge provider controls become available to every caller the day the codec supports
  them, with degradation recorded rather than invented per caller.
- Cost accepted: adding a provider means declaring more than a URL. The declaration is data, so
  the cost is in the catalog, not in code.
- Prohibited: extension-implemented `stream` functions, per-extension HTTP clients to provider
  hosts, per-extension token refresh.

## Status in omp

**Status: Implemented.** Provider infrastructure owns auth, retry, routing and an enumerated operation surface. (Verified 2026-10-04 against `omp2` at `083b38fe7d`.)

- Infrastructure: `crates/ai/src/provider`, `crates/ai/src/auth`, `crates/ai/src/discovery`, with the bounded HTTP authority in `crates/envd/src/model_discovery.rs`.
- Operation surface: `OperationKind` in `crates/catalog/src/capability.rs` enumerates Chat, CountTokens, Tokenize, Detokenize, Embed, GenerateImage, GenerateVideo, Speak, Transcribe, Realtime, Search, Usage, DiscoverModels, Auth, Native, Extract.
- Extension hooks (`provider_login`, `provider_refresh`, `before_request`, `provider_usage`, `search_parse`) exist in `crates/py/python/omp/hooks.py` and `crates/ai/src/codec/provider_hooks.rs`. Unverified: that no extension path can hold or refresh a credential itself.

## Amendment (2026-10-08): credential failures are typed and keep the provider's answer

Found live: a v1 Hugging Face key imported by `omp config import-v1` failed every request with the opaque `inference Authentication error during Authentication`, and so did a valid key answered 402 or 401, and every stored login of a process without a terminal. The fixes sit on the one shared engine path (decision 1):

- **One stored kind per leased authentication.** A route leases its own catalog authentication plus the provider-declared ones its codec also takes (Anthropic an OAuth or bearer token after its key, Bedrock Converse a bearer token first, Bedrock Mantle SigV4, Perplexity search an OAuth token first). `omp_ai::auth::provider_auth_specs` lists exactly those for every route of a provider, from the same table (`declared_auth_lease`) the route lease plan reads: a `models.toml` auth that replaces the routes' own (the `apiKey` the v1 importer writes for a keyed `models.yml` provider) replaces it there too, and a declared authentication no route leases takes no part. Every control-plane write of a static secret (`AuthControlHandle::store`, which the v1 `agent.db` and `models.yml` importers, extension `provider_login` answers, and `omp.creds` stores use) and native `/login` (which picks the provider's declared authentication) store an API key or bearer token under a kind some route leases where moving it cannot change what reaches the provider, under this configuration or a later one: the written kind when a route leases it, else the kind of the first leased authentication that takes a static key, but only for a provider whose bundled routes (`Catalog::embedded`) treat a key and a bearer token alike (none takes a key, and either one takes a static bearer token or none takes a bearer credential at all: Hugging Face, Z.ai, Copilot, keyless providers, providers only `models.toml` defines), and a bearer token becomes `api-key` only where every leased key authentication sends it as `Authorization: Bearer`, as every `models.toml` auth does. Every other provider keeps the written kind whatever its `models.toml` auth says: one whose bundled routes take a key header, query parameter (Google's `?key=`), cookie, or sealed body, and one whose bundled routes take a bearer credential only through OAuth or application-default credentials. The kind is authenticated with the ciphertext and nothing records the kind a row was written under, so a looser rule (the first follow-up's, which moved rows both ways for any provider) let a transient `models.toml` auth rewrite rows for good: an Anthropic key re-stored as `bearer` under `auth = "bearer"` leased on Anthropic's OAuth authentication once the auth was removed, and a Google Antigravity key re-stored the same way leased on its OAuth authentication, with no rule able to move either back. Under the rule, a row of a provider that moves follows the configuration (an API key under `auth = "apiKey"`, a bearer token without it, both sent as `Authorization: Bearer`), and a launch without the auth leaves every row leasing exactly as a row written without it. A key or token that is not moved keeps its kind, and a route that requires another reports `kind_mismatch`. The extension SDK's spellings (`api_key`, `session`) become the store's. The importers had stored every key as `api-key` (both now name the store's vocabulary, `CredentialKind::ApiKey`), and `/login` stored the declared kind even where a `models.toml` auth replaced the routes' (a Hugging Face key under `auth = "apiKey"` was stored `bearer`); the routes reject both. The kind is authenticated with the ciphertext, so such rows are repaired by re-encryption, never accepted under the wrong kind: every production composition calls `AuthControlHandle::repair_static_secret_kinds` before any request or discovery refresh, which re-stores each static secret whose stored kind differs from the one a write stores now (a new generation; only the account's credential generation moves in the pool, so an enable, disable, or route change made by another process meanwhile is kept) and logs each re-store with its account and both kinds. This covers rows an earlier writer left and rows a catalog or `models.toml` change left behind, in every profile, with or without a v1 install, and moves a row only where a write would; under one configuration a second pass moves nothing. It decrypts nothing when nothing needs re-storing, and repairs each row on its own (`SecretKindRepairs`): a row it cannot decrypt (no key source without a terminal, or a row sealed under a key the source does not hold) or a write another process won is logged with its account and left for a later launch, and the rows after it are still repaired; composition never fails for it. `kind_mismatch` carries its remedy (`KindMismatchRemedy`), decided by the route from the same rule: `Restart` when the next launch's repair re-stores the row under a kind the route leases (this process could not: another process wrote it first, or the catalog changed while it ran), else `StoreRequiredKind`, since omp keeps the row's kind on purpose and the route needs a credential of its own kind (its environment variable, or a login method that stores one). Neither suggests logging out, which deletes a row a restart may repair or another route leases.
- **Typed credential failures.** `CredentialError` gains `KindMismatch { expected, actual }` (the broker's kind check), `StorageLocked { cause }` (the store has no usable key: `CredentialStorageLock::NoKeySource` when no key source resolved, `KeychainRefused` when the OS keychain refused or could not answer), and `RefreshFailed` (a renewal that ran and failed). The stored refresh engine answers an account it cannot renew (a static key, or a provider with no OAuth runtime) with the typed `not_renewable` failure, which the refreshing source tells apart from a refresh that failed and from a cancellation. A route that gets no credential reports `ErrorDetail::Credential { provider, reason }` with a `CredentialFailure` (`no_rotation_candidate`, `no_source`, `kind_mismatch`, `unusable_stored_credential`, `storage_locked`, `expired`, `not_renewable`, `refresh_failed`, `source_failed`, each also the error's code); the route keeps the most significant failure across its authentications (a locked store, then a wrong kind, then a selected stored row no authentication can lease) instead of discarding it. `storage_locked` is `ErrorKind::CredentialStorageUnavailable`, a cancelled acquisition `ErrorKind::Cancelled`. Route reselection stays open on every credential failure, as before. `storage_locked`'s message says why the store is locked and what unlocks it (`omp_ai::CredentialStorageLock`), so every frontend that renders the failure's message shows it, not only `omp print`'s early check: with no key source it names the key-source settings (`OMP_LLM_KEY_SOURCE=local-file` or `os-keychain`, `sv_credential_key_source`), and with a refusing keychain it says to unlock the keychain or allow access to omp's key, not to switch sources (a row the keychain's key sealed decrypts only with it). Both say the remedy applies where the process holding the store runs, so a gateway client reads it as the gateway's. The remedies name the product's key-source settings although `omp-ai` does not read them: the failure is the one message every frontend renders, and only this product's driver composes the store. The gateway's turn error (`omp-serve`) keeps a credential failure's code and reason after its login hint instead of replacing them.
- **The provider's answer survives a retry that cannot start.** When the attempt layer re-enters for an account rotation (402, 429) or a credential refresh (401) and the re-entry cannot start (no other account, `no_rotation_candidate`, or a credential that cannot be renewed, `not_renewable`) before reserving a wire attempt, the caller gets the provider failure that caused the re-entry (its status, code, and detail, with its attempt visible) instead of the credential failure. A re-entry that fails for a reason of its own (the next account's stored row, a refresh that ran and failed) reports that reason. The surfaced failure's action is `ReselectRoute` when the re-entry asked for route reselection (every route credential failure does), else `Never`. It is not always `Never` because the opaque error it replaces was `ReselectRoute` and `fallback_is_safe` reads the same receipt, so a configured fallback to another provider's credentials keeps running exactly as before; with no fallback planned, the provider's answer is final.
- **Locked storage fails early.** `omp print` refuses before composing when the key source resolves to unavailable (the default `sv_credential_key_source auto` without a terminal), the target provider (`--provider`, else the resolved model's, else the selector's `provider/` prefix when it names a catalog provider) has an enabled stored account, and nothing the request may reach can lease without the store: no catalog authentication of that provider, of another provider serving the model's other routes, or of a planned fallback model's provider (`ai_retry_fallback_chains`, walked as the router does while `ai_retry_model_fallback` is on) is anonymous, resolves from application-default, AWS, or session sources, or names a credential variable that is set. An invocation `--api-key` or a gateway skips the check. The message carries the no-key-source remedy (`omp_ai::CredentialStorageLock::NoKeySource`), naming `OMP_LLM_KEY_SOURCE=local-file` and `sv_credential_key_source`.

Evidence: `crates/ai/src/auth/{broker,store,manager}.rs`, `crates/ai/src/provider/builtin.rs` (`RouteLeaseProvider`, `credential_failure`), `crates/ai/src/layer/attempt.rs` (`unsent_reentry`), `crates/ai/src/error.rs` (`CredentialFailure`), `crates/serve/src/inference.rs` (`inference_turn_error`), `crates/driver/src/v1_import/{auth_credentials,models}.rs`, `crates/driver/src/registry.rs` (`repair_stored_secret_kinds`, `ensure_stored_logins_unlockable`), `crates/app/src/print_mode.rs`; tests at each of those seams, `crates/driver/tests/configured_models.rs` (a `models.toml` auth decides the stored kind for a re-stored row, an extension token and `/login`; the production composition re-stores the row; an Anthropic key keeps its kind across composing with and without `auth = "bearer"` and leases on its key header again, and a Google Antigravity key never leases on its OAuth authentication; for a bundled provider of every authentication shape, a key or token written under `auth = "apiKey"` or `"bearer"` leases, after a launch without the auth, exactly as one written without it; a store this process cannot decrypt still composes and leaves the row for a later launch), and `crates/driver/tests/provider_rejection_surfaces.rs` (a 402 and a 401 from a stub provider reach the caller through the production stack, one wire request each).

## References

- The Harness Playbook, "The inference" — "A provider is more than `stream`"
- pi `packages/ai/src/image-models.ts` (parallel provider surface grown outside `stream`)
- 0002, 0016, 0017, 0019, 0021
- `crates/ai/src/operation/mod.rs`, `docs/py/13-inference.md`
