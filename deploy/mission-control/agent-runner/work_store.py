"""PostgreSQL DAO for Forge coding-job durable admission and queued cancellation."""

from contextlib import closing, contextmanager
from datetime import UTC
from uuid import UUID, uuid4

from psycopg2.extras import Json, RealDictCursor, register_uuid

from queue_store import Rejected
from work_config import (
    DEFAULT_MAX_PENDING,
    MAX_MAX_PENDING,
    MIN_MAX_PENDING,
    PinnedRecipe,
    sha256_canonical,
    validate_identifier,
    validate_owner,
)

register_uuid()

ADVISORY_LOCK_SALT = 2026111901
NOTIFY_CHANNEL = "forge_coding_jobs"
MAX_REVISION = 2**53 - 1
VALID_STATUSES = frozenset({"queued", "cancelled"})


def _timestamp(value):
    formatted = value.astimezone(UTC).isoformat().replace("+00:00", "Z")
    if len(formatted.encode("utf-8")) > 80:
        raise ValueError("Formatted timestamp exceeds wire bound")
    return formatted


def _summary(row):
    return {
        "task_id": str(row["id"]),
        "project_alias": row["project_alias"],
        "base_revision": row["base_revision"],
        "mode": row["mode"],
        "agent_profile": row["agent_profile"],
        "recipe_digest": row["recipe_digest"],
        "status": row["status"],
        "revision": row["revision"],
        "created_at": _timestamp(row["created_at"]),
    }


def _submit_intent_digest(*, project_alias, base_revision, mode, agent_profile,
                          prompt, acceptance_criteria):
    return sha256_canonical({
        "project_alias": project_alias,
        "base_revision": base_revision,
        "mode": mode,
        "agent_profile": agent_profile,
        "prompt": prompt,
        "acceptance_criteria": acceptance_criteria,
    })


def _cancel_intent_digest(*, project_alias, task_id, expected_revision):
    return sha256_canonical({
        "action": "cancel",
        "project_alias": project_alias,
        "task_id": str(task_id),
        "expected_revision": expected_revision,
    })


