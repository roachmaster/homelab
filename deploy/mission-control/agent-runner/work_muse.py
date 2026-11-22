"""Pinned Muse MSP v1 adapter. Pure construction/parsing; never logs native data.

The installed 1.4.24-R9131.1 help and offline echo/schema probe establish this
contract. Each attempt owns a fresh session and isolated native configuration.
No account discovery, login, model request or allowance polling occurs here.
"""

from uuid import UUID
import os
import stat
from work_native_auth import AuthCleanupError, AuthLease, _open_directory

VERSION = "1.4.24-R9131.1"
BINARY_SHA256 = "d752c45dce079ee29e118804b091d81ac2b1b89b67f317b4a162fa1af1206465"
MAX_COUNTER = 10**40 - 1


def identity(value):
    if not isinstance(value, str) or str(UUID(value)) != value:
        raise ValueError("Invalid Muse stream identity")
    return value


def argv(binary, model, workspace, prompt_file, *, implement):
    args = (str(binary), "exec", "--json", "--provider", "meta", "--model", model,
            "--workspace", str(workspace), "--prompt-file", str(prompt_file),
            "--approval-mode", "never", "--approval-judge", "off",
            "--disable-reminders", "--disable-request-user-input",
            "--no-foreign-personal-context", "--trust-workspace",
            "--contributor-mode", "off", "-c", "agents.execution_capacity=1")
    # The outer supervisor enforces filesystem permissions for shell and native
    # tools alike. Muse's own shell sandbox remains enabled as an extra boundary.
    return args if implement else args + ("--disable-write",)


