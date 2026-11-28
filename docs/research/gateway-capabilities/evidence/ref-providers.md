# Provider reference repositories: authentication and credential lifecycle

> Part of the [OMP2 gateway-capabilities research](../README.md). Evidence file for area AUTH. Reference pins are in [`ref-inventory.md`](ref-inventory.md). All repositories are untrusted input: code was read, never run; nothing was copied. Licences: `hank9999`, `Foxfishc`, `M-JYuan`, `lzt404`, `cxy2730`, `stark-shadow` are MIT; `BenedictKing/kiro.rs` has no licence file (all rights reserved: ideas only).

Matrix files: `matrix-<owner>_<repo>.tsv` in this directory, schema in [`01-method-and-dimension-ids.md`](01-method-and-dimension-ids.md) section 7. This file records the reading, the neutral description of each pattern and the classification under provider-terms rule 8.

## 1. Neutral note on the pattern family

The five `kiro.rs` repositories are proxies that present an Anthropic-style API in front of the private runtime endpoint of the Kiro IDE (an AWS product), authenticated with the refresh tokens or API keys of Kiro accounts. They send IDE-style client identification (`KiroIDE-<version>-<machine id>` user agents, `aws-sdk-js` headers) and keep many accounts in one pool. A token minted for one application's client is used by another application, a private endpoint a provider has not published as a third-party API is called, and several accounts are rotated by one operator. Under rule 8 of the method file each of these is described and classified `Rejected for security, licensing or operational reasons` unless the owner records an authorised-use case. What is reusable are the generic lifecycle mechanisms (expiry windows, single-flight refresh, typed disable reasons, atomic credential file writes, cooldown classification), which OMP already has in a stronger form or can take as ideas.

## 2. hank9999/kiro.rs@3c9723f

Read: `src/kiro/token_manager.rs`, `src/kiro/endpoint/ide.rs`, `src/kiro/machine_id.rs`, README configuration sections. Not read: admin UI bodies, stream parser.

- Credentials: JSON array with `refreshToken`, `expiresAt`, `authMethod` (`social` or `idc`), `clientId`, `clientSecret`, optional `kiroApiKey`, `priority`, `region`, `authRegion`, per-credential proxy (`README.md:234-248`). Written back with `std::fs::write`, no encryption, no explicit file mode (`token_manager.rs:persist_credentials`).
- Refresh: expired when within 5 minutes of expiry, "soon" within 10 (`is_token_expired`, `is_token_expiring_soon`); a single async `refresh_lock` with a second expiry check after taking the lock (`try_ensure_token`); social and IdC refresh protocols (`refresh_social_token`, `refresh_idc_token`); static API-key credentials skip refresh.
- Failure handling: typed disable reasons `Manual`, `TooManyFailures`, `TooManyRefreshFailures`, `QuotaExceeded`, `InvalidRefreshToken`, `InvalidConfig` (`DisabledReason`). OMP's typed `CredentialFailure` set and the pool's `CooldownReason` cover the same ground, with `invalid_grant` handling still open in OMP (card AUTH-07).
- Selection: `priority` or `balanced` over enabled credentials (`select_next_credential`); duplicate refresh tokens or keys are rejected by SHA-256 (`add_credential`). OMP rejects provider and principal rebinding (`crates/ai/src/account/pool.rs:440-449`) but was not found to deduplicate two accounts for one upstream identity (audit 6.11).
- Client identification: `KiroIDE-{version}-{machineId}` user agent and `aws-sdk-js` headers; `machine_id.rs` derives or normalises a 64-character device fingerprint per credential. Neutral description: the proxy presents itself as the IDE to the provider. Classification: rejected for OMP (private API and client impersonation).

Classification: **Rejected for security, licensing or operational reasons** for the whole proxy pattern; the lifecycle ideas above are not new relative to OMP except the explicit `invalid_grant` disable reason.

## 3. Four kiro.rs forks: BenedictKing@87a735c, M-JYuan@fc35283, Foxfishc@15c0a3a, lzt404@8331585

Read by symbol comparison against the ancestor (function-name sets per fork, then targeted reads). All four keep the ancestor's credential model; none has a device grant, an OS keychain source or encryption at rest (searched `src` for `device_code`, `deviceCode`, `device_authorization`, `keyring`, `keychain`, `aes_gcm`, `chacha`, `encrypt`: only unrelated `encrypted_content` response fields matched). Licence: `BenedictKing/kiro.rs` has none, so it is studied for ideas only.