class WorkStore:
    """Owner- and project-scoped durable store for coding-job admission."""

    def __init__(self, owner, allowed_projects, connect_factory, *, max_pending=DEFAULT_MAX_PENDING):
        self._owner = validate_owner(owner)
        if not isinstance(allowed_projects, (frozenset, set, tuple, list)):
            raise ValueError("Invalid work store allowed_projects")
        frozen_projects = frozenset(
            validate_identifier(project, "project alias") for project in allowed_projects
        )
        if not frozen_projects:
            raise ValueError("Work store allowed_projects must be non-empty")
        if not callable(connect_factory):
            raise TypeError("connect_factory must be callable")
        if type(max_pending) is not int or not MIN_MAX_PENDING <= max_pending <= MAX_MAX_PENDING:
            raise ValueError("Invalid work store max_pending")
        self._allowed_projects = frozen_projects
        self._connect_factory = connect_factory
        self._max_pending = max_pending

    @property
    def owner(self):
        return self._owner

    @property
    def allowed_projects(self):
        return self._allowed_projects

    @property
    def max_pending(self):
        return self._max_pending

    def _require_project(self, project_alias):
        if project_alias not in self._allowed_projects:
            raise Rejected(403, "denied")

    @contextmanager
    def _transaction(self, *, read_only=False):
        with closing(self._connect_factory()) as connection:
            with connection:
                with connection.cursor(cursor_factory=RealDictCursor) as cursor:
                    if read_only:
                        cursor.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
                    yield cursor

    def submit(self, *, project_alias, base_revision, mode, agent_profile,
               prompt, acceptance_criteria, idempotency_key, resolve_recipe):
        self._require_project(project_alias)
        intent_digest = _submit_intent_digest(
            project_alias=project_alias,
            base_revision=base_revision,
            mode=mode,
            agent_profile=agent_profile,
            prompt=prompt,
            acceptance_criteria=acceptance_criteria,
        )
        lock_scope = f"{self._owner}\n{project_alias}"
        with self._transaction() as cursor:
            cursor.execute(
                "SELECT pg_catalog.pg_advisory_xact_lock("
                "pg_catalog.hashtextextended(%s::text, %s::bigint))",
                (lock_scope, ADVISORY_LOCK_SALT),
            )
            cursor.execute("""
                SELECT t.intent_digest, e.payload AS receipt_json
                FROM forge_agent.coding_tasks t
                JOIN forge_agent.coding_events e
                  ON (e.owner_id, e.project_alias, e.task_id) = (t.owner_id, t.project_alias, t.id)
                 AND e.sequence = 1
                WHERE t.owner_id = %s AND t.project_alias = %s AND t.idempotency_key = %s
            """, (self._owner, project_alias, idempotency_key))
            existing = cursor.fetchone()
            if existing is not None:
                if existing["intent_digest"] != intent_digest:
                    raise Rejected(409, "idempotency_conflict")
                return dict(existing["receipt_json"])

            cursor.execute("""
                SELECT count(*) AS pending
                FROM forge_agent.coding_tasks
                WHERE owner_id = %s AND project_alias = %s AND status = 'queued'
            """, (self._owner, project_alias))
            if cursor.fetchone()["pending"] >= self._max_pending:
                raise Rejected(503, "queue_full")

            recipe = resolve_recipe()
            if (not isinstance(recipe, PinnedRecipe)
                    or recipe.mode != mode
                    or recipe.generation_token_limit is not None):
                raise ValueError("Recipe resolver returned an invalid pinned recipe")

            task_id = uuid4()
            receipt = {
                "task_id": str(task_id),
                "project_alias": project_alias,
                "revision": 1,
                "status": "queued",
                "recipe_digest": recipe.digest,
            }
            cursor.execute("""
                INSERT INTO forge_agent.coding_tasks (
                    owner_id, project_alias, id, idempotency_key, intent_digest,
                    base_revision, mode, agent_profile, prompt, acceptance_criteria,
                    pinned_recipe, recipe_digest, status, revision
                )
                VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, 'queued', 1)
            """, (
                self._owner, project_alias, task_id, idempotency_key, intent_digest,
                base_revision, mode, agent_profile, prompt, acceptance_criteria,
                Json(recipe.to_dict()), recipe.digest,
            ))
            cursor.execute("""
                INSERT INTO forge_agent.coding_events (
                    owner_id, project_alias, task_id, sequence, event_kind, payload
                )
                VALUES (%s, %s, %s, 1, 'queued', %s)
            """, (self._owner, project_alias, task_id, Json(receipt)))
            cursor.execute("SELECT pg_catalog.pg_notify(%s, '')", (NOTIFY_CHANNEL,))
            return receipt

    def get(self, *, project_alias, task_id):
        self._require_project(project_alias)
        if not isinstance(task_id, UUID):
            raise Rejected(400, "invalid_request")
        with self._transaction(read_only=True) as cursor:
            cursor.execute("""
                SELECT id, project_alias, base_revision, mode, agent_profile,
                       recipe_digest, status, revision, created_at,
                       prompt, acceptance_criteria
                FROM forge_agent.coding_tasks
                WHERE owner_id = %s AND project_alias = %s AND id = %s
            """, (self._owner, project_alias, task_id))
            row = cursor.fetchone()
            if row is None:
                raise Rejected(404, "not_found")
            return {
                **_summary(row),
                "prompt": row["prompt"],
                "acceptance_criteria": row["acceptance_criteria"],
            }

    def list_tasks(self, *, project_alias, status_filter=None, limit=20, cursor_task_id=None):
        self._require_project(project_alias)
        if status_filter is not None and status_filter not in VALID_STATUSES:
            raise Rejected(400, "invalid_request")
        if type(limit) is not int or not 1 <= limit <= 50:
            raise Rejected(400, "invalid_request")
        if cursor_task_id is not None and not isinstance(cursor_task_id, UUID):
            raise Rejected(400, "invalid_request")

        with self._transaction(read_only=True) as cursor:
            cursor_created_at = None
            if cursor_task_id is not None:
                cursor.execute("""
                    SELECT created_at, id
                    FROM forge_agent.coding_tasks
                    WHERE owner_id = %s AND project_alias = %s AND id = %s
                """, (self._owner, project_alias, cursor_task_id))
                anchor = cursor.fetchone()
                if anchor is None:
                    raise Rejected(400, "invalid_request")
                cursor_created_at = anchor["created_at"]

            cursor.execute("""
                SELECT id, project_alias, base_revision, mode, agent_profile,
                       recipe_digest, status, revision, created_at
                FROM forge_agent.coding_tasks
                WHERE owner_id = %s AND project_alias = %s
                  AND (%s::text IS NULL OR status = %s::text)
                  AND (%s::timestamptz IS NULL OR (created_at, id) > (%s::timestamptz, %s::uuid))
                ORDER BY created_at ASC, id ASC
                LIMIT %s
            """, (
                self._owner, project_alias,
                status_filter, status_filter,
                cursor_created_at, cursor_created_at, cursor_task_id,
                limit + 1,
            ))
            rows = cursor.fetchall()

        page = rows[:limit]
        next_cursor = str(page[-1]["id"]) if len(rows) > limit else None
        return {
            "project_alias": project_alias,
            "tasks": [_summary(row) for row in page],
            "next_cursor": next_cursor,
        }

    def events(self, *, project_alias, task_id, after_sequence=0, limit=50):
        self._require_project(project_alias)
        if (not isinstance(task_id, UUID)
                or type(after_sequence) is not int or not 0 <= after_sequence <= MAX_REVISION
                or type(limit) is not int or not 1 <= limit <= 100):
            raise Rejected(400, "invalid_request")

        with self._transaction(read_only=True) as cursor:
            cursor.execute("""
                SELECT id, project_alias, revision, status
                FROM forge_agent.coding_tasks
                WHERE owner_id = %s AND project_alias = %s AND id = %s
            """, (self._owner, project_alias, task_id))
            task = cursor.fetchone()
            if task is None:
                raise Rejected(404, "not_found")

            cursor.execute("""
                SELECT sequence, event_kind, created_at
                FROM forge_agent.coding_events
                WHERE owner_id = %s AND project_alias = %s AND task_id = %s
                  AND sequence > %s
                ORDER BY sequence ASC
                LIMIT %s
            """, (self._owner, project_alias, task_id, after_sequence, limit))
            rows = cursor.fetchall()

        items = [
            {
                "sequence": row["sequence"],
                "kind": row["event_kind"],
                "created_at": _timestamp(row["created_at"]),
            }
            for row in rows
        ]
        next_sequence = items[-1]["sequence"] if items else after_sequence
        return {
            "project_alias": task["project_alias"],
            "task_id": str(task["id"]),
            "revision": task["revision"],
            "status": task["status"],
            "events": items,
            "next_sequence": next_sequence,
        }

    def cancel(self, *, project_alias, task_id, expected_revision, idempotency_key):
        self._require_project(project_alias)
        if (not isinstance(task_id, UUID)
                or type(expected_revision) is not int
                or not 1 <= expected_revision <= MAX_REVISION):
            raise Rejected(400, "invalid_request")
        intent_digest = _cancel_intent_digest(
            project_alias=project_alias,
            task_id=task_id,
            expected_revision=expected_revision,
        )
        with self._transaction() as cursor:
            cursor.execute("""
                SELECT id, project_alias, status, revision
                FROM forge_agent.coding_tasks
                WHERE owner_id = %s AND project_alias = %s AND id = %s
                FOR UPDATE
            """, (self._owner, project_alias, task_id))
            task = cursor.fetchone()
            if task is None:
                raise Rejected(404, "not_found")

            cursor.execute("""
                SELECT intent_digest, receipt_json
                FROM forge_agent.coding_control_receipts
                WHERE owner_id = %s AND project_alias = %s AND task_id = %s
                  AND action = 'cancel' AND idempotency_key = %s
            """, (self._owner, project_alias, task_id, idempotency_key))
            existing = cursor.fetchone()
            if existing is not None:
                if existing["intent_digest"] != intent_digest:
                    raise Rejected(409, "idempotency_conflict")
                return dict(existing["receipt_json"])

            if task["status"] != "queued":
                raise Rejected(409, "not_cancellable")
            if task["revision"] != expected_revision or expected_revision >= MAX_REVISION:
                raise Rejected(409, "revision_conflict")

            cursor.execute("""
                UPDATE forge_agent.coding_tasks
                SET status = 'cancelled',
                    revision = revision + 1,
                    cancelled_at = clock_timestamp(),
                    updated_at = clock_timestamp()
                WHERE owner_id = %s AND project_alias = %s AND id = %s
                  AND status = 'queued' AND revision = %s
                RETURNING revision
            """, (self._owner, project_alias, task_id, expected_revision))
            updated = cursor.fetchone()
            if updated is None:
                raise Rejected(409, "revision_conflict")
            new_revision = updated["revision"]

            receipt = {
                "project_alias": task["project_alias"],
                "task_id": str(task["id"]),
                "base_revision": expected_revision,
                "revision": new_revision,
                "status": "cancelled",
            }
            cursor.execute("""
                INSERT INTO forge_agent.coding_events (
                    owner_id, project_alias, task_id, sequence, event_kind, payload
                )
                SELECT %s, %s, %s, coalesce(max(sequence), 0) + 1, 'cancelled', %s
                FROM forge_agent.coding_events
                WHERE owner_id = %s AND project_alias = %s AND task_id = %s
            """, (
                self._owner, project_alias, task_id,
                Json({"base_revision": expected_revision, "revision": new_revision}),
                self._owner, project_alias, task_id,
            ))
            cursor.execute("""
                INSERT INTO forge_agent.coding_control_receipts (
                    owner_id, project_alias, task_id, action, idempotency_key,
                    intent_digest, base_revision, result_revision, receipt_json
                )
                VALUES (%s, %s, %s, 'cancel', %s, %s, %s, %s, %s)
            """, (
                self._owner, project_alias, task_id, idempotency_key,
                intent_digest, expected_revision, new_revision, Json(receipt),
            ))
            return receipt
