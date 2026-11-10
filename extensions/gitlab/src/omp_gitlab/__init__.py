"""GitLab T2 forge adapter (ADR 0049 D2.8).

Import mounts ``forge`` + read/create subtools. Mapping helpers stay importable
without the ``omp`` runtime for unit tests.
"""

from __future__ import annotations

from . import mapping
from .errors import ForgeUnavailable

__all__ = ["ForgeUnavailable", "mapping"]

# Device registration requires the extension host's ``omp`` package. Mapping
# unit tests import ``omp_gitlab.mapping`` directly and never load this module's
# device side-effect; the entry module is ``omp_gitlab`` under the host.
try:
    from . import devices as devices
except ImportError:  # pragma: no cover - unit tests without omp
    devices = None  # type: ignore[assignment]
else:
    __all__.append("devices")
