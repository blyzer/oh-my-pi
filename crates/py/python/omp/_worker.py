"""Embedded Python worker session server."""
from __future__ import annotations

import os

import omp_remote


def run_forever() -> None:
    """Bind the generation socket using the established remote protocol."""
    omp_remote.serve_forever(os.environ["OMP_WORKER_SOCKET"])
