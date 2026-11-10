# gitlab — GitLab T2 forge adapter (ADR 0049 D2.8)

Python extension under `extensions/` (not a Cargo member). Implements the dyn
`forge/*` device schema for GitLab SaaS (`gitlab.com`) and speaks GitLab REST
`/api/v4`.

## What works

- **Core (envd, `cap-forge`)**: `LiveForgeDeviceClient` dispatches mounted
  `forge/<op>` leaves through `DeviceControlFactory::call_mounted` →
  `omp.devices.call`. Production `forge_router` registers `github.com` on the
  Github arm and **Dyn hosts** on the Dyn arm: always `gitlab.com`, plus
  operator convar `sv_forge_dyn_hosts` (comma/whitespace-separated) and
  adapter env `GITLAB_HOST` when set. A non-github checkout host joins Dyn
  when it is in that set; otherwise it still lands on the Github arm (GHE).
  `CheckoutForge` uses the same bound client from `EnvServer`.
- **Extension read surface** (mounted at `extension_activate`):
  - `forge/change_request_view`
  - `forge/issue_view`
  - `forge/change_request_diff`
  - `forge/list_comments`
  - `forge/repo_view`
- **Extension write creates** (mounted at `extension_activate`):
  - `forge/change_request_create` — POST `/projects/:id/merge_requests`
  - `forge/issue_create` — POST `/projects/:id/issues`
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

- **DONE:** Self-hosted host registration via `sv_forge_dyn_hosts` +
  `GITLAB_HOST` (checkout / inferred hosts in that set route to Dyn; not the
  Github arm). Full automatic device-catalog host-claim discovery remains a
  smaller TODO.
- **DONE:** SessionHost / URL-resolver `forge_router` share the session’s bound
  `LiveForgeDeviceClient` with CheckoutForge / CONTROL (no second unbound client)
- **DONE:** Write creates (`forge/change_request_create`, `forge/issue_create`)
- Checkout / push still unmounted (core-owned; no honest receipt without local git)
- Search leaves; GitHub T2 parity + deletion of `envd/src/github.rs`
- Full contract stability still awaits GitHub T2 parity (ADR wording)

## Proofs

```sh
# mapping unit tests (no omp runtime)
python -m unittest discover -s extensions/gitlab/tests -v

# core
cargo check -p omp-envd --features cap-forge,cap-ci --locked
cargo nextest run -p omp-envd --features cap-forge,cap-ci \
  dyn_forge|forge_dyn_hosts|self_hosted_gitlab|checkout_host_in_dyn
```

## Related

- Schema: [`docs/adr/0049-forge-dyn-device-schema.md`](../../docs/adr/0049-forge-dyn-device-schema.md)
- Bridge: `crates/envd/src/github_adapter/dyn_forge.rs`
- Contract: `crates/forge`
