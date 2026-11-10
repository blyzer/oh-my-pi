"""Pure mapping unit tests — no network, no omp runtime."""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src"
if str(SRC) not in sys.path:
    sys.path.insert(0, str(SRC))

from omp_gitlab.mapping import (  # noqa: E402
    issue_create_body,
    map_issue,
    map_merge_request,
    map_notes,
    map_project,
    map_request_state,
    merge_request_create_body,
    project_path,
)

FIXTURES = Path(__file__).resolve().parent / "fixtures"
REPO = {"host": "gitlab.com", "owner": "acme", "name": "widget"}


def _load(name: str) -> object:
    return json.loads((FIXTURES / name).read_text(encoding="utf-8"))


class ProjectPathTests(unittest.TestCase):
    def test_encodes_nested_groups(self) -> None:
        self.assertEqual(project_path("group/sub", "proj"), "group%2Fsub%2Fproj")


class MergeRequestMappingTests(unittest.TestCase):
    def test_fixture_matches_change_request_shape(self) -> None:
        payload = _load("merge_request.json")
        assert isinstance(payload, dict)
        cr = map_merge_request(payload, REPO)
        self.assertEqual(cr["repo"], REPO)
        self.assertEqual(cr["number"], 7)
        self.assertEqual(cr["title"], "Fix the widget")
        self.assertEqual(cr["body"], "details")
        self.assertEqual(cr["state"], "open")
        self.assertFalse(cr["draft"])
        self.assertEqual(cr["base_ref"], "main")
        self.assertEqual(cr["head_ref"], "topic")
        self.assertEqual(cr["head_commit"], "bbb222")
        self.assertEqual(cr["base_commit"], "aaa111")
        self.assertEqual(
            cr["web_url"],
            "https://gitlab.com/acme/widget/-/merge_requests/7",
        )

    def test_merged_state(self) -> None:
        self.assertEqual(map_request_state("merged"), "merged")
        self.assertEqual(map_request_state("closed"), "closed")
        self.assertEqual(map_request_state("opened"), "open")


class IssueMappingTests(unittest.TestCase):
    def test_fixture_matches_issue_shape(self) -> None:
        payload = _load("issue.json")
        assert isinstance(payload, dict)
        issue = map_issue(payload, REPO)
        self.assertEqual(issue["number"], 3)
        self.assertEqual(issue["title"], "Bug")
        self.assertEqual(issue["state"], "open")
        self.assertEqual(issue["web_url"], "https://gitlab.com/acme/widget/-/issues/3")


class ProjectMappingTests(unittest.TestCase):
    def test_fixture_matches_repository_shape(self) -> None:
        payload = _load("project.json")
        assert isinstance(payload, dict)
        repo = map_project(payload, REPO)
        self.assertEqual(repo["default_branch"], "main")
        self.assertEqual(repo["clone_url"], "https://gitlab.com/acme/widget.git")
        self.assertEqual(repo["description"], "A widget")


class NotesMappingTests(unittest.TestCase):
    def test_skips_system_notes(self) -> None:
        payload = _load("notes.json")
        assert isinstance(payload, list)
        comments = map_notes(payload)
        self.assertEqual(len(comments), 1)
        self.assertEqual(comments[0]["author"], "alice")
        self.assertEqual(comments[0]["body"], "looks good")


class MergeRequestCreateBodyTests(unittest.TestCase):
    def test_fixture_maps_change_request_draft(self) -> None:
        draft = _load("change_request_draft.json")
        assert isinstance(draft, dict)
        body = merge_request_create_body(draft)
        self.assertEqual(
            body,
            {
                "title": "Fix the widget",
                "source_branch": "topic",
                "target_branch": "main",
                "description": "details",
                "draft": True,
            },
        )
        # repo is client-side only — never in the GitLab POST body
        self.assertNotIn("repo", body)
        self.assertNotIn("base", body)
        self.assertNotIn("head", body)
        self.assertNotIn("body", body)

    def test_omits_empty_description_and_false_draft(self) -> None:
        body = merge_request_create_body(
            {
                "repo": REPO,
                "title": "t",
                "body": "",
                "base": "main",
                "head": "topic",
                "draft": False,
            }
        )
        self.assertEqual(
            body,
            {
                "title": "t",
                "source_branch": "topic",
                "target_branch": "main",
            },
        )


class IssueCreateBodyTests(unittest.TestCase):
    def test_fixture_maps_issue_draft(self) -> None:
        draft = _load("issue_draft.json")
        assert isinstance(draft, dict)
        body = issue_create_body(draft)
        self.assertEqual(
            body,
            {
                "title": "Bug",
                "description": "steps",
            },
        )
        self.assertNotIn("repo", body)
        self.assertNotIn("body", body)

    def test_omits_empty_description(self) -> None:
        body = issue_create_body({"repo": REPO, "title": "t", "body": ""})
        self.assertEqual(body, {"title": "t"})


if __name__ == "__main__":
    unittest.main()
