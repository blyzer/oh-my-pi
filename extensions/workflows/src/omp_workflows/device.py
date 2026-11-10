"""Host device for ``omp.workflows`` (needs frozen omp at import).

Typed ``@omp.entry_kind`` / ``omp.journal.append`` were removed. Resume state is
kept under ``omp.state_dir()`` (rebuildable cache — see README Gaps).
"""

from __future__ import annotations

import asyncio
import json
from collections.abc import Mapping
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Literal

import omp

from .dag import (
	WorkflowBudget,
	WorkflowEdge,
	WorkflowNode,
	dependencies,
	idempotency_key,
	parse_definition,
	topological_waves,
	workflow_id,
)

_TerminalStatus = Literal["completed", "failed", "cancelled", "exhausted"]


@dataclass(frozen=True, slots=True)
class WorkflowArgs:
	"""A DAG supplied directly, or an empty request selecting the configured DAG."""

	name: str | None = None
	nodes: tuple[WorkflowNode, ...] = ()
	edges: tuple[WorkflowEdge, ...] = ()


@dataclass(frozen=True, slots=True)
class WorkflowNodeSettled:
	"""The durable terminal receipt for one workflow node."""

	workflow_id: str
	workflow: str
	node: str
	status: _TerminalStatus
	run_id: str
	session_id: str
	output_url: str
	transcript_url: str
	detail: str | None = None


@dataclass(frozen=True, slots=True)
class WorkflowNodeSkipped:
	"""A durable record that a failed dependency blocked one node."""

	workflow_id: str
	workflow: str
	node: str
	blocked_by: tuple[str, ...]
	reason: Literal["failed_dependency"] = "failed_dependency"


@dataclass(frozen=True, slots=True)
class WorkflowNodeOutcome:
	"""The folded outcome of one workflow node."""

	node: str
	status: str
	output_url: str | None = None
	blocked_by: tuple[str, ...] = ()
	resumed: bool = False


@dataclass(frozen=True, slots=True)
class WorkflowResult:
	"""The final projection of a workflow run."""

	workflow_id: str
	workflow: str
	outcomes: tuple[WorkflowNodeOutcome, ...]


async def _store_root(workflow_key: str) -> Path:
	root = await omp.state_dir()
	path = Path(str(root)) / "workflows" / workflow_key.replace(":", "_")
	path.mkdir(parents=True, exist_ok=True)
	return path


def _budget(value: WorkflowBudget) -> omp.agents.Budget:
	return omp.agents.Budget(
		max_requests=value.max_requests,
		max_input_tokens=value.max_input_tokens,
		max_output_tokens=value.max_output_tokens,
		max_usd=value.max_usd,
		max_wall=omp.Duration(value.max_wall) if value.max_wall is not None else None,
	)


def _spec(node: WorkflowNode, task: str, workflow_key: str) -> omp.agents.SubagentSpec:
	thinking = (
		None if node.thinking is None else omp.agents.ThinkingLevel(node.thinking)
	)
	isolation = omp.agents.Isolation(node.isolation)
	return omp.agents.SubagentSpec(
		task=task,
		name=node.name,
		agent=node.agent,
		system_prompt=node.system_prompt,
		model=node.model,
		thinking=thinking,
		allowed_devices=(
			frozenset(node.allowed_devices) if node.allowed_devices is not None else None
		),
		disallowed_devices=frozenset(node.disallowed_devices),
		isolation=isolation,
		max_depth=node.max_depth,
		worktree=node.worktree,
		merge=(
			omp.agents.MergeMode.PATCH
			if node.worktree
			else omp.agents.MergeMode.NONE
		),
		background=True,
		output_schema=node.output_schema,
		schema_mode=node.schema_mode,
		request_budget=node.request_budget,
		budget=_budget(node.budget),
		labels={"workflow": workflow_key, "node": node.name},
	)


def _task(
	node: WorkflowNode,
	deps: tuple[str, ...],
	settled: Mapping[str, WorkflowNodeSettled],
) -> str:
	if not deps:
		return node.task
	references = "\n".join(
		f"- {dependency}: {settled[dependency].output_url}" for dependency in deps
	)
	return (
		f"{node.task}\n\n"
		"Upstream outputs are durable references. Read them as needed; do not request inline copies:\n"
		f"{references}"
	)


async def _load_receipts(
	workflow_key: str,
) -> tuple[dict[str, WorkflowNodeSettled], dict[str, WorkflowNodeSkipped]]:
	root = await _store_root(workflow_key)
	settled: dict[str, WorkflowNodeSettled] = {}
	skipped: dict[str, WorkflowNodeSkipped] = {}
	for path in root.glob("*.json"):
		raw = json.loads(path.read_text(encoding="utf-8"))
		kind = raw.get("kind")
		if kind == "settled":
			receipt = WorkflowNodeSettled(**{k: v for k, v in raw.items() if k != "kind"})
			settled[receipt.node] = receipt
		elif kind == "skipped":
			record = WorkflowNodeSkipped(
				workflow_id=raw["workflow_id"],
				workflow=raw["workflow"],
				node=raw["node"],
				blocked_by=tuple(raw.get("blocked_by", ())),
			)
			skipped[record.node] = record
	return settled, skipped


