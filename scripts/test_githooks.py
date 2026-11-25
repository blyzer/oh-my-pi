#!/usr/bin/env python3
"""Behavioral tests for .githooks/commit-msg, .githooks/reference-transaction and the installer.

Every test runs real git in a throwaway repository, so the hooks are exercised the way git calls
them. Standard library only.
"""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
HOOKS = ROOT / ".githooks"
INSTALLER = ROOT / "scripts/install-githooks.sh"


def clean_env(**extra):
    env = {k: v for k, v in os.environ.items() if not k.startswith(("GIT_", "ALLOW_BRANCH_DELETE"))}
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null", **extra)
    return env


class RepoCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="githooks-test-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.repo = Path(self.tmp) / "repo"
        self.repo.mkdir()
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "t@example.com")
        self.git("config", "user.name", "t")
        self.git("commit", "-q", "--allow-empty", "-m", "init")

    def git(self, *args, env=None, check=True):
        result = subprocess.run(
            ["git", *args], cwd=self.repo, env=clean_env(**(env or {})), capture_output=True, text=True
        )
        if check and result.returncode != 0:
            self.fail(f"git {' '.join(args)} failed: {result.stderr}")
        return result

    def use_repo_hooks(self):
        self.git("config", "core.hooksPath", str(HOOKS))

    def branches(self):
        return self.git("branch", "--format=%(refname:short)").stdout.split()


class CommitMsgTests(RepoCase):
    def commit(self, message, *extra):
        return self.git("commit", "-q", "--allow-empty", *extra, "-m", message, check=False)

    def setUp(self):
        super().setUp()
        self.use_repo_hooks()

    def test_a_subject_of_exactly_72_characters_is_accepted(self):
        self.assertEqual(self.commit("x" * 72).returncode, 0)

    def test_a_subject_of_73_characters_is_rejected_with_its_length(self):
        result = self.commit("x" * 73)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("73 characters", result.stderr)

    def test_only_the_subject_counts_not_the_body(self):
        self.assertEqual(self.commit("short subject\n\n" + "y" * 200).returncode, 0)

    def test_no_verify_skips_the_hook(self):
        self.assertEqual(self.commit("x" * 90, "--no-verify").returncode, 0)

    def test_the_limit_can_be_changed_with_the_environment(self):
        result = self.git(
            "commit", "-q", "--allow-empty", "-m", "x" * 60, env={"GIT_SUBJECT_MAX": "50"}, check=False
        )
        self.assertNotEqual(result.returncode, 0)

    def test_generated_subjects_are_exempt(self):
        for subject in ("Merge branch 'a' into b", 'Revert "' + "x" * 80 + '"', "fixup! " + "x" * 80):
            with self.subTest(subject=subject[:20]):
                self.assertEqual(self.commit(subject).returncode, 0)

    def test_comment_lines_are_not_the_subject(self):
        message = self.repo / "msg.txt"
        message.write_text("# " + "c" * 200 + "\nreal subject\n")
        result = self.git("commit", "-q", "--allow-empty", "-F", str(message), check=False)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_multibyte_subject_is_counted_in_characters_when_a_utf8_locale_exists(self):
        listing = subprocess.run(["locale", "-a"], capture_output=True, text=True).stdout.lower()
        if "c.utf-8" not in listing and "en_us.utf-8" not in listing:
            self.skipTest("no UTF-8 locale installed")
        self.assertEqual(self.commit("é" * 72).returncode, 0)


