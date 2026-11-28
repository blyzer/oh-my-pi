# Authentication and credential lifecycle: capability cards

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 5 (area AUTH). Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Evidence: [`evidence/omp2-auth-credentials.md`](evidence/omp2-auth-credentials.md) (code audit, numbered not-verified list) and [`evidence/omp2-auth-credentials.tsv`](evidence/omp2-auth-credentials.tsv) (matrix rows A01-A12, G09-G11, S02, O02). Diagram: [`diagrams/08-authentication-and-credential-lifecycle.md`](diagrams/08-authentication-and-credential-lifecycle.md).

Constraints every card respects: AGENTS.md "Locked Deviations from pi" (providers-as-data, no vendor server-side tools, feature graphs earn their weight), ADR 0018 decision 1 (credential refresh and leasing are engine-owned), ADR 0049 D1/D4/D6 (vendor integration code never enters T0/T1; raw provider tokens stay in core; security is never traded for lightness), ADR 0075 (host places extension credentials). No card adds a second credential manager, scheduler or config store.

## 1. Findings that drive the cards

| # | Finding | Evidence (audit file section) |
|---|---|---|
| F1 | The production refresh coordinator uses one constant owner token, so the SQLite cross-process lease fence cannot report a peer (`crates/driver/src/registry.rs:1381`, `crates/ai/src/auth/store.rs:1196`) | A07 |
| F2 | Each in-process subagent composes its own credential-manager stack (`crates/driver/src/subagent/spawn.rs:726-738,932`, `crates/driver/src/headless/kernel.rs:2780-2799`) | section 3 |
| F3 | The extension credential CONTROL domain (`omp.creds.*`) has no production binding (`crates/driver/src/secrets/mod.rs:70`); only host placement is live | A05 |
| F4 | The OS keychain protects only the master key and only on macOS (`crates/ai/src/auth/key.rs:416`) | A06 |
| F5 | Refresh is lazy; `CredentialFreshness::needs_refresh` has no caller; no keeper renews before use (`crates/ai/src/account/refresh.rs:35`, `crates/ai/src/auth/manager.rs:1395`) | A07, A10 |
| F6 | `omp auth-broker import` loads CLIProxyAPI credential exports into the store (`crates/app/src/auth_broker_cmd.rs:81`) while ADR 0043 lists "No credential migration or extraction" (`docs/adr/0043-native-cli-integration.md:34`) | A11 |
| F7 | A failed extension-owned refresh disables the account, a failed built-in refresh does not (`crates/ai/src/auth/manager.rs:1550`) | A08 |
| F8 | Entitlement and region are learned from a failed request (`CooldownReason::ModelPolicy`, `crates/ai/src/account/pool.rs:76`) | A11, A12 |

## 2. What a subagent or swarm worker inherits (summary)

An in-process subagent inherits the parent's convar context (including the credential key-source mode), the data directory and the process-wide single-flight map; it does not inherit an auth manager, account pool or lease object, and it builds its own. A gateway client (`--gateway`) holds no credentials locally. An extension worker holds no raw key; the host places a header by provider id. A separate OS process opens `credentials.db` itself and, unattended, needs `OMP_LLM_KEY_SOURCE=local-file` or `os-keychain`. Details and citations: audit file section 3.

## 3. Capability cards

### AUTH-01 Per-process refresh-owner token for the persistent lease
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** persistent lease table and fencing exist: `crates/ai/src/auth/store.rs:1168` `try_acquire_lease`, `:591` `put_inner` (owner and epoch re-checked in the write transaction), `crates/ai/src/account/refresh.rs:161` `RefreshLeaseStore` (Implemented). The production owner is the literal `"omp-auth-refresh"` (`crates/driver/src/registry.rs:1381`); `try_acquire_lease` yields `Held` only for a different owner (`store.rs:1196-1199`), so the fence is inert across processes (Partial).
**Actual missing capability:** a stable per-process owner (for example pid plus a generated ULID, never credential material) passed to `RefreshCoordinator::new`, and a two-process test that proves a second process sees `HeldByPeer` and waits for the published generation.
**Alternatives considered:** (a) file lock per account: a second locking mechanism beside SQLite, rejected; (b) rely on the compare-and-swap of `expected_generation` alone: it protects the stored row but not the provider's single-use refresh token, which has already been spent by the loser (inference, not measured); (c) move refresh into one broker daemon: larger change, see AUTH-05.
**Expected benefit:** reliability (no lost refresh tokens when two OMP processes share a data directory), fewer billable-nothing but account-breaking re-logins, reduced network transmission (one provider refresh call instead of N).
**Implementation complexity:** low. One argument in one composition site, plus a test using the existing `SharedRefreshStore` support (`crates/ai/tests/support/refresh.rs:31`).
**Security implications:** the owner token is not secret; it must not derive from credentials. A crashed owner is recovered by lease expiry (30 s default, `refresh.rs:73-80`). No new attack surface.
**Maintenance cost:** low.
**Performance measurement strategy:** two to eight processes against one temp `credentials.db` and a stub token endpoint that rotates refresh tokens and counts calls; compare provider refresh calls per expiry for constant versus per-process owner. Run on the remote build host (AGENTS.md remote-build rule). Not measured here.
**Acceptance criteria:** with N processes and one expiring account, exactly one provider refresh call is made per expiry; every process ends holding the same new generation; killing the leader mid-refresh lets a peer take over after lease expiry; the stub never sees a reused refresh token.
**Follow-up:** amend ADR 0018 status text (section 4).

