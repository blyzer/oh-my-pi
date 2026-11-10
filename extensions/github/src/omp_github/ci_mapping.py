"""Pure GitHub Actions JSON → ``omp_ci`` serde shapes.

No network. Mirrors ``pipeline_from_actions`` / ``ci_status`` in
``crates/envd/src/forge_adapter/mod.rs``. Field names match ``crates/ci``
serde (snake_case).
"""

from __future__ import annotations

from typing import Any, Mapping


def _str(value: Any, default: str = "") -> str:
    if value is None:
        return default
    return str(value)


def map_ci_status(status: str | None, conclusion: str | None) -> str:
    """GitHub Actions ``status`` / ``conclusion`` → ``CiStatus`` snake_case.

    ``action_required`` maps to ``failure`` (same as in-core adapter — treating
    it as ``manual`` would leave ``watch: true`` polling forever).
    """
    if conclusion:
        raw = conclusion.lower()
        if raw == "success":
            return "success"
        if raw == "cancelled":
            return "canceled"
        if raw in {"skipped", "neutral"}:
            return "skipped"
        if raw in {"failure", "timed_out", "action_required", "startup_failure"}:
            return "failure"
        return "failure"
    raw = (status or "").lower()
    if raw in {"queued", "waiting", "pending", "requested"}:
        return "queued"
    if raw == "completed":
        return "success"
    return "running"


def map_job(payload: Mapping[str, Any]) -> dict[str, Any] | None:
    """Map one Actions job object to ``Job``."""
    job_id = payload.get("id")
    if job_id is None:
        return None
    return {
        "id": _str(job_id),
        "name": _str(payload.get("name")),
        "status": map_ci_status(
            _str(payload.get("status")) or None,
            _str(payload.get("conclusion")) or None,
        ),
        "web_url": _str(payload.get("html_url")),
    }


def map_pipeline(
    snapshot: Mapping[str, Any],
    run: Mapping[str, Any],
) -> dict[str, Any]:
    """Map an Actions ``{ run, jobs }`` snapshot to ``Pipeline``.

    ``run`` is the ``omp_ci::RunRef`` JSON (provider / pipeline / job).
    """
    body = snapshot.get("run") if isinstance(snapshot.get("run"), dict) else snapshot
    assert isinstance(body, Mapping)
    name = _str(body.get("name")) or _str(body.get("display_title"))
    status = map_ci_status(
        _str(body.get("status")) or None,
        _str(body.get("conclusion")) or None,
    )
    jobs_raw = snapshot.get("jobs")
    jobs: list[dict[str, Any]] = []
    if isinstance(jobs_raw, list):
        for item in jobs_raw:
            if isinstance(item, dict):
                mapped = map_job(item)
                if mapped is not None:
                    jobs.append(mapped)
    provider = run.get("provider") if isinstance(run.get("provider"), dict) else {}
    pipeline: dict[str, Any] = {
        "provider": {"id": _str(provider.get("id"))},
        "id": _str(run.get("pipeline")) or _str(body.get("id")),
        "name": name,
        "status": status,
        "web_url": _str(body.get("html_url")),
        "revision": None,
        "jobs": jobs,
    }
    head = body.get("head_sha")
    if head:
        pipeline["revision"] = _str(head)
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
