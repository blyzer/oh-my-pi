"""Pure Brave search response parsing (no live omp host)."""

from __future__ import annotations

import unittest

from omp_search_provider.parse import parse_results


class ParseTests(unittest.TestCase):
	def test_parse_web_results(self) -> None:
		payload = {
			"web": {
				"results": [
					{
						"title": "Example",
						"url": "https://example.test",
						"description": "A page",
					}
				]
			}
		}
		results = parse_results(payload, rank_offset=10)
		self.assertEqual(len(results), 1)
		self.assertEqual(results[0].rank, 11)
		self.assertEqual(results[0].snippet, "A page")

	def test_rejects_non_object(self) -> None:
		with self.assertRaises(ValueError):
			parse_results([])


if __name__ == "__main__":
	unittest.main()
