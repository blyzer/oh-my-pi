import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("affected_crates", ROOT / "scripts/affected-crates.py")
affected_crates = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(affected_crates)

CRATES = ["core", "mid", "tool", "app", "e2e", "reader", "data"]
DEPENDS_ON = {"mid": ["core"], "tool": ["core"], "app": ["mid"], "e2e": ["app"]}


def package_id(name):
    return f"{name} 0.1.0 (path+file:///ws/crates/{name})"


def fixture():
    packages = [
        {"id": package_id(n), "name": f"omp-{n}", "manifest_path": f"/ws/crates/{n}/Cargo.toml"} for n in CRATES
    ]
    nodes = [
        {"id": package_id(n), "deps": [{"pkg": package_id(d)} for d in DEPENDS_ON.get(n, [])]} for n in CRATES
    ]
    return {
        "workspace_root": "/ws",
        "workspace_members": [package_id(n) for n in CRATES],
        "packages": packages,
        "resolve": {"nodes": nodes},
    }


class AffectedCrates(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        for name in CRATES:
            (self.root / "crates" / name / "src").mkdir(parents=True)
            (self.root / "crates" / name / "src/lib.rs").write_text("pub fn f() {}\n")
        (self.root / "crates/reader/tests").mkdir()
        (self.root / "crates/reader/tests/read.rs").write_text('let p = "../../data/table.txt";\n')

    def affected(self, files, exclude=()):
        return affected_crates.affected(fixture(), files, self.root, set(exclude))

    def test_leaf_crate_change_affects_only_itself(self):
        self.assertEqual(self.affected(["crates/tool/src/lib.rs"]), ("subset", ["omp-tool"]))

    def test_core_crate_change_adds_every_dependent(self):
        scope, names = self.affected(["crates/core/src/lib.rs"])
        self.assertEqual(scope, "subset")
        self.assertEqual(names, ["omp-app", "omp-core", "omp-e2e", "omp-mid", "omp-tool"])

    def test_lockfile_change_reports_every_crate(self):
        scope, names = self.affected(["Cargo.lock"])
        self.assertEqual(scope, "all")
        self.assertEqual(names, sorted(f"omp-{n}" for n in CRATES))

    def test_docs_only_change_affects_nothing(self):
        self.assertEqual(self.affected(["docs/adr/0001-x.md", "README.md"]), ("none", []))

    def test_toolchain_cargo_dir_and_root_manifest_report_every_crate(self):
        for path in ("rust-toolchain.toml", ".cargo/config.toml", "Cargo.toml"):
            self.assertEqual(self.affected([path])[0], "all", path)

    def test_ci_scripts_and_justfile_affect_no_crate(self):
        self.assertEqual(self.affected(["justfile", "scripts/check-ci-paths.py", ".github/workflows/ci.yml"])[0], "none")

    def test_a_crate_that_reads_another_crates_files_is_affected_by_them(self):
        scope, names = self.affected(["crates/data/table.txt"])
        self.assertEqual((scope, names), ("subset", ["omp-data", "omp-reader"]))

    def test_excluded_package_is_dropped_and_can_leave_nothing(self):
        self.assertEqual(self.affected(["crates/e2e/src/lib.rs"], exclude=["omp-e2e"]), ("none", []))
        scope, names = self.affected(["crates/core/src/lib.rs"], exclude=["omp-e2e"])
        self.assertNotIn("omp-e2e", names)

    def test_command_line_writes_scope_and_cargo_arguments(self):
        metadata = self.root / "metadata.json"
        files = self.root / "files.txt"
        output = self.root / "github-output.txt"
        metadata.write_text(json.dumps(fixture()))
        files.write_text("crates/tool/src/lib.rs\n")
        done = subprocess.run(
            [sys.executable, str(ROOT / "scripts/affected-crates.py"), "--metadata", str(metadata),
             "--files", str(files), "--root", str(self.root), "--github-output", str(output)],
            capture_output=True, text=True,
        )
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(output.read_text(), "scope=subset\nargs=-p omp-tool\n")
        self.assertIn("omp-tool", done.stdout)


if __name__ == "__main__":
    unittest.main()
