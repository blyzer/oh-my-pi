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

from omp_forge_ext.dispatch import (  # noqa: E402
    Backend,
    host_from_args,
    host_from_search_args,
    select_backend,
)
from omp_forge_ext.errors import ForgeUnavailable  # noqa: E402


class SelectBackendTests(unittest.TestCase):
    def test_saas_hosts(self) -> None:
        self.assertEqual(select_backend("github.com"), Backend.GITHUB)
        self.assertEqual(select_backend("GitHub.COM"), Backend.GITHUB)
        self.assertEqual(select_backend("gitlab.com"), Backend.GITLAB)

    def test_gitlab_substring_and_env(self) -> None:
        self.assertEqual(select_backend("gitlab.example.com"), Backend.GITLAB)
        with mock.patch.dict(os.environ, {"GITLAB_HOST": "forge.internal"}, clear=False):
            self.assertEqual(select_backend("forge.internal"), Backend.GITLAB)

    def test_ghe_defaults_to_github(self) -> None:
        self.assertEqual(select_backend("ghe.example.com"), Backend.GITHUB)
        with mock.patch.dict(os.environ, {"GITHUB_HOST": "ghe.example.com"}, clear=False):
            self.assertEqual(select_backend("ghe.example.com"), Backend.GITHUB)

    def test_empty_host_unavailable(self) -> None:
        with self.assertRaises(ForgeUnavailable):
            select_backend("")


class HostFromArgsTests(unittest.TestCase):
    def test_repo_and_draft(self) -> None:
        self.assertEqual(
            host_from_args({"repo": {"host": "github.com"}}, op="repo_view"),
            "github.com",
        )
        self.assertEqual(
            host_from_args(
                {"draft": {"repo": {"host": "gitlab.com"}}},
                op="issue_create",
            ),
            "gitlab.com",
        )

    def test_missing_raises(self) -> None:
        with self.assertRaises(ForgeUnavailable):
            host_from_args({}, op="repo_view")


class SearchHostFromArgsTests(unittest.TestCase):
    def test_query_repo_host(self) -> None:
        self.assertEqual(
            host_from_search_args(
                {
                    "query": {
                        "kind": "issues",
                        "repo": {"host": "gitlab.com", "owner": "acme", "name": "widget"},
                    }
                },
                op="search_issues",
            ),
            "gitlab.com",
        )
        self.assertEqual(
            select_backend(
                host_from_search_args(
                    {
                        "query": {
                            "repo": {"host": "github.com", "owner": "acme", "name": "widget"},
                        }
                    },
                    op="search_change_requests",
                )
            ),
            Backend.GITHUB,
        )

    def test_missing_raises(self) -> None:
        with self.assertRaises(ForgeUnavailable):
            host_from_search_args({}, op="search_issues")
        with self.assertRaises(ForgeUnavailable):
            host_from_search_args({"query": {}}, op="search_issues")


if __name__ == "__main__":
    unittest.main()
