# gitlab — GitLab T2 forge adapter (ADR 0049 D2.8)

Python extension under `extensions/` (not a Cargo member). Implements the dyn
`forge/*` device schema for GitLab SaaS (`gitlab.com`) and speaks GitLab REST
`/api/v4`.

## What works

- **Core (envd, `cap-forge`)**: `LiveForgeDeviceClient` dispatches mounted
  `forge/<op>` leaves through `DeviceControlFactory::call_mounted` →
  `omp.devices.call`. Production `forge_router` registers `github.com` on the
  Github arm and **`gitlab.com` on the Dyn arm**. `CheckoutForge` uses the
  same bound client from `EnvServer`.
- **Extension read surface** (mounted at `extension_activate`):
  - `forge/change_request_view`
  - `forge/issue_view`
  - `forge/change_request_diff`
  - `forge/list_comments`
  - `forge/repo_view`
- **Pure mapping** (`omp_gitlab.mapping`): fixture JSON → `ChangeRequest` /
  `Issue` / `Repository` / `Comment` dicts matching `omp_forge` serde. Unit
  tests under `tests/` (no network).

## Credentials (adapter data — ADR 0049 D2.7)

Prefer host-minted scoped creds when available. Local/dev adapter data:

| Env var | Meaning |
|---|---|
| `GITLAB_TOKEN` | Private token / job token (never commit) |
| `GITLAB_HOST` | API host, default `gitlab.com` |
| `GITLAB_API_URL` | Full API root override (`https://host/api/v4`) |

No tokens live in this tree.

## Unsupported / write ops

Write, search, checkout, and push leaves are **not mounted**. Calls through
`DynForgeAdapter` for those paths become `ForgeError::Unavailable`. Do not
invent partial mutations.

## Remaining gaps

- Automatic host-claim discovery for self-hosted GitLab (non-`gitlab.com`) from
  the device catalog — still TODO at `forge_router`
- `forge@1` SessionHost composition still builds a separate unbound
  `LiveForgeDeviceClient` today; `CheckoutForge` / control bindings hold the
  production bound client
- Write ops; GitHub T2 parity + deletion of `envd/src/github.rs`
- Full contract stability still awaits GitHub T2 parity (ADR wording)

## Proofs

```sh
# mapping unit tests (no omp runtime)
python -m unittest discover -s extensions/gitlab/tests -v

# core
cargo check -p omp-envd --features cap-forge,cap-ci --locked
cargo nextest run -p omp-envd --features cap-forge,cap-ci dyn_forge
```

## Related

- Schema: [`docs/adr/0049-forge-dyn-device-schema.md`](../../docs/adr/0049-forge-dyn-device-schema.md)
- Bridge: `crates/envd/src/github_adapter/dyn_forge.rs`
- Contract: `crates/forge`
