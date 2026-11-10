"""Import smoke for the hover-cards package root."""

from __future__ import annotations

import unittest


class HoverCardsImportTests(unittest.TestCase):
	def test_package_root_imports(self) -> None:
		import omp_hover_cards

		self.assertTrue(hasattr(omp_hover_cards, "__all__"))


if __name__ == "__main__":
	unittest.main()
