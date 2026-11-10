"""Pure Actions → omp_ci mapping unit tests — no network."""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src"
if str(SRC) not in sys.path:
    sys.path.insert(0, str(SRC))

from omp_github.ci_mapping import (  # noqa: E402
    map_ci_status,
    map_job,
    map_log_page,
    map_pipeline,
)

FIXTURES = Path(__file__).resolve().parent / "fixtures"
RUN = {"provider": {"id": "github.com"}, "pipeline": "42", "job": None}


def _load(name: str) -> object:
    return json.loads((FIXTURES / name).read_text(encoding="utf-8"))


class CiStatusTests(unittest.TestCase):
    def test_conclusions_and_statuses(self) -> None:
        self.assertEqual(map_ci_status("queued", None), "queued")
        self.assertEqual(map_ci_status("in_progress", None), "running")
        self.assertEqual(map_ci_status("completed", "success"), "success")
        self.assertEqual(map_ci_status("completed", "cancelled"), "canceled")
        self.assertEqual(map_ci_status("completed", "skipped"), "skipped")
        self.assertEqual(map_ci_status("completed", "failure"), "failure")
        self.assertEqual(map_ci_status("completed", "action_required"), "failure")


class PipelineMappingTests(unittest.TestCase):
    def test_fixture_matches_pipeline_shape(self) -> None:
        snapshot = _load("actions_run.json")
        assert isinstance(snapshot, dict)
        pipeline = map_pipeline(snapshot, RUN)
        self.assertEqual(pipeline["provider"]["id"], "github.com")
        self.assertEqual(pipeline["id"], "42")
        self.assertEqual(pipeline["name"], "ci")
        self.assertEqual(pipeline["status"], "failure")
        self.assertEqual(pipeline["revision"], "abc123")
        self.assertEqual(
            pipeline["web_url"],
            "https://github.com/acme/widget/actions/runs/42",
        )
        self.assertEqual(len(pipeline["jobs"]), 2)
        self.assertEqual(pipeline["jobs"][0]["id"], "7")
        self.assertEqual(pipeline["jobs"][0]["status"], "failure")
        self.assertEqual(pipeline["jobs"][1]["status"], "success")

    def test_job_requires_id(self) -> None:
        self.assertIsNone(map_job({"name": "x"}))


class LogPageTests(unittest.TestCase):
    def test_map_log_page(self) -> None:
        run = {**RUN, "job": "7"}
        page = map_log_page(run, "boom\n", complete=True)
        self.assertEqual(page["text"], "boom\n")
        self.assertTrue(page["complete"])
        self.assertEqual(page["run"]["job"], "7")


if __name__ == "__main__":
    unittest.main()
