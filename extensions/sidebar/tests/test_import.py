"""Import smoke for the sidebar package root."""

from __future__ import annotations

import unittest


class SidebarImportTests(unittest.TestCase):
	def test_package_root_imports(self) -> None:
		import omp_sidebar

		self.assertTrue(hasattr(omp_sidebar, "__all__"))


if __name__ == "__main__":
	unittest.main()