### AUTH-02 Decide the fate of the unbound extension credential CONTROL domain (`omp.creds.*`)
**Decision class:** Requires ADR reconsideration
**Existing implementation evidence:** the domain is implemented but unbound: `crates/driver/src/auth_backend.rs:104` `CredentialSecretControlFactory` and `:163` `handles("omp.creds.*")`; `crates/driver/src/secrets/mod.rs:70` `credential_secret_control_factory` has no production caller; grants lower only host-held `allow`, with `import` and `reveal` empty (`secrets/mod.rs:32-48`); the Python surface `crates/py/python/omp/creds.py:184-301` still exists and its docstring is stale (`creds.py:1-6`). Host placement by provider id is live (`crates/driver/src/headless/kernel.rs:2849`) (Partial).
**Actual missing capability:** an explicit decision. Either bind only a narrow metadata subset (`list`, `usage`, `report_block`) for user-layer extensions with a host grant, or delete the unbound code and the Python surface (clean cutover, no shim).
**Alternatives considered:** keep as is (dead code with a security-sensitive shape: `reveal` and `import_oauth` exist); bind everything (contradicts ADR 0075 D9/O5 and ADR 0049 D4 "extensions get only `mint_scoped`" unless the owner amends them); remove placement and use reveal (contradicts ADR 0075 D1).
**Expected benefit:** security (no dormant reveal path), maintenance (one credential path for extensions), documentation correctness.
**Implementation complexity:** low to remove; medium to bind a subset (grant source, e2e proof).
**Security implications:** binding `reveal` or `import_oauth` would let an extension obtain or inject raw credentials and would contradict ADR 0049 D4; any binding must keep the grant host-held and project-layer extensions at no grant (ADR 0075 D13).
**Maintenance cost:** low after removal; medium if bound.
**Performance measurement strategy:** not applicable.
**Acceptance criteria:** a decision recorded in ADR 0075; if removed, no `omp.creds` symbol in Rust or Python and `docs/pyx` updated; if bound, an e2e test proves an ungranted extension gets `PermissionError`, `reveal` and `import_oauth` stay refused, and the audit tables record the calls.
**Follow-up:** the ADR 0075 amendment in section 4 carries this decision.

