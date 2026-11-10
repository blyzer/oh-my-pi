"""Pure fuzzy scoring helpers (no live omp host)."""

from __future__ import annotations

import unittest

from omp_fuzzy_index.score import fuzzy_score, trigrams, under_root


class ScoreTests(unittest.TestCase):
	def test_trigrams(self) -> None:
		grams = trigrams("abcdef")
		self.assertIn("abc", grams)
		self.assertIn("def", grams)

	def test_exact_beats_noise(self) -> None:
		query = "main"
		qgrams = trigrams(query)
		exact = fuzzy_score(
			query,
			"src/main.rs",
			query_grams=qgrams,
			candidate_grams=trigrams("src/main.rs"),
		)
		noise = fuzzy_score(
			query,
			"readme.md",
			query_grams=qgrams,
			candidate_grams=trigrams("readme.md"),
		)
		self.assertGreater(exact, noise)

	def test_under_root(self) -> None:
		self.assertTrue(under_root("src/a.py", "src"))
		self.assertFalse(under_root("tests/a.py", "src"))


if __name__ == "__main__":
	unittest.main()
