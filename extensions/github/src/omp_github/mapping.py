"""Pure GitHub REST JSON → ``omp_forge`` serde shapes.

No network. Unit-tested with fixture payloads under ``tests/fixtures/``.
Field names match ``crates/forge`` serde (snake_case) and the in-core
``GithubForgeAdapter`` mappers in ``crates/envd/src/github_adapter``.
"""

from __future__ import annotations

from typing import Any, Mapping
from urllib.parse import quote


def repo_slug(owner: str, name: str) -> str:
    """``owner/name`` path segment for ``/repos/:owner/:name``."""
    return f"{quote(owner, safe='')}/{quote(name, safe='')}"


def _str(value: Any, default: str = "") -> str:
    if value is None:
        return default
    return str(value)


def _repo_ref(repo: Mapping[str, Any]) -> dict[str, str]:
    return {
        "host": _str(repo.get("host")),
        "owner": _str(repo.get("owner")),
        "name": _str(repo.get("name")),
    }


def map_request_state(state: str | None, *, merged: bool = False) -> str:
    """GitHub PR ``state`` / ``merged`` → ``RequestState``."""
    if merged:
        return "merged"
    raw = (state or "").lower()
    if raw == "closed":
        return "closed"
    return "open"


def map_issue_state(state: str | None) -> str:
    """GitHub issue ``state`` → ``IssueState``."""
    raw = (state or "").lower()
    if raw == "closed":
        return "closed"
    return "open"


def map_pull_request(
    payload: Mapping[str, Any],
    repo: Mapping[str, Any],
) -> dict[str, Any]:
    """Map one GitHub pull-request JSON object to ``ChangeRequest``."""
    base = payload.get("base") or {}
    head = payload.get("head") or {}
    return {
        "repo": _repo_ref(repo),
        "number": int(payload.get("number") or 0),
        "title": _str(payload.get("title")),
        "body": _str(payload.get("body")),
        "state": map_request_state(
            _str(payload.get("state")),
            merged=bool(payload.get("merged")),
        ),
        "draft": bool(payload.get("draft")),
        "base_ref": _str(base.get("ref")),
        "head_ref": _str(head.get("ref")),
        "head_commit": _str(head.get("sha")),
        "base_commit": _str(base.get("sha")),
        "web_url": _str(payload.get("html_url")),
    }


def map_checkout_plan(
    payload: Mapping[str, Any],
    repo: Mapping[str, Any],
) -> dict[str, Any]:
    """Map a GitHub pull-request JSON object to ``CheckoutPlan`` (vendor data only).

    Core fills the worktree path after fetch (ADR 0049 D2.4). Prefers
    ``head.repo.clone_url``; falls back to constructing an HTTPS URL from
    ``repo``.
    """
    base = payload.get("base") or {}
    head = payload.get("head") or {}
    head_repo = head.get("repo") or {}
    clone_url = _str(head_repo.get("clone_url"))
    if not clone_url:
        host = _str(repo.get("host"), "github.com")
        owner = _str(repo.get("owner"))
        name = _str(repo.get("name"))
        clone_url = f"https://{host}/{owner}/{name}.git"
    return {
        "repo": _repo_ref(repo),
        "number": int(payload.get("number") or 0),
        "clone_url": clone_url,
        "base_ref": _str(base.get("ref")),
        "head_ref": _str(head.get("ref")),
        "head_sha": _str(head.get("sha")),
    }


def map_push_prep(
    payload: Mapping[str, Any],
    repo: Mapping[str, Any],
) -> dict[str, Any]:
    """Map a GitHub pull-request JSON object to ``PushPrep`` (vendor data only)."""
    plan = map_checkout_plan(payload, repo)
    return {
        "repo": plan["repo"],
        "number": plan["number"],
        "clone_url": plan["clone_url"],
        "head_ref": plan["head_ref"],
    }


def map_issue(payload: Mapping[str, Any], repo: Mapping[str, Any]) -> dict[str, Any]:
    """Map one GitHub issue JSON object to ``Issue``."""
    return {
        "repo": _repo_ref(repo),
        "number": int(payload.get("number") or 0),
        "title": _str(payload.get("title")),
        "body": _str(payload.get("body")),
        "state": map_issue_state(_str(payload.get("state"))),
        "web_url": _str(payload.get("html_url")),
    }


