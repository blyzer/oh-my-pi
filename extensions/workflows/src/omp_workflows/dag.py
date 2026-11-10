"""Pure workflow DAG helpers (no live omp host)."""

from __future__ import annotations

import hashlib
import json
from collections.abc import Mapping, Sequence
from dataclasses import asdict, dataclass
from typing import Any, Literal


@dataclass(frozen=True, slots=True)
class WorkflowBudget:
	"""Hard limits applied to one workflow node and its subtree."""

	max_requests: int | None = 8
	max_input_tokens: int | None = 200_000
	max_output_tokens: int | None = 40_000
	max_usd: float | None = 5.0
	max_wall: str | None = "20m"


@dataclass(frozen=True, slots=True)
class WorkflowNode:
	"""One declarative node (host SubagentSpec fields applied later)."""

	name: str
	task: str
	agent: str = "task"
	model: str | None = None
	system_prompt: str | None = None
	thinking: str | None = None
	allowed_devices: tuple[str, ...] | None = None
	disallowed_devices: tuple[str, ...] = ()
	isolation: str = "clean"
	max_depth: int = 0
	worktree: bool = False
	request_budget: int | None = None
	output_schema: Mapping[str, object] | None = None
	schema_mode: Literal["permissive", "strict"] = "permissive"
	budget: WorkflowBudget = WorkflowBudget()


@dataclass(frozen=True, slots=True)
class WorkflowEdge:
	"""A dependency from ``upstream`` to ``downstream``."""

	upstream: str
	downstream: str


def strict_mapping(value: object, label: str) -> Mapping[str, object]:
	if not isinstance(value, Mapping):
		raise TypeError(f"{label} must be an object")
	if any(not isinstance(key, str) for key in value):
		raise TypeError(f"{label} keys must be strings")
	return value


def configured_node(value: object) -> WorkflowNode:
	raw = dict(strict_mapping(value, "workflow node"))
	budget_raw = raw.get("budget", {})
	raw["budget"] = WorkflowBudget(**dict(strict_mapping(budget_raw, "node budget")))
	if thinking := raw.get("thinking"):
		raw["thinking"] = str(thinking)
	if isolation := raw.get("isolation"):
		raw["isolation"] = str(isolation)
	for field in ("allowed_devices", "disallowed_devices"):
		if field in raw and raw[field] is not None:
			items = raw[field]
			if not isinstance(items, list) or any(not isinstance(item, str) for item in items):
				raise TypeError(f"node {field} must be an array of strings")
			raw[field] = tuple(items)
	return WorkflowNode(**raw)


def configured_edge(value: object) -> WorkflowEdge:
	return WorkflowEdge(**dict(strict_mapping(value, "workflow edge")))


def parse_definition(
	*,
	name: str | None,
	nodes: Sequence[WorkflowNode],
	edges: Sequence[WorkflowEdge],
	settings_workflow: object,
) -> tuple[str, tuple[WorkflowNode, ...], tuple[WorkflowEdge, ...]]:
	if nodes:
		return name or "workflow", tuple(nodes), tuple(edges)
	if edges:
		raise ValueError("edges cannot be supplied without nodes")

	encoded = settings_workflow
	if not isinstance(encoded, str) or not encoded.strip():
		raise ValueError("supply nodes or configure the workflow JSON setting")
	raw = strict_mapping(json.loads(encoded), "configured workflow")
	nodes_raw = raw.get("nodes")
	edges_raw = raw.get("edges", [])
	if not isinstance(nodes_raw, list) or not isinstance(edges_raw, list):
		raise TypeError("configured workflow nodes and edges must be arrays")
	resolved = name or raw.get("name", "workflow")
	if not isinstance(resolved, str):
		raise TypeError("workflow name must be a string")
	return (
		resolved,
		tuple(configured_node(node) for node in nodes_raw),
		tuple(configured_edge(edge) for edge in edges_raw),
	)


def workflow_id(
	name: str, nodes: tuple[WorkflowNode, ...], edges: tuple[WorkflowEdge, ...]
) -> str:
	encoded = json.dumps(
		{
			"name": name,
			"nodes": [asdict(node) for node in nodes],
			"edges": [asdict(edge) for edge in edges],
		},
		sort_keys=True,
		separators=(",", ":"),
	).encode()
	return f"{name}:{hashlib.sha256(encoded).hexdigest()[:16]}"


def topological_waves(
	nodes: tuple[WorkflowNode, ...], edges: tuple[WorkflowEdge, ...]
) -> tuple[tuple[str, ...], ...]:
	if not nodes:
		raise ValueError("workflow must contain at least one node")
	order = [node.name for node in nodes]
	if any(not name.strip() for name in order):
		raise ValueError("workflow node names must not be empty")
	if len(set(order)) != len(order):
		raise ValueError("workflow node names must be unique")

	known = set(order)
	dependencies = {name: set[str]() for name in order}
	for edge in edges:
		if edge.upstream not in known or edge.downstream not in known:
			raise ValueError(
				f"workflow edge {edge.upstream!r} -> {edge.downstream!r} names an unknown node"
			)
		if edge.upstream == edge.downstream:
			raise ValueError(f"workflow node {edge.upstream!r} cannot depend on itself")
		dependencies[edge.downstream].add(edge.upstream)

	remaining = {name: len(dependencies[name]) for name in order}
	ready = [name for name in order if remaining[name] == 0]
	waves: list[tuple[str, ...]] = []
	visited = 0
	while ready:
		wave = tuple(ready)
		waves.append(wave)
		visited += len(wave)
		released = set(wave)
		ready = []
		for name in order:
			if remaining[name] == 0 or name in released:
				continue
			remaining[name] -= len(dependencies[name] & released)
			if remaining[name] == 0:
				ready.append(name)
	if visited != len(nodes):
		raise ValueError("workflow edges contain a cycle")
	return tuple(waves)


def dependencies(
	nodes: tuple[WorkflowNode, ...], edges: tuple[WorkflowEdge, ...]
) -> dict[str, tuple[str, ...]]:
	order = {node.name: index for index, node in enumerate(nodes)}
	deps: dict[str, list[str]] = {node.name: [] for node in nodes}
	for edge in edges:
		if edge.upstream not in deps[edge.downstream]:
			deps[edge.downstream].append(edge.upstream)
	return {
		name: tuple(sorted(values, key=order.__getitem__)) for name, values in deps.items()
	}


def idempotency_key(workflow_key: str, node: str, outcome: str) -> str:
	value = f"{workflow_key}\0{node}\0{outcome}".encode()
	return f"workflow-{hashlib.sha256(value).hexdigest()}"


# Keep Any for type checkers on Mapping dumps.
_Any = Any

__all__ = (
	"WorkflowBudget",
	"WorkflowEdge",
	"WorkflowNode",
	"configured_edge",
	"configured_node",
	"dependencies",
	"idempotency_key",
	"parse_definition",
	"strict_mapping",
	"topological_waves",
	"workflow_id",
)
