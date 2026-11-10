"""Typed failures the dyn forge bridge maps to ``ForgeError::Unavailable``."""

from __future__ import annotations


class ForgeUnavailable(RuntimeError):
    """Host cannot be routed or the selected backend cannot serve the op."""

    def __init__(self, op: str, detail: str = "") -> None:
        self.op = op
        message = f"forge operation {op} unavailable"
        if detail:
            message = f"{message}: {detail}"
        super().__init__(message)
