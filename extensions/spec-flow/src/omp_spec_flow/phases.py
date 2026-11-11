"""Pure spec-flow phase machine helpers (no live omp host)."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal

Phase = Literal["brainstorm", "plan", "apply", "archived"]

PHASE_ORDER: tuple[Phase, ...] = ("brainstorm", "plan", "apply", "archived")
PHASE_ROLE: dict[Phase, str] = {
	"brainstorm": "brainstormer",
	"plan": "planner",
	"apply": "builder",
	"archived": "archivist",
}
PHASE_DEVICES: dict[Phase, tuple[str, ...]] = {
	"brainstorm": ("flow", "flow/advance", "flow/status"),
	"plan": ("flow", "flow/advance", "flow/status"),
	"apply": ("flow", "flow/archive", "flow/status"),
	"archived": ("flow", "flow/start", "flow/status"),
}
DEVICE_UNIVERSE = tuple(
	dict.fromkeys(path for paths in PHASE_DEVICES.values() for path in paths)
)


@dataclass(frozen=True, slots=True)
class FlowTransition:
	"""Record one durable phase transition and its model-facing declaration."""

	ticket: str
	title: str
	phase: Phase
	role: str
	devices: tuple[str, ...]
	artifact_path: str | None


@dataclass(frozen=True, slots=True)
class FlowPayloadView:
	"""Host-free projection of the current workflow status."""

	active: bool
	ticket: str | None
	title: str | None
	phase: Phase | None
	role: str | None
	devices: tuple[str, ...]
	artifact_paths: tuple[str, ...]


def artifact_path(ticket: str, phase: Phase) -> str:
	"""Return the stable workspace path for one phase artifact."""

	return f".omp/specs/{ticket}/{phase}.md"


def artifact_paths(ticket: str, through: Phase) -> tuple[str, ...]:
	"""List the artifact paths through a completed phase."""

	end = PHASE_ORDER.index(through)
	return tuple(
		artifact_path(ticket, phase)
		for phase in PHASE_ORDER[: end + 1]
		if phase != "archived"
	)


def current_transition(rows: tuple[FlowTransition, ...]) -> FlowTransition | None:
	return rows[-1] if rows else None


def payload_view(current: FlowTransition | None) -> FlowPayloadView:
	if current is None:
		return FlowPayloadView(False, None, None, None, None, (), ())
	completed: Phase = (
		"apply"
		if current.phase == "archived"
		else PHASE_ORDER[max(0, PHASE_ORDER.index(current.phase) - 1)]
	)
	paths = () if current.phase == "brainstorm" else artifact_paths(current.ticket, completed)
	return FlowPayloadView(
		True,
		current.ticket,
		current.title,
		current.phase,
		current.role,
		current.devices,
		paths,
	)


def start_transition(ticket: str, title: str) -> FlowTransition:
	return FlowTransition(
		ticket=ticket,
		title=title,
		phase="brainstorm",
		role=PHASE_ROLE["brainstorm"],
		devices=PHASE_DEVICES["brainstorm"],
		artifact_path=None,
	)


def next_phase(current: Phase) -> Phase | None:
	if current == "brainstorm":
		return "plan"
	if current == "plan":
		return "apply"
	return None


__all__ = (
	"DEVICE_UNIVERSE",
	"FlowPayloadView",
	"FlowTransition",
	"PHASE_DEVICES",
	"PHASE_ORDER",
	"PHASE_ROLE",
	"Phase",
	"artifact_path",
	"artifact_paths",
	"current_transition",
	"next_phase",
	"payload_view",
	"start_transition",
)
