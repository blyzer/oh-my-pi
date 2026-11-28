# 0078. Credential refresh has a per-process owner, and in-process subagents share the parent auth stack

Status: proposed (draft written by the gateway-capabilities research; not owner-reviewed, not approved)
Date: 2026-11-28
Area: inference

Read against `omp2` at `85d219ae7f`. Number 0078 is provisional: it is the next free number on that commit and may be taken by another record; the owner may renumber.

**Review status.** Draft. One agent read the code and wrote this from the capability cards named below in [`docs/research/gateway-capabilities/`](../research/gateway-capabilities/README.md). No machine review, no owner review, nothing measured. It changes no existing ADR; where it would amend one, that is a proposal for the owner to apply.

## Context

The persistent refresh lease and its fencing exist (`crates/ai/src/auth/store.rs:1168` `try_acquire_lease`, `:591` `put_inner`; `crates/ai/src/account/refresh.rs:161` `RefreshLeaseStore`). The authentication audit ([`evidence/omp2-auth-credentials.md`](../research/gateway-capabilities/evidence/omp2-auth-credentials.md)) found: (a) production constructs the coordinator with the literal owner `"omp-auth-refresh"` (`crates/driver/src/registry.rs:1381`) and `try_acquire_lease` reports `Held` only for a different owner (`crates/ai/src/auth/store.rs:1196`), so two processes never see each other's lease; (b) each in-process child runs `compose_kernel` with `gateway: None` (`crates/driver/src/subagent/spawn.rs:726-738`) and builds its own credential-store connection, account pool and coordinator, so pool state (cooldowns, rate and quota windows) is per child; (c) the extension refresh path disables the account on failure but the built-in OAuth path does not (`crates/ai/src/auth/manager.rs:1550`, `:1580-1660`); (d) after a leader crash between send and write, a peer can resend an already-spent refresh token (`crates/ai/src/account/refresh.rs:566-572`). ADR 0018's Status text calls this Implemented. Double refresh with rotating single-use tokens is inferred, not tested.

## Decision

1. **Per-process owner token** (pid plus a generated ULID, never credential material) passed to `RefreshCoordinator::new` (AUTH-01).
2. **In-process children reuse the parent's auth stack** through a `KernelOptions` field carrying the parent's production stack handle; a child with a provider or invocation-key override keeps a private stack and never widens `AccountPin` or the key-source mode (AUTH-03).
3. **One rule for refresh failure**: disable only on a terminal provider answer (for example `invalid_grant`), keep the account enabled on transport errors, on both refresh paths, with the same event (AUTH-07).
4. **At-most-once send** for single-use refresh tokens: a consumed marker is written before the grant is sent; an unknown outcome yields a typed "outcome unknown, re-login may be needed" failure instead of a replay (AUTH-11).

## Consequences

Cross-process refresh becomes a guarantee that can be tested; children stop duplicating credential connections; pool state is shared inside a process. A leader crash can now surface a re-login prompt instead of a silent replay. ADR 0018's text would be amended to state the cross-process guarantee precisely.

## Alternatives considered

A file lock per account (a second locking mechanism beside SQLite). Relying on `expected_generation` compare-and-swap alone (protects the row, not the provider's single-use token). A single broker daemon (larger; AUTH-05). Routing children through a loopback gateway (adds serialization and a daemon dependency to every child). A process-global singleton (hides ownership).

## Acceptance (from the cards)

With N processes and one expiring account exactly one provider refresh is made per expiry, every process ends on the same generation, killing the leader lets a peer take over after lease expiry, and the stub never sees a reused refresh token; with a shared handle 16 children cause one credential-store open and a cooldown recorded by one child is visible to another.

## Owner questions

O1. Approve the per-process owner and the shared stack (OQ-03). O2. Confirm the terminal-error set for disabling an account.

## References

Cards AUTH-01, AUTH-03, AUTH-07, AUTH-11 in [deliverable 5b](../research/gateway-capabilities/05b-authentication-and-credential-lifecycle.md); ADR 0018; ADR 0075.
