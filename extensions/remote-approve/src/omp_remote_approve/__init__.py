"""Remote-approve rescue port (ADR 0049 D5)."""

from __future__ import annotations

from .decisions import (
	LocalDecision,
	RemoteReply,
	decode_reply,
	remote_decision,
	unreachable_decision,
)

__all__ = [
	"LocalDecision",
	"RemoteReply",
	"decode_reply",
	"remote_decision",
	"unreachable_decision",
]

# The approver and the poll hooks live in `device`, which needs a frozen `omp`.
# The host imports this entry module, so the registration has to happen here:
# without the import nothing is registered. Only a missing `omp` is tolerated
# (the pure unit tests run without it); any other import error is a real fault
# in `device` and must not be hidden.
try:
	from . import device as device
except ModuleNotFoundError as error:  # pragma: no cover - unit tests without omp
	if error.name != "omp":
		raise
	device = None  # type: ignore[assignment]
else:
	__all__.append("device")
