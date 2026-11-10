# github — GitHub T2 forge backend (ADR 0049 D2.8)

Python package under `extensions/` (not a Cargo member). Speaks GitHub REST
(`api.github.com` / GHE `/api/v3`) and maps onto `omp_forge` serde shapes.

**Catalog mounts** for `forge/<op>` are **not** declared here. `DynForgeAdapter`
calls fixed paths; a second extension mounting the same leaves would collide
with GitLab. The single mount owner is [`../forge/`](../forge/)
(`omp.forge`), which dispatches by `repo.host` to `omp_github` vs
`omp_gitlab`.

## What works

- **Backend read surface** (served when `omp.forge` mounts and routes here):
  - `repo_view`, `change_request_view`, `issue_view`, `change_request_diff`,
    `list_comments`
- **Backend write creates**:
  - `change_request_create` — POST `/repos/:owner/:repo/pulls`
  - `issue_create` — POST `/repos/:owner/:repo/issues`
- **Pure mapping** (`omp_github.mapping`): fixture JSON → `ChangeRequest` /
  `Issue` / `Repository` / `Comment` dicts matching `omp_forge` serde, plus
  `ChangeRequestDraft` / `IssueDraft` → GitHub create request bodies. Unit
  tests under `tests/` (no network).
- **Core (envd, `cap-forge`)**: production `forge_router` registers
  `github.com` on the **Dyn** arm (same bound `LiveForgeDeviceClient` as
  GitLab). In-core `GithubForgeAdapter` / `github.rs` remain for CI, list-URL
  helpers, and checkout/push via `GithubService`. GHE checkout hosts that are
  not in the Dyn set (`sv_forge_dyn_hosts`) still use the Github arm.

## Credentials (adapter data — ADR 0049 D2.7)

Prefer host-minted scoped creds when available. Local/dev adapter data:

| Env var | Meaning |
|---|---|
| `GH_TOKEN` | PAT (wins when both token vars are set; never commit) |
| `GITHUB_TOKEN` | PAT fallback |
| `GITHUB_HOST` | API host, default `github.com` (set to GHE hostname) |
| `GITHUB_API_URL` | Full API root override (`https://host/api/v3`) |

Self-hosted GHE joins Dyn via operator convar `sv_forge_dyn_hosts` (same as
other dyn forges). No tokens live in this tree.

## Unsupported / unmounted

Search, checkout, and push leaves stay unmounted on Dyn (ADR D2.4: repository
work in core). In-core `github.rs` still performs checkout/push through
`GithubService` until Dyn parity allows deletion.

## Remaining gaps (blocks deleting `github.rs`)

- Checkout / push on Dyn (honest `CheckoutReceipt` / `PushReceipt`)
- T2 CI extension body + flipping `github.com` CI to Dyn (`ci/*` bridge is
  DONE — Actions still via in-core `GithubCiAdapter` for `github.com`)
- List-URL helpers / `GithubResolver` REST client still in-core
- Autocomplete `GithubRefs`, Class C scrapers / `gh` scrub stay

## Proofs

```sh
python3 -m unittest discover -s extensions/github/tests -v
```

## Related

- Mount owner: [`../forge/`](../forge/)
- Forge schema: [`docs/adr/0049-forge-dyn-device-schema.md`](../../docs/adr/0049-forge-dyn-device-schema.md)
- CI schema: [`docs/adr/0049-ci-dyn-device-schema.md`](../../docs/adr/0049-ci-dyn-device-schema.md)
- Bridges: `crates/envd/src/github_adapter/dyn_forge.rs`, `dyn_ci.rs`
- In-core (kept): `crates/envd/src/github.rs`, `github_url.rs`
