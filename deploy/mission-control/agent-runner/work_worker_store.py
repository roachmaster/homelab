"""Scoped PostgreSQL worker and trusted reconciler adapter (WorkerDB)."""

from contextlib import closing, contextmanager
from dataclasses import dataclass
import re
import select
import unicodedata
from types import MappingProxyType
from uuid import UUID

import psycopg2
from psycopg2.extras import Json, register_uuid

from usage_store import (
    USAGE_REFRESH_NOTIFY_CHANNEL,
    validate_attempt_usage,
    validate_provider_snapshot,
    validate_safe_error_code,
    validate_utc_rfc3339,
)
from work_config import (
    DIGEST_HEX_RE,
    VALID_BACKENDS,
    VALID_MODES,
    validate_identifier,
    validate_model,
    validate_owner,
)
from work_store import NOTIFY_CHANNEL

register_uuid()

MAX_SUMMARY_BYTES = 4096
MAX_CHUNK_BYTES = 16384
MAX_CHUNKS = 1024
MAX_RESULT_BYTES = 16 * 1024 * 1024
SHA1_HEX_RE = re.compile(r"^[0-9a-f]{40}$")
UNIT_NAME_RE = re.compile(r"^forge-coding-[0-9a-f]{32}$")
VALID_OUTCOMES = frozenset({
    "awaiting_review",
    "failed",
    "quota_blocked",
    "blocked_auth",
    "blocked_permission",
    "cancelled",
    "needs_reconcile",
})
STOP_PROOF_KEYS = frozenset({"unit_name", "scope_stopped", "source", "checked_at"})
MANIFEST_KEYS = frozenset({"sha256", "byte_count", "chunk_count"})
VALID_LISTEN_CHANNELS = frozenset({NOTIFY_CHANNEL, USAGE_REFRESH_NOTIFY_CHANNEL})


class WorkerStoreError(RuntimeError):
    """Typed worker/reconciler store failure without exposing internal SQL details."""

    def __init__(self, status, code, message=None):
        self.status = status
        self.code = code
        super().__init__(message or code)


class LeaseExpiredError(WorkerStoreError):
    def __init__(self, message="Attempt lease is not active"):
        super().__init__(409, "lease_expired", message)


class OutcomeConflictError(WorkerStoreError):
    def __init__(self, message="Outcome or chunk conflict"):
        super().__init__(409, "outcome_conflict", message)


class ResultMismatchError(WorkerStoreError):
    def __init__(self, message="Result manifest does not match staged chunks"):
        super().__init__(409, "result_mismatch", message)


class WorkerAccessError(WorkerStoreError):
    def __init__(self, message="Worker role lacks required grant or attempt ownership"):
        super().__init__(403, "denied", message)


@dataclass(frozen=True)
class Lease:
    """Immutable claimed execution attempt lease returned by WorkerDB.claim()."""

    task_id: str
    project_alias: str
    base_revision: str
    mode: str
    agent_profile: str
    prompt: str
    acceptance_criteria: str
    recipe: MappingProxyType
    attempt_id: str
    attempt_token: str
    worker_id: str
    incarnation: str
    lease_expires_at: str
    deadline_at: str
    unit_name: str
    resume_workspace_digest: str | None = None
    resume_attempt_id: str | None = None
    plan_decision_id: str | None = None

    def to_dict(self):
        return {
            "task_id": self.task_id,
            "project_alias": self.project_alias,
            "base_revision": self.base_revision,
            "mode": self.mode,
            "agent_profile": self.agent_profile,
            "prompt": self.prompt,
            "acceptance_criteria": self.acceptance_criteria,
            "recipe": dict(self.recipe),
            "attempt_id": self.attempt_id,
            "attempt_token": self.attempt_token,
            "worker_id": self.worker_id,
            "incarnation": self.incarnation,
            "lease_expires_at": self.lease_expires_at,
            "deadline_at": self.deadline_at,
            "unit_name": self.unit_name,
            "resume_workspace_digest": self.resume_workspace_digest,
            "resume_attempt_id": self.resume_attempt_id,
            "plan_decision_id": self.plan_decision_id,
        }


