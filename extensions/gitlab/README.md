# gitlab — GitLab T2 forge backend (ADR 0049 D2.8)

Python package under `extensions/` (not a Cargo member). Speaks GitLab REST
`/api/v4` and maps onto `omp_forge` serde shapes.

**Catalog mounts** for `forge/<op>` moved to [`../forge/`](../forge/)
(`omp.forge`), which dispatches by `repo.host` to `omp_github` vs
`omp_gitlab`. This package no longer declares a `forge` parent (mount
collision with DynForgeAdapter fixed paths).

## What works

- **Core (envd, `cap-forge`)**: `LiveForgeDeviceClient` dispatches mounted
  `forge/<op>` leaves through `DeviceControlFactory::call_mounted` →
  `omp.devices.call`. Production `forge_router` registers **Dyn hosts** on the
  Dyn arm: always `gitlab.com` and `github.com`, plus operator convar
  `sv_forge_dyn_hosts` and adapter env `GITLAB_HOST` when set. A non-listed
  checkout host still lands on the Github arm (GHE) when it is not in the Dyn
  set. `CheckoutForge` uses the same bound client from `EnvServer`.
- **Backend read surface** (served when `omp.forge` routes here):
  - `repo_view`, `change_request_view`, `issue_view`, `change_request_diff`,
    `list_comments`
- **Backend write creates**:
  - `change_request_create` — POST `/projects/:id/merge_requests`
  - `issue_create` — POST `/projects/:id/issues`
- **Pure mapping** (`omp_gitlab.mapping`): fixture JSON → `ChangeRequest` /
  `Issue` / `Repository` / `Comment` dicts matching `omp_forge` serde, plus
  `ChangeRequestDraft` / `IssueDraft` → GitLab create request bodies. Unit
  tests under `tests/` (no network).

## Credentials (adapter data — ADR 0049 D2.7)

Prefer host-minted scoped creds when available. Local/dev adapter data:

| Env var | Meaning |
|---|---|
| `GITLAB_TOKEN` | Private token / job token (never commit) |
| `GITLAB_HOST` | API host, default `gitlab.com` |
| `GITLAB_API_URL` | Full API root override (`https://host/api/v4`) |

No tokens live in this tree.

## Unsupported / unmounted

Search, checkout, and push leaves are **not mounted**. Checkout / push need
honest `CheckoutReceipt` / `PushReceipt` (local worktree + git); ADR D2.4
keeps repository work in core, so this adapter does not fake them. Calls
through `DynForgeAdapter` for those paths become `ForgeError::Unavailable`.

## Remaining gaps

- Full automatic device-catalog host-claim discovery remains a smaller TODO
- Checkout / push still unmounted (core-owned)
- Search leaves
- In-core `github.rs` remains until checkout/push/CI/list-URL Dyn parity

## Proofs

```sh
python3 -m unittest discover -s extensions/gitlab/tests -v

# core
cargo check -p omp-envd --features cap-forge,cap-ci --locked
cargo nextest run -p omp-envd --features cap-forge,cap-ci \
  dyn_forge|forge_dyn_hosts|self_hosted_gitlab|checkout_host_in_dyn|github_com_is_registered_dyn
```

## Related

- Mount owner: [`../forge/`](../forge/)
- Schema: [`docs/adr/0049-forge-dyn-device-schema.md`](../../docs/adr/0049-forge-dyn-device-schema.md)
- Bridge: `crates/envd/src/github_adapter/dyn_forge.rs`
- Contract: `crates/forge`
