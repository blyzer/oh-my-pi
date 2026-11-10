# gitlab — GitLab T2 forge + CI backend (ADR 0049)

Python package under `extensions/` (not a Cargo member). Speaks GitLab REST
`/api/v4` and maps onto `omp_forge` / `omp_ci` serde shapes.

**Catalog mounts** for `forge/<op>` and `ci/<op>` live in
[`../forge/`](../forge/) (`omp.forge`) and [`../ci/`](../ci/) (`omp.ci`).
This package does not declare competing parents (Dyn adapter fixed-path
collision).

## What works

- **Core (envd, `cap-forge` / `cap-ci`)**: live device clients dispatch mounted
  leaves. Production routers register **`gitlab.com` on the Dyn arm** for both
  forge and CI (same bound clients as `github.com`).
- **Backend read surface** (served when `omp.forge` routes here):
  - `repo_view`, `change_request_view`, `issue_view`, `change_request_diff`,
    `list_comments`
- **Backend write creates**:
  - `change_request_create` — POST `/projects/:id/merge_requests`
  - `issue_create` — POST `/projects/:id/issues`
- **Backend search / list** (`search` → `SearchPage`):
  - `search_change_requests` / `search_issues` — project list REST with
    neutral `state:` / `author:` / `label:` filters (and optional free-text
    `search=`). Other kinds stay Unavailable.
- **CI backend** (`omp_gitlab.ci`, served when `omp.ci` mounts):
  - `status` — pipeline + jobs → `Pipeline`
  - `log_stream` — job trace (or concatenated failed-job traces) → `LogPage`
  - `capabilities` — `log_stream` only
- **Pure mapping** (`omp_gitlab.mapping` / `omp_gitlab.ci_mapping`): fixture
  JSON → forge / CI / `SearchPage` shapes. Unit tests under `tests/` (no network).

## Credentials (adapter data — ADR 0049 D2.7)

Prefer host-minted scoped creds when available. Local/dev adapter data:

| Env var | Meaning |
|---|---|
| `GITLAB_TOKEN` | Private token / job token (never commit) |
| `GITLAB_HOST` | API host, default `gitlab.com` |
| `GITLAB_API_URL` | Full API root override (`https://host/api/v4`) |
| `CI_PROJECT_PATH` / `OMP_CI_REPO` | `group/project` for CI when `args.repo` omitted |

No tokens live in this tree.

## Unsupported / unmounted

`search_code` / `search_commits` / `search_repos`, checkout, and push are
**not mounted**. CI write ops and `artifacts` stay unmounted; registration
ceilings remain `log_stream` only.

## Remaining gaps

- Full automatic device-catalog host-claim discovery remains a smaller TODO
- Checkout / push return plan / push-prep via `extensions/forge/` mounts (ADR D2.4; no local git here)
- In-core `github.rs` deleted; `github_url.rs` neutralized to `forge_url.rs`;
  crate module `github_adapter` → `forge_adapter` done; residue is Class C /
  `GithubRefs`

## Proofs

```sh
python3 -m unittest discover -s extensions/gitlab/tests -v

# core
cargo check -p omp-envd --features cap-forge,cap-ci --locked
cargo nextest run -p omp-envd --features cap-forge,cap-ci \
  dyn_ci|github_com_ci_is_registered_dyn|gitlab_com_ci_is_registered_dyn
```

## Related

- Mount owners: [`../forge/`](../forge/), [`../ci/`](../ci/)
- Schemas: [`docs/adr/0049-forge-dyn-device-schema.md`](../../docs/adr/0049-forge-dyn-device-schema.md),
  [`docs/adr/0049-ci-dyn-device-schema.md`](../../docs/adr/0049-ci-dyn-device-schema.md)
- Bridges: `crates/envd/src/forge_adapter/dyn_forge.rs`, `dyn_ci.rs`
- Contracts: `crates/forge`, `crates/ci`
