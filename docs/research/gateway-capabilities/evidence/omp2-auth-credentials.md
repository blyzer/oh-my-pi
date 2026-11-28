# OMP2 authentication and credential lifecycle: code audit

> Part of the [OMP2 gateway-capabilities research](../README.md). Evidence input to deliverables 3, 4 and 5 (area AUTH, stage 1). Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Scope: dimensions A01-A12, G09, G10, G11, S02, O02 (vocabulary in [`01-method-and-dimension-ids.md`](01-method-and-dimension-ids.md)). Every OMP2 claim cites `path:line symbol` at the audited commit and carries a maturity label. ADR text is a claim to verify, not evidence; where code departs from an ADR the departure is listed in section 5. Nothing was built, run or measured: this audit is by code reading only (section 6 lists what was not verified). The matrix rows are in [`omp2-auth-credentials.tsv`](omp2-auth-credentials.tsv).

## 1. Architecture in one page

Credential custody has one owner in the inference layer (ADR 0018 decision 1; `crates/ai/src/auth/mod.rs:1` module doc "Catalog-driven authentication, sealed credential leases, and interactive login protocols").

| Concern | Owner | Citation | Maturity |
|---|---|---|---|
| Wire-level OAuth primitives (discovery, PKCE, loopback callback, device grant, token and refresh grants, dynamic client registration) | `omp-oauth`, which "deliberately does not own credential selection, persistence, or leases" | `crates/oauth/src/lib.rs:1-4`, `device.rs` `begin_device_authorization`/`poll_device_token` (`lib.rs:23-26`) | Implemented |
| Provider auth protocol engines (PKCE, device, paste, refresh, custom per-provider exchanges) | `OAuthEngine` | `crates/ai/src/auth/oauth.rs:236` `OAuthEngine`; `begin_pkce:265`, `begin_device:387`, `poll_device:452`, `begin_paste:510`, `refresh:551`; custom handlers `OAuthCustomHandler:104` and `crates/ai/src/auth/oauth/custom/{anthropic,cursor,devin,gitlab,google_antigravity,openrouter,perplexity,zai,api_key}.rs` | Implemented |
| Encrypted durable store | `CredentialStore` (SQLite, WAL, `busy_timeout` 5 s, schema v4), table `credentials` plus `leases`, `credential_reveal_audit`, `credential_import_audit`, `credential_scoped_grants` | `crates/ai/src/auth/store.rs:416` `CredentialStore`, `:40` `SCHEMA_VERSION`, `:456` `journal_mode=WAL`, `:1612` `migrate` | Implemented |
| Account identity, eligibility, cooldown, quota, rotation, pins | `AccountPool` over `AccountStateStore` (same file `credentials.db`, second table set) | `crates/ai/src/account/pool.rs:356` `AccountPool`, `store.rs:131` `AccountStateStore`; both open `<data>/credentials.db` (`crates/driver/src/registry.rs:1370-1371`) | Implemented |
| Lease acquisition, refresh, rejection | `CredentialBroker` over typed sources; `RefreshingCredentialSource` adds refresh | `crates/ai/src/auth/broker.rs`, `manager.rs:1314` `RefreshingCredentialSource` | Implemented |
| Control plane (store, import, enable, block, reveal, scoped token, refresh, delete) | `AuthControlHandle` over the live `AuthManager` | `crates/ai/src/auth/manager.rs:185` `AuthControlHandle`, methods `:263 store`, `:321 import_oauth`, `:519 set_enabled`, `:625 mint_scoped_token`, `:644 refresh`, `:658 delete`, `:667 reveal` | Implemented |
| Operator CLI | `omp auth {login,list,status,refresh,logout,rename,audit}`, `omp token`, `omp auth-broker`, `omp auth-gateway` | `crates/app/src/cli.rs:2168` `AuthCommand`, `:2446` `AuthBrokerCommand`, `:2514` `AuthGatewayCommand`, `crates/app/src/token_cmd.rs:19` | Implemented |
| Remote credential authority | gRPC `omp.auth.v1.Auth` served by the daemon over a local socket, or TCP with a watched bearer-token file | `crates/proto/proto/omp/auth/v1/auth.proto:38-98`, `crates/serve/src/auth.rs:400` `impl Auth for AuthRpc`, `crates/app/src/daemon.rs:378,399,417,444` | Partial (section 3, A10) |

Masking of secrets in model-bound text is a different mechanism (`crates/secrets`, "closed secret-rule contract", `crates/secrets/README.md:3`; "Secret-key persistence lives in `omp-cache`"). It does not hold provider credentials and is out of this area except where noted.

