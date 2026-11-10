"""Host devices for ``omp.spec-flow`` (needs frozen omp at import).

Typed ``@omp.entry_kind`` / ``omp.journal.append`` were removed. Phase state is
kept under ``omp.state_dir()`` (rebuildable — see README Gaps). Availability
deltas still go through ``omp.devices.set_availability``.
"""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Literal

import omp

from .phases import (
	DEVICE_UNIVERSE,
	FlowTransition,
	PHASE_DEVICES,
	PHASE_ROLE,
	Phase,
	artifact_path,
	artifact_paths,
	next_phase,
	payload_view,
	start_transition,
)

_STORE_NAME = "spec-flow-transitions.jsonl"


@dataclass(frozen=True, slots=True)
class StartArgs:
	"""Name the coordinated ticket and specification."""

	ticket: str
	title: str


@dataclass(frozen=True, slots=True)
class AdvanceArgs:
	"""Supply the completed current-phase document before advancing."""

	artifact: str | None = None


@dataclass(frozen=True, slots=True)
class ArchiveArgs:
	"""Supply the completed apply document before archiving the bundle."""

	artifact: str | None = None


@dataclass(frozen=True, slots=True)
class StatusArgs:
	"""Request the current journal-derived workflow status."""


@dataclass(frozen=True, slots=True)
class FlowArchive:
	"""Keep the archived spec bundle reachable from state_dir truth."""

	ticket: str
	artifact_paths: tuple[str, ...]
	bundle: omp.BlobRef


@dataclass(frozen=True, slots=True)
class FlowPayload(omp.Payload):
	"""Describe the current phase, role, devices, and durable artifacts."""

	active: bool
	ticket: str | None
	title: str | None
	phase: Phase | None
	role: str | None
	devices: tuple[str, ...]
	artifact_paths: tuple[str, ...]
	bundle: omp.BlobRef | None = None


@dataclass(frozen=True, slots=True)
class PhaseOrderFault(omp.Fault):
	"""Refuse a phase operation that is invalid for the derived state."""

	operation: str
	current_phase: Phase | None
	expected_phase: Phase | None


@dataclass(frozen=True, slots=True)
class MissingArtifactFault(omp.Fault):
	"""Name the exact phase document required before a transition."""

	phase: Phase
	artifact_path: str
	ticket: str


async def _store_path() -> Path:
	root = await omp.state_dir()
	path = Path(str(root)) / "spec-flow"
	path.mkdir(parents=True, exist_ok=True)
	return path / _STORE_NAME


async def _transitions() -> tuple[FlowTransition, ...]:
	path = await _store_path()
	if not path.is_file():
		return ()
	rows: list[FlowTransition] = []
	for line in path.read_text(encoding="utf-8").splitlines():
		if not line.strip():
			continue
		raw = json.loads(line)
		rows.append(
			FlowTransition(
				ticket=raw["ticket"],
				title=raw["title"],
				phase=raw["phase"],
				role=raw["role"],
				devices=tuple(raw["devices"]),
				artifact_path=raw.get("artifact_path"),
			)
		)
	return tuple(rows)


async def _append_transition(transition: FlowTransition) -> None:
	path = await _store_path()
	with path.open("a", encoding="utf-8") as handle:
		handle.write(json.dumps(asdict(transition), separators=(",", ":")) + "\n")


def _current(rows: tuple[FlowTransition, ...]) -> FlowTransition | None:
	return rows[-1] if rows else None


async def _write_artifact(path: str, content: str) -> None:
	async with await omp.env.docs.open(
		omp.EnvPath(path), language="markdown", create=True
	) as doc:
		await doc.write(content)


async def _read_artifact(path: str) -> str:
	async with await omp.env.docs.open(omp.EnvPath(path), language="markdown") as doc:
		return await doc.read()


async def _set_phase_availability(phase: Phase) -> None:
	desired = frozenset(PHASE_DEVICES[phase])
	await omp.devices.set_availability(
		*(
			omp.AvailabilityDelta(
				path=path,
				mounted=path in desired,
				reason=None if path in desired else f"unavailable during {phase}",
			)
			for path in DEVICE_UNIVERSE
		)
	)


def _payload(
	current: FlowTransition | None, bundle: omp.BlobRef | None = None
) -> FlowPayload:
	view = payload_view(current)
	return FlowPayload(
		view.active,
		view.ticket,
		view.title,
		view.phase,
		view.role,
		view.devices,
		view.artifact_paths,
		bundle,
	)


