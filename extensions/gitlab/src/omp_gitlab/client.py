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
    map_issue,
    map_merge_request,
    map_notes,
    map_project,
    project_path,
)


def _host_from_repo(repo: Mapping[str, Any]) -> str:
    host = str(repo.get("host") or "").strip() or os.environ.get("GITLAB_HOST", "gitlab.com")
    return host.rstrip("/")


def _api_root(host: str) -> str:
    override = os.environ.get("GITLAB_API_URL", "").strip()
    if override:
        return override.rstrip("/")
    return f"https://{host}/api/v4"


def _token() -> str:
    token = os.environ.get("GITLAB_TOKEN", "").strip()
    if not token:
        raise ForgeUnavailable(
            "repo_view",
            "GITLAB_TOKEN unset; mint a scoped credential or export adapter data",
        )
    return token


def _get_json(url: str, *, op: str) -> Any:
    request = urllib.request.Request(
        url,
        headers={
            "PRIVATE-TOKEN": _token(),
            "Accept": "application/json",
            "User-Agent": "omp-gitlab/0.1",
        },
        method="GET",
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            body = response.read()
    except urllib.error.HTTPError as error:
        if error.code == 404:
            raise ForgeUnavailable(op, f"GitLab returned 404 for {url}") from error
        raise ForgeUnavailable(op, f"GitLab HTTP {error.code}") from error
    except urllib.error.URLError as error:
        raise ForgeUnavailable(op, "GitLab host unreachable") from error
    return json.loads(body.decode("utf-8"))


def _project_url(repo: Mapping[str, Any], *parts: str, query: Mapping[str, str] | None = None) -> str:
    host = _host_from_repo(repo)
    encoded = project_path(str(repo.get("owner") or ""), str(repo.get("name") or ""))
    path = "/".join(("projects", encoded, *parts))
    url = f"{_api_root(host)}/{path}"
    if query:
        url = f"{url}?{urlencode(query)}"
    return url


def view_repo(repo: Mapping[str, Any]) -> dict[str, Any]:
    payload = _get_json(_project_url(repo), op="repo_view")
    return map_project(payload, repo)


def view_change_request(repo: Mapping[str, Any], number: int) -> dict[str, Any]:
    payload = _get_json(
        _project_url(repo, "merge_requests", str(number)),
        op="change_request_view",
    )
    return map_merge_request(payload, repo)


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
