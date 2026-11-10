"""Typed failures the dyn forge bridge maps to ``ForgeError::Unavailable``."""

from __future__ import annotations


class ForgeUnavailable(RuntimeError):
    """Operation is not implemented or the GitLab host cannot serve it.

    The envd ``LiveForgeDeviceClient`` / ``DeviceControlFactory::call_mounted``
    path maps callback failures to ``ForgeError::Unavailable``. Prefer raising
    this for write ops and missing credentials rather than inventing mutations.
    """

    def __init__(self, op: str, detail: str = "") -> None:
        self.op = op
        message = f"forge operation {op} unavailable"
        if detail:
            message = f"{message}: {detail}"
        super().__init__(message)