def _coerce_uuid(value, label="UUID"):
    if isinstance(value, UUID):
        return value
    if isinstance(value, str):
        try:
            parsed = UUID(value)
        except ValueError:
            raise ValueError(f"Invalid {label}") from None
        if str(parsed) != value:
            raise ValueError(f"Non-canonical {label}")
        return parsed
    raise ValueError(f"Invalid {label}")


def _extract_attempt_creds(lease):
    if isinstance(lease, Lease):
        return _coerce_uuid(lease.attempt_id, "attempt_id"), _coerce_uuid(lease.attempt_token, "attempt_token")
    if isinstance(lease, dict):
        return (
            _coerce_uuid(lease.get("attempt_id"), "attempt_id"),
            _coerce_uuid(lease.get("attempt_token"), "attempt_token"),
        )
    if hasattr(lease, "attempt_id") and hasattr(lease, "attempt_token"):
        return (
            _coerce_uuid(lease.attempt_id, "attempt_id"),
            _coerce_uuid(lease.attempt_token, "attempt_token"),
        )
    raise TypeError("lease must be a Lease or dict with attempt_id and attempt_token")


def _extract_reconcile_ids(lease):
    if isinstance(lease, Lease):
        return _coerce_uuid(lease.task_id, "task_id"), _coerce_uuid(lease.attempt_id, "attempt_id")
    if isinstance(lease, dict):
        return (
            _coerce_uuid(lease.get("task_id"), "task_id"),
            _coerce_uuid(lease.get("attempt_id"), "attempt_id"),
        )
    if hasattr(lease, "task_id") and hasattr(lease, "attempt_id"):
        return (
            _coerce_uuid(lease.task_id, "task_id"),
            _coerce_uuid(lease.attempt_id, "attempt_id"),
        )
    raise TypeError("lease must provide task_id and attempt_id")


def _validate_summary(summary):
    if not isinstance(summary, str) or "\0" in summary:
        raise ValueError("Invalid attempt summary")
    try:
        encoded = summary.encode("utf-8")
    except UnicodeEncodeError:
        raise ValueError("Invalid UTF-8 attempt summary") from None
    if len(encoded) > MAX_SUMMARY_BYTES:
        raise ValueError("Attempt summary exceeds 4096 UTF-8 bytes")
    return summary


def _validate_stop_proof(stop_proof, *, require_stopped=False):
    if not isinstance(stop_proof, dict) or set(stop_proof.keys()) != STOP_PROOF_KEYS:
        raise ValueError("stop_proof must contain exact keys unit_name, scope_stopped, source, checked_at")
    unit_name = stop_proof["unit_name"]
    if not isinstance(unit_name, str) or not UNIT_NAME_RE.fullmatch(unit_name):
        raise ValueError("Invalid stop_proof.unit_name")
    scope_stopped = stop_proof["scope_stopped"]
    if type(scope_stopped) is not bool:
        raise ValueError("Invalid stop_proof.scope_stopped")
    if require_stopped and not scope_stopped:
        raise ValueError("Confirmed scope_stopped=True is required")
    if stop_proof["source"] != "systemd":
        raise ValueError("stop_proof.source must be 'systemd'")
    checked_at = validate_utc_rfc3339(stop_proof["checked_at"], "stop_proof.checked_at")
    return {
        "unit_name": unit_name,
        "scope_stopped": scope_stopped,
        "source": "systemd",
        "checked_at": checked_at,
    }


def _validate_manifest(manifest):
    if manifest is None:
        return None
    if not isinstance(manifest, dict) or set(manifest.keys()) != MANIFEST_KEYS:
        raise ValueError("manifest must contain exact keys sha256, byte_count, chunk_count")
    sha256 = manifest["sha256"]
    byte_count = manifest["byte_count"]
    chunk_count = manifest["chunk_count"]
    if not isinstance(sha256, str) or not DIGEST_HEX_RE.fullmatch(sha256):
        raise ValueError("Invalid manifest.sha256")
    if type(byte_count) is not int or not 1 <= byte_count <= MAX_RESULT_BYTES:
        raise ValueError("Invalid manifest.byte_count")
    if type(chunk_count) is not int or not 1 <= chunk_count <= MAX_CHUNKS:
        raise ValueError("Invalid manifest.chunk_count")
    return {
        "sha256": sha256,
        "byte_count": byte_count,
        "chunk_count": chunk_count,
    }