async def _save_settled(receipt: WorkflowNodeSettled) -> None:
	root = await _store_root(receipt.workflow_id)
	key = idempotency_key(receipt.workflow_id, receipt.node, "settled")
	payload = {"kind": "settled", **asdict(receipt)}
	(root / f"{key}.json").write_text(json.dumps(payload, separators=(",", ":")), encoding="utf-8")


async def _save_skipped(record: WorkflowNodeSkipped) -> None:
	root = await _store_root(record.workflow_id)
	key = idempotency_key(record.workflow_id, record.node, "skipped")
	payload = {
		"kind": "skipped",
		"workflow_id": record.workflow_id,
		"workflow": record.workflow,
		"node": record.node,
		"blocked_by": list(record.blocked_by),
	}
	(root / f"{key}.json").write_text(json.dumps(payload, separators=(",", ":")), encoding="utf-8")


async def _settle(
	workflow_key: str,
	workflow_name: str,
	node: WorkflowNode,
	handle: omp.agents.SubagentHandle,
) -> WorkflowNodeSettled:
	try:
		result = await handle.wait()
	except asyncio.CancelledError:
		raise
	except Exception as error:
		receipt = WorkflowNodeSettled(
			workflow_id=workflow_key,
			workflow=workflow_name,
			node=node.name,
			status="failed",
			run_id=handle.run_id,
			session_id=handle.session_id,
			output_url=str(handle.output_url),
			transcript_url=str(handle.transcript_url),
			detail=type(error).__name__,
		)
	else:
		receipt = WorkflowNodeSettled(
			workflow_id=workflow_key,
			workflow=workflow_name,
			node=node.name,
			status=result.status.value,  # type: ignore[arg-type]
			run_id=result.run_id,
			session_id=result.session_id,
			output_url=str(result.output_url),
			transcript_url=str(result.transcript_url),
			detail=None if result.fault is None else type(result.fault).__name__,
		)
	await _save_settled(receipt)
	return receipt


async def _run(
	name: str,
	nodes: tuple[WorkflowNode, ...],
	edges: tuple[WorkflowEdge, ...],
) -> WorkflowResult:
	waves = topological_waves(nodes, edges)
	deps = dependencies(nodes, edges)
	by_name = {node.name: node for node in nodes}
	key = workflow_id(name, nodes, edges)
	settled, skipped = await _load_receipts(key)
	resumed = set(settled) | set(skipped)

	for wave in waves:
		ready: list[WorkflowNode] = []
		for node_name in wave:
			if node_name in settled or node_name in skipped:
				continue
			blocked_by = tuple(
				dependency
				for dependency in deps[node_name]
				if dependency in skipped
				or (
					dependency in settled
					and settled[dependency].status != omp.agents.RunStatus.COMPLETED.value
				)
			)
			if blocked_by:
				record = WorkflowNodeSkipped(
					workflow_id=key,
					workflow=name,
					node=node_name,
					blocked_by=blocked_by,
				)
				await _save_skipped(record)
				skipped[node_name] = record
			else:
				ready.append(by_name[node_name])

		if not ready:
			continue
		specs = tuple(
			_spec(node, _task(node, deps[node.name], settled), key) for node in ready
		)
		handles = await omp.agents.spawn_all(specs)
		receipts = await asyncio.gather(
			*(
				_settle(key, name, node, handle)
				for node, handle in zip(ready, handles, strict=True)
			)
		)
		settled.update((receipt.node, receipt) for receipt in receipts)

	outcomes: list[WorkflowNodeOutcome] = []
	for node in nodes:
		if receipt := settled.get(node.name):
			outcomes.append(
				WorkflowNodeOutcome(
					node=node.name,
					status=receipt.status,
					output_url=receipt.output_url,
					resumed=node.name in resumed,
				)
			)
		else:
			record = skipped[node.name]
			outcomes.append(
				WorkflowNodeOutcome(
					node=node.name,
					status="skipped",
					blocked_by=record.blocked_by,
					resumed=node.name in resumed,
				)
			)
	return WorkflowResult(key, name, tuple(outcomes))


@omp.device("workflow", family="agents", rev=1, place="host")
async def workflow(args: WorkflowArgs, ctx: omp.Context) -> WorkflowResult:
	"""Run or resume a declarative DAG in all-or-nothing topological waves."""

	name, nodes, edges = parse_definition(
		name=args.name,
		nodes=args.nodes,
		edges=args.edges,
		settings_workflow=ctx.settings.get("workflow", ""),
	)
	return await _run(name, nodes, edges)
