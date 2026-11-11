"""Host hooks/command for ``omp.green-loop`` (needs frozen omp at import).

Durable session-journal typed entries (``@omp.entry_kind`` / ``omp.state``) were
removed from the frozen API. This port keeps rebuildable run/path state under
``omp.state_dir()`` and documents the session-truth Gap in the README.
"""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

import omp

from .affected import (
	_MAX_SUMMARY_BYTES,
	affected_command,
	bounded,
	collect_touched_paths,
	failure_digest,
)

_SCHEDULE_PROMPT = "[green-loop:run-affected-tests]"
_TOUCHED_NAME = "green-loop-touched.jsonl"
_RUN_NAME = "green-loop-run.json"


@dataclass(frozen=True, slots=True)
class _TouchedPaths:
	paths: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class _GreenRun:
	paths: tuple[str, ...]
	command: str
	passed: bool
	exit_code: int | None
	failure_digest: str | None
	summary: str


@dataclass(frozen=True, slots=True)
class _RunReport:
	ran: bool
	passed: bool | None
	paths: tuple[str, ...]
	command: str
	summary: str
	notified: bool


async def _state_root() -> Path:
	root = await omp.state_dir()
	path = Path(str(root)) / "green-loop"
	path.mkdir(parents=True, exist_ok=True)
	return path


async def _append_touched(paths: tuple[str, ...], *, key: str) -> None:
	root = await _state_root()
	# idempotency: skip duplicate call_id lines already present
	touched = root / _TOUCHED_NAME
	marker = f'"key":{json.dumps(key)}'
	if touched.is_file() and marker in touched.read_text(encoding="utf-8"):
		return
	with touched.open("a", encoding="utf-8") as handle:
		handle.write(json.dumps({"key": key, "paths": list(paths)}, separators=(",", ":")) + "\n")


async def _pending_paths() -> tuple[tuple[str, ...], _GreenRun | None]:
	root = await _state_root()
	previous: _GreenRun | None = None
	run_path = root / _RUN_NAME
	if run_path.is_file():
		raw = json.loads(run_path.read_text(encoding="utf-8"))
		previous = _GreenRun(
			paths=tuple(raw.get("paths", ())),
			command=str(raw.get("command", "")),
			passed=bool(raw.get("passed")),
			exit_code=raw.get("exit_code"),
			failure_digest=raw.get("failure_digest"),
			summary=str(raw.get("summary", "")),
		)
	paths: set[str] = set()
	touched = root / _TOUCHED_NAME
	if touched.is_file():
		for line in touched.read_text(encoding="utf-8").splitlines():
			if not line.strip():
				continue
			row = json.loads(line)
			for path in row.get("paths", ()):
				if isinstance(path, str):
					paths.add(path)
	return tuple(sorted(paths))[:256], previous


async def _write_run(run: _GreenRun) -> None:
	root = await _state_root()
	(root / _RUN_NAME).write_text(json.dumps(asdict(run), separators=(",", ":")), encoding="utf-8")
	# clear touched watermark after a completed fold
	touched = root / _TOUCHED_NAME
	if touched.is_file():
		touched.write_text("", encoding="utf-8")


def _paint_badge(passed: bool) -> None:
	tone = omp.ui.Token.OK if passed else omp.ui.Token.ERR
	label = "tests green" if passed else "tests red"
	omp.ui.set_status(
		"green-loop",
		omp.ui.tml("<segment fg={tone}>{label}</segment>", tone=tone, label=omp.ui.text(label)),
		order=80,
		side=omp.ui.Slot.STATUS_RIGHT,
	)


