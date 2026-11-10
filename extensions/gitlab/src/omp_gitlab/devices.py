"""Deprecated mount module — catalog ownership moved to ``omp_forge_ext``.

Kept as an empty import surface so older docs that mention
``omp_gitlab.devices`` fail closed rather than remounting ``forge/*``.
"""

from __future__ import annotations

# Mounts live in extensions/forge (omp.forge). Do not declare omp.devices.parent
# here — DynForgeAdapter fixed paths cannot have two owners.
