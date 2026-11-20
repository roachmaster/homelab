"""PostgreSQL DAO for Forge coding-job durable admission, execution queries, and control."""

from contextlib import closing, contextmanager
from datetime import UTC
import json
import psycopg2
import re
import unicodedata
from uuid import UUID, uuid4

from psycopg2.extras import Json, RealDictCursor, register_uuid

from queue_store import Rejected
from work_config import (
    DEFAULT_MAX_PENDING,
    DEFAULT_REPAIR_POLICY,freeze_repair_policy,
    MAX_MAX_PENDING,
    MAX_PROFILES,
    MAX_PROJECTS,
    MIN_MAX_PENDING,
    PinnedRecipe,
    WorkConfig,
    prepare_title,
    sha256_canonical,
    validate_identifier,
    validate_owner,
)
from work_policy import POLICY_DIGEST, POLICY_ID
from work_feedback import validate_findings

register_uuid()

ADVISORY_LOCK_SALT = 2026111901
NOTIFY_CHANNEL = "forge_coding_jobs"
MAX_REVISION = 2**53 - 1
RESULT_MIN_LIMIT_BYTES = 4
RESULT_MAX_LIMIT_BYTES = 16384
RESULT_MAX_JSON_CONTENT_BYTES = 32768
WORKER_FRESHNESS_SECONDS = 90
SHA1_HEX_RE = re.compile(r"[0-9a-f]{40}")
RESULT_CURSOR_RE = re.compile(r"([0-9a-f]{64})\.(0|[1-9][0-9]{0,7})")
VALID_STATUSES = frozenset({
    "queued",
    "running",
    "cancel_requested",
    "awaiting_review",
    "completed",
    "cancelled",
    "failed",
    "quota_blocked",
    "blocked_auth",
    "blocked_permission",
    "needs_reconcile",
})


def _public_progress(payload):
    """Project only explicit public reports, never arbitrary event payloads."""
    if not isinstance(payload, dict) or set(payload) != {
        "attempt_id", "progress_sequence", "stage", "summary"
    }:
        return None
    attempt, ordinal = payload["attempt_id"], payload["progress_sequence"]
    stage, summary = payload["stage"], payload["summary"]
    try:
        valid = (isinstance(attempt, str) and str(UUID(attempt)) == attempt
                 and type(ordinal) is int and 1 <= ordinal <= 128
                 and stage in {"plan", "progress"}
                 and (stage == "plan") == (ordinal == 1)
                 and isinstance(summary, str) and bool(summary.strip())
                 and len(summary.encode("utf-8")) <= 4096
                 and all(unicodedata.category(c) != "Cc" or c in "\n\t" for c in summary))
    except (ValueError, TypeError, UnicodeError):
        return None
    return ({"attempt_id": attempt, "sequence": ordinal, "stage": stage,
             "summary": summary} if valid else None)
NON_RESUMABLE_STATUSES = frozenset({"queued", "running", "cancel_requested", "completed"})
MODE_ORDER = ("plan", "review", "implement")


def _timestamp(value):
    formatted = value.astimezone(UTC).isoformat().replace("+00:00", "Z")
    if len(formatted.encode("utf-8")) > 80:
        raise ValueError("Formatted timestamp exceeds wire bound")
    return formatted


