#!/usr/bin/env python3
"""Embedded-Python context, journal, and sessions spec cases."""

from __future__ import annotations

import shutil
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from harness import call, drive, extension_fixture, introspect  # noqa: E402

CONTEXT_SYMBOLS = [
	"omp.context.Anchor",
	"omp.context.CancelCompaction",
	"omp.context.CompactionBusy",
	"omp.context.CompactionEvent",
	"omp.context.CompactionOutcome",
	"omp.context.CompactionRefused",
	"omp.context.CompactionTier",
	"omp.context.CompactionVerdict",
	"omp.context.ContextGone",
	"omp.context.ContextPatch",
	"omp.context.ContextResetEvent",
	"omp.context.ContextUsage",
	"omp.context.ContextView",
	"omp.context.CustomSummary",
	"omp.context.DelegateCompaction",
	"omp.context.DropParts",
	"omp.context.Insert",
	"omp.context.MessageKind",
	"omp.context.MessageRef",
	"omp.context.NoVerdict",
	"omp.context.PatchRejected",
	"omp.context.PinBudgetExceeded",
	"omp.context.Prune",
	"omp.context.Reorder",
	"omp.context.Replace",
	"omp.context.ToolRef",
	"omp.context.compact",
	"omp.context.epoch",
	"omp.context.lane",
	"omp.context.pin",
	"omp.context.unpin",
	"omp.context.usage",
	"omp.context.view",
]

SESSIONS_SYMBOLS = [
	"omp.sessions.Bucket",
	"omp.sessions.Cost",
	"omp.sessions.GroupBy",
	"omp.sessions.SessionAccessDenied",
	"omp.sessions.SessionError",
	"omp.sessions.SessionFilter",
	"omp.sessions.SessionInfo",
	"omp.sessions.SessionKind",
	"omp.sessions.SessionLink",
	"omp.sessions.SessionNode",
	"omp.sessions.SessionNotFound",
	"omp.sessions.SessionSetup",
	"omp.sessions.SessionStatus",
	"omp.sessions.SessionTransitionDenied",
	"omp.sessions.SessionTransitionIndeterminate",
	"omp.sessions.TitleSource",
	"omp.sessions.Usage",
	"omp.sessions.UsageAccuracy",
	"omp.sessions.UsageBucket",
	"omp.sessions.UsageQuery",
	"omp.sessions.UsageReport",
	"omp.sessions.branch",
	"omp.sessions.create",
	"omp.sessions.current",
	"omp.sessions.delete",
	"omp.sessions.get",
	"omp.sessions.journal",
	"omp.sessions.lineage",
	"omp.sessions.list",
	"omp.sessions.rename",
	"omp.sessions.resume",
	"omp.sessions.tree",
	"omp.sessions.usage",
]

JOURNAL_SYMBOLS = [
	"omp.journal.EntryAccessDenied",
	"omp.journal.EntryId",
	"omp.journal.EntryTooLarge",
	"omp.journal.EntryUndecodable",
	"omp.journal.JournalEntry",
	"omp.journal.JournalError",
	"omp.journal.JournalIndeterminate",
	"omp.journal.decode",
]

COVERS = {"py": CONTEXT_SYMBOLS + SESSIONS_SYMBOLS + JOURNAL_SYMBOLS, "rpc": []}


class PyContextSessions(unittest.TestCase):
	def test_live_surface_introspects_and_constructs_value_families(self):
		self.assertEqual(set(introspect(COVERS["py"]).values()), {"ok"})
		with extension_fixture("context/surface") as directory:
			project = Path(tempfile.mkdtemp(prefix="omp-qa-context-surface-"))
			try:
				result = drive(
					call("hello"),
					"done",
					prompt="construct the context surface",
					extensions=[directory],
					project=project,
					timeout=60,
				)
				self.assertFalse(result.timed_out)
				self.assertEqual(result.exit_code, 0, result.stderr)
				self.assertIn("constructed", str(result.mock["captures"][1]))
			finally:
				shutil.rmtree(project, ignore_errors=True)

	@unittest.expectedFailure
	def test_prompt_slot_and_context_patch_are_provider_visible(self):
		"""Ledger: admitted Python prompt slots and thread projections are not composed."""
		with extension_fixture("context/prompt") as directory:
			result = drive(
				"assistant-one",
				"assistant-two",
				prompt="first turn",
				args=["--follow-up", "second turn"],
				extensions=[directory],
				timeout=60,
			)
			self.assertFalse(result.timed_out)
			self.assertEqual(result.exit_code, 0, result.stderr)
			self.assertEqual(len(result.mock["captures"]), 2)
			self.assertTrue(all("QA_CONTEXT_PROMPT_SLOT_91" in str(capture) for capture in result.mock["captures"]))
			self.assertIn("QA_CONTEXT_PATCH_91", str(result.mock["captures"][1]))


if __name__ == "__main__":
	unittest.main()