## 2. Dimension audit

### A01 API keys: Implemented

- Catalog declares where a key may come from, in order: `CredentialSourceSpec::{Environment, BasicEnvironment, Interactive, Stored, ApplicationDefault, AwsChain}` (`crates/ai/src/auth/spec.rs:440`), and where it is placed (`KeyPlacement`, `HeaderPlacement`, `QueryPlacement`, `BodyPlacement`, `spec.rs:473-539`). `AuthSpec::{ApiKey,Basic,Bearer,...}` at `spec.rs:13`.
- Environment keys are ephemeral by construction: `CredentialOrigin::Environment` is refused by persistence (`store.rs:46`, `put_inner` returns `StoreError::EphemeralCredential` when `origin == Environment`, `store.rs:591-600`).
- Stored keys are encrypted with the kind authenticated with the ciphertext (`crypto.rs`, `store.rs:211` `CredentialWrite.kind`); ADR 0018 amendment 2026-10-08 describes the kind-repair rules, verified at `manager.rs:416` `repair_static_secret_kinds` and `driver/src/registry.rs` `repair_stored_secret_kinds`.
- Command-resolved secrets for custom provider headers: `!command` header values (`crates/ai/src/auth/apply.rs:26,33` `CommandHeaderSource::from_config`), single-flight process-lifetime cache with failure TTL (`crates/ai/src/auth/command.rs:54` `CommandCredentialResolver`), one-shot 401 retry (`apply.rs:45` `CommandHeaderRetry`). The executor must cross the Environment boundary (`command.rs:13` `CommandCredentialExecutor`; production impl `crates/driver/src/bridges.rs:239` `CommandCredentials`).
- Gap: a successful command credential is cached for the process lifetime with no TTL (`command.rs:54` doc), so a rotating short-lived command token is refreshed only after a 401 (`apply.rs:45`).

### A02 OAuth: Implemented

- Authorization code with S256 PKCE, loopback callback, paste-code and device flows are catalog data (`AuthSpec::{OAuthPkce,OAuthDevice,OAuthPaste,OAuthCustom}`, `spec.rs:35-43`; `OAuthFlowSpec::Pkce` at `crates/catalog/src/provider.rs:364`).
- Per-provider exchanges that do not fit the generic shapes are typed handlers registered by catalog enum, not extension code (`oauth.rs:104,136,147`, files under `auth/oauth/custom/`). This matches ADR 0049 D6 class B ("`ai/src/auth/**` (OAuth flows, including `github_copilot`, `gitlab`, `devin`) stays").
- Remote login: `omp auth-broker login <provider> --via <ssh-alias>` forwards the loopback callback port over the native SSH service and runs the login on the remote host (`crates/app/src/auth_broker_cmd.rs:70,106-133`). The broker proto states the browser step happens on the client host and the exchange broker-side (`auth.proto:44-52`, unverified end to end, see 6.9).
- MCP OAuth is separate (`omp_envd::mcp::oauth::McpOAuth`, constructed at `crates/driver/src/registry.rs` after `auth_control`); not audited here.

### A03 Device authorization: Implemented

- RFC 8628 device grant: `DEVICE_GRANT` (`oauth.rs:54`), `OAuthEngine::begin_device:387` and `poll_device:452`, primitives `crates/oauth/src/device.rs`, catalog shape `OAuthFlowSpec::DeviceCode` (`provider.rs:~375`, field `device_authorization_url`). Which bundled providers use it was not enumerated (6.5).

### A04 Official CLI delegation: Partial (ADR 0043 status "Partially implemented")

- Rule: OMP spawns the vendor CLI and "leaves authentication to it"; it "never reads, copies or forwards another application's tokens, and it never drives a login flow" (`docs/adr/0043-native-cli-integration.md:130`, boundaries at `:15-19`).
- Code: dsh, Devin and Grok profiles are registered only when the operator names a program convar (`crates/driver/src/delegated_acp.rs:47,73,82`; "Every program convar defaults to empty", module doc `:1-40`). A parent provider key is stripped from the child environment (test `dsh_environment_cannot_be_loosened_by_the_parent`, `delegated_acp.rs:~259-280`: `DEEPSEEK_API_KEY` becomes `None`). An unauthenticated agent maps to a typed `AuthRequired` (`crates/envd/src/acp_client/delegated_session.rs:612,790`, test `tests.rs:913,965,996`).
- Codex adapter is `crates/envd/src/codex_app_server/` per STATUS; not read in this audit (6.6). ADR 0043 states login is reported `Unknown` (`0043:86`).
- Gap: there is no credential-health probe for delegated CLIs and no way to show "logged in" in `omp auth status`; the CLI's own entitlement is invisible to OMP (A11, O02).

