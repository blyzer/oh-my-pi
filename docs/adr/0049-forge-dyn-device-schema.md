# Dyn `forge/*` device schema (ADR 0049 D2.3)

Source of truth for the T2 forge device bridge. Rustdoc on
`omp_envd::github_adapter` (`dyn_forge`) and `omp-forge` point here.

T2 forge backends (`extensions/github/`, `extensions/gitlab/`) implement the
`ForgeHost` leaf set. **One** extension — `extensions/forge/` (`omp.forge`) —
mounts dyn devices under `forge/<op>` and host-dispatches by `repo.host` to
`omp_github` vs `omp_gitlab`. A second mount owner would collide:
`DynForgeAdapter` calls fixed paths. Core holds a `DynForgeAdapter` that
dispatches each `ForgeHost` method to those paths. The production router keeps
an enum `ForgeBackend { Github(...), Dyn(...) }` so heterogeneous hosts stay
unboxed (AGENTS: enums before `dyn`).

The parallel `ci/*` dyn bridge is documented in
[`0049-ci-dyn-device-schema.md`](0049-ci-dyn-device-schema.md) (DONE).

## Device paths

Primary paths are `forge/` + [`ForgeOp`](../../crates/forge/src/op.rs) snake_case
(`strum` / serde). Two `ForgeHost` methods share an op for approval but use
distinct paths so an extension can implement them separately.

| Device path | `ForgeOp` (approval) | `ForgeHost` method | Args (JSON) | Result (JSON) |
|---|---|---|---|---|
| `forge/repo_view` | `repo_view` | `view_repo` | `{ "repo": RepoRef }` | `Repository` |
| `forge/file_read` | `file_read` | `read_file` | `{ "query": FileQuery }` | `FileBody` (`content` as serde bytes) |
| `forge/change_request_view` | `change_request_view` | `view_change_request` | `{ "repo": RepoRef, "number": u64 }` | `ChangeRequest` |
| `forge/change_request_diff` | `change_request_view` | `change_request_diff` | `{ "repo": RepoRef, "number": u64 }` | string (`Str`) |
| `forge/issue_view` | `issue_view` | `view_issue` | `{ "repo": RepoRef, "number": u64 }` | `Issue` |
| `forge/list_comments` | `change_request_view` or `issue_view` from `kind` | `list_comments` | `{ "repo": RepoRef, "number": u64, "kind": ResourceKind }` | `Comment[]` |
| `forge/list_checks` | `list_checks` | `list_checks` | `{ "repo": RepoRef, "target": CheckTarget }` | `ForgeChecks` |
| `forge/search_change_requests` | `search_change_requests` | `search` | `{ "query": SearchQuery }` | `SearchPage` |
| `forge/search_issues` | `search_issues` | `search` | `{ "query": SearchQuery }` | `SearchPage` |
| `forge/search_code` | `search_code` | `search` | `{ "query": SearchQuery }` | `SearchPage` |
| `forge/search_commits` | `search_commits` | `search` | `{ "query": SearchQuery }` | `SearchPage` |
| `forge/search_repos` | `search_repos` | `search` | `{ "query": SearchQuery }` | `SearchPage` |
| `forge/change_request_create` | `change_request_create` | `create_change_request` | `{ "draft": ChangeRequestDraft }` | `ChangeRequest` |
| `forge/change_request_checkout` | `change_request_checkout` | `checkout_change_request` | `{ "request": CheckoutRequest }` | `CheckoutReceipt` |
| `forge/change_request_push` | `change_request_push` | `push_change_request` | `{ "request": PushRequest }` | `PushReceipt` |
| `forge/issue_create` | `issue_create` | `create_issue` | `{ "draft": IssueDraft }` | `Issue` |

Types are the serde shapes of `omp_forge`. Prefer those structs; do not invent
a parallel DTO vocabulary unless a field cannot serialize (none today except
documenting `FileBody.content` as bytes).

## Registration seam

`register_dyn_forge_host(builder, host, client)` registers
`ForgeBackend::Dyn(DynForgeAdapter::new(client))` for an ASCII host name
(e.g. `gitlab.com`, `github.com`).

`ForgeDeviceClient` is the injectable invoke surface. Production uses
`LiveForgeDeviceClient` (late-bound `ForgeDeviceInvoker`);
`DeviceControlFactory::forge_invoker` looks up mounted leaves and dispatches
`omp.devices.call` (same destination as `DeviceControlFactory::invoke`, via
`NestedCallbackDispatcher::dispatch_host`). Tests may inject a fake client or
a recording invoker. `forge_router` always registers `gitlab.com` and
`github.com` on the Dyn arm. Automatic discovery of additional hosts from the
extension device catalog / host claims is **not** wired yet — TODO at that
hook point.

## Evidence

Unit tests in `crates/envd/src/github_adapter/dyn_forge.rs` prove Delegated
routes dyn hosts through a fake / live client and returns `ChangeRequest` /
`Issue` for `view_change_request` / `view_issue`. `forge_router` registers
`ForgeDynHosts` (always `gitlab.com` + `github.com`, plus `sv_forge_dyn_hosts`
/ `GITLAB_HOST`) even when the live client is unbound (`Unavailable`, not
`Unregistered`). Unlisted checkout hosts still land on the in-core Github arm
(GHE).

## GitLab + GitHub T2 (D2.8) status

- **Done (catalog mounts):** `extensions/forge/` owns `forge/*` leaves and
  host-dispatches to `omp_github` / `omp_gitlab`. Backends do not declare a
  competing `forge` parent.
- **Done (GitLab read + create):** `extensions/gitlab/` maps GitLab REST onto
  `omp_forge` shapes; credentials `GITLAB_TOKEN` / `GITLAB_HOST` /
  `GITLAB_API_URL`.
- **Done (GitHub read + create):** `extensions/github/` maps GitHub REST onto
  the same leaf set; credentials `GH_TOKEN` / `GITHUB_TOKEN` / `GITHUB_HOST` /
  `GITHUB_API_URL`. `github.com` is on the Dyn arm.
- **Done (self-hosted host registration):** envd `ForgeDynHosts` registers Dyn
  hosts from always-`gitlab.com` + always-`github.com`, operator convar
  `sv_forge_dyn_hosts`, and adapter env `GITLAB_HOST`. Checkout / inferred
  hosts in that set route to Dyn; other unlisted hosts still assume GHE on the
  Github arm. Full automatic device-catalog host-claim discovery remains a
  smaller TODO.
- **DONE (search leaves + list-URL parity):** `extensions/forge/` mounts
  `forge/search_change_requests` and `forge/search_issues` (host-dispatch to
  `omp_github` / `omp_gitlab`). `search_code` / commits / repos stay
  unmounted → Unavailable. List URLs (`issue://owner/repo?state=open`,
  `pr://…`) resolve through `ForgeHost::search` (neutral `state:` /
  `author:` / `label:` tokens in `SearchQuery.text`); rendered markdown is
  cached **without** etag (search has no HTTP validator). T2 CI +
  `github.com` CI on Dyn is DONE (see
  [`0049-ci-dyn-device-schema.md`](0049-ci-dyn-device-schema.md)). SessionHost
  bound-client threading for `forge@1` is DONE.
- **Remaining before deleting `envd/src/github.rs`:** Dyn checkout/push (ADR
  D2.4). In-core `GithubForgeAdapter` / `GithubCiAdapter` / `github.rs` /
  `github_url.rs` stay for checkout/push via `GithubService` and unlisted
  GHE CI.
