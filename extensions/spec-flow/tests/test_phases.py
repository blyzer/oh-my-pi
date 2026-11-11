"""Pure spec-flow phase helpers (no live omp host)."""

from __future__ import annotations

import unittest

from omp_spec_flow.phases import (
	artifact_path,
	next_phase,
	payload_view,
	start_transition,
)


class PhaseTests(unittest.TestCase):
	def test_start_and_advance_path(self) -> None:
		transition = start_transition("T-1", "Spec")
		self.assertEqual(transition.phase, "brainstorm")
		self.assertEqual(next_phase("brainstorm"), "plan")
		self.assertEqual(artifact_path("T-1", "plan"), ".omp/specs/T-1/plan.md")

	def test_payload_view_inactive(self) -> None:
		view = payload_view(None)
		self.assertFalse(view.active)
		self.assertIsNone(view.phase)

	def test_payload_view_active(self) -> None:
		view = payload_view(start_transition("T-2", "Title"))
		self.assertTrue(view.active)
		self.assertEqual(view.ticket, "T-2")
		self.assertEqual(view.role, "brainstormer")


if __name__ == "__main__":
	unittest.main()
