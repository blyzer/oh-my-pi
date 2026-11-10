"""Pure GitLab REST JSON → ``omp_forge`` serde shapes.

No network. Unit-tested with fixture payloads under ``tests/fixtures/``.
Field names match ``crates/forge`` serde (snake_case).
"""

from __future__ import annotations

from typing import Any, Mapping
from urllib.parse import quote


def project_path(owner: str, name: str) -> str:
    """URL-encode ``owner/name`` (nested groups use ``/``) for ``/projects/:id``."""
    return quote(f"{owner}/{name}", safe="")


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


def map_request_state(state: str | None, *, detailed: str | None = None) -> str:
    """GitLab MR ``state`` / ``detailed_merge_status`` → ``RequestState``."""
    raw = (state or "").lower()
    if raw == "merged" or (detailed or "").lower() == "merged":
        return "merged"
    if raw in {"closed", "locked"}:
        return "closed"
    return "open"


def map_issue_state(state: str | None) -> str:
    """GitLab issue ``state`` → ``IssueState``."""
    raw = (state or "").lower()
    if raw == "closed":
        return "closed"
    return "open"


def map_merge_request(
    payload: Mapping[str, Any],
    repo: Mapping[str, Any],
) -> dict[str, Any]:
    """Map one GitLab merge-request JSON object to ``ChangeRequest``."""
    diff_refs = payload.get("diff_refs") or {}
    return {
        "repo": _repo_ref(repo),
        "number": int(payload.get("iid") or payload.get("id") or 0),
        "title": _str(payload.get("title")),
        "body": _str(payload.get("description")),
        "state": map_request_state(
            _str(payload.get("state")),
            detailed=_str(payload.get("detailed_merge_status") or None) or None,
        ),
        "draft": bool(payload.get("draft") or payload.get("work_in_progress")),
        "base_ref": _str(payload.get("target_branch")),
        "head_ref": _str(payload.get("source_branch")),
        "head_commit": _str(payload.get("sha") or diff_refs.get("head_sha")),
        "base_commit": _str(diff_refs.get("base_sha")),
        "web_url": _str(payload.get("web_url")),
    }


def map_issue(payload: Mapping[str, Any], repo: Mapping[str, Any]) -> dict[str, Any]:
    """Map one GitLab issue JSON object to ``Issue``."""
    return {
        "repo": _repo_ref(repo),
        "number": int(payload.get("iid") or payload.get("id") or 0),
        "title": _str(payload.get("title")),
        "body": _str(payload.get("description")),
        "state": map_issue_state(_str(payload.get("state"))),
        "web_url": _str(payload.get("web_url")),
    }


def map_project(payload: Mapping[str, Any], repo: Mapping[str, Any]) -> dict[str, Any]:
    """Map one GitLab project JSON object to ``Repository``."""
    http_url = _str(payload.get("http_url_to_repo") or payload.get("ssh_url_to_repo"))
    return {
        "repo": _repo_ref(repo),
        "default_branch": _str(payload.get("default_branch"), "main"),
        "clone_url": http_url,
        "web_url": _str(payload.get("web_url")),
        "description": _str(payload.get("description")),
    }


def map_note(payload: Mapping[str, Any]) -> dict[str, str]:
    """Map one GitLab note JSON object to ``Comment``."""
    author = payload.get("author") or {}
    return {
        "author": _str(author.get("username") or author.get("name")),
        "created_at": _str(payload.get("created_at")),
        "body": _str(payload.get("body")),
    }


def map_notes(payloads: list[Mapping[str, Any]]) -> list[dict[str, str]]:
    """Map a notes array, skipping system notes when marked."""
    out: list[dict[str, str]] = []
    for note in payloads:
        if note.get("system"):
            continue
        out.append(map_note(note))
    return out


def merge_request_create_body(draft: Mapping[str, Any]) -> dict[str, Any]:
    """Build a GitLab ``POST …/merge_requests`` body from ``ChangeRequestDraft``.

    Field names follow GitLab REST (``source_branch`` / ``target_branch`` /
    ``description`` / ``draft``). ``draft.repo`` is used only by the client for
    the project path — it is not part of the JSON body.
    """
    body: dict[str, Any] = {
        "title": _str(draft.get("title")),
        "source_branch": _str(draft.get("head")),
        "target_branch": _str(draft.get("base")),
    }
    description = _str(draft.get("body"))
    if description:
        body["description"] = description
    if bool(draft.get("draft")):
        body["draft"] = True
    return body


def issue_create_body(draft: Mapping[str, Any]) -> dict[str, Any]:
    """Build a GitLab ``POST …/issues`` body from ``IssueDraft``.

    ``draft.repo`` is used only by the client for the project path.
    """
    body: dict[str, Any] = {"title": _str(draft.get("title"))}
    description = _str(draft.get("body"))
    if description:
        body["description"] = description
    return body
