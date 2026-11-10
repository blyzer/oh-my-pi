"""GitLab REST client.

Credentials are adapter data (ADR 0049 D2.7): prefer minted scoped creds from
the host when available; for local/dev the adapter also accepts:

- ``GITLAB_TOKEN`` — personal access token / job token (never committed)
- ``GITLAB_HOST`` — API host, default ``gitlab.com``
- ``GITLAB_API_URL`` — full API root override (e.g. ``https://gitlab.example/api/v4``)

No tokens are hardcoded. Missing credentials raise ``ForgeUnavailable``.
"""

from __future__ import annotations

import json
import os
import urllib.error
import urllib.request
from typing import Any, Mapping
from urllib.parse import urlencode

from .errors import ForgeUnavailable
from .mapping import (
    issue_create_body,
    list_state_param,
    map_checkout_plan,
    map_issue,
    map_merge_request,
    map_notes,
    map_project,
    map_push_prep,
    map_search_page,
    merge_request_create_body,
    project_path,
    split_list_filters,
)


def _host_from_repo(repo: Mapping[str, Any]) -> str:
    host = str(repo.get("host") or "").strip() or os.environ.get("GITLAB_HOST", "gitlab.com")
    return host.rstrip("/")


def _api_root(host: str) -> str:
    override = os.environ.get("GITLAB_API_URL", "").strip()
    if override:
        return override.rstrip("/")
    return f"https://{host}/api/v4"


def _token(*, op: str) -> str:
    token = os.environ.get("GITLAB_TOKEN", "").strip()
    if not token:
        raise ForgeUnavailable(
            op,
            "GITLAB_TOKEN unset; mint a scoped credential or export adapter data",
        )
    return token


def _request_json(
    url: str,
    *,
    op: str,
    method: str = "GET",
    payload: Mapping[str, Any] | None = None,
) -> Any:
    headers = {
        "PRIVATE-TOKEN": _token(op=op),
        "Accept": "application/json",
        "User-Agent": "omp-gitlab/0.1",
    }
    data: bytes | None = None
    if payload is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            body = response.read()
    except urllib.error.HTTPError as error:
        if error.code == 404:
            raise ForgeUnavailable(op, f"GitLab returned 404 for {url}") from error
        raise ForgeUnavailable(op, f"GitLab HTTP {error.code}") from error
    except urllib.error.URLError as error:
        raise ForgeUnavailable(op, "GitLab host unreachable") from error
    if not body:
        return None
    return json.loads(body.decode("utf-8"))


def _get_json(url: str, *, op: str) -> Any:
    return _request_json(url, op=op, method="GET")


def _post_json(url: str, payload: Mapping[str, Any], *, op: str) -> Any:
    return _request_json(url, op=op, method="POST", payload=payload)


def _project_url(repo: Mapping[str, Any], *parts: str, query: Mapping[str, str] | None = None) -> str:
    host = _host_from_repo(repo)
    encoded = project_path(str(repo.get("owner") or ""), str(repo.get("name") or ""))
    path = "/".join(("projects", encoded, *parts))
    url = f"{_api_root(host)}/{path}"
    if query:
        url = f"{url}?{urlencode(query)}"
    return url


def _draft_repo(draft: Mapping[str, Any], *, op: str) -> dict[str, Any]:
    repo = draft.get("repo")
    if not isinstance(repo, dict):
        raise ForgeUnavailable(op, "draft.repo must be an object")
    return repo


def view_repo(repo: Mapping[str, Any]) -> dict[str, Any]:
    payload = _get_json(_project_url(repo), op="repo_view")
    return map_project(payload, repo)


def view_change_request(repo: Mapping[str, Any], number: int) -> dict[str, Any]:
    payload = _get_json(
        _project_url(repo, "merge_requests", str(number)),
        op="change_request_view",
    )
    return map_merge_request(payload, repo)


def checkout_change_request_plan(repo: Mapping[str, Any], number: int) -> dict[str, Any]:
    """REST → ``CheckoutPlan`` JSON (no local git; core owns the worktree)."""
    mr = _get_json(
        _project_url(repo, "merge_requests", str(number)),
        op="change_request_checkout",
    )
    if not isinstance(mr, dict):
        raise ForgeUnavailable("change_request_checkout", "GitLab MR payload was not an object")
    project = _get_json(_project_url(repo), op="change_request_checkout")
    if not isinstance(project, dict):
        raise ForgeUnavailable("change_request_checkout", "GitLab project payload was not an object")
    return map_checkout_plan(mr, repo, project=project)