async def _run_green(ctx: omp.Context) -> _RunReport:
	paths, previous = await _pending_paths()
	command = affected_command(paths, ctx.settings)
	if not command:
		return _RunReport(False, None, paths, "", "No affected test command matched.", False)

	timeout = omp.Duration(str(ctx.settings.get("timeout", "10m")))
	completed = await omp.env.sh.run(command, timeout=timeout)
	output = completed.text()
	passed = completed.outcome is omp.env.Outcome.EXITED and completed.exit_code == 0
	summary = bounded(
		output.strip() or ("affected tests passed" if passed else "affected tests failed"),
		_MAX_SUMMARY_BYTES,
	)
	digest = None if passed else failure_digest(command, completed.exit_code, output)
	duplicate = (
		not passed
		and previous is not None
		and previous.failure_digest == digest
	)

	await _write_run(_GreenRun(paths, command, passed, completed.exit_code, digest, summary))
	_paint_badge(passed)

	notified = False
	if not passed and not duplicate:
		notice = bounded(
			f"Affected tests failed ({digest}).\nCommand: {command}\n{summary}",
			_MAX_SUMMARY_BYTES,
		)
		await omp.agents.inject(
			notice,
			mode=omp.agents.DeliveryMode.NEXT_TURN,
			visible=True,
			role="system",
		)
		notified = True
	return _RunReport(True, passed, paths, command, summary, notified)


@omp.hook(
	"tool_result",
	phase=omp.HookPhase.OBSERVE,
	when=omp.When(
		target=frozenset({omp.TargetKind.CORE}),
		name=frozenset({"edit", "write"}),
	),
)
async def remember_touched(event: omp.ToolResultEvent, ctx: omp.Context) -> None:
	"""Append paths from each settled successful core edit or write."""
	del ctx
	if event.outcome is not omp.OutcomeKind.OK or not isinstance(event.target, omp.CoreTool):
		return
	args = getattr(event.target, "args", {}) or {}
	payload = event.payload if isinstance(event.payload, dict) else {}
	paths = collect_touched_paths(args if isinstance(args, dict) else {}, payload)
	if paths:
		await _append_touched(paths, key=f"green-loop-touch:{event.call_id}")


@omp.hook("extension_activate")
async def arm_after_idle(event: omp.ExtensionActivateEvent, ctx: omp.Context) -> None:
	"""Upsert the session-scoped AfterIdle test firing and restore its badge."""
	del event
	await omp.agents.schedule(
		"green-loop-after-idle",
		omp.agents.AfterIdle(omp.Duration(str(ctx.settings.get("idle", "20s")))),
		omp.agents.Inject(
			_SCHEDULE_PROMPT,
			mode=omp.agents.DeliveryMode.NEXT_TURN,
			visible=False,
		),
		scope=omp.agents.ScheduleScope.SESSION,
		missed=omp.agents.MissedRunPolicy.SKIP,
		overlap="skip",
	)
	_paths, previous = await _pending_paths()
	del _paths
	if previous is not None:
		_paint_badge(previous.passed)


@omp.hook("before_agent_start", phase=omp.HookPhase.REVIEW)
async def run_after_idle(event: omp.BeforeAgentStartEvent, ctx: omp.Context) -> omp.Deny | None:
	"""Consume this extension's scheduled sentinel after running affected tests."""
	if (
		event.source is not omp.InputSource.SCHEDULE
		or event.schedule_id is None
		or event.text != _SCHEDULE_PROMPT
	):
		return None
	await _run_green(ctx)
	return omp.Deny("green-loop schedule handled", code="green_loop_scheduled")


@omp.command("green", description="Run tests affected by successful edits and writes.")
async def green(inv: omp.ui.Invocation, ctx: omp.Context) -> omp.ui.Consumed:
	"""Run the affected-test fold immediately and report its latest result."""
	del inv
	report = await _run_green(ctx)
	if not report.ran:
		return omp.ui.Consumed(notice=omp.ui.text(report.summary))
	state = "passed" if report.passed else "failed"
	return omp.ui.Consumed(
		notice=omp.ui.text(
			f"Affected tests {state} for {len(report.paths)} touched path(s)."
		)
	)


# Silence unused-import lint for typing re-exports used by hooks under stubs.
_Any = Any
