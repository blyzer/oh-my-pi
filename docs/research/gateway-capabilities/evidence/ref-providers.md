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

(Other repositories follow, appended as each is read.)

Back to the [index](../README.md).
