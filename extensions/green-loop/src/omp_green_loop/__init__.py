"""Green-loop rescue port (ADR 0049 D5).

Pure helpers import without a live omp host. Device/hook registration lives in
``omp_green_loop.device``.
"""

from __future__ import annotations

from .affected import (
	DEFAULT_AFFECTED,
	affected_command,
	collect_touched_paths,
	failure_digest,
)

__all__ = [
	"DEFAULT_AFFECTED",
	"affected_command",
	"collect_touched_paths",
	"failure_digest",
]
