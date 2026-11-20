"""Parent-owned Antigravity conversation hints, never implicit workspace history.

The caller holds the task workspace lease. A hint grants no execution authority:
the database reconciliation receipt and the workspace provider still admit every
attempt. No prompt, credentials, native CLI cache or usage totals are stored here.
"""

from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import tempfile
from uuid import UUID


MAX_RECORD_BYTES = 2048
DIGEST = re.compile(r"[0-9a-f]{64}")


class SessionError(ValueError):
    def __init__(self):
        super().__init__("Antigravity continuation metadata could not be verified.")


def _uuid(value):
    try:
        if not isinstance(value, str) or str(UUID(value)) != value:
            raise SessionError()
    except (ValueError, TypeError, AttributeError):
        raise SessionError() from None
    return value


def _object(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise SessionError()
        value[key] = item
    return value


@dataclass(frozen=True)
class SessionSelection:
    task_id: str
    identity: str
    attempt_id: str
    conversation_id: str | None = None


class AntigravitySessionProvider:
    """Worker-scoped provider of explicit, same-task conversation selections."""

    def __init__(self, config):
        self.config = config
        self.directory = config.state_root / "sessions"

    def prepare(self):
        self.directory.mkdir(parents=True, mode=0o700, exist_ok=True)
        metadata = self.directory.lstat()
        if (not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.geteuid()
                or metadata.st_mode & 0o077):
            raise SessionError()

    def _path(self, task_id):
        return self.directory / (_uuid(task_id) + ".json")

    def _read(self, task_id):
        try:
            descriptor = os.open(self._path(task_id), os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        except FileNotFoundError:
            return None
        except OSError:
            raise SessionError() from None
        try:
            with os.fdopen(descriptor, "rb") as stream:
                metadata = os.fstat(stream.fileno())
                if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.geteuid()
                        or metadata.st_nlink != 1 or metadata.st_mode & 0o077):
                    raise SessionError()
                raw = stream.read(MAX_RECORD_BYTES + 1)
            if len(raw) > MAX_RECORD_BYTES:
                raise SessionError()
            record = json.loads(raw.decode("utf-8"), object_pairs_hook=_object,
                                parse_constant=lambda _value: (_ for _ in ()).throw(SessionError()))
            if (not isinstance(record, dict)
                    or set(record) != {"version", "identity", "attempt_id", "conversation_id", "workspace_digest"}
                    or type(record["version"]) is not int or record["version"] != 1
                    or not isinstance(record["identity"], str) or not DIGEST.fullmatch(record["identity"])
                    or not isinstance(record["workspace_digest"], str) or not DIGEST.fullmatch(record["workspace_digest"])):
                raise SessionError()
            _uuid(record["attempt_id"])
            _uuid(record["conversation_id"])
            return record
        except (OSError, UnicodeError, ValueError, RecursionError):
            raise SessionError() from None

    def select(self, lease, workspace, binary_sha256):
        task_id, attempt_id = _uuid(lease["task_id"]), _uuid(lease["attempt_id"])
        if (workspace.task_id != task_id or workspace.base_revision != lease["base_revision"]
                or not DIGEST.fullmatch(binary_sha256)):
            raise SessionError()
        binding = {
            "owner": self.config.owner, "task_id": task_id,
            "project_alias": lease["project_alias"], "base_revision": lease["base_revision"],
            "workspace": str(workspace.path), "repository": str(workspace.repository),
            "agent_profile": lease["agent_profile"], "recipe": lease["recipe"],
            "plan_decision_id": lease.get("plan_decision_id"),
            "scope_plan_decision_id": lease.get("scope_plan_decision_id"),
            "account": [self.config.account.user, self.config.account.uid,
                        self.config.account.gid, str(self.config.account.home)],
            "binary_sha256": binary_sha256,
        }
        identity = hashlib.sha256(json.dumps(binding, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        selection = SessionSelection(task_id, identity, attempt_id)
        # A one-off never consults agy's global latest-conversation cache, even
        # when an unrelated task uses the same child mount path or project.
        if lease.get("resume_workspace_digest") is None or lease.get("resume_attempt_id") is None:
            return selection
        previous_attempt = _uuid(lease["resume_attempt_id"])
        record = self._read(task_id)
        if (record is not None and record["identity"] == identity
                and record["attempt_id"] == previous_attempt
                and record["attempt_id"] != attempt_id
                and record["workspace_digest"] == lease["resume_workspace_digest"]
                and workspace.digest() == record["workspace_digest"]):
            return SessionSelection(task_id, identity, attempt_id, record["conversation_id"])
        # Old installations, changed models/profiles/policy, or interrupted
        # attempts have no verified conversation hint. Start a fresh context.
        return selection

    def invalidate(self, selection):
        # Do this immediately before execution: a crash or uncertain stop must
        # not leave an older successful hint eligible for another continuation.
        self._path(selection.task_id).unlink(missing_ok=True)

    def remember(self, selection, workspace, conversation_id):
        conversation_id = _uuid(conversation_id)
        if selection.conversation_id is not None and selection.conversation_id != conversation_id:
            raise SessionError()
        record = {"version": 1, "identity": selection.identity,
                  "attempt_id": selection.attempt_id, "conversation_id": conversation_id,
                  "workspace_digest": workspace.digest()}
        data = json.dumps(record, sort_keys=True, separators=(",", ":")).encode("utf-8")
        if len(data) > MAX_RECORD_BYTES:
            raise SessionError()
        descriptor, name = tempfile.mkstemp(prefix=".pending-", dir=self.directory)
        try:
            with os.fdopen(descriptor, "wb") as stream:
                stream.write(data)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(name, self._path(selection.task_id))
            directory = os.open(self.directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            Path(name).unlink(missing_ok=True)
