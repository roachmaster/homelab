"""Scoped PostgreSQL worker and trusted reconciler adapter (WorkerDB)."""

from contextlib import closing, contextmanager
from dataclasses import dataclass
import re
import select
import unicodedata
from types import MappingProxyType
from uuid import UUID

import psycopg2
from work_failure_cause import failure_cause
from psycopg2.extras import Json, register_uuid

from usage_store import (
    USAGE_REFRESH_NOTIFY_CHANNEL,
    validate_attempt_usage,
    validate_provider_snapshot,
    validate_safe_error_code,
    validate_utc_rfc3339,
)
from work_events import CAUSES
from work_lease_contract import validate_lease
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
REVIEW_ADMISSION_LOCK_SALT = 731492647451
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
    feedback_id: str | None = None
    feedback_sha256: str | None = None
    repair_source_review_id: str | None = None
    repair_candidate_commit: str | None = None
    scope_plan_decision_id: str | None = None
    parent_task_id: str | None = None
    workflow_id: str | None = None
    workflow_review_id: str | None = None
    review_target_sha256: str | None = None
    workflow_feedback_review_id: str | None = None
    workflow_feedback_sha256: str | None = None

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
            "feedback_id":self.feedback_id,"feedback_sha256":self.feedback_sha256,
            "repair_source_review_id":self.repair_source_review_id,"repair_candidate_commit":self.repair_candidate_commit,"scope_plan_decision_id":self.scope_plan_decision_id,
            "parent_task_id":self.parent_task_id,"workflow_id":self.workflow_id,"workflow_review_id":self.workflow_review_id,"review_target_sha256":self.review_target_sha256,
            "workflow_feedback_review_id":self.workflow_feedback_review_id,"workflow_feedback_sha256":self.workflow_feedback_sha256,
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
            lease = Lease(
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
                feedback_id=str(payload["feedback_id"]) if payload.get("feedback_id") else None,
                feedback_sha256=payload.get("feedback_sha256"),
                repair_source_review_id=str(payload["repair_source_review_id"]) if payload.get("repair_source_review_id") else None,
                repair_candidate_commit=payload.get("repair_candidate_commit"),
                scope_plan_decision_id=str(payload["scope_plan_decision_id"]) if payload.get("scope_plan_decision_id") else None,
                parent_task_id=str(payload["parent_task_id"]) if payload.get("parent_task_id") else None,
                workflow_id=str(payload["workflow_id"]) if payload.get("workflow_id") else None,
                workflow_review_id=str(payload["workflow_review_id"]) if payload.get("workflow_review_id") else None,
                review_target_sha256=payload.get("review_target_sha256"),
                workflow_feedback_review_id=str(payload["workflow_feedback_review_id"]) if payload.get("workflow_feedback_review_id") else None,
                workflow_feedback_sha256=payload.get("workflow_feedback_sha256"),
            )
            validate_lease(lease.to_dict())
            return lease

    def renew(self, lease):
        attempt_id, attempt_token = _extract_attempt_creds(lease)
        with self._worker_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_worker_api.renew(%s, %s)",
                (attempt_id, attempt_token),
            )
            return bool(cursor.fetchone()[0])

    RENEW_STATUSES = frozenset({"renewed", "lease_expired", "deadline_exceeded", "task_cancelled", "task_gone"})
    PROGRESS_SIGNALS = frozenset({"progress_report", "checkpoint", "output"})

    def renew_status(self, lease, progress_signal=None):
        """Typed renewal (M45): why a renewal failed, and progress-aware deadlines.

        Before M45 the typed function does not exist; fall back to the boolean
        renew(), which cannot tell a cancelled task from an expired lease.
        """
        if progress_signal is not None and progress_signal not in self.PROGRESS_SIGNALS:
            raise ValueError(f"invalid progress signal: {progress_signal!r}")
        attempt_id, attempt_token = _extract_attempt_creds(lease)
        with self._worker_cursor() as cursor:
            cursor.execute("SELECT to_regprocedure('forge_coding_worker_api.renew_status(uuid,uuid,text)') IS NOT NULL")
            if not cursor.fetchone()[0]:
                cursor.execute("SELECT forge_coding_worker_api.renew(%s, %s)", (attempt_id, attempt_token))
                return "renewed" if cursor.fetchone()[0] else "lease_expired"
            cursor.execute("SELECT forge_coding_worker_api.renew_status(%s, %s, %s)",
                           (attempt_id, attempt_token, progress_signal))
            status = cursor.fetchone()[0]
        if status not in self.RENEW_STATUSES:
            raise ValueError(f"invalid renew status: {status!r}")
        return status

    def renew_deadline(self, lease, progress_signal=None):
        """Typed renewal plus the current deadline (M47).

        Returns ``{"status", "deadline_at", "extended"}`` where deadline_at is
        an aware datetime when renewed. Before M47 returns None so the caller
        falls back to renew_status().
        """
        if progress_signal is not None and progress_signal not in self.PROGRESS_SIGNALS:
            raise ValueError(f"invalid progress signal: {progress_signal!r}")
        attempt_id, attempt_token = _extract_attempt_creds(lease)
        with self._worker_cursor() as cursor:
            cursor.execute("SELECT to_regprocedure('forge_coding_worker_api.renew_deadline(uuid,uuid,text)') IS NOT NULL")
            if not cursor.fetchone()[0]:
                return None
            cursor.execute("SELECT forge_coding_worker_api.renew_deadline(%s, %s, %s)",
                           (attempt_id, attempt_token, progress_signal))
            result = cursor.fetchone()[0]
        if not isinstance(result, dict) or result.get("status") not in self.RENEW_STATUSES:
            raise ValueError(f"invalid renew result: {result!r}")
        deadline = result.get("deadline_at")
        if deadline is not None:
            from datetime import datetime
            deadline = datetime.fromisoformat(deadline.replace("Z", "+00:00"))
        return {"status": result["status"], "deadline_at": deadline, "extended": result.get("extended") is True}

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
        arguments = (
            attempt_id,
            attempt_token,
            outcome,
            None if sanitized_manifest is None else Json(sanitized_manifest),
            None if sanitized_usage is None else Json(sanitized_usage),
            sanitized_summary,
            Json(sanitized_proof),
        )
        with self._worker_cursor() as cursor:
            if outcome == "failed":
                # M46: the failed event carries a cause code and bounded summary.
                cursor.execute("SELECT to_regprocedure('forge_coding_worker_api.publish_with_cause(uuid,uuid,text,jsonb,jsonb,text,jsonb,text)') IS NOT NULL")
                if cursor.fetchone()[0]:
                    cursor.execute("SELECT forge_coding_worker_api.publish_with_cause(%s, %s, %s, %s, %s, %s, %s, %s)",
                                   arguments + (failure_cause(sanitized_summary),))
                    return dict(cursor.fetchone()[0])
            cursor.execute("SELECT forge_coding_worker_api.publish(%s, %s, %s, %s, %s, %s, %s)", arguments)
            return dict(cursor.fetchone()[0])

    def record_stop(self, lease, stop_proof, summary, cause):
        attempt_id, attempt_token = _extract_attempt_creds(lease)
        sanitized_proof = _validate_stop_proof(stop_proof, require_stopped=False)
        sanitized_summary = _validate_summary(summary)
        if cause not in CAUSES["needs_reconcile"]:
            raise ValueError(f"invalid stop cause: {cause!r}")
        with self._worker_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_worker_api.record_stop(%s, %s, %s, %s, %s)",
                (attempt_id, attempt_token, Json(sanitized_proof), sanitized_summary, cause),
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

    def read_feedback(self,lease):
        attempt,token=_extract_attempt_creds(lease)
        with self._worker_cursor() as cursor:
            cursor.execute("SELECT forge_coding_worker_api.read_feedback(%s,%s)",(attempt,token))
            value=cursor.fetchone()[0]
        return None if value is None else dict(value)

    def read_native_delivery_context(self, lease):
        """Read build context only through this attempt's authenticated feedback."""
        attempt, token = _extract_attempt_creds(lease)
        with self._worker_cursor() as cursor:
            cursor.execute("SELECT forge_coding_worker_api.read_native_delivery_context(%s,%s)", (attempt, token))
            value = cursor.fetchone()[0]
        if value is None:
            return None
        keys = {"feedback_id", "feedback_sha256", "review_id", "source_task_id", "source_attempt_id",
                "candidate_commit", "gate_manifest_sha256", "delivery_failure"}
        if (not isinstance(value, dict) or set(value) != keys
                or not isinstance(value["delivery_failure"], dict)):
            raise ValueError("Invalid native delivery context")
        for name in ("feedback_id", "review_id", "source_task_id", "source_attempt_id"):
            _coerce_uuid(value[name], name)
        for name in ("feedback_sha256", "gate_manifest_sha256"):
            if not isinstance(value[name], str) or not DIGEST_HEX_RE.fullmatch(value[name]):
                raise ValueError("Invalid native delivery context digest")
        if not isinstance(value["candidate_commit"], str) or not SHA1_HEX_RE.fullmatch(value["candidate_commit"]):
            raise ValueError("Invalid native delivery context source")
        return dict(value)

    def project_revisions(self):
        """Only durable merged heads for this owner and this worker's project grants."""
        with self._worker_cursor() as cursor:
            cursor.execute("SELECT forge_coding_worker_api.project_revisions()")
            value=cursor.fetchone()[0]
        if not isinstance(value,dict) or any(not isinstance(k,str) or not isinstance(v,str) or not SHA1_HEX_RE.fullmatch(v) for k,v in value.items()):
            raise ValueError("Invalid verified project revisions")
        return dict(value)

    def read_workflow_feedback(self,lease):
        attempt,token=_extract_attempt_creds(lease)
        with self._worker_cursor() as cursor:
            cursor.execute("SELECT forge_coding_worker_api.read_workflow_feedback(%s,%s)",(attempt,token))
            result=cursor.fetchone()[0]
        return None if result is None else dict(result)

    def read_external_review_context(self,lease):
        attempt,token=_extract_attempt_creds(lease)
        with self._worker_cursor() as cursor:
            cursor.execute("SELECT forge_external_workflow_worker_api.read_context(%s,%s)",(attempt,token))
            value=cursor.fetchone()[0]
        return value

    def read_review_target(self,lease):
        attempt,token=_extract_attempt_creds(lease)
        with self._worker_cursor() as cursor:
            cursor.execute("SELECT forge_coding_worker_api.read_review_target(%s,%s)",(attempt,token))
            result=cursor.fetchone()[0]
        return None if result is None else dict(result)

    def merge_claim(self,worker_id):
        validate_identifier(worker_id,"worker_id")
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.merge_claim(%s)",(worker_id,))
            result=cursor.fetchone()[0]
        return None if result is None else dict(result)

    def merge_renew(self,job):
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.merge_renew(%s,%s)",(_coerce_uuid(job['merge_id']),_coerce_uuid(job['lease_token'])))
            return bool(cursor.fetchone()[0])

    def merge_intent(self,job,intent):
        if not isinstance(intent,dict):raise ValueError("Invalid merge intent")
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.merge_intent(%s,%s,%s)",(_coerce_uuid(job['merge_id']),_coerce_uuid(job['lease_token']),Json(intent)))
            return dict(cursor.fetchone()[0])

    @staticmethod
    def _resolve_open_native_retries(cursor, project_alias, workflow_id):
        if not project_alias or not workflow_id:
            return
        cursor.execute("SELECT to_regprocedure('forge_coding_reconciler_api.retry_event_resolve_open_scope(text,text,uuid)') IS NOT NULL")
        if cursor.fetchone()[0]:
            cursor.execute(
                "SELECT forge_coding_reconciler_api.retry_event_resolve_open_scope(%s, 'native_workflow', %s)",
                (project_alias, _coerce_uuid(workflow_id, "workflow_id")),
            )

    def merge_finish(self,job,status,receipt):
        if status not in {'merged','handoff','needs_reconcile'} or not isinstance(receipt,dict):raise ValueError("Invalid merge outcome")
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.merge_finish(%s,%s,%s,%s)",(_coerce_uuid(job['merge_id']),_coerce_uuid(job['lease_token']),status,Json(receipt)))
            result=dict(cursor.fetchone()[0])
            self._resolve_open_native_retries(cursor, job.get('project_alias'), job.get('workflow_id'))
            return result

    def _delivery_call(self, name, *args):
        allowed = {'delivery_available','delivery_claim','delivery_renew','delivery_start_allowed',
                   'delivery_checkpoint','delivery_finish','delivery_observe','delivery_project_head',
                   'delivery_release_claim','delivery_release_renew','delivery_release_start_allowed','delivery_release_checkpoint','delivery_release_finish'}
        if name not in allowed:
            raise ValueError('Unknown delivery port')
        try:
            with self._reconcile_cursor() as cursor:
                cursor.execute('SELECT forge_coding_reconciler_api.' + name + '(' + ','.join(['%s']*len(args)) + ')', args)
                return cursor.fetchone()[0]
        except psycopg2.Error as error:
            if error.pgcode=='P0006':
                raise OutcomeConflictError() from None
            raise

    @staticmethod
    def _delivery_lease(job, key='post_merge_id'):
        return (_coerce_uuid(job[key]), job['generation'], _coerce_uuid(job['lease_token']))

    def delivery_available(self, owner=None):
        selected=self._owner if owner is None else validate_owner(owner)
        return selected==self._owner and self._delivery_call('delivery_available',selected) is True

    def delivery_claim(self, worker_id):
        validate_identifier(worker_id,'worker_id')
        return self._delivery_call('delivery_claim',worker_id)

    def delivery_renew(self, job):
        return self._delivery_call('delivery_renew',*self._delivery_lease(job)) is True

    def delivery_start_allowed(self, job):
        return self._delivery_call('delivery_start_allowed',*self._delivery_lease(job)) is True

    def delivery_checkpoint(self, job, request):
        from uuid import uuid5
        if not isinstance(request,dict) or request.get('prepare_action_id')!=str(uuid5(_coerce_uuid(job['post_merge_id']),'prepare-v1')):
            raise ValueError('Invalid delivery preparation identity')
        return self._delivery_call('delivery_checkpoint',*self._delivery_lease(job),Json(request)) is True

    def delivery_finish(self, job, status, receipt, summary):
        return self._delivery_call('delivery_finish',*self._delivery_lease(job),status,Json(receipt),summary) is True

    def delivery_project_head(self, project_alias):
        validate_identifier(project_alias,'project_alias')
        return self._delivery_call('delivery_project_head',project_alias)

    def delivery_observe(self, project_alias, repository, commit_sha, tree_sha, expected_previous_commit=None):
        validate_identifier(project_alias,'project_alias')
        return self._delivery_call('delivery_observe',project_alias,repository,commit_sha,tree_sha,expected_previous_commit)

    def delivery_release_claim(self, worker_id):
        validate_identifier(worker_id,'worker_id')
        return self._delivery_call('delivery_release_claim',worker_id)

    def delivery_release_renew(self, job):
        return self._delivery_call('delivery_release_renew',*self._delivery_lease(job,'release_id')) is True

    def delivery_release_start_allowed(self, job):
        return self._delivery_call('delivery_release_start_allowed',*self._delivery_lease(job,'release_id')) is True

    def delivery_release_checkpoint(self, job, plan):
        return self._delivery_call('delivery_release_checkpoint',*self._delivery_lease(job,'release_id'),Json(plan)) is True

    def delivery_release_finish(self, job, status, receipt, summary):
        return self._delivery_call('delivery_release_finish',*self._delivery_lease(job,'release_id'),status,Json(receipt),summary) is True

    def post_merge_available(self, owner=None):
        """Opt-in startup probe; an unprovisioned historical schema stays inert."""
        owner = self._owner if owner is None else validate_owner(owner)
        if owner != self._owner:
            return False
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT to_regprocedure('forge_coding_reconciler_api.post_merge_available(text)') IS NOT NULL")
            if not cursor.fetchone()[0]:
                return False
            cursor.execute("SELECT forge_coding_reconciler_api.post_merge_available(%s)", (owner,))
            return cursor.fetchone()[0] is True

    def post_merge_claim(self, worker_id):
        validate_identifier(worker_id, 'worker_id')
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.post_merge_claim(%s)", (worker_id,))
            value = cursor.fetchone()[0]
            return None if value is None else dict(value)

    def post_merge_renew(self, job):
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.post_merge_renew(%s,%s,%s)",
                           (_coerce_uuid(job['post_merge_id']), job['generation'], _coerce_uuid(job['lease_token'])))
            return cursor.fetchone()[0] is True

    def post_merge_checkpoint(self, job, request):
        if not isinstance(request, dict):
            raise ValueError('Invalid post-merge request')
        from uuid import uuid5
        if request.get('prepare_action_id') != str(uuid5(_coerce_uuid(job['post_merge_id']), 'prepare-v1')):
            raise ValueError('Invalid post-merge preparation identity')
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.post_merge_checkpoint(%s,%s,%s,%s)",
                           (_coerce_uuid(job['post_merge_id']), job['generation'], _coerce_uuid(job['lease_token']), Json(request)))
            return cursor.fetchone()[0] is True

    def post_merge_start_allowed(self, job):
        """Database-time first-effect fence; renew alone still permits cleanup."""
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.post_merge_start_allowed(%s,%s,%s)",
                           (_coerce_uuid(job['post_merge_id']), job['generation'], _coerce_uuid(job['lease_token'])))
            return cursor.fetchone()[0] is True

    def post_merge_finish(self, job, status, receipt, summary):
        if status not in {'pending', 'passed', 'failed', 'superseded'} or not isinstance(receipt, dict):
            raise ValueError('Invalid post-merge result')
        if not isinstance(summary, str) or len(summary.encode('utf-8')) > MAX_SUMMARY_BYTES:
            raise ValueError('Invalid post-merge summary')
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.post_merge_finish(%s,%s,%s,%s,%s,%s)",
                           (_coerce_uuid(job['post_merge_id']), job['generation'], _coerce_uuid(job['lease_token']), status, Json(receipt), summary))
            return cursor.fetchone()[0] is True

    def native_merge_failure(self, job, outcome):
        """Return one reconciled non-merge to its bounded native author loop."""
        from work_feedback import validate_findings
        if not isinstance(outcome, dict):
            raise ValueError("Invalid native merge failure")
        outcome = {**outcome, "findings": validate_findings(outcome.get("findings", []), public=True)}
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.native_merge_failure(%s,%s,%s)",
                (_coerce_uuid(job["merge_id"]), _coerce_uuid(job["lease_token"]), Json(outcome)))
            result = dict(cursor.fetchone()[0])
            if result.get("status") == "handoff":
                self._resolve_open_native_retries(
                    cursor, job.get("project_alias"), result.get("workflow_id") or job.get("workflow_id")
                )
            return result

    def native_branch_first_replay(self, review_id, expected_revision):
        """Once append a real pre-CI conflict retry, retaining its old evidence."""
        if type(expected_revision) is not int or not 1 <= expected_revision < 2**53:
            raise ValueError("Invalid native replay revision")
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.native_branch_first_replay(%s,%s)",
                (_coerce_uuid(review_id), expected_revision))
            return dict(cursor.fetchone()[0])

    def native_branch_published(self, job, receipt):
        """Persist the confirmed owned-branch event before admitting Jenkins I/O."""
        if not isinstance(receipt, dict):
            raise ValueError("Invalid native branch receipt")
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.native_branch_published(%s,%s,%s)",
                (_coerce_uuid(job["review_id"]), _coerce_uuid(job["lease_token"]), Json(receipt)))
            value = cursor.fetchone()[0]
        if not isinstance(value, dict) or set(value) != {"event_id", "correlation_id"}:
            raise ValueError("Invalid native branch event acknowledgement")
        _coerce_uuid(value["event_id"], "native branch event")
        _coerce_uuid(value["correlation_id"], "native branch correlation")
        return dict(value)

    def queue_review(self,lease,reconciliation_receipt_id):
        attempt,token=_extract_attempt_creds(lease)
        receipt=_coerce_uuid(reconciliation_receipt_id,"reconciliation_receipt_id")
        with self._worker_cursor() as cursor:
            cursor.execute("SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtextextended(%s,%s))",
                           (self._owner,REVIEW_ADMISSION_LOCK_SALT))
            cursor.execute("SELECT forge_coding_worker_api.queue_review(%s,%s,%s)",(attempt,token,receipt))
            value=cursor.fetchone()[0]
        return str(value) if value else None

    def review_claim(self,worker_id):
        validate_identifier(worker_id,"worker_id")
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtextextended(%s,%s))",
                           (self._owner,REVIEW_ADMISSION_LOCK_SALT))
            cursor.execute("SELECT forge_coding_reconciler_api.review_claim(%s)",(worker_id,))
            value=cursor.fetchone()[0]
        return None if value is None else dict(value)

    def review_renew(self,job):
        review=_coerce_uuid(job["review_id"],"review_id");token=_coerce_uuid(job["lease_token"],"lease_token")
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.review_renew(%s,%s)",(review,token))
            return bool(cursor.fetchone()[0])

    def review_finish(self,job,outcome):
        from work_feedback import validate_findings
        if not isinstance(outcome,dict):raise ValueError("Invalid review outcome")
        outcome={**outcome,"findings":validate_findings(outcome.get("findings",[]))}
        review=_coerce_uuid(job["review_id"],"review_id");token=_coerce_uuid(job["lease_token"],"lease_token")
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.review_finish(%s,%s,%s)",(review,token,Json(outcome)))
            return dict(cursor.fetchone()[0])

    def record_hosted_checks(self,publication_id,candidate_commit,checks):
        publication=_coerce_uuid(publication_id,"publication_id")
        if not isinstance(candidate_commit,str) or not SHA1_HEX_RE.fullmatch(candidate_commit):raise ValueError("Invalid hosted source")
        with self._reconcile_cursor() as cursor:
            cursor.execute("SELECT forge_coding_reconciler_api.record_hosted_checks(%s,%s,%s)",(publication,candidate_commit,Json(checks)))
            return dict(cursor.fetchone()[0])

    def _native_ci_context(self, job, *, acquire):
        """Read only canonical CI identities; never expose prompts or lease tokens."""
        if not isinstance(job, dict) or ("version" in job and
                (type(job["version"]) is not int or job["version"] != 1)):
            raise ValueError("Invalid native CI identity")
        project = validate_identifier(job.get("project_alias"), "native CI project")
        review = _coerce_uuid(job.get("review_id"), "native CI review")
        task = _coerce_uuid(job.get("task_id"), "native CI task")
        attempt = _coerce_uuid(job.get("attempt_id"), "native CI attempt")
        with self._reconcile_cursor() as cursor:
            if acquire:
                token = _coerce_uuid(job.get("lease_token"), "native CI lease")
                cursor.execute("SELECT forge_coding_reconciler_api.native_ci_context(%s,%s,%s,%s,%s)",
                               (review, token, project, task, attempt))
            else:
                cursor.execute("SELECT forge_coding_reconciler_api.native_ci_status(%s,%s,%s,%s)",
                               (review, project, task, attempt))
            value = cursor.fetchone()[0]
        keys = {"version", "owner_id", "project_alias", "task_id", "attempt_id", "review_id",
                "workflow_id", "correlation_id", "candidate_commit", "active", "state"}
        if (not isinstance(value, dict) or set(value) != keys or type(value["version"]) is not int
                or value["version"] != 1 or value["owner_id"] != self._owner
                or value["project_alias"] != project or value["task_id"] != str(task)
                or value["attempt_id"] != str(attempt) or value["review_id"] != str(review)
                or type(value["active"]) is not bool
                or value["state"] not in {"active", "cancelled", "superseded", "completed", "inactive"}
                or value["active"] != (value["state"] == "active")
                or value["candidate_commit"] is not None and (
                    not isinstance(value["candidate_commit"], str)
                    or not SHA1_HEX_RE.fullmatch(value["candidate_commit"]))):
            raise ValueError("Invalid native CI context response")
        _coerce_uuid(value["workflow_id"], "native CI workflow")
        _coerce_uuid(value["correlation_id"], "native CI correlation")
        if acquire and not value["active"]:
            raise WorkerAccessError("Native CI source is no longer active")
        return dict(value)

    def native_ci_binding(self, job):
        """Durable scoped activity read for an existing immutable CI sidecar."""
        return self._native_ci_context(job, acquire=False)

    def native_ci_acquire_context(self, job):
        """Acquire canonical identities only through a current local-review lease."""
        return self._native_ci_context(job, acquire=True)

    # -- M39: Persistent per-agent branch state, exclusive ownership & CI/CD stage evidence --

    def task_branch_scope(self, project_alias, task_id):
        validate_identifier(project_alias, "project_alias")
        task_uuid = _coerce_uuid(task_id, "task_id")
        with self._reconcile_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_reconciler_api.task_branch_scope(%s,%s)",
                (project_alias, task_uuid),
            )
            value = cursor.fetchone()[0]
        return None if value is None else dict(value)

    def agent_branch_get(self, project_alias, agent_profile):
        validate_identifier(project_alias, "project_alias")
        validate_identifier(agent_profile, "agent_profile")
        with self._reconcile_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_reconciler_api.agent_branch_get(%s,%s)",
                (project_alias, agent_profile),
            )
            value = cursor.fetchone()[0]
        return None if value is None else dict(value)

    def agent_branch_upsert(self, *, project_alias, agent_profile, branch_name, branch_scope,
                            head_commit_sha=None, fetched_main_sha=None, fetched_main_at=None,
                            sync_status=None, conflict_paths=None, cache_status=None,
                            last_task_id=None, last_correlation_id=None):
        validate_identifier(project_alias, "project_alias")
        validate_identifier(agent_profile, "agent_profile")
        with self._reconcile_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_reconciler_api.agent_branch_upsert(%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s)",
                (
                    project_alias, agent_profile, branch_name, branch_scope,
                    head_commit_sha, fetched_main_sha, fetched_main_at,
                    sync_status,
                    Json(list(conflict_paths)) if conflict_paths is not None else None,
                    cache_status,
                    _coerce_uuid(last_task_id, "last_task_id") if last_task_id is not None else None,
                    _coerce_uuid(last_correlation_id, "last_correlation_id") if last_correlation_id is not None else None,
                ),
            )
            return dict(cursor.fetchone()[0])

    def agent_branch_acquire(self, *, project_alias, agent_profile, branch_name, branch_scope,
                             scope_kind, scope_id, root_task_id, task_id,
                             review_id=None, publication_id=None):
        validate_identifier(project_alias, "project_alias")
        validate_identifier(agent_profile, "agent_profile")
        with self._reconcile_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_reconciler_api.agent_branch_acquire(%s,%s,%s,%s,%s,%s,%s,%s,%s,%s)",
                (
                    project_alias, agent_profile, branch_name, branch_scope,
                    scope_kind,
                    _coerce_uuid(scope_id, "scope_id"),
                    _coerce_uuid(root_task_id, "root_task_id"),
                    _coerce_uuid(task_id, "task_id"),
                    _coerce_uuid(review_id, "review_id") if review_id is not None else None,
                    _coerce_uuid(publication_id, "publication_id") if publication_id is not None else None,
                ),
            )
            return dict(cursor.fetchone()[0])

    def agent_branch_verify_owner(self, *, project_alias, agent_profile, scope_kind, scope_id,
                                  expected_epoch):
        validate_identifier(project_alias, "project_alias")
        validate_identifier(agent_profile, "agent_profile")
        if type(expected_epoch) is not int or expected_epoch < 0:
            raise ValueError("Invalid expected_epoch")
        with self._reconcile_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_reconciler_api.agent_branch_verify_owner(%s,%s,%s,%s,%s)",
                (project_alias, agent_profile, scope_kind, _coerce_uuid(scope_id, "scope_id"), expected_epoch),
            )
            return dict(cursor.fetchone()[0])

    def agent_branch_record_publication(self, *, project_alias, agent_profile, scope_kind, scope_id,
                                        expected_epoch, candidate_commit=None, pr_url=None,
                                        publication_id=None):
        validate_identifier(project_alias, "project_alias")
        validate_identifier(agent_profile, "agent_profile")
        if type(expected_epoch) is not int or expected_epoch < 0:
            raise ValueError("Invalid expected_epoch")
        with self._reconcile_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_reconciler_api.agent_branch_record_publication(%s,%s,%s,%s,%s,%s,%s,%s)",
                (
                    project_alias, agent_profile, scope_kind, _coerce_uuid(scope_id, "scope_id"),
                    expected_epoch, candidate_commit, pr_url,
                    _coerce_uuid(publication_id, "publication_id") if publication_id is not None else None,
                ),
            )
            return dict(cursor.fetchone()[0])

    def agent_branch_set_blocker(self, *, project_alias, agent_profile, scope_kind, scope_id,
                                 expected_epoch, blocker):
        validate_identifier(project_alias, "project_alias")
        validate_identifier(agent_profile, "agent_profile")
        if type(expected_epoch) is not int or expected_epoch < 0:
            raise ValueError("Invalid expected_epoch")
        with self._reconcile_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_reconciler_api.agent_branch_set_blocker(%s,%s,%s,%s,%s,%s)",
                (project_alias, agent_profile, scope_kind, _coerce_uuid(scope_id, "scope_id"), expected_epoch, blocker),
            )
            return dict(cursor.fetchone()[0])

    def agent_branch_release(self, *, project_alias, agent_profile, scope_kind, scope_id,
                             expected_epoch, release_reason, remote_open_pr_confirmed_none,
                             merge_commit=None):
        validate_identifier(project_alias, "project_alias")
        validate_identifier(agent_profile, "agent_profile")
        if type(expected_epoch) is not int or expected_epoch < 0:
            raise ValueError("Invalid expected_epoch")
        if type(remote_open_pr_confirmed_none) is not bool:
            raise ValueError("Invalid remote_open_pr_confirmed_none")
        with self._reconcile_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_reconciler_api.agent_branch_release(%s,%s,%s,%s,%s,%s,%s,%s)",
                (
                    project_alias, agent_profile, scope_kind, _coerce_uuid(scope_id, "scope_id"),
                    expected_epoch, release_reason, remote_open_pr_confirmed_none, merge_commit,
                ),
            )
            return dict(cursor.fetchone()[0])

    def agent_branch_reconcilable_reservations(self, limit=64):
        if type(limit) is not int or not 1 <= limit <= 128:
            raise ValueError("Invalid reconciliation limit")
        with self._reconcile_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_reconciler_api.agent_branch_reconcilable_reservations(%s)",
                (limit,),
            )
            value = cursor.fetchone()[0]
        return list(value) if isinstance(value, list) else []

    def review_claim_for_branch_policy(self, worker_id):
        validate_identifier(worker_id, "worker_id")
        with self._reconcile_cursor() as cursor:
            cursor.execute(
                "SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtextextended(%s,%s))",
                (self._owner, REVIEW_ADMISSION_LOCK_SALT),
            )
            cursor.execute(
                "SELECT forge_coding_reconciler_api.review_claim_for_branch_policy(%s)",
                (worker_id,),
            )
            value = cursor.fetchone()[0]
        return None if value is None else dict(value)

    def publication_claim_for_branch_policy(self, worker_id):
        validate_identifier(worker_id, "worker_id")
        with self._reconcile_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_reconciler_api.publication_claim_for_branch_policy(%s)",
                (worker_id,),
            )
            value = cursor.fetchone()[0]
        return None if value is None else dict(value)

    def native_agent_branch_published(self, job, receipt):
        """Persist confirmed persistent-agent-branch event and update branch head."""
        if not isinstance(receipt, dict):
            raise ValueError("Invalid native agent branch receipt")
        with self._reconcile_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_reconciler_api.native_agent_branch_published(%s,%s,%s)",
                (_coerce_uuid(job["review_id"]), _coerce_uuid(job["lease_token"]), Json(receipt)),
            )
            value = cursor.fetchone()[0]
        if not isinstance(value, dict) or set(value) != {"event_id", "correlation_id"}:
            raise ValueError("Invalid native agent branch event acknowledgement")
        _coerce_uuid(value["event_id"], "native agent branch event")
        _coerce_uuid(value["correlation_id"], "native agent branch correlation")
        return dict(value)

    def record_jenkins_stage_evidence(self, evidence, chunks=()):
        """Persist immutable per-stage CI/CD evidence and bounded redacted log chunks."""
        if not isinstance(evidence, dict):
            raise ValueError("Invalid stage evidence")
        with self._reconcile_cursor() as cursor:
            cursor.execute(
                "SELECT forge_coding_reconciler_api.record_jenkins_stage_evidence(%s,%s)",
                (Json(evidence), Json(list(chunks or ()))),
            )
            return dict(cursor.fetchone()[0])
