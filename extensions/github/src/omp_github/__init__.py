"""GitHub T2 forge + CI adapter (ADR 0049 D2.8 / T2 CI).

Backend package for dyn ``forge/*`` (mounted by ``omp_forge_ext``) and dyn
``ci/*`` (mounted by ``omp_ci_ext``). Mapping helpers stay importable without
the ``omp`` runtime for unit tests. This package does **not** declare a
``forge`` or ``ci`` parent (mount-collision rule).
"""

from __future__ import annotations

from . import ci, ci_mapping, mapping
from .errors import ForgeUnavailable

__all__ = ["ForgeUnavailable", "ci", "ci_mapping", "mapping"]