def _translate_pg_error(exc):
    code = getattr(exc, "pgcode", None)
    if code == "P0008":
        raise LeaseExpiredError() from None
    if code == "P0009":
        raise OutcomeConflictError() from None
    if code == "P0007":
        raise ResultMismatchError() from None
    if code == "42501":
        raise WorkerAccessError() from None
    if code == "22023":
        raise ValueError("Database rejected invalid worker request") from None
    if code == "P0002":
        raise WorkerStoreError(404, "not_found") from None
    raise exc


class WorkerDB:
    """Injected-connection PostgreSQL port for coding workers and trusted reconcilers."""

    def __init__(self, connect_factory, owner, *, reconcile_connect_factory=None):
        if not callable(connect_factory):
            raise TypeError("connect_factory must be callable")
        if reconcile_connect_factory is not None and not callable(reconcile_connect_factory):
            raise TypeError("reconcile_connect_factory must be callable or None")
        self._connect_factory = connect_factory
        self._owner = validate_owner(owner)
        self._reconcile_connect_factory = reconcile_connect_factory

    @property
    def owner(self):
        return self._owner

    @contextmanager
    def _worker_cursor(self):
        with closing(self._connect_factory()) as connection:
            try:
                with connection, connection.cursor() as cursor:
                    # Bind this trusted native port to its configured owner for
                    # every transaction; SQL still checks session_user grants.
                    cursor.execute("SELECT pg_catalog.set_config('forge.coding_owner', %s, true)",
                                   (self._owner,))
                    yield cursor
            except psycopg2.Error as exc:
                _translate_pg_error(exc)

    @contextmanager
    def _reconcile_cursor(self):
        if self._reconcile_connect_factory is None:
            raise WorkerAccessError("Trusted reconcile_connect_factory is not configured")
        with closing(self._reconcile_connect_factory()) as connection:
            try:
                with connection, connection.cursor() as cursor:
                    # Bind this trusted native port to its configured owner for
                    # every transaction; SQL still checks session_user grants.
                    cursor.execute("SELECT pg_catalog.set_config('forge.coding_owner', %s, true)",
                                   (self._owner,))
                    yield cursor
            except psycopg2.Error as exc:
                _translate_pg_error(exc)

    @contextmanager
    def listen(self, channel=NOTIFY_CHANNEL):
        """Open a dedicated LISTEN connection committed on the worker role."""
        if channel not in VALID_LISTEN_CHANNELS:
            raise ValueError("Unknown worker notification channel")
        connection = self._connect_factory()
        try:
            connection.autocommit = True
            with connection.cursor() as cursor:
                cursor.execute(f"LISTEN {channel}")
            yield connection
        finally:
            connection.close()

    @staticmethod
    def wait(listener, timeout):
        """Wait up to timeout seconds for a PostgreSQL notification on listener."""
        if timeout < 0:
            timeout = 0
        if select.select([listener], [], [], timeout)[0]:
            listener.poll()
            notified = bool(listener.notifies)
            listener.notifies.clear()
            return notified
        return False

    def advertise(self, worker_id, incarnation, projects, profiles, policy_digest):
        validate_identifier(worker_id, "worker_id")
        incarnation_uuid = _coerce_uuid(incarnation, "incarnation")
        if not isinstance(policy_digest, str) or not DIGEST_HEX_RE.fullmatch(policy_digest):
            raise ValueError("Invalid policy_digest")
        if not isinstance(projects, (list, tuple)) or len(projects) > 64:
            raise ValueError("Invalid advertised projects")
        sanitized_projects = []
        for item in projects:
            if not isinstance(item, dict) or set(item.keys()) != {"alias", "base_revision"}:
                raise ValueError("Invalid advertised project entry")
            alias = validate_identifier(item["alias"], "project alias")
            rev = item["base_revision"]
            if rev is not None and (not isinstance(rev, str) or not SHA1_HEX_RE.fullmatch(rev)):
                raise ValueError("Invalid advertised base_revision")
            sanitized_projects.append({"alias": alias, "base_revision": rev})

        if not isinstance(profiles, (list, tuple)) or len(profiles) > 64:
            raise ValueError("Invalid advertised profiles")
        sanitized_profiles = []
        for item in profiles:
            if not isinstance(item, dict) or set(item.keys()) != {"alias", "backend", "model", "modes"}:
                raise ValueError("Invalid advertised profile entry")
            alias = validate_identifier(item["alias"], "profile alias")
            backend = item["backend"]
            if not isinstance(backend, str) or backend not in VALID_BACKENDS:
                raise ValueError("Invalid advertised profile backend")
            model = validate_model(item["model"])
            modes = item["modes"]
            if (not isinstance(modes, (list, tuple)) or len(modes) > 3
                    or any(not isinstance(m, str) or m not in VALID_MODES for m in modes)):
                raise ValueError("Invalid advertised profile modes")
            sanitized_profiles.append({
                "alias": alias,
                "backend": backend,
                "model": model,
                "modes": list(dict.fromkeys(modes)),
            })

        with self._worker_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_worker_api.advertise(%s, %s, %s, %s, %s)",
                (
                    worker_id,
                    incarnation_uuid,
                    Json(sanitized_projects),
                    Json(sanitized_profiles),
                    policy_digest,
                ),
            )

    def claim(self, worker_id, incarnation, allowed_profiles):
        validate_identifier(worker_id, "worker_id")
        incarnation_uuid = _coerce_uuid(incarnation, "incarnation")
        if (not isinstance(allowed_profiles, (list, tuple, set, frozenset))
                or not allowed_profiles or len(allowed_profiles) > 64):
            raise ValueError("allowed_profiles must contain between 1 and 64 profile aliases")
        profiles_list = [validate_identifier(alias, "profile alias") for alias in allowed_profiles]
        with self._worker_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_worker_api.claim(%s, %s, %s)",
                (worker_id, incarnation_uuid, profiles_list),
            )
            payload = cursor.fetchone()[0]
        if payload is None:
            return None
        return Lease(
            task_id=str(payload["task_id"]),
            project_alias=payload["project_alias"],
            base_revision=payload["base_revision"],
            mode=payload["mode"],
            agent_profile=payload["agent_profile"],
            prompt=payload["prompt"],
            acceptance_criteria=payload["acceptance_criteria"],
            recipe=MappingProxyType(dict(payload["recipe"])),
            attempt_id=str(payload["attempt_id"]),
            attempt_token=str(payload["attempt_token"]),
            worker_id=payload["worker_id"],
            incarnation=str(payload["incarnation"]),
            lease_expires_at=payload["lease_expires_at"],
            deadline_at=payload["deadline_at"],
            unit_name=payload["unit_name"],
            resume_workspace_digest=payload.get("resume_workspace_digest"),
            resume_attempt_id=str(payload["resume_attempt_id"]) if payload.get("resume_attempt_id") else None,
            plan_decision_id=str(payload["plan_decision_id"]) if payload.get("plan_decision_id") else None,
        )

    def renew(self, lease):
        attempt_id, attempt_token = _extract_attempt_creds(lease)
        with self._worker_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_worker_api.renew(%s, %s)",
                (attempt_id, attempt_token),
            )
            return bool(cursor.fetchone()[0])

    def checkpoint(self, lease, summary):
        attempt_id, attempt_token = _extract_attempt_creds(lease)
        sanitized_summary = _validate_summary(summary)
        with self._worker_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_worker_api.checkpoint(%s, %s, %s)",
                (attempt_id, attempt_token, sanitized_summary),
            )

    def report_progress(self, lease, sequence, kind, summary):
        attempt_id, attempt_token = _extract_attempt_creds(lease)
        if type(sequence) is not int or not 1 <= sequence <= 128:
            raise ValueError("Progress sequence must be an integer in 1..128")
        if not isinstance(kind, str) or kind not in {"plan", "progress"}:
            raise ValueError("Invalid progress kind")
        summary = _validate_summary(summary)
        if not summary.strip() or any(
            unicodedata.category(char) == "Cc" and char not in "\n\t" for char in summary
        ):
            raise ValueError("Progress summary must be nonblank text without control characters")
        with self._worker_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_worker_api.report_progress(%s, %s, %s, %s, %s)",
                (attempt_id, attempt_token, sequence, kind, summary),
            )

    def append_result(self, lease, index, content):
        attempt_id, attempt_token = _extract_attempt_creds(lease)
        if type(index) is not int or not 0 <= index < MAX_CHUNKS:
            raise ValueError("Chunk index must be an integer in 0..1023")
        if not isinstance(content, str) or "\0" in content:
            raise ValueError("Chunk content must be a valid string without NUL bytes")
        try:
            encoded = content.encode("utf-8")
        except UnicodeEncodeError:
            raise ValueError("Chunk content must be valid UTF-8") from None
        if not 1 <= len(encoded) <= MAX_CHUNK_BYTES:
            raise ValueError("Chunk content must be 1..16384 UTF-8 bytes")
        with self._worker_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_worker_api.append_result(%s, %s, %s, %s)",
                (attempt_id, attempt_token, index, content),
            )

    def publish(self, lease, outcome, manifest, usage, summary, stop_proof):
        attempt_id, attempt_token = _extract_attempt_creds(lease)
        if not isinstance(outcome, str) or outcome not in VALID_OUTCOMES:
            raise ValueError("Invalid attempt outcome")
        sanitized_manifest = _validate_manifest(manifest)
        if outcome == "awaiting_review" and sanitized_manifest is None:
            raise ValueError("awaiting_review outcome requires a nonempty result manifest")
        sanitized_usage = None if usage is None else validate_attempt_usage(usage)
        sanitized_summary = _validate_summary(summary)
        sanitized_proof = _validate_stop_proof(
            stop_proof, require_stopped=(outcome != "needs_reconcile")
        )
        with self._worker_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_worker_api.publish(%s, %s, %s, %s, %s, %s, %s)",
                (
                    attempt_id,
                    attempt_token,
                    outcome,
                    None if sanitized_manifest is None else Json(sanitized_manifest),
                    None if sanitized_usage is None else Json(sanitized_usage),
                    sanitized_summary,
                    Json(sanitized_proof),
                ),
            )
            return dict(cursor.fetchone()[0])

    def record_stop(self, lease, stop_proof, summary):
        attempt_id, attempt_token = _extract_attempt_creds(lease)
        sanitized_proof = _validate_stop_proof(stop_proof, require_stopped=False)
        sanitized_summary = _validate_summary(summary)
        with self._worker_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_worker_api.record_stop(%s, %s, %s, %s)",
                (attempt_id, attempt_token, Json(sanitized_proof), sanitized_summary),
            )

    def cancellation_requested(self, lease):
        attempt_id, attempt_token = _extract_attempt_creds(lease)
        with self._worker_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_worker_api.cancellation_requested(%s, %s)",
                (attempt_id, attempt_token),
            )
            return bool(cursor.fetchone()[0])

    def expire_leases(self):
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.expire_leases()")
            return int(cursor.fetchone()[0])

    def pending_reconciliations(self, limit=64):
        if type(limit) is not int or not 1 <= limit <= 64:
            raise ValueError("limit must be an integer in 1..64")
        with self._reconcile_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_reconciler_api.pending_reconciliations(%s)",
                (limit,),
            )
            return list(cursor.fetchone()[0])

    def reconcile(self, lease, workspace_digest, stop_proof):
        task_id, attempt_id = _extract_reconcile_ids(lease)
        if not isinstance(workspace_digest, str) or not DIGEST_HEX_RE.fullmatch(workspace_digest):
            raise ValueError("Invalid workspace_digest")
        sanitized_proof = _validate_stop_proof(stop_proof, require_stopped=True)
        with self._reconcile_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_reconciler_api.reconcile(%s, %s, %s, %s)",
                (task_id, attempt_id, workspace_digest, Json(sanitized_proof)),
            )
            return str(cursor.fetchone()[0])

    def begin_usage_poll(self, profile_alias):
        validate_identifier(profile_alias, "profile alias")
        with self._worker_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_worker_api.begin_usage_poll(%s)",
                (profile_alias,),
            )
            return dict(cursor.fetchone()[0])

    def next_refresh(self):
        with self._worker_cursor() as cursor:
            cursor.execute("SELECT forge_coding_worker_api.next_refresh()")
            row = cursor.fetchone()[0]
        return None if row is None else dict(row)

    def record_usage(self, profile_alias, generation, configuration_epoch, snapshot):
        validate_identifier(profile_alias, "profile alias")
        if type(generation) is not int or generation < 1:
            raise ValueError("generation must be a positive integer")
        if type(configuration_epoch) is not int or configuration_epoch < 1:
            raise ValueError("configuration_epoch must be a positive integer")
        sanitized_snapshot = validate_provider_snapshot(snapshot)
        with self._worker_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_worker_api.record_usage(%s, %s, %s, %s)",
                (profile_alias, generation, configuration_epoch, Json(sanitized_snapshot)),
            )
            return bool(cursor.fetchone()[0])

    def fail_usage_poll(self, profile_alias, generation, configuration_epoch, code):
        validate_identifier(profile_alias, "profile alias")
        if type(generation) is not int or generation < 1:
            raise ValueError("generation must be a positive integer")
        if type(configuration_epoch) is not int or configuration_epoch < 1:
            raise ValueError("configuration_epoch must be a positive integer")
        sanitized_code = validate_safe_error_code(code)
        with self._worker_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_worker_api.fail_usage_poll(%s, %s, %s, %s)",
                (profile_alias, generation, configuration_epoch, sanitized_code),
            )

    def read_plan(self, lease):
        attempt, token = _extract_attempt_creds(lease)
        with self._worker_cursor() as cursor:
            cursor.execute("SELECT forge_coding_worker_api.read_plan(%s,%s)", (attempt,token))
            value = cursor.fetchone()[0]
        return None if value is None else dict(value)

    def queue_publication(self, lease, reconciliation_receipt_id):
        attempt, token = _extract_attempt_creds(lease)
        receipt = _coerce_uuid(reconciliation_receipt_id, "reconciliation_receipt_id")
        with self._worker_cursor() as cursor:
            cursor.execute("SELECT forge_coding_worker_api.queue_publication(%s,%s,%s)", (attempt,token,receipt))
            value = cursor.fetchone()[0]
        return str(value) if value is not None else None

    def publication_claim(self, worker_id):
        validate_identifier(worker_id, "worker_id")
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.publication_claim(%s)", (worker_id,))
            value = cursor.fetchone()[0]
        return None if value is None else dict(value)

    def publication_renew(self, job):
        publication = _coerce_uuid(job["publication_id"], "publication_id")
        token = _coerce_uuid(job["lease_token"], "lease_token")
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.publication_renew(%s,%s)", (publication,token))
            return bool(cursor.fetchone()[0])

    def publication_finish(self, job, status, summary, branch=None, commit_sha=None, pr_url=None):
        publication = _coerce_uuid(job["publication_id"], "publication_id")
        token = _coerce_uuid(job["lease_token"], "lease_token")
        if status not in {"publishing", "published", "failed"}:
            raise ValueError("Invalid publication outcome")
        _validate_summary(summary)
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.publication_finish(%s,%s,%s,%s,%s,%s,%s)",
                           (publication,token,status,branch,commit_sha,pr_url,summary))
            return dict(cursor.fetchone()[0])
