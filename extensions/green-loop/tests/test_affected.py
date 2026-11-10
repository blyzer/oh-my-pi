"""Pure affected-command helpers (no live omp host)."""

from __future__ import annotations

import unittest

from omp_green_loop.affected import (
	DEFAULT_AFFECTED,
	affected_command,
	collect_touched_paths,
	failure_digest,
)


class AffectedTests(unittest.TestCase):
	def test_collect_touched_paths(self) -> None:
		paths = collect_touched_paths(
			{"path": "src/app.py", "paths": ["lib/util.py"]},
			{"patch": "[pkg/mod.py#ab12]\n+hello"},
		)
		self.assertEqual(paths, ("lib/util.py", "pkg/mod.py", "src/app.py"))

	def test_affected_command_python(self) -> None:
		command = affected_command(("src/x.py",), {"affected": DEFAULT_AFFECTED})
		self.assertIn("pytest", command)
		self.assertIn("src/x.py", command)

	def test_failure_digest_stable(self) -> None:
		a = failure_digest("cargo test", 1, "failed in 12ms")
		b = failure_digest("cargo test", 1, "failed in 99ms")
		self.assertEqual(a, b)


if __name__ == "__main__":
	unittest.main()
