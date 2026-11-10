# forge — dyn `forge/*` catalog owner (ADR 0049)

Thin Python extension that **owns** the `forge/<op>` device mounts and
dispatches each call by `repo.host` (and known dyn hosts) to:

| Backend package | Hosts |
|---|---|
| [`omp_github`](../github/) | `github.com`, `GITHUB_HOST`, other Dyn hosts (GHE default) |
| [`omp_gitlab`](../gitlab/) | `gitlab.com`, `GITLAB_HOST`, hostnames containing `gitlab` |

`DynForgeAdapter` calls fixed paths (`forge/change_request_view`, …). A second
extension must **not** also mount those leaves — that is why mounts moved here
out of `omp.gitlab`, and `omp.github` never declares a `forge` parent.

## Mounted leaves

Reads: `repo_view`, `change_request_view`, `issue_view`, `change_request_diff`,
`list_comments`.

Creates: `change_request_create`, `issue_create`.

Search / checkout / push stay unmounted (ADR D2.4).

## Proofs

```sh
PYTHONPATH=extensions/forge/src:extensions/github/src:extensions/gitlab/src \
  python3 -m unittest discover -s extensions/forge/tests -v
python3 -m unittest discover -s extensions/github/tests -v
python3 -m unittest discover -s extensions/gitlab/tests -v
```