### AUTH-03 Share one auth stack across in-process subagents
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** `spawn_child` builds `KernelOptions::default()` plus session fields (`crates/driver/src/subagent/spawn.rs:726-738`), so `compose_kernel` takes the `None` arm and calls `production_inference_for_session` per child (`crates/driver/src/headless/kernel.rs:2780-2799`, `crates/driver/src/registry.rs:1189-1262`): own `CredentialStore` connection, `AccountPool` hydrated once (`crates/ai/src/account/pool.rs:385-396`), `RefreshCoordinator`, discovery refresh (Implemented per child; not shared). A kernel that holds no local stack already exists for `--gateway` (`kernel.rs` `Err(channel)` arm, `crates/app/src/cli.rs:1871`) (Implemented).
**Actual missing capability:** a `KernelOptions` field carrying the parent's existing production stack handle (or its `AuthControlHandle` plus registry) so children reuse the parent's pool, refresh engine and credential store instead of composing a new one. No new manager type.
**Alternatives considered:** (a) route children through a loopback gateway channel (`gateway: Some`): reuses an existing seam but adds serialization and a daemon dependency to every child; (b) keep per-child stacks and rely on SQLite: correct only up to AUTH-01 and leaves pool state (cooldowns, rate and quota windows) per child because the pool never re-reads other writers (`pool.rs:387-389,457`; inference); (c) process-global singleton: hides ownership, conflicts with the session-scoped overrides (`InferenceSessionOverrides`).
**Expected benefit:** memory and latency (no per-child SQLite open, pool hydration and discovery refresh; not measured), reliability (one pool sees one set of cooldowns), fewer duplicated discovery requests.
**Implementation complexity:** medium. Touches `KernelOptions`, `compose_kernel` and the subagent spawn path; session-scoped overrides (provider, invocation key, catalog) must still force a private stack.
**Security implications:** child gains the parent's account set; the child must not widen `AccountPin` or the key-source mode (it inherits the parent's convars already); sandbox rules protecting `credentials.db` are unchanged.
**Maintenance cost:** low (removes duplicated composition).
**Performance measurement strategy:** spawn 1, 4, 16 children with a no-op prompt on a stub provider; record wall time to first request, RSS and number of SQLite opens and discovery fetches for `gateway: None` versus a shared handle; record `uptime`. Not measured.
**Acceptance criteria:** with a shared handle, 16 children cause one credential-store open and one discovery refresh in the process; a cooldown recorded by child A is visible to child B on its next selection; a child launched with a provider and invocation key override still uses a private stack.
**Follow-up:** benchmark before merge; coordinate with the swarm area (SWM) because it owns the subagent spawn path.

### AUTH-04 OS credential facility for the master key on Linux and Windows
**Decision class:** Missing and justified: propose
**Existing implementation evidence:** macOS keychain key source `crates/ai/src/auth/key.rs:416` `OsCredentialKeySource`; every other target returns `KeyError::Unavailable` (`key.rs:431-440,492-540`); default non-macOS storage is the owner-only key file beside the database (`crates/driver/src/registry.rs:212-250,300-318`) (Partial).
**Actual missing capability:** an optional key source for the Linux Secret Service and Windows Credential Manager behind the existing `KeySource` trait (`key.rs:127`) and the existing `os-keychain` setting value, selectable only explicitly, never a default, so credentials.db alone is not enough to decrypt on those platforms.
**Alternatives considered:** keep the file key (documented as a directory-permission boundary, `key.rs:218-229`); require the operator to inject bytes with `HeadlessKeySource` (`key.rs:139`); a keychain extension (not possible: the master key must stay in core, ADR 0075 D12 and ADR 0049 D4).
**Expected benefit:** security on non-macOS (separates ciphertext from key), parity of the documented `os-keychain` option.
**Implementation complexity:** medium: platform crates, headless CI behaviour, feature-graph weight (AGENTS.md "Feature graphs earn their weight"; cold build time is a gate).
**Security implications:** a locked or absent facility must fail closed and surface `KeychainRefused` (`store.rs:1332-1340`); no plaintext or file fallback inside the keychain mode (`key.rs:416` doc).
**Maintenance cost:** medium (platform behaviour drift, per-OS tests).
**Performance measurement strategy:** cold `cargo run --bin omp` build time and binary size delta with the feature on and off; first-lease latency. Not measured.
**Acceptance criteria:** with the feature off nothing changes and the crate graph is unchanged; with it on, a store created under `os-keychain` is unreadable without the facility; a locked facility yields the typed `storage_locked` failure; the macOS path behaves as before.
**Follow-up:** low priority; revisit if remote or multi-user Linux deployments become a stated target (ADR 0039).

