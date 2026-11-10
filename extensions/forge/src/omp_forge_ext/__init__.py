"""Dyn ``forge/*`` catalog owner (ADR 0049).

One mount owner routes ``forge/<op>`` by ``repo.host`` to ``omp_github`` or
``omp_gitlab``. Backend packages must not declare a competing ``forge`` parent.
"""

from __future__ import annotations

from . import dispatch
from .errors import ForgeUnavailable

__all__ = ["ForgeUnavailable", "dispatch"]

try:
    from . import devices as devices
except ImportError:  # pragma: no cover - unit tests without omp
    devices = None  # type: ignore[assignment]
else:
    __all__.append("devices")
