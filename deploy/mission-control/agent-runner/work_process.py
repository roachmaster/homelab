"""Bounded cleanup for a caller-owned, new-session CLI launcher.

This controls only the local launcher process group created with
``start_new_session=True``. It is not proof that the separately supervised
systemd agent scope stopped; that proof remains the supervisor's responsibility.
No program, process group or command is selected from agent output.
"""

import math
import os
import signal
import subprocess
import time

_MAX_WAIT_SECONDS = 10
_POLL_SECONDS = .02


def _duration(value):
    if (type(value) not in (int, float) or not math.isfinite(value)
            or not 0 <= value <= _MAX_WAIT_SECONDS):
        raise ValueError("Launcher cleanup needs a bounded nonnegative timeout")
    return value


def _group_alive(group):
    try:
        os.killpg(group, 0)
        return True
    except ProcessLookupError:
        return False


def _signal_group(group, signum):
    try:
        os.killpg(group, signum)
    except ProcessLookupError:
        pass  # A stopped group still needs its owned parent reaped.


def _wait_stopped(process, group, seconds):
    deadline = time.monotonic() + seconds
    while True:
        if process.poll() is not None and not _group_alive(group):
            return True
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return False
        # Do not wait only for the leader: it can exit while a descendant keeps
        # stdout/stderr open. Group observation bounds those descendants too.
        if process.returncode is None:
            try:
                process.wait(timeout=min(remaining, _POLL_SECONDS))
            except subprocess.TimeoutExpired:
                pass
        else:
            time.sleep(min(remaining, _POLL_SECONDS))


def terminate_and_reap(process, *, term_seconds=2, kill_seconds=2):
    """Stop the owned launcher group with TERM, then KILL, and reap its parent.

    Return True only when the parent was reaped and its process group no longer
    exists. Permission/identity/observation failures return False. The caller
    must pass its own Popen created with start_new_session=True and retain the
    independent systemd scope-stop proof before publishing success.

    A leader that has already exited does not skip group cleanup: descendants
    may still own inherited pipes. A live leader must have pgid == sid == pid;
    neither this process nor its current group is ever a permitted target.
    """
    term_seconds, kill_seconds = _duration(term_seconds), _duration(kill_seconds)
    if process is None:
        return True
    group = process.pid
    if type(group) is not int or group <= 1 or group in (os.getpid(), os.getpgrp()):
        return False
    try:
        reaped = process.poll() is not None
        try:
            actual_group, session = os.getpgid(group), os.getsid(group)
        except ProcessLookupError:
            if not reaped and process.poll() is None:
                return False
        else:
            # An existing PID after our child has been reaped is a different
            # process. Refuse it rather than trusting a recycled numeric PID.
            if reaped or actual_group != group or session != group:
                return False
        if not _group_alive(group):
            return process.poll() is not None
        _signal_group(group, signal.SIGTERM)
        if _wait_stopped(process, group, term_seconds):
            return True
        _signal_group(group, signal.SIGKILL)
        return _wait_stopped(process, group, kill_seconds)
    except (OSError, subprocess.SubprocessError):
        return False