### A05 Credential references (secret never inline): Implemented for providers; Partial for extensions

- Catalog data holds only names and placements, never values: environment variable names (`CredentialSourceSpec::Environment.variables`, `spec.rs:444`), stored-profile selectors (`Stored.profile`, `spec.rs:456`).
- Runtime values are sealed: `CredentialLease` is opaque with redacted `Debug` (`crates/ai/src/auth/lease.rs:155,496`), applied at the innermost request mutation (`lease.rs:441 apply`, `:458 apply_streaming`), secrets travel as `SecretString` (`omp_core`).
- Extensions name a provider id, never a value (ADR 0075 D2, BuildBuddy: `extensions/buildbuddy/src/omp_buildbuddy/client.py:7,67,90` `credential=_PROVIDER`, `devices.py:22,72` "an `api_key` in `args` is not read"). The host binds placement at composition: `crates/driver/src/headless/kernel.rs:2849` `bind_placement(...)`, `crates/driver/src/placement_credentials.rs:90`. The enforcement details (`PlacementAuthority`, `HttpEgressHost::begin_placement`, per-hop grant re-check, `<data>/placement-audit.jsonl`) are ADR 0075 claims whose call site I verified but whose bodies I did not read (6.7).
- The older extension path that read the key itself is gone from the BuildBuddy extension (grep of `extensions/buildbuddy/src` for `find-generic-password|BUILDBUDDY_API_KEY` returned no match).

### A06 OS keychain: Partial (macOS only; keychain holds the master key, not provider credentials)

- What the keychain stores: a 32-byte master encryption key with an active-key pointer, service `dev.omp.llm`, account `credential-store-master` (`crates/driver/src/registry.rs:125-126`; `key.rs:416-470` `OsCredentialKeySource::rotate`, items `<account>:active` and `<account>:key:<id>`). Provider secrets stay in `credentials.db` (A01).
- Platforms: macOS only via `security_framework` (`key.rs:431-440`, `#[cfg(target_os = "macos")]`); every other target returns `KeyError::Unavailable` and "never falls back to plaintext or a local file" (`key.rs:416` doc). Linux Secret Service and Windows Credential Manager: Absent (searched `key.rs` for `keyring`, `secret-service`, `windows`; only the macOS branch exists).
- Key source selection: `sv_credential_key_source` (`auto|unavailable|local-file|os-keychain`, `registry.rs:100-123`) and `OMP_LLM_KEY_SOURCE` (`registry.rs:124`). `auto` resolves to an owner-only key file `credentials.key` beside the database when stdin and stderr are terminals, else `Unavailable` (fails closed) (`registry.rs:212-250` `CredentialKeyMode::from_configuration`/`resolve`). The file source uses `O_NOFOLLOW`, `0600`, create-new (`key.rs:230-345`). Its doc states it does not protect against an attacker who can read the data directory.
- Migration: on macOS the local-file mode keeps the keychain as a read-only historical fallback and re-encrypts once (`registry.rs:~300-318` `FallbackKeySource` plus `store.rotate_keys()`, `store.rs:1077`).
- Refusal handling: only `errSecItemNotFound` (-25300) is "missing"; every other status is `KeyError::OsCredential` and surfaces as `CredentialStorageLock::KeychainRefused` (`key.rs:473-486`, `store.rs:1332-1340`).
- Observation: `os-keychain` mode runs `if key_source.active_key().is_err() { key_source.rotate()?; }` (`registry.rs:283-287`). A read that fails for any reason, including a refused read, tries to provision a new active key. Under a locked keychain the write fails too, so the launch fails as ADR 0018 says; behaviour for a keychain that refuses reads but accepts writes was not tested (6.4).
- Containment: the exec sandbox denies the credential store files (`credentials.db`, `-wal`, `-shm`, `-journal`, `credentials.key`) to reads and writes: `crates/envd/src/exec_settings/sandbox.rs:546-549`. ADR 0075 documents macOS Seatbelt proofs only.

### A07 Refresh coordination: Partial (the cross-process fence is ineffective in the production composition)

What exists:

