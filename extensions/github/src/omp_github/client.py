"""GitHub REST client.

Credentials are adapter data (ADR 0049 D2.7): prefer minted scoped creds from
the host when available; for local/dev the adapter also accepts:

- ``GITHUB_TOKEN`` / ``GH_TOKEN`` — PAT (``GH_TOKEN`` wins when both set; never
  committed)
- ``GITHUB_HOST`` — API host, default ``github.com`` (GHE hostname when set)
- ``GITHUB_API_URL`` — full API root override (e.g. ``https://ghe/api/v3``)

No tokens are hardcoded. Missing credentials raise ``ForgeUnavailable``.
"""

from __future__ import annotations

import json
import os
import urllib.error
import urllib.request
from typing import Any, Mapping

from .errors import ForgeUnavailable
from .mapping import (
    issue_create_body,
    map_comments,
    map_issue,
    map_pull_request,
    map_repository,
    pull_request_create_body,
    repo_slug,
)

GITHUB_COM = "github.com"


def _host_from_repo(repo: Mapping[str, Any]) -> str:
    host = str(repo.get("host") or "").strip() or os.environ.get("GITHUB_HOST", GITHUB_COM)
    return host.rstrip("/")


def _api_root(host: str) -> str:
    override = os.environ.get("GITHUB_API_URL", "").strip()
    if override:
        return override.rstrip("/")
    if host.lower() == GITHUB_COM:
        return "https://api.github.com"
    return f"https://{host}/api/v3"


def _token(*, op: str) -> str:
    # Match envd / gh CLI precedence: GH_TOKEN then GITHUB_TOKEN.
    token = os.environ.get("GH_TOKEN", "").strip() or os.environ.get("GITHUB_TOKEN", "").strip()
    if not token:
        raise ForgeUnavailable(
            op,
            "GITHUB_TOKEN/GH_TOKEN unset; mint a scoped credential or export adapter data",
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
        "Authorization": f"Bearer {_token(op=op)}",
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": "omp-github/0.1",
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
            raise ForgeUnavailable(op, f"GitHub returned 404 for {url}") from error
        raise ForgeUnavailable(op, f"GitHub HTTP {error.code}") from error
    except urllib.error.URLError as error:
        raise ForgeUnavailable(op, "GitHub host unreachable") from error
    if not body:
        return None
    return json.loads(body.decode("utf-8"))


def _get_json(url: str, *, op: str) -> Any:
    return _request_json(url, op=op, method="GET")


def _post_json(url: str, payload: Mapping[str, Any], *, op: str) -> Any:
    return _request_json(url, op=op, method="POST", payload=payload)


def _repo_url(repo: Mapping[str, Any], *parts: str) -> str:
    host = _host_from_repo(repo)
    slug = repo_slug(str(repo.get("owner") or ""), str(repo.get("name") or ""))
    path = "/".join(("repos", slug, *parts))
    return f"{_api_root(host)}/{path}"


def _draft_repo(draft: Mapping[str, Any], *, op: str) -> dict[str, Any]:
    repo = draft.get("repo")
    if not isinstance(repo, dict):
        raise ForgeUnavailable(op, "draft.repo must be an object")
    return repo


def view_repo(repo: Mapping[str, Any]) -> dict[str, Any]:
    payload = _get_json(_repo_url(repo), op="repo_view")
    return map_repository(payload, repo)


def view_change_request(repo: Mapping[str, Any], number: int) -> dict[str, Any]:
    payload = _get_json(_repo_url(repo, "pulls", str(number)), op="change_request_view")
    return map_pull_request(payload, repo)


def view_issue(repo: Mapping[str, Any], number: int) -> dict[str, Any]:
    payload = _get_json(_repo_url(repo, "issues", str(number)), op="issue_view")
    return map_issue(payload, repo)


def change_request_diff(repo: Mapping[str, Any], number: int) -> str:
    """Fetch the unified diff for a pull request (``Accept: diff``)."""
    url = _repo_url(repo, "pulls", str(number))
    headers = {
        "Authorization": f"Bearer {_token(op='change_request_view')}",
        "Accept": "application/vnd.github.v3.diff",
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": "omp-github/0.1",
    }
    request = urllib.request.Request(url, headers=headers, method="GET")
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            body = response.read()
    except urllib.error.HTTPError as error:
        if error.code == 404:
            raise ForgeUnavailable(
                "change_request_view",
                f"GitHub returned 404 for {url}",
            ) from error
        raise ForgeUnavailable("change_request_view", f"GitHub HTTP {error.code}") from error
    except urllib.error.URLError as error:
        raise ForgeUnavailable("change_request_view", "GitHub host unreachable") from error
    return body.decode("utf-8")


def list_comments(repo: Mapping[str, Any], number: int, kind: str) -> list[dict[str, str]]:
    # GitHub uses the issues comments endpoint for both issues and PRs.
    op = "change_request_view" if kind in {"pr", "change_request"} else "issue_view"
    payload = _get_json(_repo_url(repo, "issues", str(number), "comments"), op=op)
    if not isinstance(payload, list):
        raise ForgeUnavailable(op, "GitHub comments payload was not a list")
    return map_comments(payload)


def create_change_request(draft: Mapping[str, Any]) -> dict[str, Any]:
    """POST a pull request from ``ChangeRequestDraft`` JSON."""
    op = "change_request_create"
    repo = _draft_repo(draft, op=op)
    payload = _post_json(_repo_url(repo, "pulls"), pull_request_create_body(draft), op=op)
    if not isinstance(payload, dict):
        raise ForgeUnavailable(op, "GitHub pull-request create payload was not an object")
    return map_pull_request(payload, repo)


def create_issue(draft: Mapping[str, Any]) -> dict[str, Any]:
    """POST an issue from ``IssueDraft`` JSON."""
    op = "issue_create"
    repo = _draft_repo(draft, op=op)
    payload = _post_json(_repo_url(repo, "issues"), issue_create_body(draft), op=op)
    if not isinstance(payload, dict):
        raise ForgeUnavailable(op, "GitHub issue create payload was not an object")
    return map_issue(payload, repo)
