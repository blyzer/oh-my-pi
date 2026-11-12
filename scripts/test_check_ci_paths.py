import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
HEAVY_GATE = "needs.changes.outputs.rust != 'false'"
LIGHT_GATE = "needs.changes.outputs.ci != 'false'"


class CheckCiPaths(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        shutil.copytree(ROOT / ".github", self.tmp / ".github")
        (self.tmp / "scripts").mkdir()
        shutil.copy(ROOT / "scripts/check-ci-paths.py", self.tmp / "scripts/check-ci-paths.py")
        self.ci = self.tmp / ".github/workflows/ci.yml"

    def run_check(self):
        done = subprocess.run(
            [sys.executable, "scripts/check-ci-paths.py"], cwd=self.tmp, capture_output=True, text=True
        )
        return done.returncode, done.stdout + done.stderr

    def mutate_ci(self, old, new, count=1):
        text = self.ci.read_text()
        self.assertEqual(text.count(old), count, old)
        self.ci.write_text(text.replace(old, new))

    def mutate_job_gate(self, name, old, new):
        text = self.ci.read_text()
        start = text.index(f"\n  {name}:\n") + 1
        nxt = text.find("\n  ", start + len(name) + 4)
        while nxt != -1 and text[nxt + 3 : nxt + 4] == " ":
            nxt = text.find("\n  ", nxt + 1)
        end = len(text) if nxt == -1 else nxt
        body = text[start:end]
        self.assertEqual(body.count(old), 1, f"{name}: {old}")
        self.ci.write_text(text[:start] + body.replace(old, new) + text[end:])

    def test_unmodified_tree_passes(self):
        code, out = self.run_check()
        self.assertEqual(code, 0, out)

    def test_missing_merge_group_trigger_fails(self):
        self.mutate_ci("  merge_group:\n    types: [checks_requested]\n", "")
        code, out = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("merge_group", out)

    def test_merge_group_without_checks_requested_fails(self):
        self.mutate_ci("types: [checks_requested]", "types: [destroyed]")
        code, out = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("checks_requested", out)

    def test_changes_job_without_the_no_pull_request_branch_fails(self):
        self.mutate_ci('if [ -z "$PULL_REQUEST" ]; then', 'if [ -z "$NEVER_SET" ]; then')
        code, out = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("no pull request number", out)

    def test_heavy_job_gated_on_ci_only_fails(self):
        self.mutate_job_gate("p7_linux", HEAVY_GATE, LIGHT_GATE)
        code, out = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("p7_linux", out)

    def test_light_job_gated_on_rust_fails(self):
        self.mutate_job_gate("protobuf", LIGHT_GATE, HEAVY_GATE)
        code, out = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("protobuf", out)

    def test_rust_path_missing_from_ci_paths_fails(self):
        with (self.tmp / ".github/ci-rust-paths.txt").open("a") as handle:
            handle.write("Unlisted/**\n")
        code, out = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("Unlisted/**", out)


if __name__ == "__main__":
    unittest.main()
