"""Pure workflow DAG helpers (no live omp host)."""

from __future__ import annotations

import unittest

from omp_workflows.dag import (
	WorkflowEdge,
	WorkflowNode,
	parse_definition,
	topological_waves,
	workflow_id,
)


class DagTests(unittest.TestCase):
	def test_topological_waves(self) -> None:
		nodes = (
			WorkflowNode(name="scan", task="scan"),
			WorkflowNode(name="report", task="report"),
		)
		edges = (WorkflowEdge(upstream="scan", downstream="report"),)
		waves = topological_waves(nodes, edges)
		self.assertEqual(waves, (("scan",), ("report",)))

	def test_cycle_rejected(self) -> None:
		nodes = (
			WorkflowNode(name="a", task="a"),
			WorkflowNode(name="b", task="b"),
		)
		edges = (
			WorkflowEdge(upstream="a", downstream="b"),
			WorkflowEdge(upstream="b", downstream="a"),
		)
		with self.assertRaises(ValueError):
			topological_waves(nodes, edges)

	def test_parse_settings_json(self) -> None:
		name, nodes, edges = parse_definition(
			name=None,
			nodes=(),
			edges=(),
			settings_workflow='{"name":"review","nodes":[{"name":"scan","task":"Scan"}],"edges":[]}',
		)
		self.assertEqual(name, "review")
		self.assertEqual(nodes[0].name, "scan")
		self.assertTrue(workflow_id(name, nodes, edges).startswith("review:"))


if __name__ == "__main__":
	unittest.main()
