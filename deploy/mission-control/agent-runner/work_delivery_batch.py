"""Bounded turns over one ready release worker; no claim or cleanup authority.

A pending release is due again after five seconds. Process the next due lease
before the publisher's idle poll can select the same old row again. This handles
a small backlog; it does not promise global fairness beyond the eight-job bound.
"""
import threading
from typing import Protocol

RELEASE_TURN_LIMIT = 8


class ReleaseTurn(Protocol):
    def run_once(self) -> bool: ...


class ReleaseBatch:
    """Sequentially finish each normal worker turn before acquiring another."""
    def __init__(self, worker: ReleaseTurn, *, stop: threading.Event,
                 limit: int = RELEASE_TURN_LIMIT):
        if type(limit) is not int or not 1 <= limit <= RELEASE_TURN_LIMIT:
            raise ValueError('Release batch limit must be between one and eight')
        self.worker, self.stop, self.limit = worker, stop, limit

    def run_once(self) -> bool:
        worked = False
        for _ in range(self.limit):
            if self.stop.is_set() or not self.worker.run_once():
                break
            worked = True
        return worked
