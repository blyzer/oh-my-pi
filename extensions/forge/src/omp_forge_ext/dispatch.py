"""Host → backend selection for dyn ``forge/*`` leaves.

Known SaaS hosts are hard-coded. Adapter env ``GITLAB_HOST`` / ``GITHUB_HOST``
and a ``gitlab`` substring heuristic cover self-hosted Dyn hosts registered via
``sv_forge_dyn_hosts``. Remaining Dyn hosts default to the GitHub backend
(GHE), matching envd's historical non-github → GHE assumption when a host is
on the Dyn arm.
"""

from __future__ import annotations

import os
from enum import Enum
from typing import Any, Mapping, Protocol

from .errors import ForgeUnavailable

GITHUB_COM = "github.com"
GITLAB_COM = "gitlab.com"


class Backend(str, Enum):
    """Concrete T2 forge backend package."""

    GITHUB = "github"
    GITLAB = "gitlab"


class ForgeBackend(Protocol):
    """Read + create surface shared by ``omp_github.client`` / ``omp_gitlab.client``."""

    def view_repo(self, repo: Mapping[str, Any]) -> dict[str, Any]: ...

    def view_change_request(self, repo: Mapping[str, Any], number: int) -> dict[str, Any]: ...

    def view_issue(self, repo: Mapping[str, Any], number: int) -> dict[str, Any]: ...

    def change_request_diff(self, repo: Mapping[str, Any], number: int) -> str: ...

    def list_comments(
        self, repo: Mapping[str, Any], number: int, kind: str
    ) -> list[dict[str, str]]: ...

    def create_change_request(self, draft: Mapping[str, Any]) -> dict[str, Any]: ...

    def create_issue(self, draft: Mapping[str, Any]) -> dict[str, Any]: ...

    def search(self, query: Mapping[str, Any]) -> dict[str, Any]: ...

    def checkout_change_request_plan(
        self, repo: Mapping[str, Any], number: int
    ) -> dict[str, Any]: ...

    def push_change_request_prep(
        self, repo: Mapping[str, Any], number: int
    ) -> dict[str, Any]: ...


def _norm(host: str) -> str:
    return host.strip().rstrip("/").lower()


def select_backend(host: str) -> Backend:
    """Choose GitHub vs GitLab for an ASCII forge host."""
    normalized = _norm(host)
    if not normalized:
        raise ForgeUnavailable("repo_view", "repo.host is required")

    github_env = _norm(os.environ.get("GITHUB_HOST", ""))
    gitlab_env = _norm(os.environ.get("GITLAB_HOST", ""))

    if normalized == GITHUB_COM or (github_env and normalized == github_env):
        return Backend.GITHUB
    if normalized == GITLAB_COM or (gitlab_env and normalized == gitlab_env):
        return Backend.GITLAB
    if "gitlab" in normalized:
        return Backend.GITLAB
    # Dyn-registered GHE and other non-GitLab hosts → GitHub API dialect.
    return Backend.GITHUB


def host_from_args(args: Mapping[str, Any], *, op: str) -> str:
    """Extract ``repo.host`` (or ``draft.repo`` / ``request.repo`` for writes)."""
    repo = args.get("repo")
    if isinstance(repo, dict):
        return str(repo.get("host") or "")
    draft = args.get("draft")
    if isinstance(draft, dict):
        nested = draft.get("repo")
        if isinstance(nested, dict):
            return str(nested.get("host") or "")
    request = args.get("request")
    if isinstance(request, dict):
        nested = request.get("repo")
        if isinstance(nested, dict):
            return str(nested.get("host") or "")
    raise ForgeUnavailable(op, "args.repo (or draft.repo / request.repo) must supply host")


def host_from_search_args(args: Mapping[str, Any], *, op: str) -> str:
    """Extract ``query.repo.host`` for ``forge/search_*`` leaves."""
    query = args.get("query")
    if isinstance(query, dict):
        repo = query.get("repo")
        if isinstance(repo, dict):
            return str(repo.get("host") or "")
    raise ForgeUnavailable(op, "args.query.repo must supply host")


def client_for(host: str) -> ForgeBackend:
    """Import and return the backend client module for ``host``."""
    backend = select_backend(host)
    if backend is Backend.GITHUB:
        from omp_github import client as github_client

        return github_client
    from omp_gitlab import client as gitlab_client

    return gitlab_client
