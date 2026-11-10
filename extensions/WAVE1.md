# Wave-1 first-party extensions

Owner decision 2026-11-10: home = in-repo `extensions/`; owners = repo
maintainers / this tree.

## Starts with

| Extension | Role | ADR |
|---|---|---|
| **forge** | Sole `forge/*` catalog owner; host-dispatches to GitHub / GitLab backends | [0049](../docs/adr/0049-tiered-integration-architecture.md), [schema](../docs/adr/0049-forge-dyn-device-schema.md) |
| **ci** | Sole `ci/*` catalog owner; host-dispatches to GitHub Actions / GitLab CI | [schema](../docs/adr/0049-ci-dyn-device-schema.md) |
| **github** | GitHub T2 forge + Actions backend (`omp_github`) via dyn `forge/*` / `ci/*` | same |
| **gitlab** | GitLab T2 forge + CI backend (`omp_gitlab`) via dyn `forge/*` / `ci/*` | same |

**Mount collision (forge):** `DynForgeAdapter` calls fixed paths `forge/<op>`.
Only [`forge/`](forge/) (`omp.forge`) declares the `forge` parent and mounts
leaves. [`github/`](github/) and [`gitlab/`](gitlab/) are backends — no
competing `forge` parent.

**Mount collision (ci):** `DynCiAdapter` calls fixed paths `ci/<op>`. Only
[`ci/`](ci/) (`omp.ci`) declares the `ci` parent and mounts leaves. Backends
live under `omp_github.ci` / `omp_gitlab.ci` — no competing `ci` parent.

**D2.8 progress:** read-path + write-create adapter bodies for **both** GitLab
and GitHub, plus live `ForgeDeviceClient` wiring. Mapping unit tests cover
fixture → `ChangeRequest`/`Issue` shapes and draft → create bodies. Mounted
creates: `forge/change_request_create`, `forge/issue_create`. **`gitlab.com`
and `github.com` are registered on the dyn forge arm** (same bound client).
**DONE:** self-hosted host registration via `sv_forge_dyn_hosts` + adapter env
`GITLAB_HOST` (checkout hosts in that set route to Dyn; GHE listed there also
routes to Dyn / `omp_github`). SessionHost bound-client threading for
`forge@1` / numbered `pr://` is DONE. Checkout / push stay unmounted (ADR
D2.4). **DONE:** GitHub T2 read+create via dyn. **DONE:** `ci/*` dyn bridge +
T2 CI extension body (`extensions/ci/` mounts `status` / `log_stream` /
`capabilities`; Actions + GitLab CI backends). **`github.com` and
`gitlab.com` CI are on the Dyn arm** (same bound `LiveCiDeviceClient`; caps
`log_stream` only). In-core `github.rs` / `github_url.rs` **remain** until
checkout/push + list-URL Dyn parity allow deletion. Remaining: search leaves;
full automatic device-catalog host-claim discovery (smaller TODO). See package
READMEs.

## May grow

Further wave-1 ports (rescued v1 surfaces, other CI vendors, BuildBuddy behind
execution `Delegated`, …) are TBD. Candidates from ADR 0049 D5 remain:
`pr-review`, `green-loop`, `workflows`, `remote-approve`, `search-provider`,
`provider-pack`, `fuzzy-index`, `spec-flow`. UI-bound ports wait for ADR 0046
item 4.