def _summary(row):
    item = {
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
    if row.get("title") is not None:
        item["title"] = row["title"]
    if "workflow_required" in row:
        item["workflow_required"] = row["workflow_required"]
    if "workflow_phase" in row:
        item["workflow_phase"] = row["workflow_phase"]
    return item


def _submit_intent_digest(*, project_alias, base_revision, mode, agent_profile,
                          prompt, acceptance_criteria, auto_repair=False, workflow_policy_digest=None):
    return sha256_canonical({
        "project_alias": project_alias,
        "base_revision": base_revision,
        "mode": mode,
        "agent_profile": agent_profile,
        "prompt": prompt,
        "acceptance_criteria": acceptance_criteria,
        **({"auto_repair":True} if auto_repair else {}),
        **({"workflow_policy_digest":workflow_policy_digest} if workflow_policy_digest is not None else {}),
    })


def _cancel_intent_digest(*, project_alias, task_id, expected_revision):
    return sha256_canonical({
        "action": "cancel",
        "project_alias": project_alias,
        "task_id": str(task_id),
        "expected_revision": expected_revision,
    })


def _accept_intent_digest(*, project_alias, task_id, expected_revision, review_note):
    return sha256_canonical({
        "action": "accept",
        "project_alias": project_alias,
        "task_id": str(task_id),
        "expected_revision": expected_revision,
        "review_note": review_note,
    })


def _resume_intent_digest(*, project_alias, task_id, expected_revision,
                          agent_profile, reconciliation_receipt_id, note):
    return sha256_canonical({
        "action": "resume",
        "project_alias": project_alias,
        "task_id": str(task_id),
        "expected_revision": expected_revision,
        "agent_profile": agent_profile,
        "reconciliation_receipt_id": str(reconciliation_receipt_id),
        "note": note,
    })


def _decode_utf8_prefix(raw_bytes):
    """Decode the longest valid UTF-8 prefix of raw_bytes without splitting a codepoint."""
    end = len(raw_bytes)
    min_end = max(0, end - 3)
    while end >= min_end:
        try:
            return raw_bytes[:end].decode("utf-8")
        except UnicodeDecodeError:
            end -= 1
    raise Rejected(400, "invalid_request")


def _fit_json_content_bound(text):
    """Trim Unicode characters from the end so JSON-escaped content stays within transport bounds."""
    encoded = json.dumps(text, ensure_ascii=False).encode("utf-8")
    if len(encoded) <= RESULT_MAX_JSON_CONTENT_BYTES:
        return text
    lo, hi = 1, len(text)
    best = text[:1]
    while lo <= hi:
        mid = (lo + hi) // 2
        candidate = text[:mid]
        if len(json.dumps(candidate, ensure_ascii=False).encode("utf-8")) <= RESULT_MAX_JSON_CONTENT_BYTES:
            best = candidate
            lo = mid + 1
        else:
            hi = mid - 1
    return best


def _parse_result_cursor(cursor):
    if cursor is None:
        return None
    if not isinstance(cursor, str) or len(cursor.encode("ascii", errors="ignore")) != len(cursor) or len(cursor) > 86:
        raise Rejected(400, "invalid_request")
    match = RESULT_CURSOR_RE.fullmatch(cursor)
    if match is None:
        raise Rejected(400, "invalid_request")
    return match.group(1), int(match.group(2))


class WorkStore:
    """Owner- and project-scoped durable store for coding jobs, results, and control receipts."""

    def __init__(self, owner, allowed_projects, connect_factory, *, max_pending=DEFAULT_MAX_PENDING,repair_policy=None):
        self._repair_policy=freeze_repair_policy(repair_policy or DEFAULT_REPAIR_POLICY)
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
        try:
            with closing(self._connect_factory()) as connection:
                with connection:
                    with connection.cursor(cursor_factory=RealDictCursor) as cursor:
                        if read_only:
                            cursor.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
                        cursor.execute("SELECT pg_catalog.set_config('forge.coding_owner',%s,true)",(self._owner,))
                        yield cursor
        except psycopg2.Error as error:
            mapped={'22023':(400,'invalid_request'),'42501':(403,'denied'),'P0002':(404,'not_found'),
                    'P0006':(409,'result_mismatch'),'P0008':(409,'reconcile_required'),'P0009':(409,'not_reviewable')}
            if error.pgcode in mapped:
                raise Rejected(*mapped[error.pgcode]) from None
            raise

    def catalog(self, *, config):
        if not isinstance(config, WorkConfig) or config.owner != self._owner:
            raise ValueError("Catalog requires matching frozen WorkConfig")
        with self._transaction(read_only=True) as cursor:
            cursor.execute("""
                SELECT worker_id, policy_digest, projects, profiles, observed_at,
                       (observed_at >= clock_timestamp() - make_interval(secs => %s)) AS is_fresh
                FROM forge_agent.coding_worker_readiness
                WHERE owner_id = %s
                ORDER BY observed_at DESC, worker_id ASC
            """, (WORKER_FRESHNESS_SECONDS, self._owner))
            rows = cursor.fetchall()

        if not rows:
            worker_status = "unknown"
            observed_at = None
        else:
            observed_at = _timestamp(rows[0]["observed_at"])
            worker_status = "ready" if any(row["is_fresh"] for row in rows) else "offline"

        advertised_revisions = {}
        advertised_profile_modes = {}
        for row in rows:
            if not row["is_fresh"]:
                continue
            raw_projects = row["projects"] if isinstance(row["projects"], list) else []
            for entry in raw_projects:
                if not isinstance(entry, dict):
                    continue
                alias = entry.get("alias")
                if alias not in config.allowed_projects or alias in advertised_revisions:
                    continue
                rev = entry.get("base_revision")
                if rev is None or (isinstance(rev, str) and SHA1_HEX_RE.fullmatch(rev)):
                    advertised_revisions[alias] = rev

            if row["policy_digest"] != POLICY_DIGEST:
                continue
            raw_profiles = row["profiles"] if isinstance(row["profiles"], list) else []
            for entry in raw_profiles:
                if not isinstance(entry, dict):
                    continue
                alias = entry.get("alias")
                profile = config.profiles.get(alias)
                if profile is None or profile.resource_profile != POLICY_ID:
                    continue
                if entry.get("backend") != profile.backend or entry.get("model") != profile.model:
                    continue
                modes = entry.get("modes")
                if not isinstance(modes, list):
                    continue
                valid = {m for m in modes if isinstance(m, str) and m in config.allowed_modes}
                if valid:
                    advertised_profile_modes.setdefault(alias, set()).update(valid)

        with self._transaction(read_only=True) as cursor:
            cursor.execute("""SELECT DISTINCT ON(a.project_alias) a.project_alias,a.finish_receipt->'receipt'->>'merge_commit' AS merge_commit
             FROM forge_agent.coding_workflow_actions a JOIN forge_agent.coding_workflows w USING(workflow_id)
             WHERE a.owner_id=%s AND a.kind='merge' AND a.status='completed' AND a.finish_receipt->>'status'='merged'
             AND a.finish_receipt->'receipt'->>'merge_commit'=a.intent->>'merge_commit' AND w.phase='completed'
             ORDER BY a.project_alias,a.updated_at DESC,a.action_id DESC""",(self._owner,))
            for merged in cursor.fetchall():
                if merged['project_alias'] in config.project_workflows:
                    advertised_revisions[merged['project_alias']]=merged['merge_commit']

        projects = [
            {
                "alias": alias,
                "label": config.project_label(alias),
                "base_revision": advertised_revisions.get(alias),
                "workflow":config.project_workflows[alias].summary(config.catalog(),config.repair_policy) if alias in config.project_workflows else None,
            }
            for alias in sorted(config.allowed_projects)[:MAX_PROJECTS]
        ]
        profiles = []
        for alias in sorted(config.profiles)[:MAX_PROFILES]:
            modes = advertised_profile_modes.get(alias)
            if not modes:
                continue
            profile = config.profiles[alias]
            profiles.append({
                "alias": alias,
                "label": config.profile_label(alias),
                "backend": profile.backend,
                "model": profile.model,
                "modes": [m for m in MODE_ORDER if m in modes],
            })
        return {
            "projects": projects,
            "profiles": profiles,
            "worker_status": worker_status,
            "observed_at": observed_at,
        }

    def submit(self, *, project_alias, base_revision, mode, agent_profile,
               prompt, acceptance_criteria, idempotency_key, resolve_recipe, auto_repair=False,
               workflow_policy_digest=None,resolve_workflow=lambda:None):
        self._require_project(project_alias)
        if type(auto_repair) is not bool:
            raise Rejected(400,"invalid_request")
        title = prepare_title(prompt)
        intent_digest = _submit_intent_digest(
            project_alias=project_alias,
            base_revision=base_revision,
            mode=mode,
            agent_profile=agent_profile,
            prompt=prompt,
            acceptance_criteria=acceptance_criteria,
            auto_repair=auto_repair,workflow_policy_digest=workflow_policy_digest,
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
                WHERE owner_id = %s AND project_alias = %s
                  AND status IN ('queued', 'running', 'cancel_requested')
            """, (self._owner, project_alias))
            if cursor.fetchone()["pending"] >= self._max_pending:
                raise Rejected(503, "queue_full")

            workflow=resolve_workflow()
            if workflow is not None:
                cursor.execute("""SELECT a.finish_receipt->'receipt'->>'merge_commit' AS head FROM forge_agent.coding_workflow_actions a
                 JOIN forge_agent.coding_workflows w USING(workflow_id) WHERE a.owner_id=%s AND a.project_alias=%s AND a.kind='merge' AND a.status='completed'
                 AND a.finish_receipt->>'status'='merged' AND a.finish_receipt->'receipt'->>'merge_commit'=a.intent->>'merge_commit' AND w.phase='completed'
                 ORDER BY a.updated_at DESC,a.action_id DESC LIMIT 1""",(self._owner,project_alias))
                merged=cursor.fetchone()
                if merged and merged['head']!=base_revision:raise Rejected(409,'result_mismatch')
                auto_repair=True
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
                    base_revision, mode, agent_profile, title, prompt, acceptance_criteria,
                    pinned_recipe, recipe_digest, status, revision,auto_repair,review_required
                )
                VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, 'queued', 1,%s,%s)
            """, (
                self._owner, project_alias, task_id, idempotency_key, intent_digest,
                base_revision, mode, agent_profile, title, prompt, acceptance_criteria,
                Json(recipe.to_dict()), recipe.digest,auto_repair,mode=="implement",
            ))
            self._ensure_review_policy(cursor,project_alias,task_id)
            if workflow is not None:
                cursor.execute("INSERT INTO forge_agent.coding_workflows(owner_id,project_alias,root_task_id,policy_digest,snapshot) VALUES(%s,%s,%s,%s,%s) RETURNING workflow_id",(self._owner,project_alias,task_id,workflow["policy_digest"],Json(workflow)))
                workflow_id=cursor.fetchone()["workflow_id"]
                cursor.execute("UPDATE forge_agent.coding_tasks SET workflow_required=true,workflow_id=%s WHERE owner_id=%s AND project_alias=%s AND id=%s",(workflow_id,self._owner,project_alias,task_id))
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
                       title, prompt, acceptance_criteria, latest_attempt_id, plan_decision_id,review_required,workflow_required,workflow_id,workflow_review_id
                FROM forge_agent.coding_tasks
                WHERE owner_id = %s AND project_alias = %s AND id = %s
            """, (self._owner, project_alias, task_id))
            row = cursor.fetchone()
            if row is None:
                raise Rejected(404, "not_found")

            attempt_dto = None
            result_dto = None
            reconciliation_receipt_id = None

            if row["latest_attempt_id"] is not None and row["status"] != "queued":
                cursor.execute("""
                    SELECT attempt_id, worker_id, phase, started_at, finished_at, summary,
                           published_at, result_sha256, result_byte_count, result_chunk_count
                    FROM forge_agent.coding_attempts
                    WHERE owner_id = %s AND project_alias = %s
                      AND task_id = %s AND attempt_id = %s
                """, (self._owner, project_alias, task_id, row["latest_attempt_id"]))
                attempt = cursor.fetchone()
                if attempt is not None:
                    attempt_dto = {
                        "attempt_id": str(attempt["attempt_id"]),
                        "worker_id": attempt["worker_id"],
                        "phase": attempt["phase"],
                        "started_at": _timestamp(attempt["started_at"]),
                        "finished_at": (
                            _timestamp(attempt["finished_at"])
                            if attempt["finished_at"] is not None else None
                        ),
                        "summary": attempt["summary"],
                    }
                    if attempt["published_at"] is not None and attempt["result_sha256"] is not None:
                        result_dto = {
                            "attempt_id": str(attempt["attempt_id"]),
                            "sha256": attempt["result_sha256"],
                            "byte_count": attempt["result_byte_count"],
                            "chunk_count": attempt["result_chunk_count"],
                        }

                if row["status"] not in NON_RESUMABLE_STATUSES:
                    cursor.execute("""
                        SELECT receipt_id
                        FROM forge_agent.coding_reconciliation_receipts
                        WHERE owner_id = %s AND project_alias = %s
                          AND task_id = %s AND attempt_id = %s
                          AND task_revision = %s
                        ORDER BY created_at DESC
                        LIMIT 1
                    """, (self._owner, project_alias, task_id, row["latest_attempt_id"], row["revision"]))
                    rec = cursor.fetchone()
                    if rec is not None:
                        reconciliation_receipt_id = str(rec["receipt_id"])

            detail = {
                **_summary(row),
                "review_required":row["review_required"],
                "workflow_required":row["workflow_required"],
                "workflow":self._workflow_projection(cursor,row),
                "prompt": row["prompt"],
                "acceptance_criteria": row["acceptance_criteria"],
                "attempt": attempt_dto,
                "result": result_dto,
                "reconciliation_receipt_id": reconciliation_receipt_id,
            }
            cursor.execute("""
                SELECT publication_id,status,branch,commit_sha,pr_url,summary FROM forge_agent.coding_publications
                WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s ORDER BY created_at DESC LIMIT 1
            """, (self._owner, project_alias, task_id,
                  row["latest_attempt_id"] if row["status"] != "queued" else None))
            publication = cursor.fetchone()
            if publication is not None:
                detail["publication"] = {key: str(value) if key=="publication_id" else value for key, value in publication.items() if value is not None}
            elif (row["mode"] == "implement" and row["status"] == "awaiting_review"
                  and not row["review_required"] and self._approved_plan_implementation(cursor,row)):
                # Publication is authorized by the Plan decision. The worker's
                # reconcile/queue calls are separate commits, so that handoff
                # is already pending even before the outbox row becomes visible.
                detail["publication"] = {"status":"pending","summary":"Waiting for trusted publication."}
            review=self._review_projection(cursor,row)
            if review is not None:
                detail["review"]=review
            if "title" not in detail:
                detail["title"] = prepare_title(row["prompt"])
            return detail

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
                SELECT t.id, t.project_alias, t.base_revision, t.mode, t.agent_profile,
                       t.recipe_digest, t.status, t.revision, t.created_at, t.title,
                       t.workflow_required, w.phase AS workflow_phase
                FROM forge_agent.coding_tasks t
                LEFT JOIN forge_agent.coding_workflows w
                  ON (w.owner_id, w.project_alias, w.workflow_id) =
                     (t.owner_id, t.project_alias, t.workflow_id)
                WHERE t.owner_id = %s AND t.project_alias = %s
                  AND (%s::text IS NULL OR t.status = %s::text)
                  AND (%s::timestamptz IS NULL OR (t.created_at, t.id) > (%s::timestamptz, %s::uuid))
                ORDER BY t.created_at ASC, t.id ASC
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
                SELECT sequence, event_kind, created_at, payload
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
                **({"progress": progress} if row["event_kind"] == "checkpoint"
                   and (progress := _public_progress(row["payload"])) is not None else {}),
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

    def result(self, *, project_alias, task_id, attempt_id, cursor=None,
               limit_bytes=RESULT_MAX_LIMIT_BYTES):
        self._require_project(project_alias)
        if (not isinstance(task_id, UUID)
                or not isinstance(attempt_id, UUID)
                or type(limit_bytes) is not int
                or not RESULT_MIN_LIMIT_BYTES <= limit_bytes <= RESULT_MAX_LIMIT_BYTES):
            raise Rejected(400, "invalid_request")
        parsed_cursor = _parse_result_cursor(cursor)

        with self._transaction(read_only=True) as db_cursor:
            db_cursor.execute("""
                SELECT result_sha256, result_byte_count, result_chunk_count, published_at
                FROM forge_agent.coding_attempts
                WHERE owner_id = %s AND project_alias = %s
                  AND task_id = %s AND attempt_id = %s
            """, (self._owner, project_alias, task_id, attempt_id))
            attempt = db_cursor.fetchone()
            if (attempt is None or attempt["published_at"] is None
                    or attempt["result_sha256"] is None):
                raise Rejected(404, "not_found")

            result_sha256 = attempt["result_sha256"]
            total_bytes = attempt["result_byte_count"]

            if parsed_cursor is None:
                offset_bytes = 0
            else:
                cursor_sha, offset_bytes = parsed_cursor
                if cursor_sha != result_sha256:
                    raise Rejected(409, "result_mismatch")
                if not 0 <= offset_bytes < total_bytes:
                    raise Rejected(400, "invalid_request")

            db_cursor.execute("""
                WITH spans AS (
                    SELECT chunk_index, content, byte_length,
                           coalesce(
                               sum(byte_length) OVER (
                                   ORDER BY chunk_index
                                   ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
                               ),
                               0
                           ) AS start_byte,
                           sum(byte_length) OVER (ORDER BY chunk_index) AS end_byte
                    FROM forge_agent.coding_result_chunks
                    WHERE owner_id = %s AND project_alias = %s
                      AND task_id = %s AND attempt_id = %s
                )
                SELECT chunk_index, content, byte_length, start_byte, end_byte
                FROM spans
                WHERE end_byte > %s AND start_byte < %s
                ORDER BY chunk_index ASC
            """, (
                self._owner, project_alias, task_id, attempt_id,
                offset_bytes, offset_bytes + limit_bytes,
            ))
            rows = db_cursor.fetchall()

        if not rows:
            raise Rejected(503, "unavailable")

        raw_window = b"".join(row["content"].encode("utf-8") for row in rows)
        window_start = rows[0]["start_byte"]
        local_start = offset_bytes - window_start
        if local_start < 0 or local_start >= len(raw_window):
            raise Rejected(400, "invalid_request")
        # Reject a forged byte offset landing inside a multi-byte UTF-8 sequence.
        if (raw_window[local_start] & 0xC0) == 0x80:
            raise Rejected(400, "invalid_request")

        raw_slice = raw_window[local_start : local_start + limit_bytes]
        content = _decode_utf8_prefix(raw_slice)
        content = _fit_json_content_bound(content)
        emitted_bytes = len(content.encode("utf-8"))
        if emitted_bytes == 0:
            raise Rejected(503, "unavailable")

        next_offset = offset_bytes + emitted_bytes
        complete = next_offset >= total_bytes
        next_cursor = None if complete else f"{result_sha256}.{next_offset}"
        return {
            "project_alias": project_alias,
            "task_id": str(task_id),
            "attempt_id": str(attempt_id),
            "result_digest": result_sha256,
            "content": content,
            "offset_bytes": offset_bytes,
            "next_cursor": next_cursor,
            "complete": complete,
            "total_bytes": total_bytes,
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
            cursor.execute("SELECT pg_advisory_xact_lock(hashtextextended(%s::text,%s::bigint))",(self._owner+"\n"+project_alias,ADVISORY_LOCK_SALT))
            cursor.execute("SELECT w.root_task_id FROM forge_agent.coding_tasks t JOIN forge_agent.coding_workflows w ON w.workflow_id=t.workflow_id WHERE t.owner_id=%s AND t.project_alias=%s AND t.id=%s",(self._owner,project_alias,task_id))
            scope=cursor.fetchone()
            if scope and scope['root_task_id']!=task_id:
                cursor.execute("SELECT id FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE",(self._owner,project_alias,scope['root_task_id']))
            cursor.execute("""
                SELECT id, project_alias, status, revision,workflow_required,workflow_id
                FROM forge_agent.coding_tasks
                WHERE owner_id = %s AND project_alias = %s AND id = %s
                FOR UPDATE
            """, (self._owner, project_alias, task_id))
            task = cursor.fetchone()
            if task is None:
                raise Rejected(404, "not_found")

            cursor.execute("""
                SELECT action, intent_digest, receipt_json
                FROM forge_agent.coding_control_receipts
                WHERE owner_id = %s AND project_alias = %s AND task_id = %s
                  AND idempotency_key = %s
            """, (self._owner, project_alias, task_id, idempotency_key))
            existing = cursor.fetchone()
            if existing is not None:
                if existing["action"] != "cancel" or existing["intent_digest"] != intent_digest:
                    raise Rejected(409, "idempotency_conflict")
                return dict(existing["receipt_json"])

            if task["workflow_required"]:
                if task["revision"]!=expected_revision:raise Rejected(409,"revision_conflict")
                cursor.execute("SELECT phase FROM forge_agent.coding_workflows WHERE workflow_id=%s FOR UPDATE",(task["workflow_id"],))
                workflow=cursor.fetchone()
                if not workflow or workflow["phase"] in {"completed","cancelled","merging"}:raise Rejected(409,"not_cancellable")
                cursor.execute("UPDATE forge_agent.coding_workflows SET phase='cancelled',enabled=false,updated_at=clock_timestamp() WHERE workflow_id=%s",(task["workflow_id"],))
                cursor.execute("UPDATE forge_agent.coding_tasks SET cancelled_at=CASE WHEN status IN('running','cancel_requested') THEN NULL ELSE clock_timestamp() END,status=CASE WHEN status IN('running','cancel_requested') THEN 'cancel_requested' ELSE 'cancelled' END,revision=revision+1,updated_at=clock_timestamp() WHERE workflow_id=%s AND status NOT IN('completed','cancelled')",(task["workflow_id"],))
                cursor.execute("UPDATE forge_agent.coding_workflow_actions SET status='cancelled',lease_token=NULL,lease_expires_at=NULL,updated_at=clock_timestamp() WHERE workflow_id=%s AND status IN('pending','leased')",(task["workflow_id"],))
                target='cancel_requested' if task['status'] in {'running','cancel_requested'} else 'cancelled'
                receipt={'project_alias':project_alias,'task_id':str(task_id),'base_revision':expected_revision,'revision':expected_revision+1,'status':target}
                cursor.execute("INSERT INTO forge_agent.coding_control_receipts(owner_id,project_alias,task_id,action,idempotency_key,intent_digest,base_revision,result_revision,receipt_json) VALUES(%s,%s,%s,'cancel',%s,%s,%s,%s,%s)",(self._owner,project_alias,task_id,idempotency_key,intent_digest,expected_revision,expected_revision+1,Json(receipt)))
                cursor.execute("SELECT pg_notify(%s,'')",(NOTIFY_CHANNEL,))
                return receipt

            if task["status"] not in ("queued", "running"):
                raise Rejected(409, "not_cancellable")
            if task["revision"] != expected_revision or expected_revision >= MAX_REVISION:
                raise Rejected(409, "revision_conflict")

            if task["status"] == "queued":
                target_status = "cancelled"
                event_kind = "cancelled"
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
            else:
                target_status = "cancel_requested"
                event_kind = "cancel_requested"
                cursor.execute("""
                    UPDATE forge_agent.coding_tasks
                    SET status = 'cancel_requested',
                        revision = revision + 1,
                        updated_at = clock_timestamp()
                    WHERE owner_id = %s AND project_alias = %s AND id = %s
                      AND status = 'running' AND revision = %s
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
                "status": target_status,
            }
            cursor.execute("""
                INSERT INTO forge_agent.coding_events (
                    owner_id, project_alias, task_id, sequence, event_kind, payload
                )
                SELECT %s, %s, %s, coalesce(max(sequence), 0) + 1, %s, %s
                FROM forge_agent.coding_events
                WHERE owner_id = %s AND project_alias = %s AND task_id = %s
            """, (
                self._owner, project_alias, task_id, event_kind,
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
            if target_status == "cancel_requested":
                cursor.execute("SELECT pg_catalog.pg_notify(%s, '')", (NOTIFY_CHANNEL,))
            return receipt

    def _approved_plan_implementation(self, cursor, task):
        if task.get("plan_decision_id") is None or task["mode"] != "implement":
            return False
        cursor.execute("""SELECT 1 FROM forge_agent.coding_plan_decisions
            WHERE owner_id=%s AND project_alias=%s AND task_id=%s
              AND decision_id=%s AND decision='resume'""",
            (self._owner,task["project_alias"],task["id"],task["plan_decision_id"]))
        return cursor.fetchone() is not None

    def _publication_busy(self, cursor, task):
        cursor.execute("""SELECT 1 FROM forge_agent.coding_publications
            WHERE owner_id=%s AND project_alias=%s AND task_id=%s
              AND status IN ('pending','checking','publishing')""",
            (self._owner,task["project_alias"],task["id"]))
        if cursor.fetchone() is not None:
            return True
        if task["status"] != "awaiting_review" or (not(task.get("review_required",False) and task["mode"]=="implement") and not self._approved_plan_implementation(cursor,task)):
            return False
        cursor.execute("""SELECT 1 FROM forge_agent.coding_publications
            WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s
              AND status IN ('published','failed')""",
            (self._owner,task["project_alias"],task["id"],task["latest_attempt_id"]))
        return cursor.fetchone() is None

    def accept(self, *, project_alias, task_id, expected_revision, idempotency_key, review_note=""):
        self._require_project(project_alias)
        if (not isinstance(task_id, UUID)
                or type(expected_revision) is not int
                or not 1 <= expected_revision <= MAX_REVISION
                or not isinstance(review_note, str)
                or len(review_note.encode("utf-8")) > 2048):
            raise Rejected(400, "invalid_request")
        intent_digest = _accept_intent_digest(
            project_alias=project_alias,
            task_id=task_id,
            expected_revision=expected_revision,
            review_note=review_note,
        )
        with self._transaction() as cursor:
            cursor.execute("""
                SELECT id, project_alias, mode, status, revision, latest_attempt_id, plan_decision_id,review_required,workflow_required
                FROM forge_agent.coding_tasks
                WHERE owner_id = %s AND project_alias = %s AND id = %s
                FOR UPDATE
            """, (self._owner, project_alias, task_id))
            task = cursor.fetchone()
            if task is None:
                raise Rejected(404, "not_found")

            cursor.execute("""
                SELECT action, intent_digest, receipt_json
                FROM forge_agent.coding_control_receipts
                WHERE owner_id = %s AND project_alias = %s AND task_id = %s
                  AND idempotency_key = %s
            """, (self._owner, project_alias, task_id, idempotency_key))
            existing = cursor.fetchone()
            if existing is not None:
                if existing["action"] != "accept" or existing["intent_digest"] != intent_digest:
                    raise Rejected(409, "idempotency_conflict")
                return dict(existing["receipt_json"])

            if task["mode"] == "plan" or task["status"] != "awaiting_review" or task["latest_attempt_id"] is None:
                raise Rejected(409, "not_reviewable")

            cursor.execute("""
                SELECT phase, reservation_active, published_at, result_sha256
                FROM forge_agent.coding_attempts
                WHERE owner_id = %s AND project_alias = %s
                  AND task_id = %s AND attempt_id = %s
            """, (self._owner, project_alias, task_id, task["latest_attempt_id"]))
            attempt = cursor.fetchone()
            if (attempt is None or attempt["phase"] != "stopped"
                    or attempt["reservation_active"]
                    or attempt["published_at"] is None
                    or attempt["result_sha256"] is None):
                raise Rejected(409, "not_reviewable")

            if task.get("workflow_required"):raise Rejected(409,"not_reviewable")
            if task["revision"] != expected_revision or expected_revision >= MAX_REVISION:
                raise Rejected(409, "revision_conflict")

            if self._publication_busy(cursor,task):
                raise Rejected(409,"reconcile_required")
            if task["mode"]=="implement" and task["review_required"] and not self._hosted_green(cursor,task):
                raise Rejected(409,"not_reviewable")

            cursor.execute("""
                UPDATE forge_agent.coding_tasks
                SET status = 'completed',
                    revision = revision + 1,
                    accepted_at = clock_timestamp(),
                    review_note = %s,
                    updated_at = clock_timestamp()
                WHERE owner_id = %s AND project_alias = %s AND id = %s
                  AND status = 'awaiting_review' AND revision = %s
                RETURNING revision
            """, (review_note, self._owner, project_alias, task_id, expected_revision))
            updated = cursor.fetchone()
            if updated is None:
                raise Rejected(409, "revision_conflict")
            new_revision = updated["revision"]

            receipt = {
                "project_alias": task["project_alias"],
                "task_id": str(task["id"]),
                "base_revision": expected_revision,
                "revision": new_revision,
                "status": "completed",
            }
            cursor.execute("""
                INSERT INTO forge_agent.coding_events (
                    owner_id, project_alias, task_id, sequence, event_kind, payload
                )
                SELECT %s, %s, %s, coalesce(max(sequence), 0) + 1, 'completed', %s
                FROM forge_agent.coding_events
                WHERE owner_id = %s AND project_alias = %s AND task_id = %s
            """, (
                self._owner, project_alias, task_id,
                Json({"base_revision": expected_revision, "revision": new_revision, "review_note": review_note}),
                self._owner, project_alias, task_id,
            ))
            cursor.execute("""
                INSERT INTO forge_agent.coding_control_receipts (
                    owner_id, project_alias, task_id, action, idempotency_key,
                    intent_digest, base_revision, result_revision, receipt_json
                )
                VALUES (%s, %s, %s, 'accept', %s, %s, %s, %s, %s)
            """, (
                self._owner, project_alias, task_id, idempotency_key,
                intent_digest, expected_revision, new_revision, Json(receipt),
            ))
            return receipt

    def resume(self, *, project_alias, task_id, expected_revision, idempotency_key,
               agent_profile, reconciliation_receipt_id, note="", allowed_modes, resolve_recipe):
        self._require_project(project_alias)
        if (not isinstance(task_id, UUID)
                or not isinstance(reconciliation_receipt_id, UUID)
                or type(expected_revision) is not int
                or not 1 <= expected_revision <= MAX_REVISION
                or not isinstance(note, str)
                or len(note.encode("utf-8")) > 2048):
            raise Rejected(400, "invalid_request")
        intent_digest = _resume_intent_digest(
            project_alias=project_alias,
            task_id=task_id,
            expected_revision=expected_revision,
            agent_profile=agent_profile,
            reconciliation_receipt_id=reconciliation_receipt_id,
            note=note,
        )
        lock_scope = f"{self._owner}\n{project_alias}"
        with self._transaction() as cursor:
            cursor.execute(
                "SELECT pg_catalog.pg_advisory_xact_lock("
                "pg_catalog.hashtextextended(%s::text, %s::bigint))",
                (lock_scope, ADVISORY_LOCK_SALT),
            )
            cursor.execute("""
                SELECT id, project_alias, mode, status, revision, latest_attempt_id, plan_decision_id,review_required,workflow_required
                FROM forge_agent.coding_tasks
                WHERE owner_id = %s AND project_alias = %s AND id = %s
                FOR UPDATE
            """, (self._owner, project_alias, task_id))
            task = cursor.fetchone()
            if task is None:
                raise Rejected(404, "not_found")
            if task["mode"] not in allowed_modes:
                raise Rejected(403, "denied")

            cursor.execute("""
                SELECT action, intent_digest, receipt_json
                FROM forge_agent.coding_control_receipts
                WHERE owner_id = %s AND project_alias = %s AND task_id = %s
                  AND idempotency_key = %s
            """, (self._owner, project_alias, task_id, idempotency_key))
            existing = cursor.fetchone()
            if existing is not None:
                if existing["action"] != "resume" or existing["intent_digest"] != intent_digest:
                    raise Rejected(409, "idempotency_conflict")
                return dict(existing["receipt_json"])

            if task.get("workflow_required"):raise Rejected(409,"not_reviewable")
            if task["revision"] != expected_revision or expected_revision >= MAX_REVISION:
                raise Rejected(409, "revision_conflict")

            if self._publication_busy(cursor,task):
                raise Rejected(409,"reconcile_required")
            if task['review_required'] and task['mode']=='implement':
                cursor.execute("SELECT 1 FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s",
                    (self._owner,project_alias,task_id,task['latest_attempt_id']))
                if cursor.fetchone() is not None:
                    raise Rejected(409,'not_reviewable')
            if (task["mode"] == "plan" and task["status"] == "awaiting_review"
                    or task["status"] in NON_RESUMABLE_STATUSES or task["latest_attempt_id"] is None):
                raise Rejected(409, "reconcile_required")

            cursor.execute("""
                SELECT receipt_id, attempt_id, task_revision, workspace_digest
                FROM forge_agent.coding_reconciliation_receipts
                WHERE owner_id = %s AND project_alias = %s
                  AND task_id = %s AND receipt_id = %s
            """, (self._owner, project_alias, task_id, reconciliation_receipt_id))
            rec = cursor.fetchone()
            if rec is None:
                raise Rejected(409, "reconcile_required")
            if (rec["task_revision"] != task["revision"]
                    or rec["attempt_id"] != task["latest_attempt_id"]):
                raise Rejected(409, "stale_reconciliation")

            cursor.execute("""
                SELECT count(*) FILTER (WHERE reservation_active OR lease_expires_at IS NOT NULL) AS reserved,
                       bool_or(attempt_id = %s AND phase = 'stopped') AS attempt_stopped
                FROM forge_agent.coding_attempts
                WHERE owner_id = %s AND project_alias = %s AND task_id = %s
            """, (rec["attempt_id"], self._owner, project_alias, task_id))
            attempt_check = cursor.fetchone()
            if attempt_check["reserved"] > 0 or not attempt_check["attempt_stopped"]:
                raise Rejected(409, "reconcile_required")

            cursor.execute("""
                SELECT count(*) AS pending
                FROM forge_agent.coding_tasks
                WHERE owner_id = %s AND project_alias = %s
                  AND status IN ('queued', 'running', 'cancel_requested')
            """, (self._owner, project_alias))
            if cursor.fetchone()["pending"] >= self._max_pending:
                raise Rejected(503, "queue_full")

            recipe = resolve_recipe(task["mode"])
            if (not isinstance(recipe, PinnedRecipe)
                    or recipe.mode != task["mode"]
                    or recipe.generation_token_limit is not None):
                raise ValueError("Recipe resolver returned an invalid pinned recipe")

            cursor.execute("""
                UPDATE forge_agent.coding_tasks
                SET status = 'queued',
                    revision = revision + 1,
                    agent_profile = %s,
                    pinned_recipe = %s,
                    recipe_digest = %s,
                    resume_workspace_digest = %s,
                    cancelled_at = NULL,legacy_resume_note=%s,review_required=%s,
                    updated_at = clock_timestamp()
                WHERE owner_id = %s AND project_alias = %s AND id = %s
                  AND revision = %s
                RETURNING revision
            """, (
                agent_profile, Json(recipe.to_dict()), recipe.digest, rec["workspace_digest"],note,task["mode"]=="implement",
                self._owner, project_alias, task_id, expected_revision,
            ))
            updated = cursor.fetchone()
            if updated is None:
                raise Rejected(409, "revision_conflict")
            new_revision = updated["revision"]

            receipt = {
                "project_alias": task["project_alias"],
                "task_id": str(task["id"]),
                "base_revision": expected_revision,
                "revision": new_revision,
                "status": "queued",
                "recipe_digest": recipe.digest,
            }
            event_payload = {
                "base_revision": expected_revision,
                "revision": new_revision,
                "agent_profile": agent_profile,
                "recipe_digest": recipe.digest,
                "reconciliation_receipt_id": str(reconciliation_receipt_id),
                "workspace_digest": rec["workspace_digest"],
            }
            if note:
                event_payload["note"] = note
            cursor.execute("""
                INSERT INTO forge_agent.coding_events (
                    owner_id, project_alias, task_id, sequence, event_kind, payload
                )
                SELECT %s, %s, %s, coalesce(max(sequence), 0) + 1, 'resumed', %s
                FROM forge_agent.coding_events
                WHERE owner_id = %s AND project_alias = %s AND task_id = %s
            """, (
                self._owner, project_alias, task_id, Json(event_payload),
                self._owner, project_alias, task_id,
            ))
            cursor.execute("""
                INSERT INTO forge_agent.coding_control_receipts (
                    owner_id, project_alias, task_id, action, idempotency_key,
                    intent_digest, base_revision, result_revision, receipt_json
                )
                VALUES (%s, %s, %s, 'resume', %s, %s, %s, %s, %s)
            """, (
                self._owner, project_alias, task_id, idempotency_key,
                intent_digest, expected_revision, new_revision, Json(receipt),
            ))
            cursor.execute("SELECT pg_catalog.pg_notify(%s, '')", (NOTIFY_CHANNEL,))
            return receipt

    @contextmanager
    def _transition_transaction(self,authority=None):
        if authority is None:
            with self._transaction() as cursor:yield cursor
        else:
            from work_workflow import BoundDelegatedAuthority
            if not isinstance(authority,BoundDelegatedAuthority) or authority.store is not self:
                raise Rejected(403,"denied")
            yield authority.cursor

    def _require_transition_authority(self,cursor,task,authority,kind):
        if authority is not None:
            authority.validate(cursor,task,kind)
        elif task.get("workflow_required"):
            raise Rejected(409,"not_reviewable")

    def _workflow_projection(self,cursor,task):
        if task.get("workflow_id") is None:return None
        cursor.execute("SELECT w.*,coalesce(b.repair_rounds,0) AS code_revisions FROM forge_agent.coding_workflows w LEFT JOIN forge_agent.coding_review_budget b ON(b.owner_id,b.project_alias,b.root_task_id)=(w.owner_id,w.project_alias,w.root_task_id) WHERE w.owner_id=%s AND w.project_alias=%s AND w.workflow_id=%s",(self._owner,task["project_alias"],task["workflow_id"]))
        w=cursor.fetchone()
        if w is None:return None
        p=w["snapshot"]
        return {"workflow_id":str(w["workflow_id"]),"policy_digest":w["policy_digest"],"phase":w["phase"],
            "author_profile":p["author_profile"],"reviewer_profile":p["reviewer_profile"],"plan_revisions":w["plan_revisions"],
            "max_plan_revisions":p["max_plan_revisions"],"code_revisions":w["code_revisions"],"max_code_revisions":p["max_code_revisions"],
            "escalation_reason":w["escalation_reason"] or None,"can_cancel":w["phase"] not in {"completed","cancelled","merging"}}

    def plan_decide(self, *, project_alias, task_id, expected_revision, plan_attempt_id,
                    plan_sha256, decision, agent_profile, idempotency_key, note="", resolve_recipe,auto_repair=False,_authority=None):
        """Atomically decide one exact immutable Plan and pin the next stage."""
        self._require_project(project_alias)
        try:
            valid = (isinstance(task_id, UUID) and isinstance(plan_attempt_id, UUID)
                     and type(expected_revision) is int and 1 <= expected_revision < MAX_REVISION
                     and isinstance(plan_sha256, str) and re.fullmatch(r"[0-9a-f]{64}", plan_sha256)
                     and isinstance(decision, str) and decision in {"resume", "revise", "cancel"}
                     and isinstance(agent_profile, str) and re.fullmatch(r"[A-Za-z0-9_.-]{1,64}", agent_profile)
                     and isinstance(idempotency_key, str) and re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", idempotency_key)
                     and type(auto_repair) is bool
                     and isinstance(note, str) and len(note.encode("utf-8")) <= 2048
                     and (decision != "revise" or bool(note.strip()))
                     and all(unicodedata.category(c) != "Cc" or c in "\r\n\t" for c in note))
        except UnicodeError:
            valid = False
        if not valid:
            raise Rejected(400, "invalid_request")
        intent = sha256_canonical({"action": "plan_decide", "project_alias": project_alias,
            "task_id": str(task_id), "expected_revision": expected_revision,
            "plan_attempt_id": str(plan_attempt_id), "plan_sha256": plan_sha256,
            "decision": decision, "agent_profile": agent_profile, "note": note,
            **({"auto_repair":True} if auto_repair else {})})
        with self._transition_transaction(_authority) as cursor:
            cursor.execute("SELECT pg_advisory_xact_lock(hashtextextended(%s::text,%s::bigint))",
                           (f"{self._owner}\n{project_alias}", ADVISORY_LOCK_SALT))
            cursor.execute("""SELECT * FROM forge_agent.coding_tasks
                WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE""",
                (self._owner, project_alias, task_id))
            task = cursor.fetchone()
            if task is None:
                raise Rejected(404, "not_found")
            cursor.execute("""SELECT action,intent_digest,receipt_json FROM forge_agent.coding_control_receipts
                WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND idempotency_key=%s""",
                (self._owner, project_alias, task_id, idempotency_key))
            old = cursor.fetchone()
            if old is not None:
                if old["action"] != "plan_decide" or old["intent_digest"] != intent:
                    raise Rejected(409, "idempotency_conflict")
                return dict(old["receipt_json"])
            self._require_transition_authority(cursor,task,_authority,"plan")
            if task["revision"] != expected_revision:
                raise Rejected(409, "revision_conflict")
            if task["mode"] != "plan" or task["status"] not in {"awaiting_review", "completed"}:
                raise Rejected(409, "not_reviewable")
            if task["latest_attempt_id"] != plan_attempt_id:
                raise Rejected(409, "result_mismatch")
            cursor.execute("""SELECT * FROM forge_agent.coding_attempts
                WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s""",
                (self._owner, project_alias, task_id, plan_attempt_id))
            source = cursor.fetchone()
            if (source is None or source["phase"] != "stopped" or source["reservation_active"]
                    or source["lease_expires_at"] is not None or source["published_at"] is None
                    or source["result_sha256"] != plan_sha256 or source["pinned_recipe"]["mode"] != "plan"):
                raise Rejected(409, "result_mismatch")
            mode = "implement" if decision == "resume" else "plan"
            status = "cancelled" if decision == "cancel" else "queued"
            digest = None
            recipe_json, recipe_digest = task["pinned_recipe"], task["recipe_digest"]
            if decision != "cancel":
                cursor.execute("""SELECT workspace_digest FROM forge_agent.coding_reconciliation_receipts
                    WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s
                    ORDER BY created_at DESC LIMIT 1""", (self._owner, project_alias, task_id, plan_attempt_id))
                rec = cursor.fetchone()
                if rec is None:
                    raise Rejected(409, "reconcile_required")
                digest = rec["workspace_digest"]
                cursor.execute("""SELECT count(*) AS pending FROM forge_agent.coding_tasks
                    WHERE owner_id=%s AND project_alias=%s AND status IN ('queued','running','cancel_requested')""",
                    (self._owner, project_alias))
                if cursor.fetchone()["pending"] >= self._max_pending:
                    raise Rejected(503, "queue_full")
                recipe = resolve_recipe(mode)
                if not isinstance(recipe, PinnedRecipe) or recipe.mode != mode or recipe.generation_token_limit is not None:
                    raise ValueError("Plan resolver returned an invalid pinned recipe")
                if recipe.version != 2 or recipe.resource_profile != POLICY_ID or recipe.policy_digest != POLICY_DIGEST:
                    raise Rejected(503,"unavailable")
                recipe_json, recipe_digest = recipe.to_dict(), recipe.digest
            self._require_transition_authority(cursor,task,_authority,"plan")
            decision_id, revision = uuid4(), expected_revision + 1
            receipt = {"project_alias": project_alias, "task_id": str(task_id), "base_revision": expected_revision,
                       "revision": revision, "status": status, "mode": mode}
            cursor.execute("""INSERT INTO forge_agent.coding_plan_decisions
                (decision_id,owner_id,project_alias,task_id,plan_attempt_id,plan_sha256,source_revision,base_revision,
                 decision,agent_profile,note,idempotency_key,receipt_json)
                VALUES (%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s)""",
                (decision_id,self._owner,project_alias,task_id,plan_attempt_id,plan_sha256,expected_revision,
                 task["base_revision"],decision,agent_profile,note,idempotency_key,Json(receipt)))
            cursor.execute("""UPDATE forge_agent.coding_tasks SET status=%s,mode=%s,revision=%s,
                agent_profile=%s,pinned_recipe=%s,recipe_digest=%s,plan_decision_id=%s,resume_workspace_digest=%s,
                accepted_at=NULL,cancelled_at=CASE WHEN %s='cancelled' THEN clock_timestamp() ELSE NULL END,
                review_note=%s,auto_repair=%s,review_required=%s,updated_at=clock_timestamp()
                WHERE owner_id=%s AND project_alias=%s AND id=%s AND revision=%s""",
                (status,mode,revision,task["agent_profile"] if decision == "cancel" else agent_profile,
                 Json(recipe_json),recipe_digest,decision_id,digest,status,note,auto_repair,mode=="implement",self._owner,project_alias,task_id,expected_revision))
            cursor.execute("""INSERT INTO forge_agent.coding_control_receipts
                (owner_id,project_alias,task_id,action,idempotency_key,intent_digest,base_revision,result_revision,receipt_json)
                VALUES (%s,%s,%s,'plan_decide',%s,%s,%s,%s,%s)""",
                (self._owner,project_alias,task_id,idempotency_key,intent,expected_revision,revision,Json(receipt)))
            cursor.execute("""INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
                SELECT %s,%s,%s,coalesce(max(sequence),0)+1,%s,%s FROM forge_agent.coding_events
                WHERE owner_id=%s AND project_alias=%s AND task_id=%s""",
                (self._owner,project_alias,task_id,"cancelled" if decision == "cancel" else "resumed",
                 Json({**receipt,"decision_id":str(decision_id),"decision":decision,"plan_attempt_id":str(plan_attempt_id),
                       "plan_sha256":plan_sha256}),self._owner,project_alias,task_id))
            if decision != "cancel":
                cursor.execute("SELECT pg_notify(%s,'')", (NOTIFY_CHANNEL,))
            return receipt

    def _ensure_review_policy(self,cursor,project_alias,root_task_id):
        p=self._repair_policy
        cursor.execute("""INSERT INTO forge_agent.coding_review_policy(owner_id,project_alias,version,max_rounds,max_stagnant_rounds,max_infrastructure_retries,required_hosted_checks)
            VALUES(%s,%s,%s,%s,%s,%s,%s) ON CONFLICT(owner_id,project_alias) DO UPDATE SET version=EXCLUDED.version,max_rounds=EXCLUDED.max_rounds,
            max_stagnant_rounds=EXCLUDED.max_stagnant_rounds,max_infrastructure_retries=EXCLUDED.max_infrastructure_retries,required_hosted_checks=EXCLUDED.required_hosted_checks""",
            (self._owner,project_alias,p['version'],p['max_rounds'],p['max_stagnant_rounds'],p['max_infrastructure_retries'],list(p['required_hosted_checks'])))
        cursor.execute("""INSERT INTO forge_agent.coding_review_budget(owner_id,project_alias,root_task_id,policy)
            VALUES(%s,%s,%s,%s) ON CONFLICT DO NOTHING""",(self._owner,project_alias,root_task_id,Json(dict(p))))

    def _hosted_green(self,cursor,task):
        cursor.execute("""SELECT h.required_green FROM forge_agent.coding_publications p
            JOIN forge_agent.coding_reviews r ON r.review_id=p.review_id AND r.status='approved'
            JOIN LATERAL(SELECT x.required_green FROM forge_agent.coding_hosted_checks x
              WHERE x.publication_id=p.publication_id AND x.candidate_commit=p.commit_sha
              ORDER BY x.observed_at DESC,x.observation_id DESC LIMIT 1)h ON true
            WHERE p.owner_id=%s AND p.project_alias=%s AND p.task_id=%s AND p.attempt_id=%s
              AND p.status='published' AND p.commit_sha=r.candidate_commit
            ORDER BY p.created_at DESC LIMIT 1""",
            (self._owner,task['project_alias'],task['id'],task['latest_attempt_id']))
        value=cursor.fetchone()
        return bool(value and value['required_green'])

    def _review_projection(self,cursor,task):
        cursor.execute("SELECT review_required,auto_repair,parent_task_id FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s",
                       (self._owner,task['project_alias'],task['id']))
        flags=cursor.fetchone()
        if not flags:return None
        if not flags['review_required']:
            cursor.execute('SELECT 1 FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s LIMIT 1',(self._owner,task['project_alias'],task['id']))
            if cursor.fetchone() is None:return None
        cursor.execute("""SELECT r.*,coalesce((SELECT(b.policy->>'max_rounds')::integer FROM forge_agent.coding_review_budget b JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,coalesce(t.repair_root_task_id,t.id))=(b.owner_id,b.project_alias,b.root_task_id) WHERE t.owner_id=r.owner_id AND t.project_alias=r.project_alias AND t.id=r.task_id),p.max_rounds,3) AS max_rounds FROM forge_agent.coding_reviews r
            LEFT JOIN forge_agent.coding_review_policy p USING(owner_id,project_alias)
            WHERE r.owner_id=%s AND r.project_alias=%s AND r.task_id=%s ORDER BY r.review_no DESC LIMIT 1""",
            (self._owner,task['project_alias'],task['id']))
        row=cursor.fetchone()
        green=self._hosted_green(cursor,task) if task['latest_attempt_id'] else False
        value={'stage':'waiting_for_result','review_id':None,'feedback_id':None,'round':0,'max_rounds':3,
            'open_findings':0,'verified_findings':0,'latest_check_status':None,'toolchain':None,'handoff_reason':None,
            'source_attempt_id':str(task['latest_attempt_id']) if task['latest_attempt_id'] else None,
            'candidate_commit':None,'workspace_digest':None,'gate_manifest_sha256':None,'required_hosted_green':green,
            'can_accept':False,'can_repair':False,'can_retry':False,'auto_repair':flags['auto_repair'],'findings':[],
            'repair_task_id':None,'parent_task_id':None}
        value['parent_task_id']=str(flags['parent_task_id']) if flags['parent_task_id'] else None
        if row:
            value['repair_task_id']=str(row['repair_task_id']) if row['repair_task_id'] else None
            verified=set(row['verified_finding_ids'])|set((row['owner_decision'] or {}).get('verified_finding_ids',[]))
            findings=row['findings'];value['findings']=[{**item,'status':'verified' if item['finding_id'] in verified else item.get('status','open')} for item in findings];open_ids={item['finding_id'] for item in findings if item.get('status','open')=='open'}-verified
            stage='waiting_hosted' if row['phase']=='hosted' and row['status'] in {'pending','checking'} else ('awaiting_decision' if row['status']=='awaiting_review' else row['status'])
            latest_check_status='passed' if row['verification'].get('checks_passed') is True else ('running' if row['status'] in {'pending','checking'} else 'failed')
            if row['phase']=='hosted':
                hosted_status=row['verification'].get('hosted_status')
                latest_check_status='passed' if green else ('running' if row['status'] in {'pending','checking'} or hosted_status=='pending' else 'failed')
                if row['reason']=='hosted_wait_exhausted':latest_check_status='failed'
            if row['status']=='approved' and not green:
                stage='waiting_hosted'
            value.update(stage=stage,review_id=str(row['review_id']),feedback_id=str(row['feedback_id']) if row['feedback_id'] else None,
                round=row['round'],max_rounds=row['max_rounds'],open_findings=len(open_ids),verified_findings=len(verified),
                latest_check_status=latest_check_status,
                toolchain=row['verification'].get('toolchain'),handoff_reason=row['reason'] or None,
                source_attempt_id=str(row['attempt_id']),candidate_commit=row['candidate_commit'],workspace_digest=row['workspace_digest'],
                gate_manifest_sha256=row['gate_manifest_sha256'],can_accept=green and task['status']=='awaiting_review',
                can_repair=task['status'] in {'completed','awaiting_review'} and row['candidate_commit'] is not None and row['round']<row['max_rounds'],
                can_retry=task['status']=='awaiting_review' and row['status'] in {'handoff','retry_infrastructure','needs_reconcile'})
        return value

    def _control_task(self,cursor,project_alias,task_id,expected_revision,key,action,intent):
        self._require_project(project_alias)
        if (not isinstance(task_id,UUID) or type(expected_revision) is not int or not 1<=expected_revision<MAX_REVISION
                or not isinstance(key,str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,128}',key)):
            raise Rejected(400,'invalid_request')
        cursor.execute("SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE",(self._owner,project_alias,task_id))
        task=cursor.fetchone()
        if task is None:
            raise Rejected(404,'not_found')
        cursor.execute("SELECT action,intent_digest,receipt_json FROM forge_agent.coding_control_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND idempotency_key=%s",
                       (self._owner,project_alias,task_id,key))
        old=cursor.fetchone()
        if old:
            if old['action']!=action or old['intent_digest']!=intent:
                raise Rejected(409,'idempotency_conflict')
            return task,dict(old['receipt_json'])
        if task['revision']!=expected_revision:
            raise Rejected(409,'revision_conflict')
        if task['status'] not in {'awaiting_review','completed'}:
            raise Rejected(409,'not_reviewable')
        return task,None

    def _control_record(self,cursor,task,expected_revision,key,action,intent,receipt):
        cursor.execute("UPDATE forge_agent.coding_tasks SET revision=%s,updated_at=clock_timestamp() WHERE owner_id=%s AND project_alias=%s AND id=%s AND revision=%s",
            (expected_revision+1,self._owner,task['project_alias'],task['id'],expected_revision))
        # same-task repair may already have advanced exactly this revision.
        if cursor.rowcount!=1:
            cursor.execute("SELECT revision FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s",(self._owner,task['project_alias'],task['id']))
            if cursor.fetchone()['revision']!=expected_revision+1:
                raise Rejected(409,'revision_conflict')
        cursor.execute("""INSERT INTO forge_agent.coding_control_receipts(owner_id,project_alias,task_id,action,idempotency_key,
            intent_digest,base_revision,result_revision,receipt_json) VALUES(%s,%s,%s,%s,%s,%s,%s,%s,%s)""",
            (self._owner,task['project_alias'],task['id'],action,key,intent,expected_revision,expected_revision+1,Json(receipt)))
        cursor.execute("""INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
            SELECT %s,%s,%s,coalesce(max(sequence),0)+1,'checkpoint',%s FROM forge_agent.coding_events
            WHERE owner_id=%s AND project_alias=%s AND task_id=%s""",
            (self._owner,task['project_alias'],task['id'],Json(receipt),self._owner,task['project_alias'],task['id']))
        cursor.execute("SELECT pg_notify(%s,'')",(NOTIFY_CHANNEL,))

    def review_decide(self,*,project_alias,task_id,expected_revision,review_id,decision,agent_profile,note,idempotency_key,
                      findings=(),verified_finding_ids=(),recipe=None,resolve_recipe=None,_authority=None):
        if not isinstance(review_id,UUID) or not isinstance(decision,str) or decision not in {'approve','repair','escalate','retry_infrastructure','cancel'}:
            raise Rejected(400,'invalid_request')
        try:
            findings=validate_findings(list(findings),public=True)
            if not isinstance(note,str) or len(note.encode())>2048 or len(set(verified_finding_ids))!=len(verified_finding_ids):
                raise ValueError()
            if any(not isinstance(x,str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,64}',x) for x in verified_finding_ids):
                raise ValueError()
        except (ValueError,TypeError,UnicodeError):
            raise Rejected(400,'invalid_request') from None
        intent=sha256_canonical({'action':'review_decide','project_alias':project_alias,'task_id':str(task_id),'expected_revision':expected_revision,
            'review_id':str(review_id),'decision':decision,'agent_profile':agent_profile,'note':note,'findings':findings,'verified_finding_ids':list(verified_finding_ids)})
        with self._transition_transaction(_authority) as cursor:
            task,old=self._control_task(cursor,project_alias,task_id,expected_revision,idempotency_key,'review_decide',intent)
            if old:return old
            self._require_transition_authority(cursor,task,_authority,"code")
            if task['status']!='awaiting_review':
                raise Rejected(409,'not_reviewable')
            cursor.execute("SELECT * FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND review_id=%s FOR UPDATE",
                           (self._owner,project_alias,task_id,review_id))
            review=cursor.fetchone()
            if not review or review['attempt_id']!=task['latest_attempt_id'] or review['status'] in {'pending','checking','approved','cancelled'}:
                raise Rejected(409,'not_reviewable')
            self._require_transition_authority(cursor,task,_authority,"code")
            result={'project_alias':project_alias,'task_id':str(task_id),'base_revision':expected_revision,'revision':expected_revision+1,
                    'status':task['status'],'review_id':str(review_id)}
            if decision=='approve':
                if review['verification'].get('checks_passed') is not True or review['verification'].get('cleanup_confirmed') is not True or not review['candidate_commit'] or not review['gate_manifest_sha256']:
                    raise Rejected(409,'not_reviewable')
                open_ids={x['finding_id'] for x in review['findings'] if x.get('status','open')=='open'}
                verified=set(review['verified_finding_ids'])
                owner_ids=set(verified_finding_ids)
                if not owner_ids<=open_ids or open_ids-verified-owner_ids or (owner_ids and not note.strip()) or findings:
                    raise Rejected(409,'not_reviewable')
                attestation={'closed_by':'peer_reviewer' if _authority else 'owner','owner':self._owner,'candidate_commit':review['candidate_commit'],
                    'gate_manifest_sha256':review['gate_manifest_sha256'],'verified_finding_ids':list(verified_finding_ids),'note':note}
                if _authority:attestation.update(_authority.attestation())
                cursor.execute("UPDATE forge_agent.coding_reviews SET status='approved',owner_decision=%s,updated_at=clock_timestamp() WHERE review_id=%s",(Json(attestation),review_id))
                cursor.execute("SELECT forge_agent.coding_queue_approved_review(%s)",(review_id,))
                if next(iter(cursor.fetchone().values())) is None:raise Rejected(409,'not_reviewable')
            elif decision=='repair':
                if resolve_recipe is not None:recipe=resolve_recipe()
                if not isinstance(recipe,PinnedRecipe) or recipe.mode!='implement' or recipe.resource_profile!=POLICY_ID or recipe.policy_digest!=POLICY_DIGEST:
                    raise Rejected(503,'unavailable')
                ledger={x['finding_id']:x for x in review['findings'] if x.get('status','open')=='open'}
                ledger.update({x['finding_id']:x for x in findings})
                if not ledger:raise Rejected(400,'invalid_request')
                ledger=validate_findings(list(ledger.values()),public=True)
                cursor.execute("UPDATE forge_agent.coding_reviews SET findings=%s,status='repair',summary=%s,updated_at=clock_timestamp() WHERE review_id=%s",(Json(ledger),note,review_id))
                cursor.execute("UPDATE forge_agent.coding_tasks SET agent_profile=%s,pinned_recipe=%s,recipe_digest=%s WHERE owner_id=%s AND project_alias=%s AND id=%s",
                    (agent_profile,Json(recipe.to_dict()),recipe.digest,self._owner,project_alias,task_id))
                cursor.execute("SELECT forge_agent.coding_make_feedback(%s,%s,'repair',%s,%s)",(review_id,task_id,note,Json(ledger)))
                feedback_id=next(iter(cursor.fetchone().values()))
                cursor.execute("UPDATE forge_agent.coding_reviews SET feedback_id=%s WHERE review_id=%s",(feedback_id,review_id))
                cursor.execute("SELECT forge_agent.coding_schedule_repair(%s,%s)",(review_id,feedback_id))
                result['repair_task_id']=str(next(iter(cursor.fetchone().values())));result['status']='queued' if result['repair_task_id']==str(task_id) else task['status']
            elif decision=='retry_infrastructure':
                result['review_id']=str(self._append_review_retry(cursor,task,review,note))
            elif decision=='escalate':
                cursor.execute("UPDATE forge_agent.coding_reviews SET status='handoff',reason=%s,updated_at=clock_timestamp() WHERE review_id=%s",(note,review_id))
            else:
                if task['status']=='completed':raise Rejected(409,'not_cancellable')
                cursor.execute("UPDATE forge_agent.coding_reviews SET status='cancelled',reason=%s WHERE review_id=%s",(note,review_id))
                cursor.execute("UPDATE forge_agent.coding_tasks SET status='cancelled',cancelled_at=clock_timestamp() WHERE owner_id=%s AND project_alias=%s AND id=%s",(self._owner,project_alias,task_id));result['status']='cancelled'
            self._control_record(cursor,task,expected_revision,idempotency_key,'review_decide',intent,result)
            return result

    def _append_review_retry(self,cursor,task,review,note):
        cursor.execute("SELECT coalesce((SELECT(b.policy->>'max_infrastructure_retries')::integer FROM forge_agent.coding_review_budget b WHERE b.owner_id=%s AND b.project_alias=%s AND b.root_task_id=%s),p.max_infrastructure_retries) AS max_infrastructure_retries FROM forge_agent.coding_review_policy p WHERE owner_id=%s AND project_alias=%s",(self._owner,task['project_alias'],task['repair_root_task_id'] or task['id'],self._owner,task['project_alias']))
        policy=cursor.fetchone()
        if not policy or review['infrastructure_retries']>=policy['max_infrastructure_retries']:
            raise Rejected(409,'not_reviewable')
        cursor.execute("""INSERT INTO forge_agent.coding_reviews(owner_id,project_alias,task_id,attempt_id,plan_decision_id,reconciliation_receipt_id,
            base_revision,workspace_digest,parent_result_sha256,source_revision,review_no,round,stagnant_rounds,infrastructure_retries,previous_review_id,candidate_commit,summary,phase,owner_decision,verification,target_publication_id,source_publication_id,gate_manifest_sha256)
            SELECT owner_id,project_alias,task_id,attempt_id,plan_decision_id,reconciliation_receipt_id,base_revision,workspace_digest,parent_result_sha256,%s,
             (SELECT coalesce(max(review_no),0)+1 FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s),round,stagnant_rounds,infrastructure_retries+1,review_id,candidate_commit,%s,phase,owner_decision,verification,target_publication_id,source_publication_id,gate_manifest_sha256
            FROM forge_agent.coding_reviews WHERE review_id=%s RETURNING review_id""",
            (task['revision']+1,self._owner,task['project_alias'],task['id'],note,review['review_id']))
        return cursor.fetchone()['review_id']

    def repair(self,*,project_alias,task_id,expected_revision,candidate_commit,agent_profile,note,findings,idempotency_key,recipe):
        try:
            findings=validate_findings(list(findings),public=True)
            if not isinstance(note,str) or len(note.encode())>2048 or not isinstance(candidate_commit,str) or not SHA1_HEX_RE.fullmatch(candidate_commit):raise ValueError()
        except (ValueError,TypeError,UnicodeError):raise Rejected(400,'invalid_request') from None
        intent=sha256_canonical({'action':'repair','project_alias':project_alias,'task_id':str(task_id),'expected_revision':expected_revision,
            'candidate_commit':candidate_commit,'agent_profile':agent_profile,'note':note,'findings':findings})
        with self._transaction() as cursor:
            task,old=self._control_task(cursor,project_alias,task_id,expected_revision,idempotency_key,'repair',intent)
            if old:return old
            if task.get("workflow_required"):raise Rejected(409,"not_reviewable")
            if task['status']!='completed' or task['mode']!='implement':raise Rejected(409,'not_reviewable')
            cursor.execute("SELECT * FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND candidate_commit=%s ORDER BY review_no DESC LIMIT 1 FOR UPDATE",
                (self._owner,project_alias,task_id,candidate_commit))
            review=cursor.fetchone()
            if not review or not review['verification'].get('candidate_workspace_digest'):
                if not findings and not note.strip():raise Rejected(400,'invalid_request')
                if not findings:findings=validate_findings([{'finding_id':'owner-correction','category':'source','summary':note}],public=True)
                cursor.execute("SELECT 1 FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND phase='source' AND status IN('pending','checking')",(self._owner,project_alias,task_id))
                if cursor.fetchone():raise Rejected(409,'reconcile_required')
                cursor.execute('SELECT * FROM forge_agent.coding_attempts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s',
                    (self._owner,project_alias,task_id,task['latest_attempt_id']))
                source=cursor.fetchone()
                if not source or source['phase']!='stopped' or source['reservation_active'] or source['lease_expires_at'] or not source['result_sha256']:
                    raise Rejected(409,'reconcile_required')
                cursor.execute('SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s ORDER BY created_at DESC LIMIT 1',
                    (self._owner,project_alias,task_id,task['latest_attempt_id']))
                reconciled=cursor.fetchone()
                if not reconciled:raise Rejected(409,'reconcile_required')
                cursor.execute('SELECT * FROM forge_agent.coding_publications WHERE owner_id=%s AND project_alias=%s AND task_id=%s ORDER BY created_at DESC LIMIT 1',
                    (self._owner,project_alias,task_id));prior=cursor.fetchone()
                self._ensure_review_policy(cursor,project_alias,task['repair_root_task_id'] or task['id'])
                authorization={'decision':'repair_source','requested_by':self._owner,'source_revision':expected_revision,'candidate_commit':candidate_commit,
                    'agent_profile':agent_profile,'recipe':recipe.to_dict(),'recipe_digest':recipe.digest,'note':note}
                cursor.execute('''INSERT INTO forge_agent.coding_reviews(owner_id,project_alias,task_id,attempt_id,plan_decision_id,reconciliation_receipt_id,
                    base_revision,workspace_digest,parent_result_sha256,source_revision,review_no,phase,candidate_commit,summary,findings,owner_decision,target_publication_id,source_publication_id,round)
                    VALUES(%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,
                    (SELECT coalesce(max(review_no),0)+1 FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s),
                    'source',%s,%s,%s,%s,%s,%s,coalesce((SELECT repair_rounds FROM forge_agent.coding_review_budget WHERE owner_id=%s AND project_alias=%s AND root_task_id=%s),0)) RETURNING review_id''',
                    (self._owner,project_alias,task_id,task['latest_attempt_id'],task['plan_decision_id'],reconciled['receipt_id'],task['base_revision'],reconciled['workspace_digest'],
                     source['result_sha256'],expected_revision,self._owner,project_alias,task_id,candidate_commit,note,Json(findings),Json(authorization),
                     (prior['parent_publication_id'] or prior['publication_id']) if prior else None,prior['publication_id'] if prior else None,
                     self._owner,project_alias,task['repair_root_task_id'] or task['id']))
                source_review_id=cursor.fetchone()['review_id']
                result={'project_alias':project_alias,'task_id':str(task_id),'base_revision':expected_revision,'revision':expected_revision+1,
                        'status':'completed','repair_task_id':None,'review_id':str(source_review_id)}
                self._control_record(cursor,task,expected_revision,idempotency_key,'repair',intent,result)
                return result
            if not findings:findings=[x for x in review['findings'] if x.get('status','open')=='open']
            if not findings:
                if not note.strip():raise Rejected(400,'invalid_request')
                findings=validate_findings([{'finding_id':'owner-correction','category':'source','summary':note}],public=True)
            cursor.execute("UPDATE forge_agent.coding_tasks SET agent_profile=%s,pinned_recipe=%s,recipe_digest=%s WHERE owner_id=%s AND project_alias=%s AND id=%s",
                (agent_profile,Json(recipe.to_dict()),recipe.digest,self._owner,project_alias,task_id))
            cursor.execute("SELECT forge_agent.coding_make_feedback(%s,%s,'repair',%s,%s)",(review['review_id'],task_id,note,Json(findings)))
            fid=next(iter(cursor.fetchone().values()))
            cursor.execute("SELECT forge_agent.coding_schedule_repair(%s,%s)",(review['review_id'],fid))
            child=next(iter(cursor.fetchone().values()))
            # Parent completion and original recipe are immutable history.
            cursor.execute("UPDATE forge_agent.coding_tasks SET agent_profile=%s,pinned_recipe=%s,recipe_digest=%s WHERE owner_id=%s AND project_alias=%s AND id=%s",
                (task['agent_profile'],Json(task['pinned_recipe']),task['recipe_digest'],self._owner,project_alias,task_id))
            result={'project_alias':project_alias,'task_id':str(task_id),'base_revision':expected_revision,'revision':expected_revision+1,'status':'completed','repair_task_id':str(child)}
            self._control_record(cursor,task,expected_revision,idempotency_key,'repair',intent,result)
            return result

    def publication_retry(self,*,project_alias,task_id,expected_revision,publication_id,candidate_commit,note,idempotency_key):
        if not isinstance(publication_id,UUID) or not isinstance(note,str) or len(note.encode())>2048:
            raise Rejected(400,'invalid_request')
        intent=sha256_canonical({'action':'publication_retry','project_alias':project_alias,'task_id':str(task_id),'expected_revision':expected_revision,
            'publication_id':str(publication_id),'candidate_commit':candidate_commit,'note':note})
        with self._transaction() as cursor:
            task,old=self._control_task(cursor,project_alias,task_id,expected_revision,idempotency_key,'publication_retry',intent)
            if old:return old
            if task.get("workflow_required"):raise Rejected(409,"not_reviewable")
            cursor.execute("SELECT * FROM forge_agent.coding_publications WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND publication_id=%s FOR UPDATE",
                (self._owner,project_alias,task_id,publication_id))
            publication=cursor.fetchone()
            if not publication or publication['status']!='failed' or publication['commit_sha']!=candidate_commit or not publication['review_id']:
                raise Rejected(409,'not_reviewable')
            cursor.execute("SELECT * FROM forge_agent.coding_reviews WHERE review_id=%s",(publication['review_id'],));review=cursor.fetchone()
            review_id=self._append_review_retry(cursor,task,review,note)
            result={'project_alias':project_alias,'task_id':str(task_id),'base_revision':expected_revision,'revision':expected_revision+1,
                    'status':task['status'],'publication_id':str(publication_id),'review_id':str(review_id)}
            self._control_record(cursor,task,expected_revision,idempotency_key,'publication_retry',intent,result)
            return result
