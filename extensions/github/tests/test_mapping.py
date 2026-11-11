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

from omp_github.mapping import (  # noqa: E402
    issue_create_body,
    map_checkout_plan,
    map_comments,
    map_issue,
    map_pull_request,
    map_push_prep,
    map_repository,
    map_request_state,
    map_search_page,
    pull_request_create_body,
    repo_slug,
    split_list_filters,
)

FIXTURES = Path(__file__).resolve().parent / "fixtures"
REPO = {"host": "github.com", "owner": "acme", "name": "widget"}


def _load(name: str) -> object:
    return json.loads((FIXTURES / name).read_text(encoding="utf-8"))


class RepoSlugTests(unittest.TestCase):
    def test_joins_owner_and_name(self) -> None:
        self.assertEqual(repo_slug("acme", "widget"), "acme/widget")


class PullRequestMappingTests(unittest.TestCase):
    def test_fixture_matches_change_request_shape(self) -> None:
        payload = _load("pull_request.json")
        assert isinstance(payload, dict)
        cr = map_pull_request(payload, REPO)
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
        self.assertEqual(cr["web_url"], "https://github.com/acme/widget/pull/7")

    def test_merged_and_closed_state(self) -> None:
        self.assertEqual(map_request_state("closed", merged=True), "merged")
        self.assertEqual(map_request_state("closed", merged=False), "closed")
        self.assertEqual(map_request_state("open"), "open")


class CheckoutPlanMappingTests(unittest.TestCase):
    def test_fixture_matches_checkout_plan_shape(self) -> None:
        payload = _load("pull_request.json")
        assert isinstance(payload, dict)
        plan = map_checkout_plan(payload, REPO)
        self.assertEqual(plan["repo"], REPO)
        self.assertEqual(plan["number"], 7)
        self.assertEqual(plan["clone_url"], "https://github.com/acme/widget.git")
        self.assertEqual(plan["base_ref"], "main")
        self.assertEqual(plan["head_ref"], "topic")
        self.assertEqual(plan["head_sha"], "bbb222")
        self.assertNotIn("worktree", plan)

    def test_push_prep_omits_worktree(self) -> None:
        payload = _load("pull_request.json")
        assert isinstance(payload, dict)
        prep = map_push_prep(payload, REPO)
        self.assertEqual(prep["clone_url"], "https://github.com/acme/widget.git")
        self.assertEqual(prep["head_ref"], "topic")
        self.assertNotIn("worktree", prep)
        self.assertNotIn("head_sha", prep)


class IssueMappingTests(unittest.TestCase):
    def test_fixture_matches_issue_shape(self) -> None:
        payload = _load("issue.json")
        assert isinstance(payload, dict)
        issue = map_issue(payload, REPO)
        self.assertEqual(issue["number"], 3)
        self.assertEqual(issue["title"], "Bug")
        self.assertEqual(issue["state"], "open")
        self.assertEqual(issue["web_url"], "https://github.com/acme/widget/issues/3")


class RepositoryMappingTests(unittest.TestCase):
    def test_fixture_matches_repository_shape(self) -> None:
        payload = _load("repository.json")
        assert isinstance(payload, dict)
        repo = map_repository(payload, REPO)
        self.assertEqual(repo["default_branch"], "main")
        self.assertEqual(repo["clone_url"], "https://github.com/acme/widget.git")
        self.assertEqual(repo["description"], "A widget")
        self.assertEqual(repo["web_url"], "https://github.com/acme/widget")


class CommentsMappingTests(unittest.TestCase):
    def test_maps_author_time_and_body(self) -> None:
        payload = _load("comments.json")
        assert isinstance(payload, list)
        comments = map_comments(payload)
        self.assertEqual(len(comments), 2)
        self.assertEqual(comments[0]["author"], "alice")
        self.assertEqual(comments[0]["body"], "looks good")
        self.assertEqual(comments[1]["author"], "bob")


class PullRequestCreateBodyTests(unittest.TestCase):
    def test_fixture_maps_change_request_draft(self) -> None:
        draft = _load("change_request_draft.json")
        assert isinstance(draft, dict)
        body = pull_request_create_body(draft)
        self.assertEqual(
            body,
            {
                "title": "Fix the widget",
                "head": "topic",
                "base": "main",
                "body": "details",
                "draft": True,
            },
        )
        self.assertNotIn("repo", body)

    def test_omits_empty_body_and_false_draft(self) -> None:
        body = pull_request_create_body(
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
                "head": "topic",
                "base": "main",
            },
        )


class IssueCreateBodyTests(unittest.TestCase):
    def test_fixture_maps_issue_draft(self) -> None:
        draft = _load("issue_draft.json")
        assert isinstance(draft, dict)
        body = issue_create_body(draft)
        self.assertEqual(body, {"title": "Bug", "body": "steps"})
        self.assertNotIn("repo", body)

    def test_omits_empty_body(self) -> None:
        body = issue_create_body({"repo": REPO, "title": "t", "body": ""})
        self.assertEqual(body, {"title": "t"})


class SearchMappingTests(unittest.TestCase):
    def test_split_list_filters(self) -> None:
        filters, rest = split_list_filters("state:open author:alice label:bug fix me")
        self.assertEqual(filters, {"state": "open", "author": "alice", "label": "bug"})
        self.assertEqual(rest, "fix me")
        head, blank = split_list_filters("head:topic state:open")
        self.assertEqual(head, {"head": "topic", "state": "open"})
        self.assertEqual(blank, "")

    def test_issues_list_skips_pull_requests(self) -> None:
        payload = _load("issues_list.json")
        assert isinstance(payload, list)
        page = map_search_page("issues", payload, REPO)
        self.assertEqual(page["kind"], "issues")
        self.assertEqual(len(page["hits"]), 1)
        hit = page["hits"][0]
        self.assertEqual(hit["number"], 3)
        self.assertEqual(hit["title"], "Bug")
        self.assertEqual(hit["state"], "open")
        self.assertEqual(hit["repo"], REPO)

    def test_pulls_list_maps_merged_state(self) -> None:
        payload = _load("pulls_list.json")
        assert isinstance(payload, list)
        page = map_search_page("change_requests", payload, REPO)
        self.assertEqual(page["kind"], "change_requests")
        self.assertEqual(len(page["hits"]), 2)
        self.assertEqual(page["hits"][0]["state"], "open")
        self.assertEqual(page["hits"][1]["number"], 8)
        self.assertEqual(page["hits"][1]["state"], "merged")


if __name__ == "__main__":
    unittest.main()
