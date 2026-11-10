"""Pure remote-approval decision helpers (no live omp host)."""

from __future__ import annotations

import unittest

from omp_remote_approve.decisions import (
	RemoteReply,
	decode_reply,
	remote_decision,
	unreachable_decision,
)


class DecisionTests(unittest.TestCase):
	def test_decode_missing(self) -> None:
		self.assertEqual(decode_reply(404, None).state, "missing")

	def test_remote_allow(self) -> None:
		decision = remote_decision(RemoteReply("allow", "ok"))
		assert decision is not None
		self.assertTrue(decision.approved)
		self.assertEqual(decision.source, "external")

	def test_unreachable_ask_abstains(self) -> None:
		self.assertIsNone(
			unreachable_decision("ask", allow_fail_open=False, all_read=True)
		)

	def test_unreachable_allow_requires_fail_open_and_read(self) -> None:
		denied = unreachable_decision("allow", allow_fail_open=False, all_read=True)
		assert denied is not None
		self.assertFalse(denied.approved)
		allowed = unreachable_decision("allow", allow_fail_open=True, all_read=True)
		assert allowed is not None
		self.assertTrue(allowed.approved)


if __name__ == "__main__":
	unittest.main()
