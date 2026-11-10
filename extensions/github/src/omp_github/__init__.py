"""GitHub T2 forge adapter (ADR 0049 D2.8).

Backend package for dyn ``forge/*`` (mounted by ``omp_forge_ext``). Mapping
helpers stay importable without the ``omp`` runtime for unit tests.
"""

from __future__ import annotations

from . import mapping
from .errors import ForgeUnavailable

__all__ = ["ForgeUnavailable", "mapping"]
