# 0077. Route runtime evidence gets production producers; a circuit breaker is a health state

Status: proposed (draft written by the gateway-capabilities research; not owner-reviewed, not approved)
Date: 2026-11-28
Area: inference

Read against `omp2` at `85d219ae7f`. Number 0077 is provisional: it is the next free number on that commit and may be taken by another record; the owner may renumber.

**Review status.** Draft. One agent read the code and wrote this from the capability cards named below in [`docs/research/gateway-capabilities/`](../research/gateway-capabilities/README.md). No machine review, no owner review, nothing measured. It changes no existing ADR; where it would amend one, that is a proposal for the owner to apply.

## Context

The router already ranks routes on runtime evidence: `RuntimeRouteEvidence { health, quota_millionths, latency, affinity, capabilities }` (`crates/ai/src/plan.rs:401`) is read by `compare_evaluated` (`crates/ai/src/router.rs:1277`), and `Unavailable` rejects a candidate (`crates/ai/src/router.rs:815`). The account pool has `observe_rate` and `observe_quota` (`crates/ai/src/account/pool.rs:750`, `:769`) with persisted state. The provider audit ([`evidence/omp2-provider-runtime.md`](../research/gateway-capabilities/evidence/omp2-provider-runtime.md)) found that **production never feeds them**: `Router::new` does not call `with_runtime_evidence`, so routing falls back to default evidence (`crates/ai/src/router.rs:785`); `observe_rate` and `observe_quota` have no non-test callers; no circuit breaker exists (a case-insensitive search for `circuit`, `breaker` and `half_open` in `crates/ai`, `crates/serve`, `crates/catalog` and `crates/http` found nothing). Reference gateways that have the behaviour (CLIProxyAPI reads quota from response headers; OmniRoute scopes a breaker per connection) are described in [deliverable 3](../research/gateway-capabilities/03-gateway-capability-matrix.md). ADR 0018 records the provider runtime as Implemented; this record says which of its inputs are inert.

## Decision

1. **One in-process observer** folds finished attempts (outcome class from `ErrorKind`, handshake and first-chunk time) into a per-route `RuntimeRouteEvidence` for the current registry generation, and the production `Router` reads it. Deterministic, no LLM, no new store (PRV-01).
2. **A response observer** reads each codec's rate-limit and `Retry-After` data (`ProviderTelemetryEvent`, `crates/ai/src/codec/mod.rs:666`) and writes it through the existing `observe_rate` and `observe_quota` (PRV-02).
3. **A circuit breaker is a `RouteHealth` state with a timer**, not a new type or crate: open, half-open probe, closed (PRV-03). It is implemented only if the benchmark in that card shows repeated failures above an agreed rate.
4. Nothing here may be used to route around a provider's usage limits: header observation only avoids sending a request that will be refused to an account the user is entitled to use.

## Consequences

- Ranking becomes a total deterministic order that depends on observed evidence; ties and decay are specified by property tests over `compare_evaluated`.
- Evidence is per registry generation and is discarded when the generation changes; nothing is persisted except the existing account state.
- A single-route configuration behaves as today.

## Alternatives considered

Static ranking plus per-call fallback (each call re-tries a degraded first route). An external gateway for health (a second runtime; rejected by owner constraint 8). Persisting health in SQLite (unnecessary for session-length state). A separate breaker crate (redundant).

## Acceptance (from the cards)

After N classified failures inside a window the next plan ranks the route below a healthy equivalent and restores it after the decay interval; with two fake accounts and a recorded 429 plus `Retry-After` the second logical call selects the other account without a failed first attempt, and the state survives restart through the existing store. Demonstrated with `crates/ai/src/provider/fake.rs` and `crates/ai/src/transport/cassette.rs`, so no provider quota is consumed.

## Owner questions

O1. Approve wiring the evidence feed (research open question OQ-02). O2. Set the failure rate and window that justify a breaker (OQ-11).

## References

Cards PRV-01, PRV-02, PRV-03 in [deliverable 5](../research/gateway-capabilities/05-provider-integration-recommendations.md); ADR 0018; ADR 0022.