### AUTH-05 Refresh keeper behaviour inside the existing broker daemon
**Decision class:** Requires benchmarking
**Existing implementation evidence:** `omp auth-broker serve` starts the daemon with the `Auth` service (`crates/app/src/auth_broker_cmd.rs:63`, `crates/app/src/daemon.rs:378-399`); refresh is lazy at lease time with a 60 s skew (`crates/ai/src/auth/manager.rs:1314-1330`) or on a 401; `needs_refresh` is never called (`refresh.rs:35`) (Partial).
**Actual missing capability:** a periodic "refresh accounts nearing expiry" task, running only in the broker daemon, that calls the existing `AuthControlHandle::refresh` (`manager.rs:644`, single-flight and fenced). It would keep refresh tokens alive for providers that expire idle ones and remove refresh latency from the first request after idle. Whether any bundled provider needs it is not established.
**Alternatives considered:** a separate keeper service (a second scheduler, rejected); refresh at process start for all accounts (cost on every launch); do nothing (lazy refresh already works on use).
**Expected benefit:** latency (first-request refresh round trip removed), reliability (idle refresh tokens stay valid) only if measured.
**Implementation complexity:** low once AUTH-01 lands; without AUTH-01 a daemon plus CLI would race.
**Security implications:** a keeper increases provider refresh traffic and keeps tokens warm indefinitely; it must refresh only accounts the user enabled and never retry a revoked token in a loop; failures use the typed `refresh_failed` and, per AUTH-07, a consistent disable rule.
**Maintenance cost:** low.
**Performance measurement strategy:** measure the p50 and p95 extra latency of a refresh on the request path against a stub endpoint, and the provider-side idle expiry of each bundled OAuth provider from its documentation. Not measured.
**Acceptance criteria:** if the measurements justify it, the daemon refreshes an account within the skew window before expiry with exactly one provider call, and a stopped daemon leaves lazy refresh working.
**Follow-up:** depends on AUTH-01; extend if the benchmark passes, otherwise record as `Unnecessary or redundant`.

### AUTH-06 CLIProxyAPI credential import and credential reuse across applications
**Decision class:** Requires ADR reconsideration
**Existing implementation evidence:** `omp auth-broker import <path>` parses CLIProxyAPI credential files (`type`, `access_token`, `refresh_token`, `expired`, `email`, `account_id`, `disabled`) and stores them as OAuth accounts, with `--dry-run` (`crates/app/src/auth_broker_cmd.rs:30-56,81`) (Implemented). ADR 0043 lists "No token/key extraction from Keychain, credential files or private caches" and "No credential migration or extraction" for native provider CLIs (`docs/adr/0043-native-cli-integration.md:19,34`) and records that Google's and Anthropic's terms restrict reusing their login (`0043:146-149`).
**Actual missing capability:** an owner decision, not code: whether an import of tokens minted by another tool's OAuth client for a user-held subscription is permitted, for which providers, and whether OMP may refresh them under its own client identity. Reference gateways in this research do exactly this (section 5 of this file); that does not make it authorised.
**Alternatives considered:** keep the command with an allowlist limited to providers whose own login OMP implements and whose terms allow token portability; remove the command and require a native `omp auth login`; keep it as dry-run only.
**Expected benefit:** migration convenience only; no reliability or cost benefit that a native login does not provide.
**Implementation complexity:** low (allowlist or removal).
**Security implications:** imported refresh tokens move a long-lived credential into a second store; a token from another client id may violate provider terms or break that tool's session on refresh-token rotation (inference); the file on disk is plaintext until imported.
**Maintenance cost:** low.
**Performance measurement strategy:** not applicable.
**Acceptance criteria:** the owner records the decision in ADR 0043 or a successor; the command enforces it (allowlist or removal); the dry-run output names the provider and states which client identity will refresh the token.
**Follow-up:** if removed, drop `CliProxyCredential` and the import tests in one cutover.

### AUTH-07 One disable-on-refresh-failure rule for both refresh paths
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** the extension `provider_refresh` path disables the account and emits `credential_disabled` on failure (`crates/ai/src/auth/manager.rs:1550`); the built-in OAuth path returns the error and leaves the account enabled (`manager.rs:1580-1660`) (Partial).
**Actual missing capability:** a single deterministic rule (for example: disable only on a terminal provider answer such as `invalid_grant`, keep enabled on transport errors), applied to both paths and emitted through the same event.
**Alternatives considered:** keep the inconsistency; disable on every failure (turns a network blip into a logout); leave to extension authors (violates ADR 0018 decision 1).
**Expected benefit:** reliability (no retry storms against a dead refresh token; no false disable on outage), predictable `omp auth status`.
**Implementation complexity:** low to medium: needs the typed distinction between terminal and transient refresh errors, which `OAuthError` may already carry (not checked).
**Security implications:** disabling a revoked credential reduces repeated use of a dead token; mistaken disable denies service, so the terminal set must be explicit.
**Maintenance cost:** low.
**Performance measurement strategy:** not applicable.
**Acceptance criteria:** table-driven tests for terminal versus transient errors on both paths yield identical account state and identical events.
**Follow-up:** none.

