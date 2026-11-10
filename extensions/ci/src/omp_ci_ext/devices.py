"""Dyn ``ci/*`` device mounts — single catalog owner.

Declares one dynamic parent with ``omp.devices.parent`` and mounts status,
log_stream, and capabilities at ``extension_activate``. Each leaf dispatches by
provider host to ``omp_github.ci`` or ``omp_gitlab.ci`` so GitHub and GitLab
never compete for the same ``ci/<op>`` mount (ADR 0049 mount-collision
constraint).

Unmounted ops (artifacts, cancel, rerun_*, trigger, play_manual) stay off the
catalog; ``DynCiAdapter`` calls for those paths become ``CiError::Unavailable``.
Production registration ceilings remain ``log_stream`` only.
"""

from __future__ import annotations

from typing import Any

import omp
from omp.devices import MountSpec

from . import dispatch
from .errors import CiUnavailable

_ARGS_SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": True,
}

# Manifest-backed dynamic parent (sealed soft tool). Leaves attach post-FREEZE.
CI_PARENT = omp.devices.parent("ci", family="", rev=1, place="host")


def _run(args: dict[str, Any], *, op: str) -> dict[str, Any]:
    run = args.get("run")
    if not isinstance(run, dict):
        raise CiUnavailable(op, "args.run must be an object")
    return run


def _repo(args: dict[str, Any]) -> dict[str, Any] | None:
    repo = args.get("repo")
    if repo is None:
        return None
    if not isinstance(repo, dict):
        raise CiUnavailable("status", "args.repo must be an object when present")
    return repo


def _client(args: dict[str, Any], *, op: str):
    return dispatch.client_for(dispatch.host_from_args(args, op=op))


async def capabilities(args: dict[str, Any]) -> dict[str, bool]:
    """Report the production ceiling (log_stream only)."""
    return _client(args, op="status").capabilities(
        args.get("provider") if isinstance(args.get("provider"), dict) else {}
    )


async def status(args: dict[str, Any]) -> dict[str, Any]:
    run = _run(args, op="status")
    return _client(args, op="status").status(run, repo=_repo(args))


async def log_stream(args: dict[str, Any]) -> dict[str, Any]:
    run = _run(args, op="log_stream")
    return _client(args, op="log_stream").log_stream(run, repo=_repo(args))


_LEAVES: tuple[MountSpec, ...] = (
    MountSpec(
        subpath="capabilities",
        body=capabilities,
        schema=_ARGS_SCHEMA,
        summary="CI capability flags for one provider (log_stream ceiling)",
    ),
    MountSpec(
        subpath="status",
        body=status,
        schema=_ARGS_SCHEMA,
        summary="Pipeline status snapshot (GitHub Actions or GitLab CI by host)",
    ),
    MountSpec(
        subpath="log_stream",
        body=log_stream,
        schema=_ARGS_SCHEMA,
        summary="Job or failed-jobs log page",
    ),
)


@omp.hook("extension_activate")
async def mount_ci_leaves(_event: object) -> None:
    """Mount the status + log_stream surface into the host dynamic device catalog."""
    await CI_PARENT.mount_many(*_LEAVES)


# artifacts / cancel / rerun_* / trigger / play_manual stay unmounted.
# DynCiAdapter calls that reach a missing leaf become CiError::Unavailable.
