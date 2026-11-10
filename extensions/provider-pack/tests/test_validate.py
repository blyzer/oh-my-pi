"""Pure provider-pack uniqueness checks (no live omp host)."""

from __future__ import annotations

import unittest
from dataclasses import dataclass

from omp_provider_pack.validate import validate_pack


@dataclass(frozen=True, slots=True)
class _Model:
	id: str


@dataclass(frozen=True, slots=True)
class _Provider:
	id: str
	models: tuple[_Model, ...]


class ValidateTests(unittest.TestCase):
	def test_unique_pack(self) -> None:
		pack = validate_pack(
			(
				_Provider("moonshot", (_Model("kimi-k2"),)),
				_Provider("zai-glm", (_Model("glm-4.6"),)),
			)
		)
		self.assertEqual(len(pack), 2)

	def test_duplicate_provider(self) -> None:
		with self.assertRaises(ValueError):
			validate_pack(
				(
					_Provider("moonshot", (_Model("a"),)),
					_Provider("moonshot", (_Model("b"),)),
				)
			)

	def test_duplicate_model(self) -> None:
		with self.assertRaises(ValueError):
			validate_pack(
				(
					_Provider("a", (_Model("same"),)),
					_Provider("b", (_Model("same"),)),
				)
			)


if __name__ == "__main__":
	unittest.main()
