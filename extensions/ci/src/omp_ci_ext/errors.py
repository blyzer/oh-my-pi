"""Typed failures the dyn CI bridge maps to ``CiError::Unavailable``."""

from __future__ import annotations


class CiUnavailable(RuntimeError):
    """Host cannot be routed or the selected backend cannot serve the op."""

    def __init__(self, op: str, detail: str = "") -> None:
        self.op = op
        message = f"ci operation {op} unavailable"
        if detail:
            message = f"{message}: {detail}"
        super().__init__(message)
