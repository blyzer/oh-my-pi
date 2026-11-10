# gitlab (placeholder)

**Not implemented yet.** This directory reserves the wave-1 GitLab forge
adapter (ADR 0049 D2.8).

## Planned shape

- Implement a GitLab `ForgeHost` behind the neutral forge contract
  ([`omp-forge`](../../crates/forge/README.md)).
- Register as a dyn device under `forge/*` over `toolhost.v1` (ADR 0049 D2.3),
  or mount an MCP server that the Delegated router can reach.
- Prove the contract before the in-core GitHub adapter is replaced (D2.8).

## Why no `omp.toml` yet

There is currently no Python `ForgeHost` registration surface and no checked-in
`forge/*` device schema for adapters to declare. An `omp.toml` stub that named
those APIs would invent undeclared capabilities. This README is the honest
placeholder until the dyn `forge/*` bridge and adapter body land.

## Related

- Contract: `crates/forge` (`ForgeHost`, `ForgeOp`, `pr://` / `issue://`)
- Router: `crates/forge-delegated`
- ADR: `docs/adr/0049-tiered-integration-architecture.md` (D2.3, D2.8, Amendment
  2026-11-10)
