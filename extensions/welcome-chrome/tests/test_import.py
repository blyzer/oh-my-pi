"""Import smoke for the welcome-chrome package root."""

from __future__ import annotations

import unittest


class WelcomeChromeImportTests(unittest.TestCase):
	def test_package_root_imports(self) -> None:
		import omp_welcome_chrome

		self.assertTrue(hasattr(omp_welcome_chrome, "__all__"))


if __name__ == "__main__":
	unittest.main()
