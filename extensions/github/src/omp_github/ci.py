"""GitHub Actions CI backend (status + log_stream).

Credentials are the same adapter data as the forge client
(``GH_TOKEN`` / ``GITHUB_TOKEN``). Repository identity comes from optional
``repo`` args, then ``GITHUB_REPOSITORY`` / ``OMP_CI_REPO`` (``owner/name``).

Raises ``ForgeUnavailable`` on failure; the dyn CI bridge maps callback
errors to ``CiError::Unavailable``.
"""

from __future__ import annotations

import os
import urllib.error
import urllib.request
from typing import Any, Mapping
from urllib.parse import quote

from .ci_mapping import map_log_page, map_pipeline
from .client import _api_root, _get_json, _host_from_repo, _token
from .errors import ForgeUnavailable


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
            host = str(provider.get("id") or "github.com")
        return {
            "host": host,
            "owner": str(repo["owner"]),
            "name": str(repo["name"]),
        }
    slug = (
        os.environ.get("GITHUB_REPOSITORY", "").strip()
        or os.environ.get("OMP_CI_REPO", "").strip()
    )
    if "/" not in slug:
        raise ForgeUnavailable(
            op,
            "repo unresolved; pass args.repo or set GITHUB_REPOSITORY / OMP_CI_REPO",
        )
    owner, name = slug.split("/", 1)
    provider = run.get("provider") if isinstance(run.get("provider"), dict) else {}
    host = str(provider.get("id") or os.environ.get("GITHUB_HOST", "github.com"))
    return {"host": host, "owner": owner, "name": name}


def _slug(repo: Mapping[str, str]) -> str:
    return f"{quote(repo['owner'], safe='')}/{quote(repo['name'], safe='')}"


def _actions_url(repo: Mapping[str, str], *parts: str) -> str:
    host = _host_from_repo(repo)
    path = "/".join(("repos", _slug(repo), "actions", *parts))
    return f"{_api_root(host)}/{path}"


def _fetch_snapshot(repo: Mapping[str, str], run_id: str, *, op: str) -> dict[str, Any]:
    _token(op=op)
    run_body = _get_json(_actions_url(repo, "runs", run_id), op=op)
    if not isinstance(run_body, dict):
        raise ForgeUnavailable(op, "GitHub Actions run payload was not an object")
    jobs_body = _get_json(
        _actions_url(repo, "runs", run_id, "jobs") + "?per_page=100",
        op=op,
    )
    jobs: list[Any] = []
    if isinstance(jobs_body, dict) and isinstance(jobs_body.get("jobs"), list):
        jobs = jobs_body["jobs"]
    elif isinstance(jobs_body, list):
        jobs = jobs_body
    return {"run": run_body, "jobs": jobs}


def _job_log_text(repo: Mapping[str, str], job_id: str, *, op: str) -> str:
    """Download one job log. Missing/expired logs become empty text."""
    url = _actions_url(repo, "jobs", job_id, "logs")
    headers = {
        "Authorization": f"Bearer {_token(op=op)}",
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": "omp-github/0.1",
    }
    request = urllib.request.Request(url, headers=headers, method="GET")
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.read().decode("utf-8", errors="replace")
    except urllib.error.HTTPError as error:
        if error.code in {301, 302, 303, 307, 308}:
            location = error.headers.get("Location")
            if not location:
                return ""
            try:
                redirect = urllib.request.Request(location, method="GET")
                with urllib.request.urlopen(redirect, timeout=30) as blob:
                    return blob.read().decode("utf-8", errors="replace")
            except urllib.error.URLError:
                return ""
        if error.code in {404, 410}:
            return ""
        raise ForgeUnavailable(op, f"GitHub HTTP {error.code}") from error
    except urllib.error.URLError as error:
        raise ForgeUnavailable(op, "GitHub host unreachable") from error


def status(run: Mapping[str, Any], *, repo: Mapping[str, Any] | None = None) -> dict[str, Any]:
    """Fetch one Actions run + jobs and map to ``Pipeline``."""
    op = "status"
    resolved = _resolve_repo(run, repo=repo, op=op)
    pipeline_id = str(run.get("pipeline") or "").strip()
    if not pipeline_id:
        raise ForgeUnavailable(op, "run.pipeline is required")
    snapshot = _fetch_snapshot(resolved, pipeline_id, op=op)
    return map_pipeline(snapshot, run)


def log_stream(run: Mapping[str, Any], *, repo: Mapping[str, Any] | None = None) -> dict[str, Any]:
    """Fetch one job log, or concatenate failed-job logs when job is omitted."""
    op = "log_stream"
    resolved = _resolve_repo(run, repo=repo, op=op)
    pipeline_id = str(run.get("pipeline") or "").strip()
    if not pipeline_id:
        raise ForgeUnavailable(op, "run.pipeline is required")
    job = run.get("job")
    if job is not None and str(job).strip():
        text = _job_log_text(resolved, str(job).strip(), op=op)
        return map_log_page(run, text, complete=True)

    snapshot = _fetch_snapshot(resolved, pipeline_id, op=op)
    pipeline = map_pipeline(snapshot, run)
    chunks: list[str] = []
    for item in pipeline.get("jobs") or []:
        if not isinstance(item, dict):
            continue
        if item.get("status") != "failure":
            continue
        chunk = _job_log_text(resolved, str(item.get("id") or ""), op=op)
        if chunk:
            if chunks:
                chunks.append("\n")
            chunks.append(chunk)
    return map_log_page(run, "".join(chunks), complete=True)