- In-process single flight per account: `static FLIGHTS` (`crates/ai/src/account/refresh.rs:381`), leader and waiters (`refresh.rs:458-509`). It is a process-wide static, so every subagent kernel composed in the same process shares it.
- Persistent lease protocol: `RefreshLeaseStore` (`refresh.rs:161`), implemented by `CredentialStore` (`store.rs:1412`) on the `leases(account_id, kind, owner, epoch, expires_at_ms)` table (`store.rs:~1640`). Defaults: lease TTL 30 s, renew 10 s, up to 4 peer handoffs (`refresh.rs:73-80`). A waiter polls the store every 25 ms for a newer generation (`store.rs:1444-1493` `wait_for_newer`).
- Write fencing: an OAuth write under a refresh lease re-checks `owner AND epoch AND expires_at_ms > now` inside an immediate transaction (`store.rs:591-630` `put_inner`) and also compare-and-swaps `expected_generation` (`GenerationConflict`, `store.rs:~650`).
- Rejection-driven: a provider 401 rejects the exact generation; refresh preserves account and principal (`refresh.rs:49` `RefreshRequest`, `oauth.rs:814-890` `refresh_persisted_with`: checks generation and principal before refreshing).

The defect (code fact plus inference):

- Fact: the production coordinator is built with a constant owner string, `RefreshCoordinator::new("omp-auth-refresh", RefreshPolicy::default())` (`crates/driver/src/registry.rs:1381`). The only other constructions are tests with distinct or single owners (`store.rs:2284` uses `process-a`; `crates/ai/tests/account.rs:664` uses `process`; `provider/builtin.rs:4837` test).
- Fact: `try_acquire_lease` returns `Held` only when `held_owner != owner` (`store.rs:1196-1199`). Two processes using the same data directory both present `omp-auth-refresh`, so the second acquisition succeeds and bumps the epoch (`store.rs:1206-1213`) instead of observing `HeldByPeer`. The first holder's later write then fails the `epoch` check with `StoreError::LeaseLost` (`store.rs:~613-630`).
- `[INFERENCE]` Consequence for providers that rotate refresh tokens on use: two concurrent processes (two `omp` sessions, a daemon plus a CLI, or a worker plus its parent) can each call the provider refresh endpoint with the same old refresh token; one wins the write, the other is discarded by the fence, and the provider may have invalidated the token the winner needs. Not tested. Within one process the `FLIGHTS` static prevents this. The `RefreshLeaseRequest.owner` doc calls the owner a "Stable process owner token" (`refresh.rs:102`), which a constant is not.
- Measurement plan (not measured): a two-process harness using a stub token endpoint that rotates refresh tokens and counts calls; run `N=2..8` processes against one temp `credentials.db` with an expiring account; compare refresh calls per expiry with owner constant versus owner `pid:ulid`. Run on the remote build host, not locally (AGENTS.md remote-build rule).

Further gaps:

- Refresh is lazy, on acquisition or on a 401 (`manager.rs:1395-1456`); there is no background keeper that renews before use. `CredentialFreshness::needs_refresh` (`refresh.rs:35`) has no caller (grep over `crates/ai/src`), so the documented "proactive refresh" is only the 60 s skew applied at lease time (`manager.rs:1322`).
- Each account pool holds hydrated state in memory (see section 4), so a rotated generation written by another process is re-read only on the next lease, not pushed.

### A08 Credential expiration handling: Implemented (typed), with one inert helper

- A lease is expired when `expires_at <= valid_after` (`store.rs:1323-1366` `lease_now`, `CredentialError::Expired`); `RefreshingCredentialSource` applies a 60 s pre-expiry skew and replays acquisition once after a refresh (`manager.rs:1314-1450`).
- Failures are typed: `CredentialFailure` values `expired`, `not_renewable`, `refresh_failed`, `storage_locked`, `kind_mismatch`, `no_source`, `no_rotation_candidate`, `unusable_stored_credential`, `source_failed` (ADR 0018 amendment; `crates/ai/src/error.rs` `ErrorDetail::Credential`). A static key or a provider without OAuth answers `not_renewable` (`manager.rs:1568-1580`).
- An extension-owned refresh (`provider_refresh` hook) that fails disables the account (`manager.rs:1550` `accounts.set_enabled(&account, false)`) and emits `credential_disabled`; the built-in OAuth path does not disable on failure (`manager.rs:1580-1660`), so a dead refresh token stays enabled and fails on every lease. Inconsistent between the two paths.
- AWS shared credentials have their own skew (`AWS_REFRESH_SKEW` 60 s, `aws.rs:46`).

### A09 Credential recovery: Partial