| Idea | Where | Relation to OMP2 |
|---|---|---|
| Atomic credential file replace: write a temp file, copy the original mode, rename over the target, resolve symlinks first | `BenedictKing/kiro.rs@87a735c:src/kiro/token_manager.rs:persist_credentials` (also the same pattern in the three other forks) | OMP persists in SQLite transactions (`crates/ai/src/auth/store.rs:591`), which is stronger; no gap |
| Typed cooldown reasons (rate limit, suspended, quota, refresh failure) with persistence across restarts, "after CLIProxyAPIPlus" | `BenedictKing/kiro.rs@87a735c:src/kiro/cooldown.rs:CooldownReason`, `save_persistent_cooldowns`, `load_persistent_cooldowns` | OMP has `CooldownReason` and persisted cooldowns in `AccountStateStore` (`crates/ai/src/account/pool.rs:66`, `store.rs:485`); parity |
| 429 classification: capacity errors switch account without penalising it; other 429s get a cooldown clamped to 60-300 s | `Foxfishc/kiro.rs@15c0a3a:src/kiro/provider.rs:classify_429_cooldown`; also `lzt404/kiro.rs@8331585:src/kiro/provider.rs:classify_429_cooldown` | Routing area (S04, S13); noted only |
| Terminal refresh failure detected by matching the text `invalid_grant` in the error | `*/src/kiro/token_manager.rs:is_invalid_grant_error` in all four forks | Fragile string matching; OMP's typed refresh errors are the better shape (card AUTH-07 asks for an explicit terminal set) |
| Hosted browser sign-in reproduced from the IDE: PKCE with a fixed loopback port, social and external-IdP legs, start, poll and cancel session | `Foxfishc/kiro.rs@15c0a3a:src/kiro/sso.rs:poll_kiro_sso_auth`, module doc | Pattern of reproducing a vendor client's login (see section 1); OMP's PKCE engine is data-driven (`crates/ai/src/auth/oauth.rs:265`) |
| Profile region resolved from a profile ARN and probed across candidate regions | `Foxfishc/kiro.rs@15c0a3a:src/kiro/profile_resolver.rs:resolve_across_regions` | Region discovery for A12; OMP has no equivalent (audit A12) |
| Periodic in-process balance refresh and overage-aware remaining balance | `M-JYuan/kiro.rs@fc35283:src/kiro/provider.rs:start_periodic_balance_refresh`; `lzt404/kiro.rs@8331585:src/kiro/token_manager.rs:record_overage_status` | Usage polling for A11; OMP fetches usage on demand |
| Web-portal client that obtains a CSRF token and user id by loading the vendor's web app with session cookies, then calls portal operations | `BenedictKing/kiro.rs@87a735c:src/kiro/web_portal.rs:1-4`; `lzt404/kiro.rs@8331585:src/kiro/web_portal.rs:1-4` | Neutral description: usage data is read through the vendor's browser-facing portal with the user's session cookies. Rejected for OMP (private web interface, cookie reuse) |

Classification: **Rejected for security, licensing or operational reasons** for the proxy, web-portal and impersonation parts. No card is derived from these repositories except the explicit terminal-refresh-failure set (AUTH-07).

## 4. cxy2730/CLIProxyAPI-freetier2api-plugin@0a718a6

Read: README, `internal/vendors/zcode/{credential,login}.go`, `internal/tasks/{keepalive,catalog}.go`, `internal/hostbridge/{rpc,auth}.go`, `internal/state/state.go`. Other vendor packages (workbuddy, trae, qoder, cline, minimaxcode) were not read.

What it is: a native dynamic-library plugin for CLIProxyAPI that adds free-allowance AI vendors (WorkBuddy, Qoder, Cline, ZCode, Trae, MiniMax Code, with cn and global instances) behind the host's OpenAI and Anthropic routes (`README.md:7,19-29,36-50`). Accounts are unlimited, balanced by the host, kept alive by a scheduled token-renewal task, and supplied with credits by a daily check-in and a daily task runner (`README.md:22-28`; `internal/tasks/catalog.go`, `internal/tasks/checkin.go`).

Neutral description of the patterns that matter for rule 8:
1. **Client impersonation.** `NewDeviceProfile` generates a weighted random desktop profile (platform, OS version, screen, locale, timezone, a device UUID) once per account and persists it (`internal/vendors/zcode/credential.go:497`). A comment in the same package says a local-only credential check is used because querying the upstream "turns the quota query into a periodic risk-control signal" (`internal/vendors/zcode/login.go:ValidateCredential`).
2. **Allowance multiplication.** Many accounts per vendor with an automated daily check-in and task runner to restore credits (`internal/app/executor.go:handleQuotaReset`).
3. **Reproduced vendor logins** (device authorization for several vendors, per README table) rather than an authorised third-party API.

