"""Pure GitLab CI JSON → ``omp_ci`` serde shapes.

No network. Field names match ``crates/ci`` serde (snake_case).
"""

from __future__ import annotations

from typing import Any, Mapping


def _str(value: Any, default: str = "") -> str:
    if value is None:
        return default
    return str(value)


def map_ci_status(status: str | None) -> str:
    """GitLab pipeline/job ``status`` → ``CiStatus`` snake_case."""
    raw = (status or "").lower()
    if raw in {"created", "waiting_for_resource", "preparing", "pending", "scheduled"}:
        return "queued"
    if raw == "running":
        return "running"
    if raw == "success":
        return "success"
    if raw == "failed":
        return "failure"
    if raw in {"canceled", "cancelled"}:
        return "canceled"
    if raw == "skipped":
        return "skipped"
    if raw == "manual":
        return "manual"
    return "running"


def map_job(payload: Mapping[str, Any]) -> dict[str, Any] | None:
    """Map one GitLab job object to ``Job``."""
    job_id = payload.get("id")
    if job_id is None:
        return None
    return {
        "id": _str(job_id),
        "name": _str(payload.get("name")),
        "status": map_ci_status(_str(payload.get("status")) or None),
        "web_url": _str(payload.get("web_url")),
    }


def map_pipeline(
    pipeline_payload: Mapping[str, Any],
    jobs_payload: list[Mapping[str, Any]] | list[Any],
    run: Mapping[str, Any],
) -> dict[str, Any]:
    """Map a GitLab pipeline + jobs list to ``Pipeline``."""
    provider = run.get("provider") if isinstance(run.get("provider"), dict) else {}
    jobs: list[dict[str, Any]] = []
    for item in jobs_payload:
        if isinstance(item, dict):
            mapped = map_job(item)
            if mapped is not None:
                jobs.append(mapped)
    pipeline: dict[str, Any] = {
        "provider": {"id": _str(provider.get("id"))},
        "id": _str(run.get("pipeline")) or _str(pipeline_payload.get("id")),
        "name": _str(pipeline_payload.get("name")) or _str(pipeline_payload.get("ref")),
        "status": map_ci_status(_str(pipeline_payload.get("status")) or None),
        "web_url": _str(pipeline_payload.get("web_url")),
        "revision": None,
        "jobs": jobs,
    }
    sha = pipeline_payload.get("sha")
    if sha:
        pipeline["revision"] = _str(sha)
    return pipeline


def map_log_page(run: Mapping[str, Any], text: str, *, complete: bool = True) -> dict[str, Any]:
    """Build a ``LogPage`` for ``run``."""
    provider = run.get("provider") if isinstance(run.get("provider"), dict) else {}
    job = run.get("job")
    return {
        "run": {
            "provider": {"id": _str(provider.get("id"))},
            "pipeline": _str(run.get("pipeline")),
            "job": None if job is None else _str(job),
        },
        "text": text,
        "complete": complete,
    }