- Exists: `omp auth login|refresh|logout|status|rename|audit` (`cli.rs:2168-2224`), `omp auth-broker migrate` (re-encrypt under the active key, `cli.rs:2491`, `store.rs:1077 rotate_keys`), `backup_metadata` (`store.rs:1265`), kind repair (`manager.rs:416`), typed storage-lock remedies naming `OMP_LLM_KEY_SOURCE=local-file` or the keychain (`error.rs:304-311`), early refusal in `omp print` (`print_mode.rs`, `registry.rs:363 ensure_stored_logins_unlockable`), import from v1 `agent.db` and `models.yml` (`crates/driver/src/v1_import/`) and from CLIProxyAPI credential exports (`auth_broker_cmd.rs:30-56 CliProxyCredential`, `:81 import`, dry-run supported).
- Absent: automatic re-login prompt when a refresh token is dead (proto state `STATE_EXPIRED` says "needs re-login", `auth.proto:~125`, but nothing starts the flow); encrypted-store backup of secrets (only `backup_metadata`); recovery from a lost master key (rows sealed under a missing key are not recoverable, ADR 0018 amendment `NotRestored`).

### A10 Independent credential keepers: Partial (a keeper process exists; production use by subagents and extensions is not wired)

- `omp auth-broker serve --endpoint <uds|tcp>` starts the daemon that registers `Gateway`, `ForwardProxy`, `Inference`, `Blob` and `Auth` services (`auth_broker_cmd.rs:63-68`, `daemon.rs:378,399,444`). On a UDS it is owner-only; on TCP it requires a watched bearer-token file and refuses to start without one (`daemon.rs:417` `UnauthenticatedTcp`, `daemon.rs:221-260` `BearerAuth`, `load_gateway_token`). The token file is `auth-broker.token` written owner-only (`auth_broker_cmd.rs:89-104`). ADR 0039 records there is no TLS on TCP (`0039:24`); I read `auth_gateway_cmd.rs:39` printing `http://{bind}` but did not verify that `omp_rpc::tls` has no caller (6.8).
- `omp auth-gateway serve` is a credential-injecting forward proxy for OpenAI-compatible clients (`crates/app/src/auth_gateway_cmd.rs:26-80`; `crates/chat/src/tips.txt:14`).
- The client side: `--gateway` (`cli.rs:1871` `gateway: Option<LocalEndpoint>`) makes a kernel use `GatewayInference` and compose no local credential stack (`crates/driver/src/headless/kernel.rs:2780-2825`, `Err(channel)` arm). The Python extension credential client has a remote variant, `GatewayCredentialSecretControlFactory` (`crates/driver/src/auth_backend.rs:633`) but `gateway_credential_control_factory` is called only from tests (`crates/driver/tests/gateway_credential_authority.rs:52`): Prototype.
- Not a keeper: no process renews tokens ahead of use. The daemon refreshes lazily like any other stack (A07). No health endpoint for keepers beyond `Gateway.Hello` and the `Auth` RPCs (O02).
- `crates/ai/src/auth/attribution.rs` carries stable client identity "only across OMP gateway and broker transports" (`attribution.rs:1`, `ClientUsageIdentity`), so a keeper can meter clients (not otherwise audited).

### A11 Authorized subscription entitlements: Partial

- Account usage and plan data exist as operations: `Usage` in `OperationKind` (`crates/catalog/src/capability.rs`, ADR 0018 status), `GetUsage`/`GetUsageHistory`/`GetClientUsage`/`MarkUsageStale` RPCs (`auth.proto:86-92`), `omp usage` and `omp dry-balance` commands (`crates/app/src/usage_cmd.rs:31`, `dry_balance_cmd.rs:39`), Copilot plan-endpoint discovery (`crates/ai/src/auth/github_copilot.rs`, `discover_copilot_api_endpoint`, `auth/mod.rs:55-60`), Codex plan claim parsing (`oauth.rs:55` `CODEX_AUTH_CLAIM`).
- Model-level entitlement is learned after the fact: a provider denial of exactly one model becomes `CooldownReason::ModelPolicy` (`pool.rs:~76`), and quota windows gate selection (`QuotaExhausted`, `QuotaReserved`, `pool.rs:86-132`).
- Absent: a declared per-account entitlement set (plan, allowed models) consulted before the first request; entitlements are not fetched at login. Whether usage fetchers exist for every OAuth provider was not enumerated (6.3).
- Provider-terms boundary: ADR 0043 states Google's and Anthropic's terms restrict wrapping their CLIs or reusing their login and records an owner decision to accept the residual risk for ACP only; OMP never extracts another application's tokens (`0043:146-149`). The import path for CLIProxyAPI credential files (`auth_broker_cmd.rs:81`) moves tokens the user already holds into OMP's store; whether any provider permits that reuse is a terms question for the owner, not answered by code (section 6.10).

