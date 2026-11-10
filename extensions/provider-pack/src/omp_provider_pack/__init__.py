"""Provider-pack rescue port (ADR 0049 D5).

Pure uniqueness helpers import without a live omp host. Provider registration
lives in ``omp_provider_pack.device``.
"""

from __future__ import annotations

from .validate import validate_pack

__all__ = ["validate_pack"]