class ReferenceTransactionTests(RepoCase):
    def setUp(self):
        super().setUp()
        self.git("branch", "feat")
        self.use_repo_hooks()

    def test_branch_force_delete_is_refused_and_the_branch_survives(self):
        result = self.git("branch", "-D", "feat", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("refusing to delete local branch: feat", result.stderr)
        self.assertIn("feat", self.branches())

    def test_safe_delete_of_a_merged_branch_is_refused_too(self):
        self.assertNotEqual(self.git("branch", "-d", "feat", check=False).returncode, 0)
        self.assertIn("feat", self.branches())

    def test_update_ref_delete_is_refused(self):
        self.assertNotEqual(self.git("update-ref", "-d", "refs/heads/feat", check=False).returncode, 0)
        self.assertIn("feat", self.branches())

    def test_the_environment_override_allows_the_delete(self):
        self.git("branch", "-D", "feat", env={"ALLOW_BRANCH_DELETE": "1"})
        self.assertNotIn("feat", self.branches())

    def test_rename_is_refused_because_git_renames_by_deleting_then_creating(self):
        self.assertNotEqual(self.git("branch", "-m", "feat", "feature", check=False).returncode, 0)
        self.assertIn("feat", self.branches())

    def test_rename_works_with_the_override(self):
        self.git("branch", "-m", "feat", "feature", env={"ALLOW_BRANCH_DELETE": "1"})
        self.assertEqual(sorted(self.branches()), ["feature", "main"])

    def test_delete_with_an_expected_old_value_is_refused_too(self):
        head = self.git("rev-parse", "feat").stdout.strip()
        self.assertNotEqual(self.git("update-ref", "-d", "refs/heads/feat", head, check=False).returncode, 0)
        self.assertIn("feat", self.branches())

    def test_housekeeping_that_packs_refs_is_not_blocked(self):
        self.git("pack-refs", "--all", "--prune")
        self.git("-C", str(self.repo), "pack-refs", "--all")
        self.git("gc", "-q")
        task = self.git("maintenance", "run", "--task=pack-refs", check=False)
        self.assertNotIn("refusing to delete", task.stderr)  # older git has no pack-refs task
        self.assertEqual(sorted(self.branches()), ["feat", "main"])

    def test_a_branch_named_pack_refs_is_still_protected(self):
        self.git("branch", "pack-refs")
        self.assertNotEqual(self.git("branch", "-D", "pack-refs", check=False).returncode, 0)
        self.assertIn("pack-refs", self.branches())

    def test_creating_switching_committing_and_fast_forwarding_are_allowed(self):
        self.git("switch", "-q", "-c", "work")
        self.git("commit", "-q", "--allow-empty", "-m", "on work")
        self.git("switch", "-q", "main")
        self.git("merge", "-q", "--ff-only", "work")
        self.assertIn("work", self.branches())

    def test_tags_remote_tracking_refs_and_stash_are_not_blocked(self):
        head = self.git("rev-parse", "HEAD").stdout.strip()
        self.git("tag", "v1")
        self.git("tag", "-d", "v1")
        self.git("update-ref", "refs/remotes/origin/gone", head)
        self.git("update-ref", "-d", "refs/remotes/origin/gone")

    def test_a_hook_that_is_not_installed_does_not_block(self):
        self.git("config", "--unset", "core.hooksPath")
        self.git("branch", "-D", "feat")


class InstallerTests(RepoCase):
    def install(self):
        return subprocess.run(
            ["bash", str(INSTALLER)], cwd=self.repo, env=clean_env(), capture_output=True, text=True
        )

    def setUp(self):
        super().setUp()
        (self.repo / ".githooks").mkdir()
        for name in ("commit-msg", "reference-transaction"):
            shutil.copy2(HOOKS / name, self.repo / ".githooks" / name)
        shutil.copy2(INSTALLER, self.repo / "scripts-install.sh")
        (self.repo / "scripts").mkdir()
        shutil.move(self.repo / "scripts-install.sh", self.repo / "scripts/install-githooks.sh")
        self.hooks = self.repo / ".git/hooks"

    def run_installer(self):
        return subprocess.run(
            ["bash", str(self.repo / "scripts/install-githooks.sh")],
            cwd=self.repo, env=clean_env(), capture_output=True, text=True,
        )

    def test_installs_both_hooks_executable_and_they_then_work(self):
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        for name in ("commit-msg", "reference-transaction"):
            self.assertTrue(os.access(self.hooks / name, os.X_OK), name)
        self.git("branch", "feat")
        self.assertNotEqual(self.git("branch", "-D", "feat", check=False).returncode, 0)
        self.assertNotEqual(
            self.git("commit", "-q", "--allow-empty", "-m", "x" * 80, check=False).returncode, 0
        )

    def test_is_idempotent(self):
        self.run_installer()
        again = self.run_installer()
        self.assertEqual(again.returncode, 0)
        self.assertEqual(again.stdout.count("up to date"), 2)

    def test_updates_an_older_copy_of_its_own_hook(self):
        self.run_installer()
        stale = self.hooks / "commit-msg"
        stale.write_text("#!/bin/sh\n# omp-githooks: commit-msg\n# old version\nexit 0\n")
        self.assertEqual(self.run_installer().returncode, 0)
        self.assertIn("subject", stale.read_text())

    def test_refuses_to_overwrite_a_foreign_hook_and_leaves_other_hooks_alone(self):
        self.hooks.mkdir(exist_ok=True)
        foreign = self.hooks / "commit-msg"
        foreign.write_text("#!/bin/sh\n# someone else's hook\nexit 0\n")
        lfs = self.hooks / "pre-push"
        lfs.write_text("#!/bin/sh\ngit lfs pre-push \"$@\"\n")
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("refusing to overwrite", result.stderr)
        self.assertIn("someone else's hook", foreign.read_text())
        self.assertIn("git lfs pre-push", lfs.read_text())
        self.assertTrue((self.hooks / "reference-transaction").exists())

    def test_installs_into_core_hooks_path_when_it_is_set(self):
        custom = Path(self.tmp) / "custom-hooks"
        self.git("config", "core.hooksPath", str(custom))
        self.assertEqual(self.run_installer().returncode, 0)
        self.assertTrue((custom / "commit-msg").exists())
        self.assertFalse((self.hooks / "commit-msg").exists())


if __name__ == "__main__":
    unittest.main()
