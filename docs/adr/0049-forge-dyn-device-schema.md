# Dyn `forge/*` device schema (ADR 0049 D2.3)

Source of truth for the T2 forge device bridge. Rustdoc on
`omp_envd::github_adapter` (`dyn_forge`) and `omp-forge` point here.

T2 extensions (starting with GitLab under `extensions/gitlab/`) implement
`ForgeHost` by mounting dyn devices under `forge/<op>` and answering
`omp.devices.call`. Core holds a `DynForgeAdapter` that dispatches each
`ForgeHost` method to those paths. The production router keeps an enum
`ForgeBackend { Github(...), Dyn(...) }` so heterogeneous hosts stay
unboxed (AGENTS: enums before `dyn`).

A parallel `ci/*` dyn bridge is a follow-up; it is not part of this schema.

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
(e.g. `gitlab.com`) on the same `Delegated` builder as `github.com`.

`ForgeDeviceClient` is the injectable invoke surface (tests use a fake;
production will call mounted devices via `omp.devices.call`). Automatic
discovery from the extension device catalog / host claims is **not** wired
yet — `forge_router` documents a TODO at that hook point.

## Evidence

Unit tests in `crates/envd/src/github_adapter/dyn_forge.rs` prove Delegated
routes `gitlab.example` to the dyn adapter, which calls the fake client and
returns `ChangeRequest` / `Issue` for `view_change_request` / `view_issue`.
The `github.com` arm still wins for that host.

## What remains for GitLab T2 (D2.8)

- Python body under `extensions/gitlab/` that mounts these paths and speaks
  GitLab HTTP.
- Live `ForgeDeviceClient` wiring from envd device invoke / exthost claims.
- Optional `omp.toml` once those mounts are real.
