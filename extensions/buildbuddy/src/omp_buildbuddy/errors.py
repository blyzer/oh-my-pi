"""BuildBuddy extension errors."""

from __future__ import annotations


class BuildBuddyUnavailable(Exception):
    """BuildBuddy operation unavailable."""

    def __init__(self, op: str, reason: str) -> None:
        super().__init__(f"BuildBuddy {op}: {reason}")
        self.op = op
        self.reason = reason


class BuildBuddyNotGranted(BuildBuddyUnavailable):
    """The host did not place the BuildBuddy credential, so nothing was sent.

    Carries no credential text: the extension never holds the key.
    """

    def __init__(self, op: str) -> None:
        super().__init__(
            op, "the host did not place the buildbuddy credential (no grant); nothing was sent"
        )
