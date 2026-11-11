# ci — dyn `ci/*` catalog owner (ADR 0049)

Thin Python extension that **owns** the `ci/<op>` device mounts and dispatches
each call by provider host (`run.provider.id`) to:

| Backend package | Hosts |
|---|---|
| [`omp_github.ci`](../github/) | `github.com`, `GITHUB_HOST`, other Dyn hosts (GHE default) |
| [`omp_gitlab.ci`](../gitlab/) | `gitlab.com`, `GITLAB_HOST`, hostnames containing `gitlab` |

Adapter env `GITHUB_HOST` / `GITLAB_HOST` join the Dyn CI arm automatically
via the same envd `ForgeDynHosts` claim set as forge. `DynCiAdapter` calls
fixed paths (`ci/status`, `ci/log_stream`, `ci/capabilities`, …). A second
extension must **not** also mount those leaves — that is why mounts live
here, not under `omp.github` / `omp.gitlab`.

## Mounted leaves

- `ci/capabilities` — production ceiling (`log_stream` only; status always allowed)
- `ci/status` — Actions / GitLab pipeline → `Pipeline`
- `ci/log_stream` — job log (or concatenated failed-job logs) → `LogPage`

Unmounted: `artifacts`, `cancel`, `rerun_failed`, `rerun_job`, `trigger`,
`play_manual` → `CiError::Unavailable` via the live client.

## Credentials (adapter data)

Resolved by the selected backend — no secrets in this tree:

| Backend | Env |
|---|---|
| GitHub | `GH_TOKEN` / `GITHUB_TOKEN`, optional `GITHUB_REPOSITORY` / `OMP_CI_REPO` |
| GitLab | `GITLAB_TOKEN`, optional `CI_PROJECT_PATH` / `OMP_CI_REPO` |

Optional `args.repo` (`{host,owner,name}`) overrides env for tests / callers.

## Proofs

```sh
PYTHONPATH=extensions/ci/src:extensions/github/src:extensions/gitlab/src \
  python3 -m unittest discover -s extensions/ci/tests -v
python3 -m unittest discover -s extensions/github/tests -v
python3 -m unittest discover -s extensions/gitlab/tests -v
```
