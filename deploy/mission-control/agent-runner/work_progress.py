"""Bounded public agent reports; this module also runs as the scoped child helper.

Reports are model claims, never task-state or execution authority. No credentials,
task identities, provider events, tool arguments or private reasoning cross here.
"""

import fcntl
import json
import os
from pathlib import Path
import re
import stat
import sys
from uuid import uuid4

MAX_REPORTS = 128
MAX_SUMMARY_BYTES = 4096
MAX_RECORD_BYTES = 16_384
REPORT_NAME = re.compile(r"([0-9]{6})\.json")
TEMP_NAME = re.compile(r"\.report-[0-9a-f]{32}\.tmp")


class ProgressError(Exception):
    def __init__(self):
        super().__init__("Agent public progress channel is invalid or unavailable.")


class ProgressSinkError(Exception):
    def __init__(self):
        super().__init__("Agent public progress persistence is uncertain.")


def _object(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ProgressError()
        value[key] = item
    return value


def validate_report(value, *, expected=None):
    if (not isinstance(value, dict) or set(value) != {"version", "sequence", "kind", "summary"}
            or type(value["version"]) is not int or value["version"] != 1
            or type(value["sequence"]) is not int or not 1 <= value["sequence"] <= MAX_REPORTS
            or (expected is not None and value["sequence"] != expected)
            or value["kind"] != ("plan" if value["sequence"] == 1 else "progress")
            or not isinstance(value["summary"], str) or not value["summary"].strip()
            or any((ord(c) < 32 and c not in "\n\t") or 127 <= ord(c) < 160 for c in value["summary"])):
        raise ProgressError()
    try:
        if len(value["summary"].encode("utf-8")) > MAX_SUMMARY_BYTES:
            raise ProgressError()
    except UnicodeError:
        raise ProgressError() from None
    return value


def _read(directory, name):
    descriptor = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or before.st_size > MAX_RECORD_BYTES:
            raise ProgressError()
        chunks = []
        size = 0
        while True:
            chunk = os.read(descriptor, MAX_RECORD_BYTES + 1 - size)
            if not chunk:
                break
            chunks.append(chunk)
            size += len(chunk)
            if size > MAX_RECORD_BYTES:
                raise ProgressError()
        after = os.fstat(descriptor)
        if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (after.st_size, after.st_mtime_ns, after.st_ctime_ns):
            raise ProgressError()
        return b"".join(chunks)
    finally:
        os.close(descriptor)


def _names(directory):
    names = os.listdir(directory)
    if len(names) > MAX_REPORTS * 2 + 1:
        raise ProgressError()
    reports = {}
    for name in names:
        matched = REPORT_NAME.fullmatch(name)
        if matched:
            sequence = int(matched[1])
            if not 1 <= sequence <= MAX_REPORTS:
                raise ProgressError()
            reports[sequence] = name
        elif name == ".lock" or TEMP_NAME.fullmatch(name):
            try:
                entry = os.stat(name, dir_fd=directory, follow_symlinks=False)
            except FileNotFoundError:
                if TEMP_NAME.fullmatch(name):
                    continue  # A cooperative writer just atomically published it.
                raise
            if not stat.S_ISREG(entry.st_mode) or entry.st_nlink != 1 or entry.st_size > MAX_RECORD_BYTES:
                raise ProgressError()
        else:
            raise ProgressError()
    if set(reports) != set(range(1, len(reports) + 1)):
        raise ProgressError()
    return reports


def _decode(data, sequence):
    try:
        value = json.loads(data.decode("utf-8"), object_pairs_hook=_object,
                           parse_constant=lambda _v: (_ for _ in ()).throw(ProgressError()))
        return validate_report(value, expected=sequence)
    except (UnicodeError, ValueError, RecursionError):
        raise ProgressError() from None


class ProgressReader:
    """Attempt-owned reader pins a directory FD before the child can mutate it."""

    def __init__(self, path, callback):
        self.path = Path(path)
        self.callback = callback
        self.descriptor = os.open(self.path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        self.identity = os.fstat(self.descriptor)
        self.accepted = {}

    def poll(self):
        try:
            current = self.path.stat(follow_symlinks=False)
            if (current.st_dev, current.st_ino) != (self.identity.st_dev, self.identity.st_ino):
                raise ProgressError()
            reports = _names(self.descriptor)
            if len(reports) < len(self.accepted):
                raise ProgressError()
            for sequence, name in sorted(reports.items()):
                data = _read(self.descriptor, name)
                value = _decode(data, sequence)
                if sequence in self.accepted:
                    if self.accepted[sequence] != data:
                        raise ProgressError()
                else:
                    try:
                        self.callback(sequence, value["kind"], value["summary"])
                    except Exception:
                        raise ProgressSinkError() from None
                    self.accepted[sequence] = data
        except OSError:
            raise ProgressError() from None

    def close(self):
        if self.descriptor is not None:
            os.close(self.descriptor)
            self.descriptor = None


def provision(scratch):
    scratch = Path(scratch)
    path = scratch / "progress"
    path.mkdir(mode=0o700)
    helper = scratch / "report_progress.py"
    # The helper is self-contained and has no application imports or DB access.
    data = Path(__file__).read_bytes()
    descriptor = os.open(helper, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "wb") as stream:
        stream.write(data)
    return path


def write_report(path, kind, summary):
    """Child-only atomic report creation. The parent independently validates it."""
    directory = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    lock = None
    temporary = None
    try:
        lock = os.open(".lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600, dir_fd=directory)
        lock_stat = os.fstat(lock)
        if not stat.S_ISREG(lock_stat.st_mode) or lock_stat.st_nlink != 1:
            raise ProgressError()
        fcntl.flock(lock, fcntl.LOCK_EX)
        sequence = len(_names(directory)) + 1
        value = validate_report({"version": 1, "sequence": sequence, "kind": kind, "summary": summary})
        encoded = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8") + b"\n"
        if len(encoded) > MAX_RECORD_BYTES:
            raise ProgressError()
        temporary = ".report-" + uuid4().hex + ".tmp"
        descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=directory)
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(encoded)
            stream.flush()
            os.fsync(stream.fileno())
        # Cooperative writers hold the same lock; the next name cannot exist.
        # Atomic rename ensures the parent never sees a partly written JSON file.
        os.rename(temporary, f"{sequence:06d}.json", src_dir_fd=directory, dst_dir_fd=directory)
        temporary = None
        os.fsync(directory)
    except (OSError, ValueError):
        raise ProgressError() from None
    finally:
        if temporary is not None:
            try:
                os.unlink(temporary, dir_fd=directory)
            except OSError:
                pass
        if lock is not None:
            os.close(lock)
        os.close(directory)


def main():
    if len(sys.argv) != 3 or sys.argv[1] not in {"plan", "progress"}:
        raise ProgressError()
    write_report(Path(__file__).resolve().parent / "progress", sys.argv[1], sys.argv[2])


if __name__ == "__main__":
    try:
        main()
    except ProgressError:
        print("Public report rejected: expected a bounded plan, then progress summaries.", file=sys.stderr)
        raise SystemExit(2)