def map_repository(payload: Mapping[str, Any], repo: Mapping[str, Any]) -> dict[str, Any]:
    """Map one GitHub repository JSON object to ``Repository``."""
    return {
        "repo": _repo_ref(repo),
        "default_branch": _str(payload.get("default_branch"), "main"),
        "clone_url": _str(payload.get("clone_url")),
        "web_url": _str(payload.get("html_url")),
        "description": _str(payload.get("description")),
    }


def map_comment(payload: Mapping[str, Any]) -> dict[str, str]:
    """Map one GitHub issue/PR comment JSON object to ``Comment``."""
    user = payload.get("user") or {}
    return {
        "author": _str(user.get("login")),
        "created_at": _str(payload.get("created_at")),
        "body": _str(payload.get("body")),
    }


def map_comments(payloads: list[Mapping[str, Any]]) -> list[dict[str, str]]:
    """Map a comments array."""
    return [map_comment(row) for row in payloads]


def pull_request_create_body(draft: Mapping[str, Any]) -> dict[str, Any]:
    """Build a GitHub ``POST …/pulls`` body from ``ChangeRequestDraft``.

    Field names follow GitHub REST (``head`` / ``base`` / ``body`` / ``draft``).
    ``draft.repo`` is used only by the client for the repo path — it is not part
    of the JSON body.
    """
    body: dict[str, Any] = {
        "title": _str(draft.get("title")),
        "head": _str(draft.get("head")),
        "base": _str(draft.get("base")),
    }
    description = _str(draft.get("body"))
    if description:
        body["body"] = description
    if bool(draft.get("draft")):
        body["draft"] = True
    return body


def issue_create_body(draft: Mapping[str, Any]) -> dict[str, Any]:
    """Build a GitHub ``POST …/issues`` body from ``IssueDraft``.

    ``draft.repo`` is used only by the client for the repo path.
    """
    body: dict[str, Any] = {"title": _str(draft.get("title"))}
    description = _str(draft.get("body"))
    if description:
        body["body"] = description
    return body


_LIST_FILTER_KEYS = frozenset({"state", "author", "label"})


def split_list_filters(text: str) -> tuple[dict[str, str], str]:
    """Split neutral list-URL tokens from free-text search.

    List URLs encode filters as ``state:open author:alice label:bug``. Remaining
    tokens are free-text search. Empty free-text with a repo scope means "list
    the scope" (REST list endpoints).
    """
    filters: dict[str, str] = {}
    rest: list[str] = []
    for token in (text or "").split():
        key, sep, value = token.partition(":")
        if sep and key in _LIST_FILTER_KEYS and value:
            filters[key] = value
            continue
        rest.append(token)
    return filters, " ".join(rest)


def map_issue_hit(payload: Mapping[str, Any], repo: Mapping[str, Any]) -> dict[str, Any] | None:
    """Map one GitHub issue JSON object to a ``SearchHit`` (skip PRs)."""
    if payload.get("pull_request") is not None:
        return None
    return {
        "repo": _repo_ref(repo),
        "title": _str(payload.get("title")),
        "web_url": _str(payload.get("html_url")),
        "number": int(payload.get("number") or 0) or None,
        "commit": None,
        "state": map_issue_state(_str(payload.get("state"))),
    }


def map_pull_request_hit(
    payload: Mapping[str, Any],
    repo: Mapping[str, Any],
) -> dict[str, Any]:
    """Map one GitHub pull-request JSON object to a ``SearchHit``."""
    return {
        "repo": _repo_ref(repo),
        "title": _str(payload.get("title")),
        "web_url": _str(payload.get("html_url")),
        "number": int(payload.get("number") or 0) or None,
        "commit": None,
        "state": map_request_state(
            _str(payload.get("state")),
            merged=bool(payload.get("merged")),
        ),
    }


def map_search_page(
    kind: str,
    payloads: list[Mapping[str, Any]],
    repo: Mapping[str, Any],
) -> dict[str, Any]:
    """Map a GitHub issues/pulls list (or search ``items``) to ``SearchPage``."""
    hits: list[dict[str, Any]] = []
    if kind in {"change_requests", "search_change_requests"}:
        for row in payloads:
            hits.append(map_pull_request_hit(row, repo))
        page_kind = "change_requests"
    elif kind in {"issues", "search_issues"}:
        for row in payloads:
            hit = map_issue_hit(row, repo)
            if hit is not None:
                hits.append(hit)
        page_kind = "issues"
    else:
        page_kind = kind
    return {"kind": page_kind, "hits": hits}