### AUTH-08 Entitlement and region facts as catalog data feeding existing eligibility
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** usage and plan data exist (`crates/proto/proto/omp/auth/v1/auth.proto:86-92`, `crates/ai/src/auth/github_copilot.rs`, `oauth.rs:55-57` claim names); a provider denial of one model becomes `CooldownReason::ModelPolicy` (`crates/ai/src/account/pool.rs:76`); `Eligibility` has no entitlement or region variant (`pool.rs:86-132`) (Partial).
**Actual missing capability:** optional per-account entitlement facts (plan, model allow-list, region) recorded at login or on first usage fetch, as non-secret metadata in the existing account state store, consulted by the existing `Eligibility` check before the first request.
**Alternatives considered:** keep learning from failures (costs one wasted request per account-model pair, a provider-visible 4xx); a new entitlement service (a second registry, rejected).
**Expected benefit:** reduced network transmission (skip requests that are certain to be refused), latency, reliability.
**Implementation complexity:** medium: per-provider fact extraction must stay data-driven (AGENTS.md: no `model_id.contains` conditionals), so only providers whose entitlement is exposed by a documented endpoint or token claim qualify.
**Security implications:** facts are metadata only; a wrong cached fact must degrade to the current behaviour (try and learn), never block permanently.
**Maintenance cost:** medium (provider drift).
**Performance measurement strategy:** count refused-by-entitlement requests per account-model pair in a recorded session set; not measured.
**Acceptance criteria:** with a recorded entitlement, a request for an unentitled model selects another eligible account or fails with typed evidence without a wire request; an expired fact falls back to try-and-learn.
**Follow-up:** coordinate with the routing area (PRV) which owns selection (S01-S14).

### AUTH-09 Authentication health from existing refresh receipts
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** `RefreshReceipt` records every coordination step (`crates/ai/src/account/refresh.rs:268-281`); per-credential states and `ProbeCredentials` exist (`auth.proto:95`, `crates/serve/src/auth.rs:986`); `omp auth status` (`crates/app/src/cli.rs:2197`). Consumption by any metric sink was not found (audit 6.12) (Partial).
**Actual missing capability:** emit refresh outcomes (role leader or waiter, steps, failure kind), lease waits and storage-lock events through the existing structured runtime events and `omp auth status`, with no new service.
**Alternatives considered:** a dedicated health endpoint (second surface); logs only (not queryable).
**Expected benefit:** reliability and diagnosability of AUTH-01 and AUTH-05; supports the owner's O02 requirement.
**Implementation complexity:** low.
**Security implications:** events must stay secret-free (`RefreshReceipt` is already non-secret by construction).
**Maintenance cost:** low.
**Performance measurement strategy:** not applicable.
**Acceptance criteria:** a forced refresh in a test emits one event with its step list; a locked store emits one `storage_locked` health event per launch.
**Follow-up:** hand the event schema to the observability owner.

### AUTH-10 Perplexity desktop-token borrowing is on by default
**Decision class:** Requires ADR reconsideration
**Existing implementation evidence:** `crates/ai/src/auth/oauth/custom/perplexity.rs:456-475` `borrow_native_token` runs `defaults read ai.perplexity.mac authToken` on macOS and `:77-79` tries it before the email one-time-password login, skipped only when `OMP_AUTH_NO_BORROW=1` (Implemented, opt-out). ADR 0059 names the file (`docs/adr/0059-provider-catalog-additions.md:23`) but not the borrowing; ADR 0043 states "No token/key extraction from Keychain, credential files or private caches" for native CLIs (`0043:19`).
**Actual missing capability:** an owner decision on reading another application's stored token, and a rule consistent with ADR 0043: either opt-in only (an explicit setting or a prompt naming the source), or removal and a native login only.
**Alternatives considered:** keep opt-out (current); opt-in with a named confirmation; remove the borrow path and keep the one-time-password login.
**Expected benefit:** security and provider-terms posture; no loss of function for users who can log in with the one-time password.
**Implementation complexity:** low.
**Security implications:** a default read of another product's credential store is credential extraction by the ADR 0043 definition; the borrowed token is a full account credential and is then held in OMP's store; the code path is macOS-only.
**Maintenance cost:** low (removal) or low (a setting).
**Performance measurement strategy:** not applicable.
**Acceptance criteria:** the decision is recorded in an ADR 0059 or ADR 0043 amendment; with the chosen rule, a fresh login never reads the desktop application's preferences unless the user opted in; the unit tests that inject `borrow` (`perplexity.rs:571-576`) keep passing.
**Follow-up:** the research reference `stark-shadow/cpa-plugin-perplexity` solves a related problem by cookie capture and is rejected (section 5 below); do not use it as justification.

