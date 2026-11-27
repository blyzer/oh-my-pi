import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

JUSTFILE = """\
# E2E.
[group('e2e')]
e2e-core:
    cargo nextest run -p omp-e2e --locked --lib --test alpha

[group('e2e')]
e2e-more:
    cargo nextest run -p omp-e2e --locked \\
        --test beta \\
        --test gamma

[group('e2e')]
e2e: e2e-core e2e-more
"""

CI = """\
jobs:
  rust:
    steps:
      - run: cargo nextest run --profile ci -p omp-e2e --test alpha --test beta
"""

MACOS = """\
cargo nextest run --profile ci -p omp-e2e --test gamma --locked
"""


class CheckE2eWiring(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        (self.tmp / "scripts").mkdir()
        shutil.copy(ROOT / "scripts/check-e2e-wiring.py", self.tmp / "scripts/check-e2e-wiring.py")
        tests = self.tmp / "crates/e2e/tests"
        tests.mkdir(parents=True)
        for name in ("alpha", "beta", "gamma"):
            (tests / f"{name}.rs").write_text("")
        (self.tmp / "justfile").write_text(JUSTFILE)
        workflows = self.tmp / ".github/workflows"
        workflows.mkdir(parents=True)
        (workflows / "ci.yml").write_text(CI)
        (self.tmp / "scripts/ci-macos-objective.sh").write_text(MACOS)

    def run_check(self):
        done = subprocess.run(
            [sys.executable, "scripts/check-e2e-wiring.py"],
            cwd=self.tmp,
            capture_output=True,
            text=True,
        )
        return done.returncode, done.stdout + done.stderr

    def edit(self, relative, old, new):
        path = self.tmp / relative
        text = path.read_text()
        self.assertEqual(text.count(old), 1, old)
        path.write_text(text.replace(old, new))

    def test_a_fully_wired_tree_passes(self):
        code, out = self.run_check()
        self.assertEqual(code, 0, out)
        self.assertIn("3 proofs", out)

    def test_a_proof_just_e2e_never_runs_is_refused(self):
        self.edit("justfile", "        --test gamma\n", "")
        self.edit("justfile", "--test beta \\\n", "--test beta\n")
        code, out = self.run_check()
        self.assertEqual(code, 1, out)
        self.assertIn("gamma: `just e2e` never runs it", out)
        self.assertNotIn("beta: `just e2e`", out)

    def test_a_proof_in_a_recipe_e2e_does_not_reach_is_refused(self):
        self.edit("justfile", "e2e: e2e-core e2e-more\n", "e2e: e2e-core\n")
        code, out = self.run_check()
        self.assertEqual(code, 1, out)
        self.assertIn("beta: `just e2e` never runs it", out)
        self.assertIn("gamma: `just e2e` never runs it", out)

    def test_a_recipe_reached_through_another_recipe_counts(self):
        self.edit("justfile", "e2e: e2e-core e2e-more\n", "e2e-all: e2e-core e2e-more\n\ne2e: e2e-all\n")
        code, out = self.run_check()
        self.assertEqual(code, 0, out)

    def test_a_proof_no_ci_job_runs_is_refused(self):
        self.edit("scripts/ci-macos-objective.sh", "--test gamma ", "")
        code, out = self.run_check()
        self.assertEqual(code, 1, out)
        self.assertIn("gamma: no CI job runs it", out)
        self.assertNotIn("alpha: no CI job", out)

    def test_a_proof_named_only_in_a_comment_does_not_count(self):
        self.edit("justfile", "# E2E.\n", "# E2E. Not run here: --test delta\n")
        (self.tmp / "crates/e2e/tests/delta.rs").write_text("")
        code, out = self.run_check()
        self.assertEqual(code, 1, out)
        self.assertIn("delta: `just e2e` never runs it", out)

    def test_a_directory_target_is_a_proof(self):
        (self.tmp / "crates/e2e/tests/delta").mkdir()
        (self.tmp / "crates/e2e/tests/delta/main.rs").write_text("")
        code, out = self.run_check()
        self.assertEqual(code, 1, out)
        self.assertIn("delta: `just e2e` never runs it", out)
        self.assertIn("delta: no CI job runs it", out)

    def test_a_support_file_without_main_is_not_a_proof(self):
        (self.tmp / "crates/e2e/tests/support").mkdir()
        (self.tmp / "crates/e2e/tests/support/helpers.rs").write_text("")
        code, out = self.run_check()
        self.assertEqual(code, 0, out)

    def test_a_target_naming_no_file_is_refused_in_just_and_in_ci(self):
        self.edit("justfile", "--test alpha", "--test alpha --test missing_one")
        self.edit("scripts/ci-macos-objective.sh", "--test gamma", "--test gamma --test missing_two")
        code, out = self.run_check()
        self.assertEqual(code, 1, out)
        self.assertIn("just runs `--test missing_one`", out)
        self.assertIn("CI runs `--test missing_two`", out)

    def test_other_test_flags_are_not_targets(self):
        self.edit("justfile", "--test alpha", "--test alpha --test-threads=1 --tests")
        code, out = self.run_check()
        self.assertEqual(code, 0, out)

    def test_a_missing_entry_recipe_is_reported(self):
        self.edit("justfile", "e2e: e2e-core e2e-more\n", "")
        code, out = self.run_check()
        self.assertEqual(code, 1, out)
        self.assertIn("no `e2e` recipe", out)


if __name__ == "__main__":
    unittest.main()
