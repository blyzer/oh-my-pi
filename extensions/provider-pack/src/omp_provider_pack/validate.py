"""Pure provider-pack uniqueness checks (no live omp host)."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Protocol


class _ModelLike(Protocol):
	id: str


class _ProviderLike(Protocol):
	id: str
	models: Sequence[_ModelLike]


def validate_pack(specs: Sequence[_ProviderLike]) -> tuple[_ProviderLike, ...]:
	"""Reject ambiguous provider or model identifiers before registration."""

	providers: set[str] = set()
	models: dict[str, str] = {}
	for spec in specs:
		if spec.id in providers:
			raise ValueError(f"duplicate provider id in provider pack: {spec.id}")
		providers.add(spec.id)
		for model in spec.models:
			owner = models.get(model.id)
			if owner is not None:
				raise ValueError(
					f"duplicate model id in provider pack: {model.id} "
					f"({owner}, {spec.id})"
				)
			models[model.id] = spec.id
	return tuple(specs)


__all__ = ("validate_pack",)