def stage_auth(account, destination):
    """Acquire only selected auth, with retained inode ownership until scope stop."""
    directory = None
    target = None
    staged = None
    try:
        directory = os.open(account.home, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        for name in (".config", "muse"):
            child = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            os.close(directory)
            directory = child
            info = os.fstat(directory)
            if info.st_uid != account.uid or info.st_mode & 0o022:
                raise ValueError("Unsafe Muse native state directory")
        descriptor = os.open("auth.json", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
        with os.fdopen(descriptor, "rb") as source:
            info = os.fstat(source.fileno())
            if (not stat.S_ISREG(info.st_mode) or info.st_uid != account.uid
                    or info.st_mode & 0o077 or not 0 < info.st_size <= 1024 * 1024):
                raise ValueError("Unsafe Muse native account state")
            data = source.read(1024 * 1024 + 1)
            if len(data) != info.st_size:
                raise ValueError("Muse native account state changed during acquisition")
        target = _open_directory(destination.parent)
        descriptor = os.open(destination.name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o400, dir_fd=target)
        staged = AuthLease(target, descriptor, destination.name, "home/.config/muse")
        target = None
        with os.fdopen(os.dup(descriptor), "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        result = staged
        staged = None
        return result
    except FileNotFoundError:
        return False
    finally:
        try:
            if staged is not None:
                try:
                    if not staged.cleanup():
                        raise AuthCleanupError(staged.identity)
                finally:
                    staged.close()
        finally:
            if target is not None:
                os.close(target)
            if directory is not None:
                os.close(directory)


class MuseEvents:
    """One bounded run, one session, one terminal result, one usage envelope."""

    def __init__(self):
        self.session = None
        self.run = None
        self.sequences = {}
        self.response = ""
        self.usage = None
        self.complete = False
        self.failure = None
        self.saw_work = False
        self.started = False
        self.linked = False
        self.cancelled = False

    def record(self, event):
        if (type(event.get("schema_version")) is not int or event["schema_version"] != 1
                or type(event.get("payload_schema_version")) is not int or event["payload_schema_version"] != 1):
            raise ValueError("Unsupported Muse event schema")
        stream, payload = event.get("stream"), event.get("payload")
        if not isinstance(stream, dict) or not isinstance(payload, dict):
            raise ValueError("Invalid Muse event envelope")
        session = identity(stream.get("id"))
        if stream.get("kind") not in {"session", "exec.usage"}:
            raise ValueError("Unexpected Muse event stream")
        if self.session is not None and session != self.session:
            raise ValueError("Muse changed session identity")
        self.session = session
        sequence = event.get("sequence")
        key = stream["kind"]
        if type(sequence) is not int or sequence <= self.sequences.get(key, 0):
            raise ValueError("Invalid Muse stream sequence")
        self.sequences[key] = sequence
        kind = event.get("payload_type")
        if self.complete:
            raise ValueError("Muse emitted data after its terminal result")
        run_stream = payload.get("run_stream")
        if run_stream is not None:
            if not isinstance(run_stream, dict) or run_stream.get("kind") != "run":
                raise ValueError("Invalid Muse run stream")
            run = identity(run_stream.get("id"))
            if self.run is not None and run != self.run:
                raise ValueError("Muse changed run identity")
            if payload.get("command_id") != run or event.get("causation_id") != run:
                raise ValueError("Muse command/run binding mismatch")
            self.run = run
        if kind in {"run.output.delta", "run.usage"} or isinstance(kind, str) and kind.startswith("run.terminal."):
            if self.run is None or run_stream is None:
                raise ValueError("Missing Muse run binding")
        if kind == "session.run.linked":
            if stream["kind"] != "session" or payload.get("kind") != "session_run_linked" or run_stream is None:
                raise ValueError("Invalid Muse session/run link")
            self.linked = True
        elif kind == "run.lifecycle.started":
            if (not self.linked or self.started or stream["kind"] != "session"
                    or payload.get("kind") != "run_started"):
                raise ValueError("Muse run started without a valid session link")
            self.started = True
            self.saw_work = True
        elif kind == "run.output.delta":
            if (stream["kind"] != "session" or not self.started
                    or payload.get("kind") != "run_output_delta" or not isinstance(payload.get("text"), str)):
                raise ValueError("Invalid Muse output delta")
            self.saw_work = True
        elif kind == "run.usage":
            if self.usage is not None or payload.get("kind") != "run_usage" or stream["kind"] != "exec.usage":
                raise ValueError("Invalid Muse usage envelope")
            self.usage = self._usage(payload)
        elif isinstance(kind, str) and kind.startswith("run.terminal."):
            terminal = kind.removeprefix("run.terminal.")
            if (stream["kind"] != "session" or payload.get("kind") != "run_terminal"
                    or payload.get("terminal") != terminal or terminal not in {"completed", "failed", "cancelled"}):
                raise ValueError("Invalid Muse terminal result")
            if terminal == "completed":
                if not self.linked or not self.started or not isinstance(payload.get("text"), str):
                    raise ValueError("Invalid Muse final response")
                self.response = payload["text"]
            else:
                self.cancelled = terminal == "cancelled"
                reason = payload.get("reason")
                self.failure = reason if isinstance(reason, str) else terminal
            self.complete = True

    @staticmethod
    def _usage(payload):
        models = payload.get("models")
        if (payload.get("children_included") is not False or not isinstance(models, list)
                or not 1 <= len(models) <= 64):
            raise ValueError("Unattributable Muse run usage")
        if any(not isinstance(model, dict) or "model" not in model
               or model["model"] is not None and not isinstance(model["model"], str)
               or not {"input_tokens", "output_tokens", "cached_tokens"} <= model.keys() for model in models):
            raise ValueError("Malformed Muse model usage")
        counts = {}
        for field in ("input_tokens", "output_tokens", "cached_tokens"):
            entries = [model.get(field) if isinstance(model, dict) else None for model in models]
            if any(value is not None and (type(value) is not int or not 0 <= value <= MAX_COUNTER)
                   for value in entries):
                raise ValueError("Invalid Muse usage counter")
            total = sum(entries) if all(value is not None for value in entries) else None
            if total is not None and total > MAX_COUNTER:
                raise ValueError("Muse usage counter overflow")
            counts[field] = total
        if (counts["cached_tokens"] is not None and counts["input_tokens"] is not None
                and counts["cached_tokens"] > counts["input_tokens"]):
            raise ValueError("Muse Meta cached input exceeds input tokens")
        return counts