### A12 Account and regional restrictions: Partial (encoded in endpoints and credentials, not in eligibility)

- Region and residency appear as provider-specific data: Codex data and compute residency claims (`oauth.rs:56-57`), Alibaba Token Plan CN base URL (`auth/mod.rs:28 ALIBABA_TOKEN_PLAN_CN_BASE_URL`), Copilot enterprise domains and public-host list (`auth/mod.rs:56-59`), AWS region chain (`aws.rs`). Consumption of the Codex claims was not traced (6.3).
- Absent as an eligibility concept: `Eligibility` has no region or jurisdiction variant (`pool.rs:86-132`); selection filters by route support (`RouteIneligible`), health, quota, rotation and pin only. A route that a region blocks fails at the provider and is learned from the failure.

### G09 Authentication requirements declared per provider: Implemented

- `AuthSpecKind` (`crates/catalog/src/provider.rs:95`): `None, ApiKey, Basic, Bearer, OptionalBearer, Oauth, AwsSigv4, GcpAdc, AzureAd, GithubApp, OmpSession`; per-route `AuthSpec` (`spec.rs:13`); `provider_auth_specs` lists exactly what every route of a provider leases (`broker.rs`, ADR 0018). `/login` refuses a stored kind no route leases (`ErrorDetail::LoginKindNotLeased`, ADR 0018). Providers-as-data, lintx-enforced (AGENTS.md Locked Deviations, "Inference").

### G10 Free-tier and anonymous discovery: Partial

- Anonymous operation is expressible: `AuthSpecKind::None` and `OptionalBearer` ("permits anonymous requests when no credential is available", `provider.rs:104`); the locked-store early check treats anonymous authentications as usable without the store (`registry.rs:363-380`).
- Absent as data: no free-tier or quota-class flag on providers or models (grep of `crates/catalog/src/provider.rs` for `free_tier|free-tier|anonymous` found only the `OptionalBearer` doc; the `crates/catalog/compat/providers` path returned no match, possibly because it uses another extension or location). Discovery refresh runs at composition (`kernel.rs:~2780` comment) using whichever credential leases, with a 2 h cache TTL (`registry.rs` `MODEL_DISCOVERY_CACHE_TTL`).

### G11 Connection identity and deduplication: Partial

- Identity is typed and secret-free: `AccountId`, `PrincipalId` (`crates/ai/src/id.rs`), `CredentialMetadata.principal_id` (`store.rs:55-59`), accounts bound to one provider and one principal; a registration that rebinds either is refused (`pool.rs:440-449` `ProviderRebind`, principal mismatch), an OAuth refresh that changes principal fails (`RefreshErrorKind::PrincipalChanged`, `refresh.rs:337`).
- Journal-safe affinity: pins and affinity record an opaque digest, never id or name (`crates/ai/src/account/pin.rs:1-12`, `manager.rs:711-780` `CredentialAffinityResolver`); names are unique per provider (`account/name.rs`).
- Quota sharing scope: a catalog enum documents "the scope at which one authenticated principal and its quota are shared" (`provider.rs:~125`), used by `AccountSelectionRequest.quota_scope` (`pool.rs:~262`).
- Not verified: whether two logins of the same upstream identity (same principal, two accounts) are deduplicated or both rotate (6.11). Connection-level deduplication of one HTTP connection per credential is a provider-runtime concern (E08, other agent).

### S02 Credential eligibility: Implemented (selection belongs to the routing audit)

- `Eligibility` (12 variants: `Eligible, Disabled, RouteIneligible, CredentialRejected, Cooldown, RateLimited, QuotaExhausted, QuotaReserved, RotationForbidden, PrincipalMismatch, PreviousAccount, NotPinned`, `pool.rs:86-132`), `CandidateEvidence`/`SelectionReceipt` (`pool.rs:158-199`), exclusive pins that never fall back (`AccountPin`, `pool.rs:232`), `RotationPolicy` (`pool.rs:212`).

### O02 Authentication health: Partial

