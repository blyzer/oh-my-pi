"""Dyn ``forge/*`` device mounts — single catalog owner.

Declares one dynamic parent with ``omp.devices.parent`` and mounts the read and
create surfaces at ``extension_activate``. Each leaf dispatches by ``repo.host``
to ``omp_github`` or ``omp_gitlab`` so GitHub and GitLab never compete for the
same ``forge/<op>`` mount (ADR 0049 mount-collision constraint).
"""

from __future__ import annotations

from typing import Any

import omp
from omp.devices import MountSpec

from . import dispatch
from .errors import ForgeUnavailable

_ARGS_SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": True,
}

# Manifest-backed dynamic parent (sealed soft tool). Leaves attach post-FREEZE.
FORGE_PARENT = omp.devices.parent("forge", family="", rev=1, place="host")


def _repo(args: dict[str, Any]) -> dict[str, Any]:
    repo = args.get("repo")
    if not isinstance(repo, dict):
        raise ForgeUnavailable("repo_view", "args.repo must be an object")
    return repo


def _number(args: dict[str, Any], *, op: str) -> int:
    raw = args.get("number")
    if isinstance(raw, bool) or not isinstance(raw, int):
        raise ForgeUnavailable(op, "args.number must be an integer")
    return raw


def _draft(args: dict[str, Any], *, op: str) -> dict[str, Any]:
    draft = args.get("draft")
    if not isinstance(draft, dict):
        raise ForgeUnavailable(op, "args.draft must be an object")
    return draft


def _client(args: dict[str, Any], *, op: str):
    return dispatch.client_for(dispatch.host_from_args(args, op=op))


async def repo_view(args: dict[str, Any]) -> dict[str, Any]:
    repo = _repo(args)
    return _client(args, op="repo_view").view_repo(repo)


async def change_request_view(args: dict[str, Any]) -> dict[str, Any]:
    return _client(args, op="change_request_view").view_change_request(
        _repo(args),
        _number(args, op="change_request_view"),
    )


async def issue_view(args: dict[str, Any]) -> dict[str, Any]:
    return _client(args, op="issue_view").view_issue(
        _repo(args),
        _number(args, op="issue_view"),
    )


async def change_request_diff(args: dict[str, Any]) -> str:
    return _client(args, op="change_request_view").change_request_diff(
        _repo(args),
        _number(args, op="change_request_view"),
    )


async def list_comments(args: dict[str, Any]) -> list[dict[str, str]]:
    kind = str(args.get("kind") or "pr")
    op = "change_request_view" if kind in {"pr", "change_request"} else "issue_view"
    return _client(args, op=op).list_comments(_repo(args), _number(args, op=op), kind)


async def change_request_create(args: dict[str, Any]) -> dict[str, Any]:
    draft = _draft(args, op="change_request_create")
    return _client(args, op="change_request_create").create_change_request(draft)


async def issue_create(args: dict[str, Any]) -> dict[str, Any]:
    draft = _draft(args, op="issue_create")
    return _client(args, op="issue_create").create_issue(draft)


_LEAVES: tuple[MountSpec, ...] = (
    MountSpec(
        subpath="repo_view",
        body=repo_view,
        schema=_ARGS_SCHEMA,
        summary="View a forge repository (GitHub or GitLab by host)",
    ),
    MountSpec(
        subpath="change_request_view",
        body=change_request_view,
        schema=_ARGS_SCHEMA,
        summary="View a change request / merge request / pull request",
    ),
    MountSpec(
        subpath="issue_view",
        body=issue_view,
        schema=_ARGS_SCHEMA,
        summary="View a forge issue",
    ),
    MountSpec(
        subpath="change_request_diff",
        body=change_request_diff,
        schema=_ARGS_SCHEMA,
        summary="Unified diff for a change request",
    ),
    MountSpec(
        subpath="list_comments",
        body=list_comments,
        schema=_ARGS_SCHEMA,
        summary="List comments on a change request or issue",
    ),
    MountSpec(
        subpath="change_request_create",
        body=change_request_create,
        schema=_ARGS_SCHEMA,
        summary="Create a change request from ChangeRequestDraft",
    ),
    MountSpec(
        subpath="issue_create",
        body=issue_create,
        schema=_ARGS_SCHEMA,
        summary="Create an issue from IssueDraft",
    ),
)


@omp.hook("extension_activate")
async def mount_forge_leaves(_event: object) -> None:
    """Mount the read + create surface into the host dynamic device catalog."""
    await FORGE_PARENT.mount_many(*_LEAVES)


# Search / checkout / push stay unmounted (ADR D2.4). DynForgeAdapter calls that
# reach a missing leaf become ForgeError::Unavailable via the live client.
