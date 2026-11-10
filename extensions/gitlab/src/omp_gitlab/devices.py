"""Dyn ``forge/*`` device mounts for the GitLab adapter.

Declares a dynamic parent with ``omp.devices.parent`` and mounts the read
surface at ``extension_activate`` through ``omp.devices.dynamic_mount``. That
puts leaves in envd's ``DeviceControlFactory`` catalog so
``LiveForgeDeviceClient`` can dispatch ``omp.devices.call`` (ADR 0049 D2.8).
"""

from __future__ import annotations

from typing import Any

import omp
from omp.devices import MountSpec

from . import client
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


async def repo_view(args: dict[str, Any]) -> dict[str, Any]:
    return client.view_repo(_repo(args))


async def change_request_view(args: dict[str, Any]) -> dict[str, Any]:
    return client.view_change_request(_repo(args), _number(args, op="change_request_view"))


async def issue_view(args: dict[str, Any]) -> dict[str, Any]:
    return client.view_issue(_repo(args), _number(args, op="issue_view"))


async def change_request_diff(args: dict[str, Any]) -> str:
    return client.change_request_diff(
        _repo(args),
        _number(args, op="change_request_view"),
    )


async def list_comments(args: dict[str, Any]) -> list[dict[str, str]]:
    kind = str(args.get("kind") or "pr")
    op = "change_request_view" if kind in {"pr", "change_request"} else "issue_view"
    return client.list_comments(_repo(args), _number(args, op=op), kind)


_READ_LEAVES: tuple[MountSpec, ...] = (
    MountSpec(
        subpath="repo_view",
        body=repo_view,
        schema=_ARGS_SCHEMA,
        summary="View a GitLab project as Repository",
    ),
    MountSpec(
        subpath="change_request_view",
        body=change_request_view,
        schema=_ARGS_SCHEMA,
        summary="View a GitLab merge request as ChangeRequest",
    ),
    MountSpec(
        subpath="issue_view",
        body=issue_view,
        schema=_ARGS_SCHEMA,
        summary="View a GitLab issue as Issue",
    ),
    MountSpec(
        subpath="change_request_diff",
        body=change_request_diff,
        schema=_ARGS_SCHEMA,
        summary="Unified diff for a GitLab merge request",
    ),
    MountSpec(
        subpath="list_comments",
        body=list_comments,
        schema=_ARGS_SCHEMA,
        summary="List notes on a merge request or issue",
    ),
)


@omp.hook("extension_activate")
async def mount_forge_leaves(_event: object) -> None:
    """Mount the read surface into the host dynamic device catalog."""
    await FORGE_PARENT.mount_many(*_READ_LEAVES)


# Write / search / checkout surfaces stay unmounted. DynForgeAdapter calls that
# reach a missing leaf become ForgeError::Unavailable via the live client.