@omp.device("flow", family="spec", rev=1, place="host")
async def flow(args: StatusArgs, ctx: omp.Context) -> FlowPayload:
	"""Report the current spec workflow from state_dir transitions."""

	del args, ctx
	return _payload(_current(await _transitions()))


@flow.subtool("start")
async def start(args: StartArgs, ctx: omp.Context) -> FlowPayload | PhaseOrderFault:
	"""Start a brainstorm phase for one durable ticket identity."""

	del ctx
	current = _current(await _transitions())
	if current is not None and current.phase != "archived":
		return PhaseOrderFault("start", current.phase, None)
	ticket = args.ticket.strip()
	title = args.title.strip()
	if not ticket or not title:
		return PhaseOrderFault("start", current.phase if current else None, None)
	transition = start_transition(ticket, title)
	await _append_transition(transition)
	await _set_phase_availability("brainstorm")
	return _payload(transition)


@flow.subtool("advance")
async def advance(
	args: AdvanceArgs, ctx: omp.Context
) -> FlowPayload | PhaseOrderFault | MissingArtifactFault:
	"""Write the current artifact and advance brainstorm to plan or plan to apply."""

	del ctx
	current = _current(await _transitions())
	if current is None or current.phase not in {"brainstorm", "plan"}:
		return PhaseOrderFault("advance", current.phase if current else None, "brainstorm")
	path = artifact_path(current.ticket, current.phase)
	artifact = (args.artifact or "").strip()
	if not artifact:
		return MissingArtifactFault(current.phase, path, current.ticket)
	await _write_artifact(path, artifact)
	nxt = next_phase(current.phase)
	assert nxt is not None
	transition = FlowTransition(
		ticket=current.ticket,
		title=current.title,
		phase=nxt,
		role=PHASE_ROLE[nxt],
		devices=PHASE_DEVICES[nxt],
		artifact_path=path,
	)
	await _append_transition(transition)
	await _set_phase_availability(nxt)
	return _payload(transition)


@flow.subtool("status")
async def status(args: StatusArgs, ctx: omp.Context) -> FlowPayload:
	"""Return the current derived phase without mutating it."""

	del args, ctx
	return _payload(_current(await _transitions()))


@flow.subtool("archive")
async def archive(
	args: ArchiveArgs, ctx: omp.Context
) -> FlowPayload | PhaseOrderFault | MissingArtifactFault:
	"""Write the apply artifact, spill the complete bundle, and record its reference."""

	del ctx
	current = _current(await _transitions())
	if current is None or current.phase != "apply":
		return PhaseOrderFault("archive", current.phase if current else None, "apply")
	path = artifact_path(current.ticket, "apply")
	artifact = (args.artifact or "").strip()
	if not artifact:
		return MissingArtifactFault("apply", path, current.ticket)
	await _write_artifact(path, artifact)
	paths = artifact_paths(current.ticket, "apply")
	documents = tuple([await _read_artifact(item) for item in paths])
	encoded = json.dumps(
		{
			"ticket": current.ticket,
			"title": current.title,
			"artifacts": [
				{"path": item, "content": content}
				for item, content in zip(paths, documents, strict=True)
			],
		},
		ensure_ascii=False,
		separators=(",", ":"),
		sort_keys=True,
	).encode("utf-8")
	bundle = await omp.env.blobs.put(encoded)
	transition = FlowTransition(
		ticket=current.ticket,
		title=current.title,
		phase="archived",
		role=PHASE_ROLE["archived"],
		devices=PHASE_DEVICES["archived"],
		artifact_path=path,
	)
	# Persist archive metadata beside transitions (no journal.append_atomic).
	store = await _store_path()
	archive_path = store.with_name("spec-flow-archive.json")
	archive_path.write_text(
		json.dumps(
			{
				"ticket": current.ticket,
				"artifact_paths": list(paths),
				"bundle_hex": getattr(bundle, "hex", str(bundle)),
				"bundle_size": getattr(bundle, "size", None),
			},
			separators=(",", ":"),
		),
		encoding="utf-8",
	)
	await _append_transition(transition)
	await _set_phase_availability("archived")
	del FlowArchive  # type only; receipt fields live in state_dir JSON
	return _payload(transition, bundle)


# Keep Literal import used by Phase annotations in faults.
_Literal = Literal