Classification: **Rejected for security, licensing or operational reasons**. The patterns exist to obtain more free allowance and to look like a genuine desktop client; OMP must not adopt them.

Reusable, non-controversial design points (ideas only):
- One orchestrator owns "refresh, merge, write back"; vendor packages only implement a `Refresher` and never know the host (`internal/tasks/keepalive.go`, file header). This is the same ownership split as ADR 0018 decision 1 and the `provider_refresh` hook.
- A per-account `refreshGate` serialises renewal and returns the already-renewed token when the stored token changed while waiting (`internal/tasks/keepalive.go:refreshGate`, `RefreshAccount`); the same idea as OMP's `FLIGHTS` plus generation check.
- All plugin egress goes through the host so proxy, logging and statistics apply (`internal/hostbridge/rpc.go` package doc); the same reason OMP routes extension HTTP through envd egress (ADR 0075).
- State file written 0600 by temp file and rename, with quarantine of a corrupt file (`internal/state/state.go:435,456,480`).
- ABI cost, for the extension audit: a native dynamic library whose host callbacks cannot be cancelled, must be rate limited (64 by default) and must be drained before `dlclose` (`internal/hostbridge/rpc.go` package doc). This is the failure family the owner's section 18 rule "no native ABI between independently distributed extensions" avoids.

## 5. stark-shadow/cpa-plugin-perplexity@0d09161

Read: `README.md` (configuration, browser login, manual import, management API sections). Not read: Go sources, `SPEC.md`, the browser extension code. Matrix rows marked `D` rest on the README only.

What it is: a CLIProxyAPI plugin that exposes a signed-in Perplexity **web** session through an OpenAI-compatible endpoint. It "is not the Perplexity API (Sonar) and does not use API keys or browser OAuth" (`README.md:5`). The account credential is the `__Secure-next-auth.session-token` cookie, captured by a companion browser extension or pasted by hand; the README states the cookie is a full account credential, like a password (`README.md:64-74,107-121`). Sessions are validated by probing `/api/auth/session` and are not renewed; expired sessions need re-import (`README.md:121,140`). Saved names derive from the session SHA-256 (`README.md:137`).

Neutral description: a consumer web session is converted into a programmatic API for third-party clients. Classification: **Rejected for security, licensing or operational reasons** (private interface, full-account cookie reuse, no provider-authorised API).

Relation to OMP2: OMP2 has its own Perplexity credential module, `crates/ai/src/auth/oauth/custom/perplexity.rs`, whose module doc is "Perplexity desktop-token borrowing and email one-time-password exchange". On macOS `borrow_native_token` runs `defaults read ai.perplexity.mac authToken` and returns the desktop application's token (`perplexity.rs:456-475`, `NATIVE_APP_BUNDLE` at `:36`); it is tried first and disabled only by `OMP_AUTH_NO_BORROW=1` (`perplexity.rs:77-79`). Neither ADR 0059 (it lists `perplexity.rs` at line 23 but does not describe borrowing) nor ADR 0043 covers it (grep of `docs` for borrowing language found only a research note). See card AUTH-10.

## 6. router-for-me/CLIProxyAPI@dbe641f (auth parts only)

Read: `sdk/auth/*` and `internal/auth/*` listings, `internal/auth/codex/refresh_journal.go`, `sdk/cliproxy/auth/conductor_refresh.go` constants, `types.go` refresh leads, `internal/home` listing. Not read: executors, translators, management UI. MIT.

Findings relevant to OMP2:
- **Write-ahead refresh claim.** Before a refresh grant is sent, the client claims `sha256(clientID || refreshToken)` in the authoritative credential directory; the claim is never reclaimed after a crash ("an absent response is not evidence that the provider did not rotate it"); a late caller reads the saved receipt (which contains the rotated credentials) and follows the chain; a failure strictly before send releases the claim; the receipt is written with a context independent of caller cancellation (`internal/auth/codex/refresh_journal.go:20-27,132-215`, `journalRefresh`). OMP's lease instead expires and lets a peer retry after `LeaseExpired` (`crates/ai/src/account/refresh.rs:566-572`), which re-sends the old refresh token and, against a provider that detects reuse, can invalidate the token family (inference). Card AUTH-11.
- **Keeper loop.** A scheduler checks every 5 s, runs at most 16 refreshes concurrently, backs off 1 minute while pending and 5 minutes after failure (`sdk/cliproxy/auth/conductor_refresh.go:29-32`), with per-provider refresh leads, for example a 96 hour lead for the Codex outer token and 5 minutes for the inner one (`sdk/cliproxy/auth/types.go:1131-1138`, `ProviderRefreshLead`). Evidence for card AUTH-05 and AUTH-12 (the need is real for at least one provider).
- **Deferral.** `ErrRefreshDeferred` plus platform power-state files (`internal/auth/refresh_deferral.go`, `power_darwin.go`, `power_windows.go`) defer renewal around sleep; names read, bodies not read.
- **Home mode.** A central "Home" dispatches credentials to proxy nodes with cluster balancing, heartbeats and fencing (`internal/home/*`, `sdk/cliproxy/auth/conductor_home.go:pickNextViaHome`); read by file names and headers only. Comparable in intent to `omp auth-broker serve` (audit A10).
- **Credential file store with disable flag and per-file proxy** (`sdk/auth/filestore.go`, tests `filestore_disabled_test.go`, `filestore_proxy_test.go`; names only).