- Present: per-credential state `ACTIVE|EXPIRED|BLOCKED|DISABLED` with `blocks` and `expires_at_ms` (`auth.proto:~108-140`), `WatchCredentials` resumable stream (`auth.proto:43`), `ProbeCredentials` (`auth.proto:95`, `serve/auth.rs:986 probe_credentials`), `omp auth status` (`cli.rs:2197`), `credential_health` helper imported by the serve tests (`serve/auth.rs:1441` import list; definition not read).
- Absent: no metric export of refresh outcomes or lease contention (the `RefreshReceipt.steps` timeline exists, `refresh.rs:268-281`, but nothing in `crates/observability` was verified to consume it, 6.12); no keeper heartbeat; no per-extension credential usage report beyond the audit tables (`credential_reveal_audit`, `credential_import_audit`, `credential_scoped_grants`, `store.rs:1650-1700`, `omp auth audit`).

## 3. What a subagent or swarm worker inherits

Verified paths, by worker kind:

| Worker kind | Credential path | Duplicates the credential manager? | Evidence |
|---|---|---|---|
| In-process subagent (`task` tool) | `spawn_child` calls `compose_kernel(data_dir, run_root, model, parent ctx, options)` with `KernelOptions::default()` plus session fields (`gateway: None`, `api_key: None`), so the kernel takes the `None` arm and calls `production_inference_for_session` | Yes. Each child opens its own `CredentialStore` connection (`registry.rs:1200`), `AccountPool` plus `AccountStateStore::open` on the same file, a new `RefreshCoordinator("omp-auth-refresh")` (`registry.rs:1371-1381`), and refreshes model discovery while composing | `crates/driver/src/subagent/spawn.rs:726-738,932-939`; `crates/driver/src/headless/kernel.rs:2780-2799`; `registry.rs:1189-1262` |
| Same, what is shared | The parent convar context (so the key-source mode and `sv_*` settings), `data_dir`, the process-wide `FLIGHTS` single-flight map, the SQLite file. No `AuthManager`, pool, or lease object is shared | | `spawn.rs:932-936` `Arc::clone(&prepared.ctx)`; `refresh.rs:381` |
| Account pin | Not seeded: the composed routes use `SessionAccountPins::NONE` | `[INFERENCE]` a parent's `/pin` does not reach a child; ADR 0013 seeding may cover it through the journaled convar, not checked | `kernel.rs:~2866`; `pin.rs:1-12` |
| Gateway client (`omp chat --gateway`) | No local stack; all inference through one daemon that holds credentials | No | `cli.rs:1871`; `kernel.rs:2780-2825` `Err(channel)` arm |
| Extension worker (Python) | No raw key. HTTP placement by provider id through the host (live); `omp.creds.*` CONTROL domain unbound in production | No (the host owns the stack) | `kernel.rs:2849`; `crates/driver/src/secrets/mod.rs:32,70` (`credential_secret_control_factory` has no production caller: grep over `crates/driver/src/headless`, `crates/app/src`, `crates/envd/src`, `crates/serve/src`, `crates/e2e`) |
| Separate OS process (headless `omp -p`, remote worker) | Opens `<data>/credentials.db` itself. `auto` key source needs a terminal on stdin and stderr, else `Unavailable`, so an unattended worker needs `OMP_LLM_KEY_SOURCE=local-file` or `os-keychain` | Yes (full stack per process) | `registry.rs:212-250`; error text `error.rs:304-311` |
| Delegated native CLI (Codex, dsh, Devin, Grok) | The CLI's own login; OMP passes no OMP credential | No OMP credential involved | `delegated_acp.rs:47-82`, ADR 0043 |

Consequences for the owner's rule "do not duplicate credential managers":

1. A swarm of N in-process subagents today builds N credential-manager stacks in one process. The shared SQLite file and the `FLIGHTS` static keep them correct but not cheap; the cost per child (SQLite open, pool hydration, discovery refresh) was not measured (6.1).
2. Cross-process correctness depends on the lease owner token (A07), which is a constant.
3. The only existing mechanism that gives a worker a credential without a local manager is a gateway or broker connection (A10). It is a Prototype for extension control and Partial for inference.

## 4. Persistent versus in-memory state

- Persistent (SQLite `credentials.db`, key in `credentials.key` or the macOS keychain): encrypted secrets, metadata and generation, leases, reveal, import and scoped-grant audit rows (`store.rs:1612-1700`), account records, rejections, cooldowns, rate and quota windows, affinity, names (`account/store.rs:131-790`).
- In memory per stack: `AccountPool` state hydrated once at open (`pool.rs:385-396 with_store`), mutated through `upsert`, `set_enabled`, `cooldown` which write through to the store. No code path was found that re-reads other writers' cooldown or rate state into an open pool (grep for `load_account`/`load_accounts` in `pool.rs` found only `with_store:387-389` and `upsert:457`), so siblings in one process or across processes observe each other's cooldowns only after re-hydration (`[INFERENCE]`, not tested).
- Secrets in memory: `SecretBox`, `SecretString`, zeroize on drop (`store.rs:193-207`, `lease.rs:127-153`).