def push_change_request_prep(repo: Mapping[str, Any], number: int) -> dict[str, Any]:
    """REST → ``PushPrep`` JSON (no local git; core owns the push)."""
    mr = _get_json(
        _project_url(repo, "merge_requests", str(number)),
        op="change_request_push",
    )
    if not isinstance(mr, dict):
        raise ForgeUnavailable("change_request_push", "GitLab MR payload was not an object")
    project = _get_json(_project_url(repo), op="change_request_push")
    if not isinstance(project, dict):
        raise ForgeUnavailable("change_request_push", "GitLab project payload was not an object")
    return map_push_prep(mr, repo, project=project)


def view_issue(repo: Mapping[str, Any], number: int) -> dict[str, Any]:
    payload = _get_json(_project_url(repo, "issues", str(number)), op="issue_view")
    return map_issue(payload, repo)


def change_request_diff(repo: Mapping[str, Any], number: int) -> str:
    payload = _get_json(
        _project_url(repo, "merge_requests", str(number), "changes"),
        op="change_request_view",
    )
    changes = payload.get("changes") or []
    chunks: list[str] = []
    for change in changes:
        diff = change.get("diff")
        if isinstance(diff, str) and diff:
            old = change.get("old_path") or change.get("new_path") or "file"
            new = change.get("new_path") or old
            chunks.append(f"diff --git a/{old} b/{new}\n{diff}")
    return "\n".join(chunks)


def list_comments(repo: Mapping[str, Any], number: int, kind: str) -> list[dict[str, str]]:
    if kind in {"pr", "change_request"}:
        url = _project_url(repo, "merge_requests", str(number), "notes")
        op = "change_request_view"
    else:
        url = _project_url(repo, "issues", str(number), "notes")
        op = "issue_view"
    payload = _get_json(url, op=op)
    if not isinstance(payload, list):
        raise ForgeUnavailable(op, "GitLab notes payload was not a list")
    return map_notes(payload)


def create_change_request(draft: Mapping[str, Any]) -> dict[str, Any]:
    """POST a merge request from ``ChangeRequestDraft`` JSON."""
    op = "change_request_create"
    repo = _draft_repo(draft, op=op)
    payload = _post_json(
        _project_url(repo, "merge_requests"),
        merge_request_create_body(draft),
        op=op,
    )
    if not isinstance(payload, dict):
        raise ForgeUnavailable(op, "GitLab merge-request create payload was not an object")
    return map_merge_request(payload, repo)


def create_issue(draft: Mapping[str, Any]) -> dict[str, Any]:
    """POST an issue from ``IssueDraft`` JSON."""
    op = "issue_create"
    repo = _draft_repo(draft, op=op)
    payload = _post_json(
        _project_url(repo, "issues"),
        issue_create_body(draft),
        op=op,
    )
    if not isinstance(payload, dict):
        raise ForgeUnavailable(op, "GitLab issue create payload was not an object")
    return map_issue(payload, repo)


def _query_repo(query: Mapping[str, Any], *, op: str) -> dict[str, Any]:
    repo = query.get("repo")
    if not isinstance(repo, dict):
        raise ForgeUnavailable(op, "query.repo must be an object")
    return repo


def _clamp_limit(raw: Any) -> int:
    try:
        limit = int(raw or 0)
    except (TypeError, ValueError):
        limit = 0
    if limit <= 0:
        return 30
    return min(limit, 100)


def search(query: Mapping[str, Any]) -> dict[str, Any]:
    """Search or list issues / merge requests → ``SearchPage``.

    Neutral list-URL filters with no free-text remainder use project list
    endpoints. Free-text uses GitLab search scoped to the project. Other
    ``SearchKind`` values stay Unavailable (leaves unmounted).
    """
    kind = str(query.get("kind") or "").strip().lower()
    if kind in {"change_requests", "search_change_requests"}:
        op = "search_change_requests"
        page_kind = "change_requests"
        family = "merge_requests"
    elif kind in {"issues", "search_issues"}:
        op = "search_issues"
        page_kind = "issues"
        family = "issues"
    else:
        raise ForgeUnavailable(
            "search_issues",
            f"GitLab dyn search does not mount kind={kind!r}",
        )

    repo = _query_repo(query, op=op)
    filters, freeform = split_list_filters(str(query.get("text") or ""))
    limit = _clamp_limit(query.get("limit"))
    params: dict[str, str] = {
        "state": list_state_param(filters.get("state", "open")),
        "per_page": str(limit),
    }
    if author := filters.get("author"):
        params["author_username"] = author
    if label := filters.get("label"):
        params["labels"] = label
    if freeform:
        params["search"] = freeform

    payload = _get_json(_project_url(repo, family, query=params), op=op)
    if not isinstance(payload, list):
        raise ForgeUnavailable(op, f"GitLab {family} list payload was not a list")
    return map_search_page(page_kind, payload, repo)