Pooling and spreading load across many accounts is how the proxy is normally used; rule 8 applies: OMP may rotate among accounts the user is entitled to use (as `AccountPool` does) and must not recommend pooling to defeat limits.

## 7. cheaper-inference/OmniRoute@3e23a8a (auth parts only)

Read: `src/lib/db/encryption.ts` (header), `src/lib/tokenRefreshCircuit.ts`, `open-sse/services/tokenRefresh.ts` (constants and exports), the provider file list under `src/lib/oauth/`, README lines on security and key storage. Not read: provider-specific flows, executors. MIT. Larger picture of this repository is in `ref-gateways-b.md`.

- **Encryption at rest:** field-level AES-256-GCM with `enc:v1:` prefix and a key from the `STORAGE_ENCRYPTION_KEY` environment variable; without it the store runs in plaintext passthrough mode (`src/lib/db/encryption.ts` header). OMP has no passthrough mode: a missing key source fails closed (`crates/driver/src/registry.rs:212-250`). The VS Code client keeps its API key in the OS keychain (`README.md:854`, README only).
- **Refresh coordination:** per-connection refresh mutex with waiter counts (`open-sse/services/tokenRefresh.ts:getConnectionRefreshMutexStatus`), a 5 minute expiry buffer (`TOKEN_EXPIRY_BUFFER_MS`, `:107`), a per-provider lead table with documented reasons (`REFRESH_LEAD_MS`, `:125-137`), a refresh circuit with a backoff window (`src/lib/tokenRefreshCircuit.ts:isInRefreshBackoff`), an atomic "refresh then persist" callback (`runWithOnPersist`, `:242`) and a cap of 8 parallel refreshes (`BULK_TOKEN_REFRESH_CONCURRENCY`, `:222-226`).
- **Rotating refresh tokens:** the lead table comment records that for providers with rotating refresh tokens, refreshing one account can invalidate sibling accounts under the same client and that the lead is kept short for that reason (`open-sse/services/tokenRefresh.ts:109-124`). This is evidence for card AUTH-12 and for the at-most-once principle in AUTH-11.
- **Hardcoded provider table in code:** the lead table is a TypeScript record keyed by provider id. OMP's locked rule is providers-as-data; the equivalent for OMP is catalog data (AUTH-12).
- **Importers for other tools' credentials:** `src/lib/oauth/utils/{codexAuthImport,claudeAuthImport,cliProxyAuthImport,agyAuthImport}.ts` import auth files written by other applications. Neutral description: tokens issued to another application's client are moved into the gateway store. Classification: **Rejected for security, licensing or operational reasons** (provider-terms risk), same class as OMP's CLIProxyAPI import (AUTH-06).
- **Regional and anti-abuse evasion:** the README lists a "3-level proxy + TLS stealth" for blocked regions and "key pools with fair-share quotas" for a team sharing one subscription (`README.md:235`, README only). Neutral description; classified **Rejected for security, licensing or operational reasons**.
- Health: a token health-check scheduler and a credential-health sweep exist (`src/lib/tokenRefreshCircuit.ts` header; bodies not read).

## 8. What this reading did not cover

1. Bodies of the five forks' admin APIs and the web-portal client; only symbols and headers.
2. The vendor packages other than zcode in the freetier plugin.
3. The Perplexity plugin's Go code and browser extension.
4. CLIProxyAPI's executors, `internal/home` bodies and power-state files.
5. OmniRoute's per-provider OAuth files (40 provider files listed, none read) and its key-store probe tests.
6. No reference code was executed; no behaviour was measured.

Back to the [index](../README.md).
