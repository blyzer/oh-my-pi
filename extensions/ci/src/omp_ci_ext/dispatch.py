"""Host → backend selection for dyn ``ci/*`` leaves.

Known SaaS hosts are hard-coded. Adapter env ``GITLAB_HOST`` / ``GITHUB_HOST``
and a ``gitlab`` substring heuristic cover self-hosted Dyn hosts. Remaining
Dyn hosts default to the GitHub Actions backend (GHE), matching forge dispatch.
"""

from __future__ import annotations

import os
from enum import Enum
from typing import Any, Mapping, Protocol

from .errors import CiUnavailable

GITHUB_COM = "github.com"
GITLAB_COM = "gitlab.com"

# Production caps stay log_stream-only (status always allowed).
LOG_STREAM_CAPS: dict[str, bool] = {
    "rerun_failed": False,
    "rerun_job": False,
    "cancel": False,
    "trigger_with_inputs": False,
    "artifacts": False,
    "log_stream": True,
    "play_manual": False,
}


class Backend(str, Enum):
    """Concrete T2 CI backend package."""

    GITHUB = "github"
    GITLAB = "gitlab"


class CiBackend(Protocol):
    """Status + log_stream surface shared by ``omp_github.ci`` / ``omp_gitlab.ci``."""

    def capabilities(self, provider: Mapping[str, Any]) -> dict[str, bool]: ...

    def status(self, run: Mapping[str, Any], *, repo: Mapping[str, Any] | None = None) -> dict[str, Any]: ...

    def log_stream(
        self, run: Mapping[str, Any], *, repo: Mapping[str, Any] | None = None
    ) -> dict[str, Any]: ...


def _norm(host: str) -> str:
    return host.strip().rstrip("/").lower()


def select_backend(host: str) -> Backend:
    """Choose GitHub Actions vs GitLab CI for an ASCII CI provider host."""
    normalized = _norm(host)
    if not normalized:
        raise CiUnavailable("status", "provider.id (host) is required")

    github_env = _norm(os.environ.get("GITHUB_HOST", ""))
    gitlab_env = _norm(os.environ.get("GITLAB_HOST", ""))

    if normalized == GITHUB_COM or (github_env and normalized == github_env):
        return Backend.GITHUB
    if normalized == GITLAB_COM or (gitlab_env and normalized == gitlab_env):
        return Backend.GITLAB
    if "gitlab" in normalized:
        return Backend.GITLAB
    return Backend.GITHUB


def host_from_args(args: Mapping[str, Any], *, op: str) -> str:
    """Extract provider host from ``run.provider.id`` or ``provider.id``."""
    run = args.get("run")
    if isinstance(run, dict):
        provider = run.get("provider")
        if isinstance(provider, dict):
            return str(provider.get("id") or "")
    provider = args.get("provider")
    if isinstance(provider, dict):
        return str(provider.get("id") or "")
    raise CiUnavailable(op, "args.run.provider.id (or provider.id) is required")


def client_for(host: str) -> CiBackend:
    """Import and return the CI backend module for ``host``."""
    backend = select_backend(host)
    if backend is Backend.GITHUB:
        from omp_github import ci as github_ci

        return github_ci
    from omp_gitlab import ci as gitlab_ci

    return gitlab_ci
