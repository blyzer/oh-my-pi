# github — GitHub T2 forge + Actions CI backend (ADR 0049)

Python package under `extensions/` (not a Cargo member). Speaks GitHub REST
(`api.github.com` / GHE `/api/v3`) and maps onto `omp_forge` / `omp_ci` serde
shapes.

**Catalog mounts** for `forge/<op>` and `ci/<op>` are **not** declared here.
`DynForgeAdapter` / `DynCiAdapter` call fixed paths; a second extension
mounting the same leaves would collide. Mount owners:

- Forge: [`../forge/`](../forge/) (`omp.forge`)
- CI: [`../ci/`](../ci/) (`omp.ci`)

## What works

- **Backend read surface** (served when `omp.forge` mounts and routes here):
  - `repo_view`, `change_request_view`, `issue_view`, `change_request_diff`,
    `list_comments`
- **Backend write creates**:
  - `change_request_create` — POST `/repos/:owner/:repo/pulls`
  - `issue_create` — POST `/repos/:owner/:repo/issues`
- **Backend search / list** (`search` → `SearchPage`):
  - `search_change_requests` / `search_issues` — repo list REST when filters
    are only `state:` / `author:` / `label:` (list-URL path); Search API when
    free-text remains. Other kinds stay Unavailable.
- **CI backend** (`omp_github.ci`, served when `omp.ci` mounts):
  - `status` — Actions run + jobs → `Pipeline`
  - `log_stream` — job logs (or concatenated failed-job logs) → `LogPage`
  - `capabilities` — `log_stream` only
- **Pure mapping** (`omp_github.mapping` / `omp_github.ci_mapping`): fixture
  JSON → forge / CI / `SearchPage` shapes. Unit tests under `tests/` (no network).
- **Core (envd)**: production `forge_router` and `ci_router` register
  `github.com` on the **Dyn** arm (shared bound live clients). List URLs go
  through `ForgeHost::search`. In-core `github.rs` / Github adapters are
  deleted; unlisted checkout hosts also land on Dyn.

## Credentials (adapter data — ADR 0049 D2.7)

Prefer host-minted scoped creds when available. Local/dev adapter data:

| Env var | Meaning |
|---|---|
| `GH_TOKEN` | PAT (wins when both token vars are set; never commit) |
| `GITHUB_TOKEN` | PAT fallback |
| `GITHUB_HOST` | API host, default `github.com` (set to GHE hostname) |
| `GITHUB_API_URL` | Full API root override (`https://host/api/v3`) |
| `GITHUB_REPOSITORY` / `OMP_CI_REPO` | `owner/name` for CI when `args.repo` omitted |

Self-hosted GHE joins Dyn via operator convar `sv_forge_dyn_hosts` (forge) /
checkout-host Dyn registration (CI). No tokens live in this tree.

## Unsupported / unmounted

`search_code` / `search_commits` / `search_repos` stay unmounted.
Checkout / push return plan / push-prep JSON via the forge catalog mounts
(ADR 0049 D2.4); no local git in this package. CI write ops (`cancel`,
`rerun_*`, `trigger`, `play_manual`) and `artifacts` stay unmounted;
registration ceilings remain `log_stream` only.

## Intentional residue (not blocking)

- Autocomplete `GithubRefs`, Class C scrapers / `gh` scrub stay by design

## Proofs

```sh
python3 -m unittest discover -s extensions/github/tests -v
```

## Related

- Mount owners: [`../forge/`](../forge/), [`../ci/`](../ci/)
- Schemas: [`docs/adr/0049-forge-dyn-device-schema.md`](../../docs/adr/0049-forge-dyn-device-schema.md),
  [`docs/adr/0049-ci-dyn-device-schema.md`](../../docs/adr/0049-ci-dyn-device-schema.md)
- Bridges: `crates/envd/src/forge_adapter/dyn_forge.rs`, `dyn_ci.rs`
- Scheme/credential surface: `crates/envd/src/forge_url.rs` (historically
  `github_url.rs`)
