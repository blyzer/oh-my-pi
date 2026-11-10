"""Dyn ``ci/*`` catalog owner (ADR 0049).

One mount owner routes ``ci/<op>`` by provider host to ``omp_github`` or
``omp_gitlab`` CI backends. Backend packages must not declare a competing
``ci`` parent.
"""

from __future__ import annotations

from . import dispatch
from .errors import CiUnavailable

__all__ = ["CiUnavailable", "dispatch"]

try:
    from . import devices as devices
except ImportError:  # pragma: no cover - unit tests without omp
    devices = None  # type: ignore[assignment]
else:
    __all__.append("devices")