### AUTH-11 At-most-once refresh send for single-use refresh tokens
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** the lease expires after 30 s and a waiter that sees `LeaseExpired` loops and acquires the lease again (`crates/ai/src/account/refresh.rs:566-572,528`); the refresh then loads the stored bundle and sends the stored refresh token (`crates/ai/src/auth/oauth.rs:814-860`). A leader that dies after the provider consumed the token and before the new bundle is written leaves the old token in the store; the next attempt re-sends it (Partial; behaviour with a real rotating provider not tested).
**Actual missing capability:** a consumed marker written before the grant is sent (for example a generation-scoped flag in the existing `leases` row or credential metadata) so that an unknown outcome stops further sends for that token and reports a typed "outcome unknown, re-login may be needed" failure instead of replaying the old token. The reference design is CLIProxyAPI's write-ahead refresh claim, which never reclaims an incomplete claim (section 5 below).
**Alternatives considered:** keep lease-expiry handoff (simplest, risks replaying a spent token against providers with reuse detection); a separate journal directory (a second store, rejected); accept the risk and document it.
**Expected benefit:** reliability (fewer revoked token families and forced re-logins), security (no replay of a spent refresh token).
**Implementation complexity:** medium: a schema change in `credentials.db` (migration), a new typed failure, and tests for crash points (before send, after send before write).
**Security implications:** the consumed marker is non-secret; the receipt design in the reference stores credentials, which OMP does not need because the generation CAS already carries the new bundle on success.
**Maintenance cost:** medium.
**Performance measurement strategy:** crash-injection tests at three points with a stub endpoint that rotates and enforces reuse detection; count revoked families with and without the marker. Not measured.
**Acceptance criteria:** after a crash between send and write, no second grant is sent for the same refresh token and the account reports the typed unknown-outcome failure; a peer that waits sees the new generation when the write did land.
**Follow-up:** do together with AUTH-01 (same files); requires an ADR 0018 amendment sentence on the changed handoff rule.

