"""Pure remote-approval decision helpers (no live omp host)."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal

RemoteState = Literal["missing", "pending", "allow", "deny"]
UnreachableMode = Literal["ask", "deny", "allow"]


@dataclass(frozen=True, slots=True)
class RemoteReply:
	"""Represent the remote service's current answer for one ticket."""

	state: RemoteState
	reason: str | None = None


@dataclass(frozen=True, slots=True)
class LocalDecision:
	"""Host-free projection of an approval decision."""

	approved: bool
	reason: str | None
	source: Literal["external", "unavailable"]
	audited: bool


def decode_reply(status: int, body: object) -> RemoteReply:
	"""Decode an HTTP status + JSON body into a RemoteReply."""

	if status == 404:
		return RemoteReply("missing")
	if status < 200 or status >= 300:
		raise ValueError(f"approval service returned HTTP {status}")
	if not isinstance(body, dict):
		raise ValueError("approval service returned an invalid response")
	state = body.get("state")
	if state not in {"missing", "pending", "allow", "deny"}:
		raise ValueError(f"approval service returned unknown state {state!r}")
	reason = body.get("reason")
	return RemoteReply(state, None if reason is None else str(reason))


def remote_decision(reply: RemoteReply) -> LocalDecision | None:
	"""Translate a completed remote answer without inventing a pending answer."""

	if reply.state not in {"allow", "deny"}:
		return None
	return LocalDecision(
		approved=reply.state == "allow",
		reason=reply.reason,
		source="external",
		audited=False,
	)


def unreachable_decision(
	mode: UnreachableMode,
	*,
	allow_fail_open: bool,
	all_read: bool,
) -> LocalDecision | None:
	"""Map an unavailable service to local fallback, denial, or gated read-only allow."""

	if mode == "ask":
		return None
	approved = mode == "allow" and allow_fail_open and all_read
	if mode == "allow" and not approved:
		reason = "fail-open requires allow_fail_open=true and read-only reasons"
	else:
		reason = "remote approval service is unreachable"
	return LocalDecision(
		approved=approved,
		reason=reason,
		source="unavailable",
		audited=approved,
	)


def parse_unreachable(value: object) -> UnreachableMode:
	text = str(value) if value is not None else "ask"
	return text if text in {"ask", "deny", "allow"} else "deny"


__all__ = (
	"LocalDecision",
	"RemoteReply",
	"RemoteState",
	"UnreachableMode",
	"decode_reply",
	"parse_unreachable",
	"remote_decision",
	"unreachable_decision",
)
