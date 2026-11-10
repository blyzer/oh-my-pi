"""GitLab CI backend (status + log_stream).

Credentials are the same adapter data as the forge client (``GITLAB_TOKEN``).
Project identity comes from optional ``repo`` args, then ``CI_PROJECT_PATH`` /
``OMP_CI_REPO`` (``group/project`` path).
"""

from __future__ import annotations

import os
import urllib.error
import urllib.request
from typing import Any, Mapping

from .ci_mapping import map_log_page, map_pipeline
from .client import _api_root, _get_json, _host_from_repo, _token
from .errors import ForgeUnavailable
from .mapping import project_path


def capabilities(_provider: Mapping[str, Any]) -> dict[str, bool]:
    """Production ceiling: log_stream only (status always allowed)."""
    return {
        "rerun_failed": False,
        "rerun_job": False,
        "cancel": False,
        "trigger_with_inputs": False,
        "artifacts": False,
        "log_stream": True,
        "play_manual": False,
    }


def _resolve_repo(
    run: Mapping[str, Any],
    *,
    repo: Mapping[str, Any] | None,
    op: str,
) -> dict[str, str]:
    if isinstance(repo, dict) and repo.get("owner") and repo.get("name"):
        host = str(repo.get("host") or "").strip()
        if not host:
            provider = run.get("provider") if isinstance(run.get("provider"), dict) else {}
            host = str(provider.get("id") or "gitlab.com")
        return {
            "host": host,
            "owner": str(repo["owner"]),
            "name": str(repo["name"]),
        }
    slug = (
        os.environ.get("CI_PROJECT_PATH", "").strip()
        or os.environ.get("OMP_CI_REPO", "").strip()
    )
    if "/" not in slug:
        raise ForgeUnavailable(
            op,
            "repo unresolved; pass args.repo or set CI_PROJECT_PATH / OMP_CI_REPO",
        )
    owner, name = slug.split("/", 1)
    provider = run.get("provider") if isinstance(run.get("provider"), dict) else {}
    host = str(provider.get("id") or os.environ.get("GITLAB_HOST", "gitlab.com"))
    return {"host": host, "owner": owner, "name": name}


def _project_url(repo: Mapping[str, str], *parts: str) -> str:
    host = _host_from_repo(repo)
    encoded = project_path(repo["owner"], repo["name"])
    path = "/".join(("projects", encoded, *parts))
    return f"{_api_root(host)}/{path}"


def _fetch_pipeline_and_jobs(
    repo: Mapping[str, str],
    pipeline_id: str,
    *,
    op: str,
) -> tuple[dict[str, Any], list[Any]]:
    _token(op=op)
    pipeline_body = _get_json(_project_url(repo, "pipelines", pipeline_id), op=op)
    if not isinstance(pipeline_body, dict):
        raise ForgeUnavailable(op, "GitLab pipeline payload was not an object")
    jobs_body = _get_json(_project_url(repo, "pipelines", pipeline_id, "jobs"), op=op)
    jobs: list[Any] = jobs_body if isinstance(jobs_body, list) else []
    return pipeline_body, jobs


def _job_trace(repo: Mapping[str, str], job_id: str, *, op: str) -> str:
    """Download one job trace (plain text). Missing logs become empty text."""
    url = _project_url(repo, "jobs", job_id, "trace")
    headers = {
        "PRIVATE-TOKEN": _token(op=op),
        "Accept": "text/plain, application/json",
        "User-Agent": "omp-gitlab/0.1",
    }
    request = urllib.request.Request(url, headers=headers, method="GET")
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.read().decode("utf-8", errors="replace")
    except urllib.error.HTTPError as error:
        if error.code in {404, 410}:
            return ""
        raise ForgeUnavailable(op, f"GitLab HTTP {error.code}") from error
    except urllib.error.URLError as error:
        raise ForgeUnavailable(op, "GitLab host unreachable") from error


def status(run: Mapping[str, Any], *, repo: Mapping[str, Any] | None = None) -> dict[str, Any]:
    """Fetch one GitLab pipeline + jobs and map to ``Pipeline``."""
    op = "status"
    resolved = _resolve_repo(run, repo=repo, op=op)
    pipeline_id = str(run.get("pipeline") or "").strip()
    if not pipeline_id:
        raise ForgeUnavailable(op, "run.pipeline is required")
    pipeline_body, jobs = _fetch_pipeline_and_jobs(resolved, pipeline_id, op=op)
    return map_pipeline(pipeline_body, jobs, run)


def log_stream(run: Mapping[str, Any], *, repo: Mapping[str, Any] | None = None) -> dict[str, Any]:
    """Fetch one job trace, or concatenate failed-job traces when job is omitted."""
    op = "log_stream"
    resolved = _resolve_repo(run, repo=repo, op=op)
    pipeline_id = str(run.get("pipeline") or "").strip()
    if not pipeline_id:
        raise ForgeUnavailable(op, "run.pipeline is required")
    job = run.get("job")
    if job is not None and str(job).strip():
        text = _job_trace(resolved, str(job).strip(), op=op)
        return map_log_page(run, text, complete=True)

    pipeline_body, jobs = _fetch_pipeline_and_jobs(resolved, pipeline_id, op=op)
    pipeline = map_pipeline(pipeline_body, jobs, run)
    chunks: list[str] = []
    for item in pipeline.get("jobs") or []:
        if not isinstance(item, dict):
            continue
        if item.get("status") != "failure":
            continue
        chunk = _job_trace(resolved, str(item.get("id") or ""), op=op)
        if chunk:
            if chunks:
                chunks.append("\n")
            chunks.append(chunk)
    return map_log_page(run, "".join(chunks), complete=True)
