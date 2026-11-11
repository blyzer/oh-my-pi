"""Host-dispatch unit tests — no network, no omp runtime."""

from __future__ import annotations

import os
import sys
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src"
GITHUB_SRC = ROOT.parent / "github" / "src"
GITLAB_SRC = ROOT.parent / "gitlab" / "src"
for path in (SRC, GITHUB_SRC, GITLAB_SRC):
    text = str(path)
    if text not in sys.path:
        sys.path.insert(0, text)

from omp_ci_ext.dispatch import (  # noqa: E402
    Backend,
    LOG_STREAM_CAPS,
    host_from_args,
    select_backend,
)
from omp_ci_ext.errors import CiUnavailable  # noqa: E402


class SelectBackendTests(unittest.TestCase):
    def test_saas_hosts(self) -> None:
        self.assertEqual(select_backend("github.com"), Backend.GITHUB)
        self.assertEqual(select_backend("GitHub.COM"), Backend.GITHUB)
        self.assertEqual(select_backend("gitlab.com"), Backend.GITLAB)

    def test_gitlab_substring_and_env(self) -> None:
        self.assertEqual(select_backend("gitlab.example.com"), Backend.GITLAB)
        with mock.patch.dict(os.environ, {"GITLAB_HOST": "ci.internal"}, clear=False):
            self.assertEqual(select_backend("ci.internal"), Backend.GITLAB)

    def test_ghe_defaults_to_github(self) -> None:
        self.assertEqual(select_backend("ghe.example.com"), Backend.GITHUB)
        with mock.patch.dict(os.environ, {"GITHUB_HOST": "ghe.example.com"}, clear=False):
            self.assertEqual(select_backend("ghe.example.com"), Backend.GITHUB)

    def test_empty_host_unavailable(self) -> None:
        with self.assertRaises(CiUnavailable):
            select_backend("")


class HostFromArgsTests(unittest.TestCase):
    def test_run_provider(self) -> None:
        self.assertEqual(
            host_from_args(
                {"run": {"provider": {"id": "github.com"}, "pipeline": "1"}},
                op="status",
            ),
            "github.com",
        )

    def test_top_level_provider(self) -> None:
        self.assertEqual(
            host_from_args({"provider": {"id": "gitlab.com"}}, op="status"),
            "gitlab.com",
        )

    def test_missing_raises(self) -> None:
        with self.assertRaises(CiUnavailable):
            host_from_args({}, op="status")


class CapsTests(unittest.TestCase):
    def test_log_stream_only(self) -> None:
        self.assertTrue(LOG_STREAM_CAPS["log_stream"])
        self.assertFalse(LOG_STREAM_CAPS["cancel"])
        self.assertFalse(LOG_STREAM_CAPS["artifacts"])


if __name__ == "__main__":
    unittest.main()
