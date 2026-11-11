"""Pure GitLab CI → omp_ci mapping unit tests — no network."""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src"
if str(SRC) not in sys.path:
    sys.path.insert(0, str(SRC))

from omp_gitlab.ci_mapping import (  # noqa: E402
    map_ci_status,
    map_job,
    map_log_page,
    map_pipeline,
)

FIXTURES = Path(__file__).resolve().parent / "fixtures"
RUN = {"provider": {"id": "gitlab.com"}, "pipeline": "42", "job": None}


def _load(name: str) -> object:
    return json.loads((FIXTURES / name).read_text(encoding="utf-8"))


class CiStatusTests(unittest.TestCase):
    def test_statuses(self) -> None:
        self.assertEqual(map_ci_status("pending"), "queued")
        self.assertEqual(map_ci_status("running"), "running")
        self.assertEqual(map_ci_status("success"), "success")
        self.assertEqual(map_ci_status("failed"), "failure")
        self.assertEqual(map_ci_status("canceled"), "canceled")
        self.assertEqual(map_ci_status("skipped"), "skipped")
        self.assertEqual(map_ci_status("manual"), "manual")


class PipelineMappingTests(unittest.TestCase):
    def test_fixture_matches_pipeline_shape(self) -> None:
        pipeline_payload = _load("pipeline.json")
        jobs_payload = _load("pipeline_jobs.json")
        assert isinstance(pipeline_payload, dict)
        assert isinstance(jobs_payload, list)
        pipeline = map_pipeline(pipeline_payload, jobs_payload, RUN)
        self.assertEqual(pipeline["provider"]["id"], "gitlab.com")
        self.assertEqual(pipeline["id"], "42")
        self.assertEqual(pipeline["name"], "ci")
        self.assertEqual(pipeline["status"], "failure")
        self.assertEqual(pipeline["revision"], "abc123")
        self.assertEqual(len(pipeline["jobs"]), 3)
        self.assertEqual(pipeline["jobs"][0]["status"], "failure")
        self.assertEqual(pipeline["jobs"][1]["status"], "success")
        self.assertEqual(pipeline["jobs"][2]["status"], "manual")

    def test_job_requires_id(self) -> None:
        self.assertIsNone(map_job({"name": "x"}))


class LogPageTests(unittest.TestCase):
    def test_map_log_page(self) -> None:
        run = {**RUN, "job": "7"}
        page = map_log_page(run, "boom\n", complete=True)
        self.assertEqual(page["text"], "boom\n")
        self.assertEqual(page["run"]["pipeline"], "42")


if __name__ == "__main__":
    unittest.main()
