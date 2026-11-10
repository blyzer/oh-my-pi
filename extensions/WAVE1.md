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
**DONE:** self-hosted host registration + adapter host-claim discovery via
`sv_forge_dyn_hosts` + adapter env `GITLAB_HOST` / `GITHUB_HOST`
(`ForgeDynHosts`; forge and CI routers share the claim set — GHE via
`GITHUB_HOST` joins Dyn automatically). SessionHost bound-client threading for
`forge@1` / numbered `pr://` is DONE. **DONE:** Dyn checkout/push (D2.4) —
plan / push-prep from adapters, core `forge_worktree`. **DONE:** GitHub T2
read+create via dyn. **DONE:** `ci/*` dyn bridge + T2 CI extension body
(`extensions/ci/` mounts `status` / `log_stream` / `capabilities`; Actions +
GitLab CI backends). **`github.com` and `gitlab.com` CI are on the Dyn arm**
(same bound `LiveCiDeviceClient`; caps `log_stream` only). **DONE:** search
leaves (`forge/search_change_requests`, `forge/search_issues`) + list-URL /
`ForgeResolver` parity via `ForgeHost::search` (cached without etag).
**DONE:** in-core `github.rs` / `GithubService` / Github forge+CI adapters
deleted; forge/CI routers are Dyn-only. **DONE:** `github_url.rs`
neutralized to `forge_url.rs` (credentials / `pr://` / `issue://` /
`infer_repo`). Intentional residue: Class C, autocomplete `GithubRefs`.
**DONE:** crate module `github_adapter` → `forge_adapter` (`dyn_forge` /
`dyn_ci` filenames kept). **DONE:** conformance CI + first-cut manifest effect
checks (`just check-extensions` / `python3 scripts/check-extension-conformance.py`;
CI job `First-party extension conformance`). **DONE:** rescue wave 1 — eight
ports restored under `extensions/` against the frozen API (see
[`RESCUE.md`](RESCUE.md)); several keep documented Gaps (`state_dir` standing
in for removed typed journal/`omp.state` APIs). **DONE:** ADR 0046 item 4 —
chat host slot/overlay/`dynamic_mount` arms plus UI-bound ports
(`sidebar`, `hover-cards`, `welcome-chrome`). Phase 5 remainder is empty. See
package READMEs.

## Rescue wave 1

| Path | Role |
|---|---|
| [`pr-review/`](pr-review/) | Parallel model-tiered PR review |
| [`green-loop/`](green-loop/) | Affected-test feedback |
| [`workflows/`](workflows/) | Declarative subagent DAGs |
| [`remote-approve/`](remote-approve/) | Remote approval relay |
| [`search-provider/`](search-provider/) | Brave search provider |
| [`provider-pack/`](provider-pack/) | Extra provider catalog data |
| [`fuzzy-index/`](fuzzy-index/) | Warm fuzzy path/content index |
| [`spec-flow/`](spec-flow/) | Spec brainstorm→plan→apply→archive |

UI-bound ports (`sidebar`, `hover-cards`, `welcome-chrome`) restored with
ADR 0046 item 4 host arms (see [`RESCUE.md`](RESCUE.md)).

## Conformance gate

```sh
just check-extensions
# or: python3 scripts/check-extension-conformance.py
```

Manifests must declare closed-vocab `capabilities`. Hard `env.net.fetch`
applies to forge/ci/github/gitlab (and any package that declares network use),
**not** every `extensions/*` package. Package `tests/` run with the
PYTHONPATHs in each README. Frozen `omp.*` import smoke uses native stubs (no
live host); rescue public modules are import-clean under stubs; device modules
that bind at import are smuggled like forge/ci `devices` where feasible.
