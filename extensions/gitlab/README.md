# gitlab (placeholder)

**Adapter body not implemented yet.** This directory reserves the wave-1
GitLab forge adapter (ADR 0049 D2.8).

## Bridge status (D2.3)

The dyn `forge/*` device bridge is landed in core:

- Schema (paths + JSON args/results):
  [`docs/adr/0049-forge-dyn-device-schema.md`](../../docs/adr/0049-forge-dyn-device-schema.md)
- Envd: `DynForgeAdapter` + `ForgeBackend { Github, Dyn }` +
  `register_dyn_forge_host` under `cap-forge`
  (`crates/envd/src/github_adapter/dyn_forge.rs`)

GitLab should mount those `forge/<op>` devices and answer `omp.devices.call`.
Do not edit Cargo-workspace vendor code to add GitLab.

## Still to implement (D2.8)

- Python `ForgeHost` body that speaks GitLab HTTP behind the schema above
- Live registration of `gitlab.com` (and checkout host) via
  `register_dyn_forge_host` / extension host claims
- Optional `omp.toml` once mounts and the Python surface are real

## Related

- Contract: `crates/forge` (`ForgeHost`, `ForgeOp`, `pr://` / `issue://`)
- Router: `crates/forge-delegated`
- ADR: `docs/adr/0049-tiered-integration-architecture.md` (D2.3 DONE for the
  bridge; D2.8 OPEN for this adapter)