### AUTH-12 Refresh lead as per-provider catalog data
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** one global 60 s pre-expiry skew (`crates/ai/src/auth/manager.rs:1322` `RefreshingCredentialSource::new`); AWS has its own constant (`crates/ai/src/auth/aws.rs:46`) (Partial). The catalog already carries per-provider auth data (`crates/ai/src/auth/spec.rs:547` `OAuthRefreshSpec`, not read in detail).
**Actual missing capability:** an optional lead (and "do not refresh early" flag for providers where one account's refresh can invalidate its siblings) in catalog OAuth data, read by the existing refresh path. References: OmniRoute keeps a code table with 1, 5 and 15 minute leads and documents why (`cheaper-inference/OmniRoute@3e23a8a:open-sse/services/tokenRefresh.ts:125-137`); CLIProxyAPI uses a 96 hour lead for one provider's outer token (`router-for-me/CLIProxyAPI@dbe641f:sdk/cliproxy/auth/types.go:1131-1138`). OMP's locked rule is providers-as-data, so the table must live in the catalog, not in `.rs`.
**Alternatives considered:** keep one constant; per-provider constants in Rust (reviewer-reject under AGENTS.md "Inference").
**Expected benefit:** reliability (no expiry mid-request for long-lived-session providers), latency, fewer refreshes where early refresh is harmful.
**Implementation complexity:** low to medium (catalog schema field, lintx and catalog conformance tests).
**Security implications:** none beyond refresh traffic volume.
**Maintenance cost:** low.
**Performance measurement strategy:** not applicable.
**Acceptance criteria:** a provider with a configured lead refreshes inside that window and one without keeps today's 60 s; a provider flagged "no early refresh" never refreshes before the skew window.
**Follow-up:** prerequisite for the keeper in AUTH-05.

### AUTH-13 Allowance pooling, free-credit automation, client impersonation and cross-application token import
**Decision class:** Rejected for security, licensing or operational reasons
**Existing implementation evidence:** none of these are implemented as such in OMP2 except the two imports covered by AUTH-06 and AUTH-10. `AccountPool` rotates among accounts the user holds and respects cooldowns and quotas (`crates/ai/src/account/pool.rs:356,86-132`) (Implemented, legitimate use).
**Actual missing capability:** none proposed. The reference patterns are: many accounts per vendor with automated daily check-ins to restore free credits and randomised desktop client profiles (`cxy2730/CLIProxyAPI-freetier2api-plugin@0a718a6:internal/vendors/zcode/credential.go:497`, `internal/tasks/checkin.go`); IDE-style user agents and device fingerprints for a private runtime endpoint (`hank9999/kiro.rs@3c9723f:src/kiro/endpoint/ide.rs:4-7`, `src/kiro/machine_id.rs`); sessions of a consumer web application reused as an API (`stark-shadow/cpa-plugin-perplexity@0d09161:README.md:5,64-74`); importers for other tools' credential files and "TLS stealth" for blocked regions (`cheaper-inference/OmniRoute@3e23a8a:src/lib/oauth/utils/codexAuthImport.ts`, README only for the stealth claim).
**Alternatives considered:** none; the owner's rule is that routing may spread load across credentials the user is entitled to use and must not circumvent usage limits or use an unauthorised private API.
**Expected benefit:** not applicable.
**Implementation complexity:** not applicable.
**Security implications:** account termination risk, provider-terms violation, and exposure of full-account credentials; plaintext credential files in the references.
**Maintenance cost:** not applicable.
**Performance measurement strategy:** not applicable.
**Acceptance criteria:** no OMP2 feature, extension or documentation recommends these patterns; any future proposal must name an authorised-use case recorded by the owner.
**Follow-up:** the adoptable ideas from the same repositories are carried in AUTH-07, AUTH-11 and AUTH-12 instead.

## 4. Draft ADR needed?

Propose only; nothing below is drafted or applied.

| Candidate | Action | Why |
|---|---|---|
| ADR 0018 amendment | Needed (small) | Status text claims persistent lease coordination is Implemented while the production owner token is constant (F1). The amendment would record the per-process owner decision (AUTH-01), state the cross-process guarantee precisely and add the shared-stack rule for subagents (AUTH-03). Owner approval required before editing the ADR. |
| ADR 0075 amendment | Needed if AUTH-02 proceeds | Records whether `omp.creds.*` is bound (which operations) or removed, and aligns D9/O5 and ADR 0049 D4 with the code. |
| ADR 0043 note or successor | Needed for AUTH-06 | The import command and the "no credential migration" rule must be reconciled by an owner decision on provider terms. |
| ADR 0059 or ADR 0043 amendment for AUTH-10 | Needed | Records the rule for reading another application's stored token (opt-in or removal). |
| ADR 0018 amendment sentence for AUTH-11 | Needed with AUTH-01 | States the handoff rule for an unknown refresh outcome. |
| ADR for a refresh keeper | Not needed now | AUTH-05 is gated by measurement and stays inside the existing broker daemon. |
| ADR for entitlement facts | Not needed now | AUTH-08 extends data in the existing account store; reconsider if it needs a new catalog schema field. |

## 5. Reference comparison

Full notes: [`evidence/ref-providers.md`](evidence/ref-providers.md) (seven provider repositories, CLIProxyAPI and OmniRoute auth parts) and the seven matrix files `evidence/matrix-<owner>_<repo>.tsv`. All code was read, not run; nothing was copied (several licences are permissive, one has none).

| Dimension | OMP2 (audited commit) | References (owner/repo@sha7) | Reading |
|---|---|---|---|
| A01 API keys | Catalog-ordered sources, sealed storage (`crates/ai/src/auth/spec.rs:440`, `store.rs:591`) | `hank9999/kiro.rs@3c9723f:src/kiro/token_manager.rs:try_ensure_token` (static key skips refresh); plaintext JSON files in all kiro forks; `cheaper-inference/OmniRoute@3e23a8a:src/lib/db/encryption.ts` (AES-256-GCM, plaintext if no key) | OMP stronger: fails closed without a key source |
| A02/A03 OAuth, device | PKCE, paste, device and custom exchanges as data; 3 device-code, 2 PKCE and 15 custom flows in `fixtures/llm-oracle/catalog/oauth.toml` | `Foxfishc/kiro.rs@15c0a3a:src/kiro/sso.rs` (vendor sign-in reproduced); `cxy2730/CLIProxyAPI-freetier2api-plugin@0a718a6:README.md:36-50` (device flows per vendor); `cheaper-inference/OmniRoute@3e23a8a:src/lib/oauth/providers/*` (about 40 provider files) | Breadth exists in references by reproducing vendor clients; OMP's flows are data plus typed handlers (ADR 0049 D6 class B) |
| A04 CLI delegation | ACP and Codex delegated executors, no token reading (ADR 0043) | None of the seven repositories delegates to an official CLI | OMP2 differs by design |
| A05 references, A06 keychain | Names only in catalog; macOS keychain for the master key | `cxy2730/CLIProxyAPI-freetier2api-plugin@0a718a6:internal/state/state.go:435` (0600 state file); no keychain in any of the seven (searched); OmniRoute VS Code client uses SecretStorage (README only) | OMP ahead on macOS, behind nowhere; Linux and Windows gap is AUTH-04 |
| A07 refresh coordination | In-process single flight plus SQLite lease; inert cross-process owner (AUTH-01) | `hank9999/kiro.rs@3c9723f:src/kiro/token_manager.rs:try_ensure_token` (async mutex plus recheck); `cxy2730/...@0a718a6:internal/tasks/keepalive.go:refreshGate`; `router-for-me/CLIProxyAPI@dbe641f:internal/auth/codex/refresh_journal.go:journalRefresh` (write-ahead claim, never reclaimed) | The only cross-process design in the references is CLIProxyAPI's journal; it motivates AUTH-01 and AUTH-11 |
| A08 expiry | 60 s skew, typed failures, extension-path disable | kiro 5 and 10 minute windows and `invalid_grant` disable (`hank9999/kiro.rs@3c9723f:src/kiro/token_manager.rs:is_token_expired`); OmniRoute lead table (`open-sse/services/tokenRefresh.ts:125-137`); CLIProxyAPI leads (`sdk/cliproxy/auth/types.go:1131-1138`) | References are per-provider; OMP is global (AUTH-12) |
| A09 recovery | CLI verbs, key rotation, repair, import | `BenedictKing/kiro.rs@87a735c:src/kiro/cooldown.rs:load_persistent_cooldowns`; corrupt-state quarantine in `cxy2730/...@0a718a6:internal/state/state.go:480` | OMP covers storage corruption by SQLite; no gap |
| A10 keepers | Broker daemon, lazy refresh | `router-for-me/CLIProxyAPI@dbe641f:sdk/cliproxy/auth/conductor_refresh.go:29-32` (5 s check, 16 workers, 1 and 5 minute backoffs); Home mode; `cxy2730/...@0a718a6:internal/tasks/keepalive.go:Keepalive`; `M-JYuan/kiro.rs@fc35283:src/kiro/provider.rs:start_periodic_balance_refresh` | Keepers are common in references; AUTH-05 keeps it inside the existing broker and behind a benchmark |
| A11 entitlements | Usage RPCs, plan discovery, learned model denial | `Foxfishc/kiro.rs@15c0a3a:src/admin/handlers.rs:get_overage_status`; `cxy2730/...@0a718a6:internal/tasks/checkin.go` (credit automation, rejected) | AUTH-08 adopts only the legitimate part (read entitlement, do not automate claims) |
| A12 regional | Endpoint and claim based | `Foxfishc/kiro.rs@15c0a3a:src/kiro/profile_resolver.rs:resolve_across_regions`; separate cn and global vendor instances in the freetier plugin | Region resolution from account data is a legitimate idea (AUTH-08) |
| G10 free tier | `None` and `OptionalBearer` only | The freetier plugin's whole purpose, with credit farming (rejected, AUTH-13) | Do not copy; a neutral "anonymous allowed" flag already exists |
| G11 identity, dedup | Typed ids, rebind refusal | `hank9999/kiro.rs@3c9723f:src/kiro/token_manager.rs:add_credential` (SHA-256 duplicate rejection); `stark-shadow/cpa-plugin-perplexity@0d09161:README.md:137` (name from session hash) | Possible small gap: duplicate upstream identity (audit 6.11) |
| O02 health | Per-credential state, probes, no metrics | Admin snapshots in all references; `cheaper-inference/OmniRoute@3e23a8a:src/lib/tokenRefreshCircuit.ts` (health sweep and backoff window) | AUTH-09 |

Provider-terms classification of the repositories themselves (rule 8): the five `kiro.rs` repositories, the free-tier plugin and the Perplexity plugin are described neutrally in `evidence/ref-providers.md` and classified `Rejected for security, licensing or operational reasons` as products; the generic lifecycle mechanisms listed above are the only reusable material. CLIProxyAPI and OmniRoute are gateways whose normal use is pooling many accounts; only their refresh and storage mechanisms are compared here.

Back to the [index](README.md).
