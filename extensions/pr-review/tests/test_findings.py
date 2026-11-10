"""Pure finding / area / comment helpers (no live omp host)."""

from __future__ import annotations

import unittest

from omp_pr_review.findings import (
	format_comment,
	parse_area_budgets,
	parse_finding,
	sort_findings,
)


class FindingsTests(unittest.TestCase):
	def test_parse_area_budgets_rejects_empty(self) -> None:
		with self.assertRaises(ValueError):
			parse_area_budgets([])

	def test_parse_finding_and_comment(self) -> None:
		finding = parse_finding(
			"security",
			{
				"area": "security",
				"severity": "high",
				"path": "src/main.rs",
				"line": 12,
				"rationale": "unchecked unwrap",
			},
		)
		body = format_comment([finding])
		self.assertIn("HIGH", body)
		self.assertIn("src/main.rs:12", body)
		ordered = sort_findings(
			[
				parse_finding(
					"correctness",
					{
						"area": "correctness",
						"severity": "low",
						"path": "a.py",
						"line": 1,
						"rationale": "nit",
					},
				),
				finding,
			]
		)
		self.assertEqual(ordered[0].severity, "high")


if __name__ == "__main__":
	unittest.main()