## 5. ADR and documentation divergences

| # | Text | Code | Effect |
|---|---|---|---|
| X1 | ADR 0018 amendment and `refresh.rs:1`: "persistent lease coordination" across processes | The production owner token is the constant `"omp-auth-refresh"` (`registry.rs:1381`) and `try_acquire_lease` fences only different owners (`store.rs:1196-1199`) | Cross-process refresh exclusion is not effective in production; tests use distinct owners. ADR 0018 status "Implemented" overstates this row |
| X2 | `crates/py/python/omp/creds.py:1-6`: operations restricted "to providers named by the extension's `credentials.allow` declaration" | `credential_control_grants` reads only host grants and never the manifest (`crates/driver/src/secrets/mod.rs:19-48`), per ADR 0075 | Docstring is stale; also the docs page `docs/pyx/reference/omp.creds.md` should be checked |
| X3 | ADR 0018 decision 1 "an extension NEVER holds or refreshes a provider credential itself"; the status section marks it "Unverified" | `omp.creds.reveal` and `import_oauth` exist in the CONTROL authority (`auth_backend.rs:163-180`) but their grant scopes are empty by construction (`secrets/mod.rs:43-45`) and the domain is unbound in production | Consistent today. Keep an e2e check so that wiring the domain cannot grant `reveal` |
| X4 | ADR 0049 D6 class A: vendor integration never enters T0/T1 | `GithubCredentialAuthority` and `github_authority` live in `crates/driver/src/auth_backend.rs:66-95` (adapter to `omp_envd::forge_url::CredentialAuthority`) | A vendor name in a core crate; it acts only as a credential adapter. Flag for the ADR 0049 owner; not asserted as a violation |
| X5 | STATUS row 0043 cites `crates/agent/src/delegated/` | The contract lives in `crates/contract/src/delegated` (parent record D2) | Citation refresh only |
| X6 | ADR 0039 row: TCP gateway has no TLS | `daemon.rs:417` requires a bearer token; `auth_gateway_cmd.rs:39` prints `http://` | Consistent for the part I read; TLS absence not re-verified (6.8) |

## 6. Not verified

1. Per-child cost of the duplicated stack (SQLite open, pool hydration, discovery refresh) and whether `production_assembly_with_catalog` memoizes anything by `data_dir`.
2. Behaviour of two live processes refreshing the same account (A07). The conclusion rests on code at `registry.rs:1381` and `store.rs:1196-1207`; no test ran.
3. Which providers have usage fetchers, which parse plan or residency claims, and where `CODEX_*_RESIDENCY_CLAIM` is consumed.
4. `os-keychain` mode when the keychain refuses reads but accepts writes (`registry.rs:283-287`).
5. Which bundled catalog providers use device flow, paste flow, or custom exchanges (catalog KDL not enumerated).
6. The Codex adapter (`crates/envd/src/codex_app_server/`) and ACP launch-profile environment filtering beyond the dsh test; STATUS says no credential extraction, I did not read the child-environment code (`crates/envd/src/child_env.rs`).
7. The bodies of `PlacementAuthority`, `HttpEgressHost::begin_placement` and the placement audit file (ADR 0075 claims).
8. Whether `omp_rpc::tls` has any caller; whether the TCP bearer token is compared in constant time.
9. End-to-end remote login through the SSH forward (`auth_broker_cmd.rs:106`).
10. Provider terms for any import path (`auth_broker_cmd.rs:81`): code cannot answer this.
11. Deduplication of two accounts for one upstream principal.
12. Whether any observability crate consumes `RefreshReceipt` or account-pool events.
13. Windows and Linux behaviour of every keychain and sandbox claim (ADR 0075 verified macOS Seatbelt only).

## 7. Candidate gaps carried to stage 2 (not cards yet)

Derived from sections 2 to 4, in the order they affect other areas: (a) per-process refresh owner token; (b) one shared auth stack across in-process subagents; (c) refresh-on-expiry keeper behaviour inside an existing broker rather than a new service; (d) unify disable-on-refresh-failure between the extension and built-in paths; (e) health surface for refresh and lease events through existing observability; (f) entitlement and region facts as catalog data feeding existing `Eligibility`; (g) platform keychain parity for key sources; (h) ADR 0049 D4 conformance for any credential-bearing extension proposal.

Back to the [index](../README.md).
