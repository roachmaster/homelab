"""PostgreSQL DAO for Forge coding-job durable admission, execution queries, and control."""

from contextlib import closing, contextmanager
from datetime import UTC, datetime
import hashlib
import json
import psycopg2
import re
import unicodedata
from uuid import UUID, uuid4

from psycopg2.extras import Json, RealDictCursor, register_uuid

from work_retry_policy import AGENT_RETRY_LIMIT
from queue_store import Rejected
from work_events import COORDINATOR_ACTOR, insert_event, require_actor, require_cancel_cause
from work_external_policy import POLICY_ID as EXTERNAL_POLICY_ID, MODES as EXTERNAL_MODES, POLICIES as EXTERNAL_POLICIES
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
from work_policy import POLICY_DIGEST, POLICY_ID, NATIVE_POLICIES, native_policy_matches
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
TASK_SCOPES = frozenset({"active", "attention", "history", "all"})
VALID_LIST_FILTERS = VALID_STATUSES | TASK_SCOPES


def _projection_actor(payload, kind):
    """True when the payload carries no actor, or exactly the expected
    ``{kind, id}`` actor. Fix 2 adds the actor to every emitted event
    payload; the public projections validate it like any other field and
    never expose it."""
    if "actor" not in payload:
        return True
    actor = payload["actor"]
    return (isinstance(actor, dict) and set(actor) == {"kind", "id"}
            and actor["kind"] == kind
            and isinstance(actor["id"], str) and bool(actor["id"])
            and len(actor["id"]) <= 128)


def _public_progress(payload):
    """Project only explicit public reports, never arbitrary event payloads."""
    if (not isinstance(payload, dict)
            or set(payload) - {"actor"} != {
                "attempt_id", "progress_sequence", "stage", "summary"
            }
            or not _projection_actor(payload, "worker")):
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
def _public_external_progress(payload, binding):
    if (not isinstance(payload, dict)
            or set(payload) - {"actor"} != {"provider", "assignment_id", "generation", "summary"}
            or not _projection_actor(payload, "agent") or not isinstance(binding, dict)):
        return None
    try:
        identity = str(UUID(payload["assignment_id"]))
        summary = payload["summary"]
        generation = payload["generation"]
        valid = (payload["provider"] == "muse-db" and payload["assignment_id"] == identity == str(binding["assignment_id"])
                 and type(generation) is int and 1 <= generation <= binding["generation"]
                 and isinstance(summary, str) and bool(summary.strip()) and len(summary.encode("utf-8")) <= 4096
                 and all(unicodedata.category(c) != "Cc" or c in "\n\t" for c in summary))
    except (ValueError, TypeError, KeyError, UnicodeError):
        return None
    return ({"provider": payload["provider"], "assignment_id": payload["assignment_id"],
             "generation": generation, "summary": summary} if valid else None)


def _public_workflow_progress(payload, workflow, related_allowed=False):
    if (not isinstance(payload, dict)
            or not ({"workflow_id", "phase", "summary"} <= payload.keys() - {"actor"} <= {"workflow_id", "phase", "summary", "related_task_id"})
            or not _projection_actor(payload, "coordinator") or not isinstance(workflow, dict)):
        return None
    try:
        identity = str(UUID(payload["workflow_id"]))
        summary = payload["summary"]
        related = payload.get("related_task_id")
        related_valid = related is None or (related_allowed and isinstance(related,str) and str(UUID(related)) == related)
        valid = (related_valid and payload["workflow_id"] == identity == workflow["workflow_id"]
                 and payload["phase"] in {"plan", "plan_review", "plan_revision", "implement", "checks", "code_review", "repair", "publishing", "hosted", "merging", "completed", "cancelled", "escalated"}
                 and isinstance(summary, str) and bool(summary.strip()) and len(summary.encode("utf-8")) <= 4096
                 and all(unicodedata.category(c) != "Cc" or c in "\n\t" for c in summary))
    except (ValueError, TypeError, KeyError, UnicodeError):
        return None
    if not valid:
        return None
    projected = {"workflow_id": payload["workflow_id"], "phase": payload["phase"], "summary": summary}
    if related is not None:
        projected["related_task_id"] = related
    return projected


NON_RESUMABLE_STATUSES = frozenset({"queued", "running", "cancel_requested", "completed"})
MODE_ORDER = ("plan", "review", "implement")


def _timestamp(value):
    formatted = value.astimezone(UTC).isoformat().replace("+00:00", "Z")
    if len(formatted.encode("utf-8")) > 80:
        raise ValueError("Formatted timestamp exceeds wire bound")
    return formatted


def _attempt_summary(value):
    from work_quota_diagnostics import parse_quota_summary
    diagnostic=parse_quota_summary(value)
    if diagnostic is None:return value
    return ('Provider quota exhausted; waiting for allowance.' if diagnostic['retry_after_seconds']is None
            else f"Provider quota exhausted; reset reported in {diagnostic['retry_after_seconds']} seconds.")


def _summary(row):
    item = {
        "task_id": str(row["id"]),
        "correlation_id": str(row["correlation_id"]),
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

    def __init__(self, owner, allowed_projects, connect_factory, *, max_pending=DEFAULT_MAX_PENDING,repair_policy=None, external_port=None, delivery_enabled=False,retry_events=None):
        self.retry_events=retry_events
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
        if external_port is not None and (
            not isinstance(external_port.profiles, frozenset)
            or not all(callable(getattr(external_port, name, None)) for name in ("enqueue", "catalogue", "artifact_by_task", "reconcile"))
        ):
            raise TypeError("External work requires an explicit scoped port")
        self._external = external_port
        if type(delivery_enabled) is not bool:raise ValueError("Invalid delivery selection")
        self._delivery_enabled=delivery_enabled

    @property
    def owner(self):
        return self._owner

    @property
    def allowed_projects(self):
        return self._allowed_projects

    @property
    def max_pending(self):
        return self._max_pending

    def _delivery_projection(self, cursor, project_alias, task_id=None):
        if not self._delivery_enabled:
            return None
        function='delivery_retry_projection' if self.retry_events is not None else 'delivery_projection'
        cursor.execute('SELECT forge_agent.'+function+'(%s,%s) AS value',(project_alias,task_id))
        return cursor.fetchone()['value']

    def _delivery_head(self, cursor, project_alias):
        if not self._delivery_enabled:
            return None
        cursor.execute('SELECT forge_agent.delivery_project_head(%s)',(project_alias,))
        value=cursor.fetchone()['delivery_project_head']
        return value.get('commit_sha') if value else None

    def _external_workflow_for(self, project_alias):
        workflow = getattr(self._external,"workflow",None)
        return workflow if workflow is not None and workflow.manages(project_alias) else None

    def _require_project(self, project_alias):
        if project_alias not in self._allowed_projects:
            raise Rejected(403, "denied")

    def maintain_external(self):
        if self._external is not None:
            with self._transaction() as cursor:
                self._external.reconcile(cursor)

    @contextmanager
    def _transaction(self, *, read_only=False, repeatable_read=False):
        try:
            with closing(self._connect_factory()) as connection:
                with connection:
                    with connection.cursor(cursor_factory=RealDictCursor) as cursor:
                        if read_only:
                            cursor.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
                        elif repeatable_read:
                            cursor.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
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
            external = self._external.catalogue(cursor) if self._external is not None else ()

        if not rows:
            worker_status = "unknown"
            observed_at = None
        else:
            observed_at = _timestamp(rows[0]["observed_at"])
            worker_status = "ready" if any(row["is_fresh"] for row in rows) else "offline"

        advertised_revisions = {}
        advertised_profile_modes = {}
        for entry in external:
            profile = config.profiles.get(entry.profile_alias)
            if (entry.project_alias not in config.allowed_projects or profile is None
                    or not profile.is_external or not profile.execution_enabled):
                continue
            if entry.observed_at is not None and (observed_at is None or datetime.fromisoformat(entry.observed_at.replace("Z", "+00:00")) > datetime.fromisoformat(observed_at.replace("Z", "+00:00"))):
                observed_at = entry.observed_at
            if entry.is_fresh:
                worker_status = "ready"
                advertised_profile_modes.setdefault(entry.profile_alias, set()).update(EXTERNAL_POLICIES[profile.resource_profile][1] & config.allowed_modes)
            elif worker_status == "unknown":
                worker_status = "offline"
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

            if row["policy_digest"] not in {entry[0] for entry in NATIVE_POLICIES.values()}:
                continue
            raw_profiles = row["profiles"] if isinstance(row["profiles"], list) else []
            for entry in raw_profiles:
                if not isinstance(entry, dict):
                    continue
                alias = entry.get("alias")
                profile = config.profiles.get(alias)
                if (profile is None or not profile.execution_enabled
                        or not native_policy_matches(profile.resource_profile, profile.backend, row["policy_digest"])):
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
                "external_workflow":getattr(self._external,"workflow",None).project_policy(alias) if getattr(self._external,"workflow",None) is not None else None,
            }
            for alias in sorted(config.allowed_projects)[:MAX_PROJECTS]
        ]
        if self._delivery_enabled:
            with self._transaction(read_only=True) as cursor:
                for project in projects:
                    head=self._delivery_head(cursor,project['alias'])
                    if head is not None:project['base_revision']=head
                    delivery=self._delivery_projection(cursor,project['alias'])
                    if delivery is not None:project['delivery']=delivery
        profiles = []
        for alias in sorted(alias for alias, profile in config.profiles.items() if profile.execution_enabled)[:MAX_PROFILES]:
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
                "branch": profile.branch,
                **({"transport": "external_db"} if profile.is_external else {}),
            })
        agent_branches = self._agent_branch_states(config)
        return {
            "projects": projects,
            "profiles": profiles,
            "worker_status": worker_status,
            "observed_at": observed_at,
            "agent_branches": agent_branches,
        }

    def _branch_row_to_dto(self, row):
        return {
            "project_alias": row["project_alias"],
            "agent_profile": row["agent_profile"],
            "branch_name": row["branch_name"],
            "branch_scope": row["branch_scope"],
            "head_commit_sha": row.get("head_commit_sha"),
            "fetched_main_sha": row.get("fetched_main_sha"),
            "fetched_main_at": _timestamp(row["fetched_main_at"]) if row.get("fetched_main_at") is not None else None,
            "sync_status": row.get("sync_status") or "unknown",
            "conflict_paths": list(row.get("conflict_paths") or []),
            "cache_status": row.get("cache_status") or "unknown",
            "cache_updated_at": _timestamp(row["cache_updated_at"]) if row.get("cache_updated_at") is not None else None,
            "last_task_id": str(row["last_task_id"]) if row.get("last_task_id") is not None else None,
            "last_correlation_id": str(row["last_correlation_id"]) if row.get("last_correlation_id") is not None else None,
            "reservation_status": row.get("reservation_status") or "idle",
            "active_scope_kind": row.get("active_scope_kind"),
            "active_scope_id": str(row["active_scope_id"]) if row.get("active_scope_id") is not None else None,
            "active_root_task_id": str(row["active_root_task_id"]) if row.get("active_root_task_id") is not None else None,
            "active_task_id": str(row["active_task_id"]) if row.get("active_task_id") is not None else None,
            "active_pr_url": row.get("active_pr_url"),
            "reservation_epoch": int(row.get("reservation_epoch") or 0),
            "reservation_blocker": row.get("reservation_blocker"),
        }

    def _agent_branch_states(self, config):
        from work_agent_branch import branch_scope_for_branch
        live = {}
        with self._transaction(read_only=True) as cursor:
            cursor.execute(
                "SELECT to_regclass('forge_agent.agent_branch_state') IS NOT NULL "
                "AND has_table_privilege(current_user, 'forge_agent.agent_branch_state', 'SELECT') AS has_table"
            )
            if cursor.fetchone()["has_table"]:
                cursor.execute("""
                    SELECT project_alias, agent_profile, branch_name, branch_scope,
                           head_commit_sha, fetched_main_sha, fetched_main_at,
                           sync_status, conflict_paths, cache_status, cache_updated_at,
                           last_task_id, last_correlation_id,
                           reservation_status, active_scope_kind, active_scope_id,
                           active_root_task_id, active_task_id, active_pr_url,
                           reservation_epoch, reservation_blocker
                    FROM forge_agent.agent_branch_state
                    WHERE owner_id = %s AND project_alias = ANY(%s)
                    ORDER BY project_alias ASC, agent_profile ASC
                """, (self._owner, sorted(config.allowed_projects)))
                for row in cursor.fetchall():
                    live[(row["project_alias"], row["agent_profile"])] = self._branch_row_to_dto(row)
        result = []
        for project_alias in sorted(config.allowed_projects)[:MAX_PROJECTS]:
            for alias in sorted(config.profiles)[:MAX_PROFILES]:
                existing = live.get((project_alias, alias))
                if existing is not None:
                    result.append(existing)
                else:
                    profile = config.profiles[alias]
                    result.append({
                        "project_alias": project_alias,
                        "agent_profile": alias,
                        "branch_name": profile.branch,
                        "branch_scope": branch_scope_for_branch(profile.branch),
                        "head_commit_sha": None,
                        "fetched_main_sha": None,
                        "fetched_main_at": None,
                        "sync_status": "unknown",
                        "conflict_paths": [],
                        "cache_status": "unknown",
                        "cache_updated_at": None,
                        "last_task_id": None,
                        "last_correlation_id": None,
                        "reservation_status": "idle",
                        "active_scope_kind": None,
                        "active_scope_id": None,
                        "active_root_task_id": None,
                        "active_task_id": None,
                        "active_pr_url": None,
                        "reservation_epoch": 0,
                        "reservation_blocker": None,
                    })
                if len(result) >= 128:
                    return result
        return result

    def submit(self, *, project_alias, base_revision, mode, agent_profile,
               prompt, acceptance_criteria, idempotency_key, resolve_recipe, auto_repair=False,
               workflow_policy_digest=None,resolve_workflow=lambda:None,delivery_admission=None,actor=None):
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

            if delivery_admission is not None:
                if (not self._delivery_enabled or not isinstance(delivery_admission,dict)
                        or set(delivery_admission)!={'operation_id','generation','lease_token'}):
                    raise Rejected(403,'denied')
                cursor.execute('SELECT forge_agent.delivery_repair_admit(%s,%s,%s,%s)',
                               (UUID(delivery_admission['operation_id']),delivery_admission['generation'],
                                UUID(delivery_admission['lease_token']),intent_digest))
            trusted_main=self._delivery_head(cursor,project_alias)
            if trusted_main is not None and trusted_main!=base_revision:raise Rejected(409,"result_mismatch")
            workflow=resolve_workflow()
            if workflow is not None:
                cursor.execute("""SELECT a.finish_receipt->'receipt'->>'merge_commit' AS head FROM forge_agent.coding_workflow_actions a
                 JOIN forge_agent.coding_workflows w USING(workflow_id) WHERE a.owner_id=%s AND a.project_alias=%s AND a.kind='merge' AND a.status='completed'
                 AND a.finish_receipt->>'status'='merged' AND a.finish_receipt->'receipt'->>'merge_commit'=a.intent->>'merge_commit' AND w.phase='completed'
                 ORDER BY a.updated_at DESC,a.action_id DESC LIMIT 1""",(self._owner,project_alias))
                merged=cursor.fetchone()
                trusted_head=trusted_main or (merged["head"] if merged else None)
                if trusted_head is not None and trusted_head!=base_revision:raise Rejected(409,'result_mismatch')
                auto_repair=True
            recipe = resolve_recipe()
            if (not isinstance(recipe, PinnedRecipe)
                    or recipe.mode != mode
                    or recipe.generation_token_limit is not None):
                raise ValueError("Recipe resolver returned an invalid pinned recipe")

            task_id = uuid4()
            actor = require_actor(actor, self._owner)
            receipt = {
                "task_id": str(task_id),
                "project_alias": project_alias,
                "revision": 1,
                "status": "queued",
                "recipe_digest": recipe.digest,
                "actor": actor,
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
            insert_event(cursor, self._owner, project_alias, task_id, "queued",
                         receipt, actor=actor, first=True)
            if recipe.resource_profile in EXTERNAL_POLICIES:
                if self._external is None or agent_profile not in self._external.profiles:
                    raise Rejected(503, "unavailable")
                self._external.enqueue(cursor, project_alias, task_id)
            if delivery_admission is not None:
                cursor.execute('SELECT forge_agent.delivery_repair_bind(%s,%s,%s,%s,%s)',
                               (UUID(delivery_admission['operation_id']),delivery_admission['generation'],
                                UUID(delivery_admission['lease_token']),task_id,Json(receipt)))
            cursor.execute("SELECT pg_catalog.pg_notify(%s, '')", (NOTIFY_CHANNEL,))
            return receipt

    def get(self, *, project_alias, task_id):
        self._require_project(project_alias)
        if not isinstance(task_id, UUID):
            raise Rejected(400, "invalid_request")
        # External artifact authorization retains a scoped FOR SHARE fence.
        # Allow that lock while retaining one coherent task/artifact snapshot;
        # this read path performs no data mutations. Native-only configs stay RO.
        read_mode = {"repeatable_read": True} if self._external is not None else {"read_only": True}
        with self._transaction(**read_mode) as cursor:
            cursor.execute("""
                SELECT id, correlation_id, project_alias, base_revision, mode, agent_profile,
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
                        "summary": _attempt_summary(attempt["summary"]),
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
            event=self._retry_projection(cursor,row)
            if event is not None:detail["retry_event"]=event
            review=self._review_projection(cursor,row)
            if review is not None:
                detail["review"]=review
            if "title" not in detail:
                detail["title"] = prepare_title(row["prompt"])
            external_workflow = self._external_workflow_for(project_alias)
            if external_workflow is not None:
                projection = external_workflow.project(cursor,project_alias,task_id)
                if projection is not None:
                    detail["workflow_required"] = True
                    detail["workflow"] = projection
            if self._external is not None and row["agent_profile"] in self._external.profiles:
                artifact = self._external.artifact_by_task(cursor, project_alias, task_id)
                if artifact is not None:
                    detail["external_result"] = artifact
            delivery=self._delivery_projection(cursor,project_alias,task_id)
            if delivery is not None:detail["delivery"]=delivery
            correlation_cicd = self._correlation_cicd_projection(cursor, row, delivery=delivery)
            if correlation_cicd is not None:
                detail["correlation_cicd"] = correlation_cicd
            return detail

    def _correlation_cicd_projection(self, cursor, task, *, delivery=None):
        from work_agent_branch import branch_scope_for_branch, default_agent_branch
        from work_pipeline_projection import project_correlation_cicd
        correlation_id = task.get("correlation_id")
        if correlation_id is None:
            return None
        project_alias = task["project_alias"]
        corr_str = str(correlation_id)

        def _can_select(relation_name):
            cursor.execute(
                "SELECT to_regclass(%s) IS NOT NULL AND has_table_privilege(current_user, %s, 'SELECT') AS ok",
                (relation_name, relation_name),
            )
            return bool(cursor.fetchone()["ok"])

        # 1. Branch state and whether this task is currently waiting on another workflow's reservation.
        branch_state = None
        waiting_on_branch_owner = False
        if _can_select("forge_agent.agent_branch_state"):
            cursor.execute("""
                SELECT project_alias, agent_profile, branch_name, branch_scope,
                       head_commit_sha, fetched_main_sha, fetched_main_at,
                       sync_status, conflict_paths, cache_status, cache_updated_at,
                       last_task_id, last_correlation_id,
                       reservation_status, active_scope_kind, active_scope_id,
                       active_root_task_id, active_task_id, active_pr_url,
                       reservation_epoch, reservation_blocker
                FROM forge_agent.agent_branch_state
                WHERE owner_id = %s AND project_alias = %s AND agent_profile = %s
            """, (self._owner, project_alias, task["agent_profile"]))
            brow = cursor.fetchone()
            if brow is not None:
                branch_state = self._branch_row_to_dto(brow)
                if brow["reservation_status"] == "active" and task.get("status") not in {"completed", "cancelled", "failed"}:
                    task_scope_id = (
                        str(task["workflow_id"])
                        if task.get("workflow_id") is not None
                        else str(task.get("repair_root_task_id") or task["id"])
                    )
                    if (str(brow["active_scope_id"]) != task_scope_id
                            and str(brow["active_task_id"]) != str(task["id"])
                            and str(brow.get("active_root_task_id") or "") != task_scope_id):
                        waiting_on_branch_owner = True
        if branch_state is None:
            default_b = default_agent_branch(task["agent_profile"])
            branch_state = {
                "project_alias": project_alias,
                "agent_profile": task["agent_profile"],
                "branch_name": default_b,
                "branch_scope": branch_scope_for_branch(default_b),
                "head_commit_sha": None,
                "fetched_main_sha": None,
                "fetched_main_at": None,
                "sync_status": "unknown",
                "conflict_paths": [],
                "cache_status": "unknown",
                "cache_updated_at": None,
                "last_task_id": None,
                "last_correlation_id": None,
                "reservation_status": "idle",
                "active_scope_kind": None,
                "active_scope_id": None,
                "active_root_task_id": None,
                "active_task_id": None,
                "active_pr_url": None,
                "reservation_epoch": 0,
                "reservation_blocker": None,
            }

        # 2. Collect linked task IDs, correlation IDs & candidate commits across this workflow / repair family.
        root_id = task.get("repair_root_task_id") or task["id"]
        if task.get("workflow_id") is not None:
            cursor.execute("""
                SELECT id, correlation_id FROM forge_agent.coding_tasks
                WHERE owner_id = %s AND project_alias = %s
                  AND (correlation_id = %s OR workflow_id = %s OR id = %s OR repair_root_task_id = %s)
                LIMIT 64
            """, (self._owner, project_alias, correlation_id, task["workflow_id"], root_id, root_id))
        else:
            cursor.execute("""
                SELECT id, correlation_id FROM forge_agent.coding_tasks
                WHERE owner_id = %s AND project_alias = %s
                  AND (correlation_id = %s OR id = %s OR repair_root_task_id = %s)
                LIMIT 64
            """, (self._owner, project_alias, correlation_id, root_id, root_id))
        linked_rows = cursor.fetchall()
        linked_ids = [r["id"] for r in linked_rows]
        linked_corrs = list({r["correlation_id"] for r in linked_rows} | {correlation_id})
        linked_corr_strs = [str(c) for c in linked_corrs]

        candidate_commits = []
        hosted_observations = []
        if linked_ids:
            cursor.execute("""
                SELECT DISTINCT candidate_commit FROM forge_agent.coding_reviews
                WHERE owner_id = %s AND project_alias = %s AND task_id = ANY(%s)
                  AND candidate_commit IS NOT NULL
            """, (self._owner, project_alias, linked_ids))
            candidate_commits = [r["candidate_commit"] for r in cursor.fetchall()]
            cursor.execute("""
                SELECT h.candidate_commit, h.checks
                FROM forge_agent.coding_hosted_checks h
                JOIN forge_agent.coding_publications p ON p.publication_id = h.publication_id
                WHERE h.owner_id = %s AND h.project_alias = %s AND p.task_id = ANY(%s)
                ORDER BY h.observed_at ASC, h.observation_id ASC
                LIMIT 32
            """, (self._owner, project_alias, linked_ids))
            hosted_observations = [dict(r) for r in cursor.fetchall()]

        # 3. Query immutable stage_results for this correlation / workflow family.
        stage_results = []
        if _can_select("forge_agent.jenkins_stage_results"):
            cursor.execute("""
                SELECT action_id, stage_index, correlation_id, task_id, job_name, build_number,
                       commit_sha, branch_name, branch_scope, pipeline_scope,
                       logical_stage_id, phase_id, phase_kind, label, status,
                       returncode, duration_ms, tests_passed, tests_failed, tests_skipped,
                       test_summary, error_excerpt, log_sha256, log_byte_count, log_chunk_count,
                       created_at
                FROM forge_agent.jenkins_stage_results
                WHERE owner_id = %s AND project_alias = %s AND correlation_id = ANY(%s)
                ORDER BY created_at ASC, action_id ASC, stage_index ASC
                LIMIT 512
            """, (self._owner, project_alias, linked_corrs))
            for r in cursor.fetchall():
                item = dict(r)
                item["action_id"] = str(r["action_id"])
                item["correlation_id"] = str(r["correlation_id"])
                item["task_id"] = str(r["task_id"]) if r["task_id"] is not None else None
                item["created_at"] = _timestamp(r["created_at"]) if r["created_at"] is not None else None
                stage_results.append(item)

        # 4. Query jenkins_events scoped by correlation_id OR linked candidate commits.
        jenkins_events = []
        if _can_select("forge_agent.jenkins_events"):
            cursor.execute("""
                SELECT EXISTS(
                    SELECT 1 FROM pg_catalog.pg_attribute
                    WHERE attrelid = 'forge_agent.jenkins_events'::regclass
                      AND attname = 'action_id' AND NOT attisdropped
                ) AS has_m39_cols
            """)
            has_m39_cols = cursor.fetchone()["has_m39_cols"]
            extra_cols = ", action_id, branch_scope" if has_m39_cols else ""
            if candidate_commits:
                cursor.execute(f"""
                    SELECT event_kind, job_name, build_number, queue_id, commit_sha, branch,
                           build_result, stage_name, stage_status, occurred_at, sequence,
                           correlation_id {extra_cols}
                    FROM forge_agent.jenkins_events
                    WHERE owner_id = %s AND project_alias = %s
                      AND (correlation_id = ANY(%s) OR commit_sha = ANY(%s))
                    ORDER BY sequence ASC
                    LIMIT 512
                """, (self._owner, project_alias, linked_corr_strs, candidate_commits))
            else:
                cursor.execute(f"""
                    SELECT event_kind, job_name, build_number, queue_id, commit_sha, branch,
                           build_result, stage_name, stage_status, occurred_at, sequence,
                           correlation_id {extra_cols}
                    FROM forge_agent.jenkins_events
                    WHERE owner_id = %s AND project_alias = %s AND correlation_id = ANY(%s)
                    ORDER BY sequence ASC
                    LIMIT 512
                """, (self._owner, project_alias, linked_corr_strs))
            for r in cursor.fetchall():
                item = dict(r)
                if item.get("occurred_at") is not None:
                    item["occurred_at"] = _timestamp(item["occurred_at"])
                if item.get("action_id") is not None:
                    item["action_id"] = str(item["action_id"])
                jenkins_events.append(item)

        # 5. Derive post-merge main verification & release state from the authorized delivery projection.
        post_merge_runs = []
        release_status = None
        if isinstance(delivery, dict):
            verification = delivery.get("verification")
            if isinstance(verification, dict) and delivery.get("verification_id"):
                post_merge_runs.append({
                    "post_merge_id": str(delivery["verification_id"]),
                    "merge_commit": delivery.get("commit_sha"),
                    "status": verification.get("status"),
                    "receipt": {"build_number": verification.get("build_number")} if verification.get("build_number") is not None else {},
                    "summary": verification.get("summary") or "",
                    "created_at": verification.get("updated_at"),
                    "updated_at": verification.get("updated_at"),
                })
            release = delivery.get("release")
            if isinstance(release, dict) and isinstance(release.get("status"), str):
                release_status = release["status"]

        return project_correlation_cicd(
            correlation_id=corr_str,
            branch_state=branch_state,
            waiting_on_branch_owner=waiting_on_branch_owner,
            stage_results=stage_results,
            jenkins_events=jenkins_events,
            hosted_observations=hosted_observations,
            post_merge_runs=post_merge_runs,
            release_status=release_status,
        )

    def cicd_stage_log(self, *, project_alias, task_id, correlation_id, action_id,
                       stage_id, view, cursor_token=None, limit_bytes=32768):
        self._require_project(project_alias)
        if (not isinstance(task_id, UUID) or not isinstance(correlation_id, UUID)
                or not isinstance(action_id, UUID)
                or not isinstance(stage_id, str) or not re.fullmatch(r"[A-Za-z0-9_.-]{1,64}", stage_id)
                or view not in {"errors", "tests", "console"}
                or type(limit_bytes) is not int or not 1 <= limit_bytes <= 32768):
            raise Rejected(400, "invalid_request")
        req_digest = None
        offset = 0
        if cursor_token is not None:
            if not isinstance(cursor_token, str) or not re.fullmatch(r"[0-9a-f]{64}\.(0|[1-9][0-9]{0,7})", cursor_token):
                raise Rejected(400, "invalid_request")
            req_digest, raw_off = cursor_token.split(".", 1)
            offset = int(raw_off)

        with self._transaction(read_only=True) as cursor:
            cursor.execute("""
                SELECT id, correlation_id, workflow_id, repair_root_task_id
                FROM forge_agent.coding_tasks
                WHERE owner_id = %s AND project_alias = %s AND id = %s
            """, (self._owner, project_alias, task_id))
            task_row = cursor.fetchone()
            if task_row is None:
                raise Rejected(404, "not_found")
            if str(task_row["correlation_id"]) != str(correlation_id):
                raise Rejected(403, "denied")
            root_id = task_row["repair_root_task_id"] or task_row["id"]
            if task_row["workflow_id"] is not None:
                cursor.execute("""
                    SELECT correlation_id FROM forge_agent.coding_tasks
                    WHERE owner_id = %s AND project_alias = %s
                      AND (correlation_id = %s OR workflow_id = %s OR id = %s OR repair_root_task_id = %s)
                    LIMIT 64
                """, (self._owner, project_alias, correlation_id, task_row["workflow_id"], root_id, root_id))
            else:
                cursor.execute("""
                    SELECT correlation_id FROM forge_agent.coding_tasks
                    WHERE owner_id = %s AND project_alias = %s
                      AND (correlation_id = %s OR id = %s OR repair_root_task_id = %s)
                    LIMIT 64
                """, (self._owner, project_alias, correlation_id, root_id, root_id))
            allowed_corrs = list({r["correlation_id"] for r in cursor.fetchall()} | {correlation_id})

            cursor.execute("SELECT to_regclass('forge_agent.jenkins_stage_results') IS NOT NULL AS has_table")
            if not cursor.fetchone()["has_table"]:
                raise Rejected(404, "not_found")

            cursor.execute("""
                SELECT stage_index, phase_id, logical_stage_id, label, status, returncode,
                       tests_passed, tests_failed, tests_skipped, test_summary,
                       error_excerpt, log_sha256, log_byte_count, log_chunk_count
                FROM forge_agent.jenkins_stage_results
                WHERE owner_id = %s AND project_alias = %s
                  AND action_id = %s AND correlation_id = ANY(%s)
                  AND (logical_stage_id = %s OR phase_id = %s)
                ORDER BY stage_index ASC
            """, (self._owner, project_alias, action_id, allowed_corrs, stage_id, stage_id))
            stage_rows = cursor.fetchall()
            if not stage_rows:
                raise Rejected(404, "not_found")

            stage_indices = [int(r["stage_index"]) for r in stage_rows]
            t_passed_vals = [r["tests_passed"] for r in stage_rows if type(r["tests_passed"]) is int]
            t_failed_vals = [r["tests_failed"] for r in stage_rows if type(r["tests_failed"]) is int]
            t_skipped_vals = [r["tests_skipped"] for r in stage_rows if type(r["tests_skipped"]) is int]
            tests_passed = sum(t_passed_vals) if t_passed_vals else None
            tests_failed = sum(t_failed_vals) if t_failed_vals else None
            tests_skipped = sum(t_skipped_vals) if t_skipped_vals else None
            test_cases = []
            for r in stage_rows:
                ts = r.get("test_summary")
                if isinstance(ts, dict):
                    for case in ts.get("cases") or ():
                        if isinstance(case, dict) and isinstance(case.get("name"), str) and len(test_cases) < 64:
                            test_cases.append({"name": case["name"][:160], "status": str(case.get("status") or "unknown")})
            excerpts = [r["error_excerpt"] for r in stage_rows if isinstance(r.get("error_excerpt"), str) and r["error_excerpt"]]
            combined_excerpt = "\n\n".join(excerpts) if excerpts else None
            if combined_excerpt is not None and len(combined_excerpt.encode("utf-8")) > 4096:
                combined_excerpt = combined_excerpt.encode("utf-8")[:4096].decode("utf-8", errors="ignore").rstrip()

            if view == "errors":
                full_bytes = (combined_excerpt or "").encode("utf-8")
            elif view == "tests":
                lines = []
                if tests_passed is not None or tests_failed is not None or tests_skipped is not None:
                    lines.append(
                        f"Tests: {tests_passed or 0} passed, {tests_failed or 0} failed, {tests_skipped or 0} skipped"
                    )
                for case in test_cases:
                    lines.append(f"[{case['status']}] {case['name']}")
                full_bytes = "\n".join(lines).encode("utf-8")
            else:
                cursor.execute("""
                    SELECT stage_index, chunk_index, chunk_bytes
                    FROM forge_agent.jenkins_stage_log_chunks
                    WHERE owner_id = %s AND project_alias = %s
                      AND action_id = %s AND stage_index = ANY(%s)
                    ORDER BY stage_index ASC, chunk_index ASC
                """, (self._owner, project_alias, action_id, stage_indices))
                parts = [bytes(r["chunk_bytes"]) for r in cursor.fetchall()]
                full_bytes = b"".join(parts)

        digest = hashlib.sha256(full_bytes).hexdigest()
        total_bytes = len(full_bytes)
        if req_digest is not None and req_digest != digest:
            raise Rejected(409, "result_mismatch")
        if offset > total_bytes:
            raise Rejected(400, "invalid_request")
        raw_end = min(total_bytes, offset + limit_bytes)
        if raw_end < total_bytes:
            while raw_end > offset and (full_bytes[raw_end] & 0xC0) == 0x80:
                raw_end -= 1
        page_bytes = full_bytes[offset:raw_end]
        end_offset = offset + len(page_bytes)
        content = page_bytes.decode("utf-8", errors="replace")
        next_cursor = f"{digest}.{end_offset}" if end_offset < total_bytes else None
        if offset > 0:
            prev_offset = max(0, offset - limit_bytes)
            while prev_offset > 0 and prev_offset < total_bytes and (full_bytes[prev_offset] & 0xC0) == 0x80:
                prev_offset -= 1
            prev_cursor = f"{digest}.{prev_offset}"
        else:
            prev_cursor = None
        return {
            "project_alias": project_alias,
            "task_id": str(task_id),
            "correlation_id": str(correlation_id),
            "action_id": str(action_id),
            "stage_id": stage_id,
            "view": view,
            "content": content,
            "truncated": end_offset < total_bytes,
            "total_bytes": total_bytes,
            "byte_offset": offset,
            "next_cursor": next_cursor,
            "prev_cursor": prev_cursor,
            "tests_passed": tests_passed,
            "tests_failed": tests_failed,
            "tests_skipped": tests_skipped,
            "test_cases": test_cases,
            "error_excerpt": combined_excerpt,
        }

    def list_tasks(self, *, project_alias, status_filter=None, limit=20, cursor_task_id=None, delivery_visible=True):
        self._require_project(project_alias)
        if status_filter is not None and (not isinstance(status_filter, str) or status_filter not in VALID_LIST_FILTERS):
            raise Rejected(400, "invalid_request")
        if type(limit) is not int or not 1 <= limit <= 50:
            raise Rejected(400, "invalid_request")
        if cursor_task_id is not None and not isinstance(cursor_task_id, UUID):
            raise Rejected(400, "invalid_request")
        if status_filter in TASK_SCOPES:
            return self._list_task_scope(project_alias, status_filter, limit, cursor_task_id, delivery_visible)

        mode = {"repeatable_read": True} if self._external_workflow_for(project_alias) is not None else {"read_only": True}
        external_filter = "AND NOT forge_external_workflow_api.is_internal(t.owner_id,t.project_alias,t.id)" if self._external_workflow_for(project_alias) is not None else ""
        with self._transaction(**mode) as cursor:
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

            cursor.execute(f"""
                SELECT t.id, t.correlation_id, t.project_alias, t.base_revision, t.mode, t.agent_profile,
                       t.recipe_digest, t.status, t.revision, t.created_at, t.title, t.workflow_id,
                       t.workflow_required, w.phase AS workflow_phase
                FROM forge_agent.coding_tasks t
                LEFT JOIN forge_agent.coding_workflows w
                  ON (w.owner_id, w.project_alias, w.workflow_id) =
                     (t.owner_id, t.project_alias, t.workflow_id)
                WHERE t.owner_id = %s AND t.project_alias = %s
                  {external_filter}
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
            if self.retry_events is not None:
                for row in rows:row["retry_event"]=self._retry_projection(cursor,row)
            if self._delivery_enabled:
                for row in rows:row["delivery"]=self._delivery_projection(cursor,project_alias,row["id"])
            projected = {}
            external_workflow = self._external_workflow_for(project_alias)
            if external_workflow is not None:
                for row in rows:
                    current = external_workflow.project(cursor,project_alias,row["id"])
                    if current is not None:
                        projected[str(row["id"])]=current

        page = rows[:limit]
        summaries = []
        for row in page:
            item = _summary(row)
            if row.get("retry_event") is not None:item["retry_event"]=row["retry_event"]
            if row.get("delivery") is not None:item["delivery"]=row["delivery"]
            if str(row["id"]) in projected:
                item["workflow_required"] = True
                item["workflow_phase"] = projected[str(row["id"])]["phase"]
            summaries.append(item)
        next_cursor = str(page[-1]["id"]) if len(rows) > limit else None
        return {
            "project_alias": project_alias,
            "tasks": summaries,
            "next_cursor": next_cursor,
        }

    def _list_task_scope(self, project_alias, scope, limit, cursor_task_id, delivery_visible=True):
        """Classify complete workflow roots before counting and keyset paging.

        Raw statuses remain evidence. A running workflow can have a stopped
        author while an internal reviewer or repair task performs the next step.
        The independent external provider retains its existing scope guard.
        """
        external = self._external_workflow_for(project_alias)
        external_join = ""
        external_filter = ""
        phase, enabled = "w.phase", "w.enabled"
        activity = ""
        if external is not None:
            external_join = """
                LEFT JOIN forge_external_workflow.tasks x
                  ON (x.owner_id,x.project_alias,x.task_id)=(t.owner_id,t.project_alias,t.id)
                LEFT JOIN forge_external_workflow.workflows ew
                  ON (ew.owner_id,ew.project_alias,ew.workflow_id)=(x.owner_id,x.project_alias,x.workflow_id)
            """
            external_filter = "AND NOT forge_external_workflow_api.is_internal(t.owner_id,t.project_alias,t.id)"
            phase, enabled = "coalesce(w.phase,ew.phase)", "coalesce(w.enabled,ew.enabled)"
            activity = """, coalesce(ew.updated_at,t.updated_at), coalesce((
                SELECT max(member.updated_at) FROM forge_external_workflow.tasks member_link
                JOIN forge_agent.coding_tasks member
                  ON (member.owner_id,member.project_alias,member.id)=
                     (member_link.owner_id,member_link.project_alias,member_link.task_id)
                WHERE (member_link.owner_id,member_link.project_alias,member_link.workflow_id)=
                      (ew.owner_id,ew.project_alias,ew.workflow_id)
            ),t.updated_at)"""
        delivery_function = 'delivery_retry_projection' if self.retry_events is not None else 'delivery_projection'
        delivery = f"forge_agent.{delivery_function}(t.project_alias,t.id)" if self._delivery_enabled and delivery_visible else "NULL::jsonb"
        classified = f"""
            WITH classified AS (
                SELECT t.id,t.correlation_id,t.project_alias,t.base_revision,t.mode,t.agent_profile,t.workflow_id,
                       t.recipe_digest,t.status,t.revision,t.created_at,t.title,
                       (t.workflow_required OR {phase} IS NOT NULL) AS workflow_required,
                       {phase} AS workflow_phase, {enabled} AS workflow_enabled,
                       {delivery} AS delivery,
                       greatest(t.updated_at,coalesce(({delivery}->>'updated_at')::timestamptz,t.updated_at),coalesce(w.updated_at,t.updated_at),coalesce((
                           SELECT max(member.updated_at) FROM forge_agent.coding_tasks member
                           WHERE (member.owner_id,member.project_alias,member.workflow_id)=
                                 (w.owner_id,w.project_alias,w.workflow_id)
                       ),t.updated_at){activity}) AS list_updated_at,
                       CASE WHEN t.status='completed' AND {delivery} IS NOT NULL THEN forge_agent.delivery_scope({delivery})
                            WHEN {phase} IN ('completed','cancelled') THEN 'history'
                            WHEN {phase} IS NOT NULL THEN
                                CASE WHEN {phase}='escalated' OR NOT {enabled} THEN 'attention' ELSE 'active' END
                            WHEN t.workflow_required THEN 'attention'
                            WHEN t.status IN ('completed','cancelled') THEN 'history'
                            WHEN t.status IN ('queued','running','cancel_requested') THEN 'active'
                            ELSE 'attention' END AS task_scope
                FROM forge_agent.coding_tasks t
                LEFT JOIN forge_agent.coding_workflows w
                  ON (w.owner_id,w.project_alias,w.workflow_id)=(t.owner_id,t.project_alias,t.workflow_id)
                {external_join}
                WHERE t.owner_id=%s AND t.project_alias=%s
                  AND (w.workflow_id IS NULL OR t.id=w.root_task_id)
                  {external_filter}
            )
        """
        mode = {"repeatable_read": True} if external is not None else {"read_only": True}
        with self._transaction(**mode) as cursor:
            cursor.execute(classified + """
                SELECT count(*) FILTER(WHERE task_scope='active') AS active,
                       count(*) FILTER(WHERE task_scope='attention') AS attention,
                       count(*) FILTER(WHERE task_scope='history') AS history,count(*) AS "all"
                FROM classified
            """, (self._owner, project_alias))
            counts = dict(cursor.fetchone())
            anchor = None
            if cursor_task_id is not None:
                cursor.execute(classified + "SELECT id,list_updated_at FROM classified WHERE id=%s",
                               (self._owner, project_alias, cursor_task_id))
                anchor = cursor.fetchone()
                if anchor is None:
                    raise Rejected(400, "invalid_request")
            cursor.execute(classified + """
                SELECT * FROM classified WHERE (%s='all' OR task_scope=%s)
                  AND (%s::timestamptz IS NULL OR (list_updated_at,id)<(%s::timestamptz,%s::uuid))
                ORDER BY list_updated_at DESC,id DESC LIMIT %s
            """, (self._owner, project_alias, scope, scope,
                  anchor["list_updated_at"] if anchor else None,
                  anchor["list_updated_at"] if anchor else None, cursor_task_id, limit + 1))
            rows = cursor.fetchall()
            if self.retry_events is not None:
                for row in rows:row["retry_event"]=self._retry_projection(cursor,row)
            page = rows[:limit]
            summaries = []
            for row in page:
                item = _summary(row)
                if row.get("retry_event") is not None:item["retry_event"]=row["retry_event"]
                if row.get("delivery") is not None:item["delivery"]=row["delivery"]
                item["updated_at"] = _timestamp(row["list_updated_at"])
                if row["workflow_enabled"] is not None:
                    item["workflow_enabled"] = row["workflow_enabled"]
                if external is not None:
                    projection = external.project(cursor, project_alias, row["id"])
                    if projection is not None:
                        item.update(workflow_required=True, workflow_phase=projection["phase"])
                summaries.append(item)
        return {"project_alias": project_alias, "tasks": summaries,
                "next_cursor": str(page[-1]["id"]) if len(rows) > limit else None,
                "scope_counts": counts}

    def events(self, *, project_alias, task_id, after_sequence=0, limit=50):
        self._require_project(project_alias)
        if (not isinstance(task_id, UUID)
                or type(after_sequence) is not int or not 0 <= after_sequence <= MAX_REVISION
                or type(limit) is not int or not 1 <= limit <= 100):
            raise Rejected(400, "invalid_request")

        mode = {"repeatable_read": True} if self._external is not None else {"read_only": True}
        with self._transaction(**mode) as cursor:
            cursor.execute("""
                SELECT id, correlation_id, project_alias, revision, status
                FROM forge_agent.coding_tasks
                WHERE owner_id = %s AND project_alias = %s AND id = %s
            """, (self._owner, project_alias, task_id))
            task = cursor.fetchone()
            if task is None:
                raise Rejected(404, "not_found")
            cursor.execute("""
                SELECT event_id, correlation_id, sequence, event_kind, created_at, payload
                FROM forge_agent.coding_events
                WHERE owner_id = %s AND project_alias = %s AND task_id = %s
                  AND sequence > %s
                ORDER BY sequence ASC
                LIMIT %s
            """, (self._owner, project_alias, task_id, after_sequence, limit))
            rows = cursor.fetchall()
            cursor.execute("""SELECT attempt_id FROM forge_agent.coding_attempts
                WHERE owner_id=%s AND project_alias=%s AND task_id=%s""", (self._owner,project_alias,task_id))
            owned_attempts = {str(row["attempt_id"]) for row in cursor.fetchall()}
            binding = None
            workflow = None
            if self._external is not None:
                cursor.execute("SELECT forge_external_agent_api.event_binding(%s,%s,%s) AS binding",(self._owner,project_alias,task_id))
                binding = cursor.fetchone()["binding"]
                external_workflow = self._external_workflow_for(project_alias)
                if external_workflow is not None:
                    workflow = external_workflow.project(cursor,project_alias,task_id)
            items = []
            for row in rows:
                if row["correlation_id"] != task["correlation_id"]:
                    raise Rejected(409,"result_mismatch")
                item = {"event_id":str(row["event_id"]),"correlation_id":str(row["correlation_id"]),
                        "sequence":row["sequence"],"kind":row["event_kind"],"created_at":_timestamp(row["created_at"])}
                if row["event_kind"] == "checkpoint":
                    native = _public_progress(row["payload"])
                    external = _public_external_progress(row["payload"],binding)
                    related_allowed = False
                    payload = row["payload"]
                    if workflow is not None and isinstance(payload,dict) and payload.get("workflow_id") == workflow["workflow_id"]:
                        related = payload.get("related_task_id")
                        try:
                            valid_related = isinstance(related,str) and str(UUID(related)) == related
                        except (ValueError,TypeError):
                            valid_related = False
                        if valid_related:
                            related_allowed = external_workflow.related_task(cursor,project_alias,workflow["workflow_id"],related)
                    flow = _public_workflow_progress(payload,workflow,related_allowed)
                    if native is not None and native["attempt_id"] in owned_attempts:
                        item["progress"] = native
                        item["progress_verified"] = True
                    elif external is not None:
                        item["external_progress"] = external
                    elif flow is not None:
                        item["workflow_progress"] = flow
                items.append(item)
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

    def cancel(self, *, project_alias, task_id, expected_revision, idempotency_key, actor=None, cause=None):
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
            cursor.execute("SELECT to_regclass('forge_external_workflow.workflows') IS NOT NULL AS available")
            if cursor.fetchone()["available"]:
                # Match ExternalWorkflowStore.{run_once,cancel} lock order: lock the
                # external workflow before locking coding_tasks or retry settlement.
                cursor.execute("""
                    SELECT w.* FROM forge_external_workflow.workflows w
                    JOIN forge_external_workflow.tasks x USING(workflow_id)
                    WHERE (x.owner_id, x.project_alias, x.task_id) = (%s, %s, %s)
                    FOR UPDATE OF w
                """, (self._owner, project_alias, task_id))
                ew = cursor.fetchone()
                if ew is not None:
                    if task_id != ew["root_task_id"]:
                        raise Rejected(409, "not_cancellable")
                    cursor.execute("""
                        SELECT id, correlation_id, project_alias, status, revision, workflow_required, workflow_id, repair_root_task_id
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
                    if task["revision"] != expected_revision or expected_revision >= MAX_REVISION:
                        raise Rejected(409, "revision_conflict")
                    if ew["phase"] in {"completed", "cancelled", "escalated", "merging"}:
                        raise Rejected(409, "not_cancellable")
                    actor = require_actor(actor, self._owner)
                    cause = require_cancel_cause(cause)
                    cursor.execute("""
                        SELECT t.* FROM forge_agent.coding_tasks t
                        JOIN forge_external_workflow.tasks x
                          ON (x.owner_id, x.project_alias, x.task_id) = (t.owner_id, t.project_alias, t.id)
                        WHERE x.workflow_id = %s
                          AND t.status IN ('queued', 'running', 'awaiting_review', 'needs_reconcile',
                                           'blocked_auth', 'blocked_permission', 'quota_blocked')
                        ORDER BY t.created_at, t.id
                        FOR UPDATE OF t
                    """, (ew["workflow_id"],))
                    for child in cursor.fetchall():
                        cursor.execute("""
                            SELECT EXISTS(
                                SELECT FROM forge_agent.coding_attempts
                                WHERE (owner_id, project_alias, task_id) = (%s, %s, %s)
                                  AND (phase <> 'stopped' OR reservation_active OR lease_expires_at IS NOT NULL)
                            ) AS active
                        """, (self._owner, project_alias, child["id"]))
                        child_status = "cancel_requested" if cursor.fetchone()["active"] else "cancelled"
                        cursor.execute("""
                            UPDATE forge_agent.coding_tasks
                            SET status = %s,
                                cancelled_at = CASE WHEN %s = 'cancelled' THEN clock_timestamp() ELSE NULL END,
                                revision = revision + 1,
                                updated_at = clock_timestamp()
                            WHERE (owner_id, project_alias, id) = (%s, %s, %s)
                        """, (child_status, child_status, self._owner, project_alias, child["id"]))
                        child_cause = cause if child["id"] == task_id else "workflow_cancelled"
                        insert_event(
                            cursor, self._owner, project_alias, child["id"], child_status,
                            {"base_revision": child["revision"], "revision": child["revision"] + 1,
                             "workflow_id": str(ew["workflow_id"])},
                            actor=actor, cause=child_cause,
                        )
                    cursor.execute("""
                        SELECT * FROM forge_external_workflow.actions
                        WHERE workflow_id = %s AND kind = 'verify' AND status IN ('queued', 'leased', 'failed')
                    """, (ew["workflow_id"],))
                    cleanup_actions = cursor.fetchall()
                    cursor.execute("""
                        UPDATE forge_external_workflow.actions
                        SET status = 'failed', result = %s, lease_token = NULL, lease_role = NULL,
                            lease_until = NULL, updated_at = clock_timestamp()
                        WHERE workflow_id = %s AND status IN ('queued', 'leased')
                    """, (Json({"status": "failed", "receipt": {}, "summary": "Workflow admission stopped before delivery completed."}), ew["workflow_id"]))
                    for action in cleanup_actions:
                        action_id = uuid4()
                        intent = {
                            "target_action_id": str(action["action_id"]),
                            "target_kind": action["kind"],
                            "target_request": action["intent"],
                            "target_receipt": (action["result"] or {}).get("receipt", {}),
                            "workflow_id": str(ew["workflow_id"]),
                            "project_alias": project_alias,
                            "correlation_id": str(task["correlation_id"]),
                            "action_id": str(action_id),
                        }
                        cursor.execute("""
                            INSERT INTO forge_external_workflow.actions(
                                action_id, workflow_id, owner_id, project_alias, workflow_revision, kind, source_key, intent
                            ) VALUES (%s, %s, %s, %s, %s, 'cancel', %s, %s)
                            ON CONFLICT (workflow_id, kind, source_key) DO NOTHING
                            RETURNING action_id
                        """, (action_id, ew["workflow_id"], self._owner, project_alias, ew["revision"], str(action["action_id"]), Json(intent)))
                        if cursor.fetchone() is not None:
                            cursor.execute("SELECT pg_notify('forge_external_delivery', '')")
                    cursor.execute("""
                        UPDATE forge_external_workflow.workflows
                        SET phase = 'cancelled', enabled = false, revision = revision + 1, updated_at = clock_timestamp()
                        WHERE workflow_id = %s AND revision = %s
                        RETURNING revision
                    """, (ew["workflow_id"], ew["revision"]))
                    if cursor.fetchone() is None:
                        raise Rejected(409, "result_mismatch")
                    from work_events import COORDINATOR_ACTOR
                    insert_event(
                        cursor, self._owner, project_alias, task_id, "checkpoint",
                        {"workflow_id": str(ew["workflow_id"]), "phase": "cancelled",
                         "summary": "User cancelled the complete external workflow."},
                        actor=COORDINATOR_ACTOR,
                    )
                    self._resolve_open_retries(cursor, project_alias, "external_workflow", ew["workflow_id"])
                    cursor.execute("""
                        UPDATE forge_agent.coding_tasks
                        SET revision = %s, updated_at = clock_timestamp()
                        WHERE (owner_id, project_alias, id) = (%s, %s, %s)
                    """, (expected_revision + 1, self._owner, project_alias, task_id))
                    receipt = {
                        "project_alias": project_alias,
                        "task_id": str(task_id),
                        "base_revision": expected_revision,
                        "revision": expected_revision + 1,
                        "status": "cancelled",
                        "actor": actor,
                    }
                    cursor.execute("""
                        INSERT INTO forge_agent.coding_control_receipts (
                            owner_id, project_alias, task_id, action, idempotency_key,
                            intent_digest, base_revision, result_revision, receipt_json
                        ) VALUES (%s, %s, %s, 'cancel', %s, %s, %s, %s, %s)
                    """, (
                        self._owner, project_alias, task_id, idempotency_key,
                        intent_digest, expected_revision, expected_revision + 1, Json(receipt),
                    ))
                    cursor.execute("SELECT pg_notify(%s, '')", (NOTIFY_CHANNEL,))
                    return receipt

            cursor.execute("SELECT pg_advisory_xact_lock(hashtextextended(%s::text,%s::bigint))",(self._owner+"\n"+project_alias,ADVISORY_LOCK_SALT))
            cursor.execute("SELECT w.root_task_id FROM forge_agent.coding_tasks t JOIN forge_agent.coding_workflows w ON w.workflow_id=t.workflow_id WHERE t.owner_id=%s AND t.project_alias=%s AND t.id=%s",(self._owner,project_alias,task_id))
            scope=cursor.fetchone()
            if scope and scope['root_task_id']!=task_id:
                cursor.execute("SELECT id FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE",(self._owner,project_alias,scope['root_task_id']))
            cursor.execute("""
                SELECT id, correlation_id, project_alias, status, revision, workflow_required, workflow_id, repair_root_task_id
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
                actor=require_actor(actor,self._owner);cause=require_cancel_cause(cause)
                cursor.execute("SELECT id,project_alias,status,revision FROM forge_agent.coding_tasks WHERE workflow_id=%s AND status NOT IN('completed','cancelled') FOR UPDATE",(task["workflow_id"],))
                affected=cursor.fetchall()
                cursor.execute("UPDATE forge_agent.coding_workflows SET phase='cancelled',enabled=false,updated_at=clock_timestamp() WHERE workflow_id=%s",(task["workflow_id"],))
                cursor.execute("UPDATE forge_agent.coding_tasks SET cancelled_at=CASE WHEN status IN('running','cancel_requested') THEN NULL ELSE clock_timestamp() END,status=CASE WHEN status IN('running','cancel_requested') THEN 'cancel_requested' ELSE 'cancelled' END,revision=revision+1,updated_at=clock_timestamp() WHERE workflow_id=%s AND status NOT IN('completed','cancelled')",(task["workflow_id"],))
                cursor.execute("UPDATE forge_agent.coding_workflow_actions SET status='cancelled',lease_token=NULL,lease_expires_at=NULL,updated_at=clock_timestamp() WHERE workflow_id=%s AND status IN('pending','leased')",(task["workflow_id"],))
                target='cancel_requested' if task['status'] in {'running','cancel_requested'} else 'cancelled'
                receipt={'project_alias':project_alias,'task_id':str(task_id),'base_revision':expected_revision,'revision':expected_revision+1,'status':target,'actor':actor}
                for affected_task in affected:
                    affected_target='cancel_requested' if affected_task['status'] in {'running','cancel_requested'} else 'cancelled'
                    # The directly cancelled task carries the caller's cause;
                    # sibling tasks are cancelled because the workflow was.
                    affected_cause=cause if affected_task['id']==task_id else 'workflow_cancelled'
                    insert_event(cursor,self._owner,affected_task['project_alias'],affected_task['id'],affected_target,
                                 {'base_revision':affected_task['revision'],'revision':affected_task['revision']+1,
                                  'workflow_id':str(task["workflow_id"])},actor=actor,cause=affected_cause)
                self._resolve_open_retries(cursor, project_alias, "native_workflow", task["workflow_id"])
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

            actor=require_actor(actor,self._owner);cause=require_cancel_cause(cause)
            receipt = {
                "project_alias": task["project_alias"],
                "task_id": str(task["id"]),
                "base_revision": expected_revision,
                "revision": new_revision,
                "status": target_status,
                "actor": actor,
            }
            insert_event(cursor, self._owner, project_alias, task_id, event_kind,
                         {"base_revision": expected_revision, "revision": new_revision},
                         actor=actor, cause=cause)
            if target_status == "cancelled":
                manual_root = task["repair_root_task_id"] or task_id
                if manual_root == task_id:
                    self._resolve_open_retries(cursor, project_alias, "manual_task", manual_root)
                else:
                    cursor.execute(
                        "SELECT status FROM forge_agent.coding_tasks WHERE owner_id = %s AND project_alias = %s AND id = %s",
                        (self._owner, project_alias, manual_root),
                    )
                    root_row = cursor.fetchone()
                    if root_row is not None and root_row["status"] in ("cancelled", "completed"):
                        self._resolve_open_retries(cursor, project_alias, "manual_task", manual_root)
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

    def accept(self, *, project_alias, task_id, expected_revision, idempotency_key, review_note="", actor=None):
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
            insert_event(cursor, self._owner, project_alias, task_id, "completed",
                         {"base_revision": expected_revision, "revision": new_revision, "review_note": review_note},
                         actor=require_actor(actor, self._owner))
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
               agent_profile, reconciliation_receipt_id, note="", allowed_modes, resolve_recipe,_authority=None,actor=None):
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
        with self._transition_transaction(_authority) as cursor:
            cursor.execute(
                "SELECT pg_catalog.pg_advisory_xact_lock("
                "pg_catalog.hashtextextended(%s::text, %s::bigint))",
                (lock_scope, ADVISORY_LOCK_SALT),
            )
            cursor.execute("""
                SELECT id, project_alias, mode, status, revision, latest_attempt_id, plan_decision_id,review_required,workflow_required,workflow_id,workflow_review_id
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

            from work_provider_model_route import BoundProviderModelAuthority,BoundBinaryPinAuthority
            transition_kind=('stopped_binary_pin' if isinstance(_authority,BoundBinaryPinAuthority)
                             else 'provider_model_route' if isinstance(_authority,BoundProviderModelAuthority)
                             else 'reviewer_correction' if task['mode']=='review' else 'native_timeout')
            self._require_transition_authority(cursor,task,_authority,transition_kind)
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
            insert_event(cursor, self._owner, project_alias, task_id, "resumed",
                         event_payload, actor=require_actor(actor, self._owner))
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

    def handoff_author(self, command, *, target_profile, recipe, workspace_digest, _authority):
        """Change only future execution under the selected handoff authority.

        The caller has locked and verified the stopped source and atomically
        staged its delegation action. No public Resume or approval path can
        supply this authority, and existing queued work keeps its workspace.
        """
        from work_agent_handoff import AuthorHandoff, BoundAuthorHandoffAuthority
        if (not isinstance(command, AuthorHandoff)
                or not isinstance(_authority, BoundAuthorHandoffAuthority)
                or not isinstance(recipe, PinnedRecipe)
                or target_profile != _authority.target_profile
                or recipe.digest != _authority.recipe_digest
                or workspace_digest != _authority.workspace_digest):
            raise Rejected(403, "denied")
        self._require_project(command.project_alias)
        with self._transition_transaction(_authority) as cursor:
            cursor.execute("SELECT * FROM forge_agent.coding_tasks WHERE (owner_id,project_alias,id)=(%s,%s,%s) FOR UPDATE",
                (self.owner, command.project_alias, command.task_id))
            task = cursor.fetchone()
            if task is None:
                raise Rejected(404, "not_found")
            self._require_transition_authority(cursor, task, _authority, "author_handoff")
            if (task["cancelled_at"] is not None or task["workflow_review_id"] is not None
                    or recipe.mode != task["mode"] or recipe.backend != "codex"
                    or recipe.generation_token_limit is not None
                    or command.expected_revision >= MAX_REVISION):
                raise Rejected(409, "not_reviewable")
            if task["status"] == "quota_blocked":
                cursor.execute("SELECT count(*) AS pending FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND status IN ('queued','running','cancel_requested')",
                    (self.owner, command.project_alias))
                if cursor.fetchone()["pending"] >= self._max_pending:
                    raise Rejected(503, "queue_full")
            elif task["resume_workspace_digest"] != workspace_digest:
                raise Rejected(409, "reconcile_required")
            cursor.execute("""
                UPDATE forge_agent.coding_tasks SET status='queued',revision=revision+1,
                  agent_profile=%s,pinned_recipe=%s,recipe_digest=%s,resume_workspace_digest=%s,
                  updated_at=clock_timestamp()
                WHERE (owner_id,project_alias,id,revision)=(%s,%s,%s,%s)
                RETURNING revision
            """, (target_profile, Json(recipe.to_dict()), recipe.digest, workspace_digest,
                  self.owner, command.project_alias, command.task_id, command.expected_revision))
            changed = cursor.fetchone()
            if changed is None:
                raise Rejected(409, "revision_conflict")
            receipt = dict(project_alias=command.project_alias, task_id=str(command.task_id),
                base_revision=command.expected_revision, revision=changed["revision"],
                status="queued", recipe_digest=recipe.digest)
            insert_event(cursor, self.owner, command.project_alias, command.task_id, "resumed",
                dict(base_revision=command.expected_revision, revision=changed["revision"],
                    agent_profile=target_profile, recipe_digest=recipe.digest,
                    workspace_digest=workspace_digest, reason="provider_author_handoff"),
                actor=COORDINATOR_ACTOR)
            return receipt

    @contextmanager
    def _transition_transaction(self,authority=None):
        if authority is None:
            with self._transaction() as cursor:yield cursor
        else:
            from work_workflow import BoundDelegatedAuthority,BoundNativeRetryAuthority,BoundPreparationRetryAuthority
            from work_reviewer_correction import BoundReviewerCorrectionAuthority
            from work_provider_model_route import BoundProviderModelAuthority,BoundBinaryPinAuthority
            from work_agent_handoff import BoundAuthorHandoffAuthority
            if not isinstance(authority,(BoundDelegatedAuthority,BoundNativeRetryAuthority,BoundPreparationRetryAuthority,BoundReviewerCorrectionAuthority,BoundProviderModelAuthority,BoundBinaryPinAuthority,BoundAuthorHandoffAuthority)) or authority.store is not self:
                raise Rejected(403,"denied")
            yield authority.cursor

    def _require_transition_authority(self,cursor,task,authority,kind):
        if authority is not None:
            authority.validate(cursor,task,kind)
        elif task.get("workflow_required"):
            raise Rejected(409,"not_reviewable")

    def _resolve_open_retries(self, cursor, project_alias, scope_kind, scope_id):
        if self.retry_events is None:
            return 0
        from work_retry_events import RetryScope
        return self.retry_events.resolve_open_scope(
            cursor, RetryScope(self._owner, project_alias, scope_kind, UUID(str(scope_id)))
        )

    def _retry_projection(self,cursor,task):
        if self.retry_events is None:return None
        from work_retry_events import RetryScope
        cursor.execute("SELECT to_regprocedure('forge_agent.retry_event_project(text,text,uuid)') IS NOT NULL AS available")
        if not cursor.fetchone()['available']:return None
        workflow_id=task.get('workflow_id')
        if workflow_id is not None:
            kind,scope='native_workflow',workflow_id
        else:
            cursor.execute('SELECT workflow_id FROM forge_external_workflow.tasks WHERE owner_id=%s AND project_alias=%s AND task_id=%s',
                (self._owner,task['project_alias'],task['id']))
            external=cursor.fetchone()
            if external:kind,scope='external_workflow',external['workflow_id']
            else:
                cursor.execute('SELECT coalesce(repair_root_task_id,id) AS root FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s',
                    (self._owner,task['project_alias'],task['id']))
                root=cursor.fetchone()
                if not root:return None
                kind,scope='manual_task',root['root']
        return self.retry_events.project(cursor,RetryScope(self._owner,task['project_alias'],kind,UUID(str(scope))))

    def _workflow_projection(self,cursor,task):
        if task.get("workflow_id") is None:return None
        cursor.execute("SELECT w.*,coalesce(b.repair_rounds,0) AS code_revisions FROM forge_agent.coding_workflows w LEFT JOIN forge_agent.coding_review_budget b ON(b.owner_id,b.project_alias,b.root_task_id)=(w.owner_id,w.project_alias,w.root_task_id) WHERE w.owner_id=%s AND w.project_alias=%s AND w.workflow_id=%s",(self._owner,task["project_alias"],task["workflow_id"]))
        w=cursor.fetchone()
        if w is None:return None
        p=w["snapshot"]
        cursor.execute("SELECT to_regprocedure('forge_agent.coding_workflow_author_handoff(uuid)') IS NOT NULL AS available")
        if cursor.fetchone()['available']:
            cursor.execute('SELECT forge_agent.coding_workflow_author_handoff(%s) AS handoff',(w['workflow_id'],))
            handed=cursor.fetchone()['handoff']
            if handed is not None:p=handed['intent']['effective_snapshot']
        limits={key:p[key] for key in ('max_plan_revisions','max_code_revisions')}
        cursor.execute("SELECT payload FROM forge_agent.coding_events WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND event_kind='checkpoint' AND payload->>'stage'='retry_policy_updated' AND payload->>'policy'='forge-agent-retry-v1' AND payload->>'workflow_id'=%s AND payload->>'snapshot_digest'=%s ORDER BY sequence DESC LIMIT 1",
            (self._owner,w['project_alias'],w['root_task_id'],str(w['workflow_id']),w['policy_digest']))
        allowance=cursor.fetchone()
        if allowance:
            for key in limits:
                if allowance['payload'].get(key)==AGENT_RETRY_LIMIT:
                    limits[key]=AGENT_RETRY_LIMIT
        result={"workflow_id":str(w["workflow_id"]),"policy_digest":w["policy_digest"],"phase":w["phase"],
            "author_profile":p["author_profile"],"reviewer_profile":p["reviewer_profile"],"plan_revisions":w["plan_revisions"],
            "max_plan_revisions":limits["max_plan_revisions"],"code_revisions":w["code_revisions"],"max_code_revisions":limits["max_code_revisions"],
            "escalation_reason":w["escalation_reason"] or None,"can_cancel":w["phase"] not in {"completed","cancelled","merging"}}
        event=self._retry_projection(cursor,task)
        if event is not None:result["retry_event"]=event
        return result

    def plan_decide(self, *, project_alias, task_id, expected_revision, plan_attempt_id,
                    plan_sha256, decision, agent_profile, idempotency_key, note="", resolve_recipe,auto_repair=False,_authority=None,actor=None,cause=None):
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
                if recipe.version != 2 or not native_policy_matches(recipe.resource_profile, recipe.backend, recipe.policy_digest):
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
            decision_payload={**receipt,"decision_id":str(decision_id),"decision":decision,"plan_attempt_id":str(plan_attempt_id),
                              "plan_sha256":plan_sha256}
            actor=require_actor(actor,self._owner)
            if decision == "cancel":
                # The plan decision and the cancellation are separate events:
                # the decision is a checkpoint, the cancellation carries cause.
                insert_event(cursor,self._owner,project_alias,task_id,"checkpoint",
                             decision_payload,actor=actor)
                insert_event(cursor,self._owner,project_alias,task_id,"cancelled",
                             {"base_revision":expected_revision,"revision":revision,
                              "decision_id":str(decision_id),"decision":decision},
                             actor=actor,cause=require_cancel_cause(cause))
            else:
                insert_event(cursor,self._owner,project_alias,task_id,"resumed",
                             decision_payload,actor=actor)
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
        if self.retry_events is None and all(p[key]==AGENT_RETRY_LIMIT for key in ('max_rounds','max_stagnant_rounds','max_infrastructure_retries')):
            cursor.execute('SELECT policy FROM forge_agent.coding_review_budget WHERE owner_id=%s AND project_alias=%s AND root_task_id=%s',
                (self._owner,project_alias,root_task_id))
            if cursor.fetchone()['policy']!={**p,'required_hosted_checks':list(p['required_hosted_checks'])}:
                cursor.execute('SELECT forge_agent.amend_agent_retry_policy(%s,%s,%s)',(project_alias,root_task_id,Json(dict(p))))

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

    def _pipeline_projection(self,cursor,task,row):
        # Pipeline diagram data for the latest review. The stage list comes
        # from the frozen plan identified by the latest hosted observation's
        # check-name set; states come from the latest native build's Jenkins
        # events for the review's candidate commit plus the terminal
        # observation. Single-instance assumption: the deployment has one
        # Jenkins instance, so events are scoped by job + commit only.
        from work_pipeline_projection import (NATIVE_JENKINS_JOB, identify_plan,
            project_pipeline_stages, select_latest_build_events)
        no_run={'pipeline_stages':[],'pipeline_availability':'no_run','pipeline_phase':'unknown'}
        if row is None or not row.get('candidate_commit'):
            return no_run
        candidate=row['candidate_commit']
        observation=None
        if row.get('source_publication_id') is not None:
            cursor.execute("""SELECT checks FROM forge_agent.coding_hosted_checks
                WHERE owner_id=%s AND project_alias=%s AND publication_id=%s AND candidate_commit=%s
                ORDER BY observed_at DESC,observation_id DESC LIMIT 1""",
                (self._owner,task['project_alias'],row['source_publication_id'],candidate))
            found=cursor.fetchone()
            if found is not None:
                observation=found['checks']
        plan=identify_plan([check.get('name') for check in observation]) if observation else None
        cursor.execute("""SELECT event_kind,build_number,queue_id,commit_sha,build_result,
                stage_name,stage_status,occurred_at,sequence
            FROM forge_agent.jenkins_events
            WHERE owner_id=%s AND project_alias=%s AND job_name=%s AND commit_sha=%s
            ORDER BY sequence""",
            (self._owner,task['project_alias'],NATIVE_JENKINS_JOB,candidate))
        events=select_latest_build_events([dict(item) for item in cursor.fetchall()])
        return project_pipeline_stages(plan=plan,events=events,observation=observation,
            findings=row.get('findings') or (),
            verification_unavailable=row.get('reason')=='hosted_wait_exhausted')

    def _review_projection(self,cursor,task):
        cursor.execute("SELECT review_required,auto_repair,parent_task_id FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s",
                       (self._owner,task['project_alias'],task['id']))
        flags=cursor.fetchone()
        if not flags:return None
        if not flags['review_required']:
            cursor.execute('SELECT 1 FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s LIMIT 1',(self._owner,task['project_alias'],task['id']))
            if cursor.fetchone() is None:return None
        cursor.execute("""SELECT r.*,coalesce((SELECT(b.policy->>'max_rounds')::integer FROM forge_agent.coding_review_budget b JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,coalesce(t.repair_root_task_id,t.id))=(b.owner_id,b.project_alias,b.root_task_id) WHERE t.owner_id=r.owner_id AND t.project_alias=r.project_alias AND t.id=r.task_id),p.max_rounds,%s) AS max_rounds FROM forge_agent.coding_reviews r
            LEFT JOIN forge_agent.coding_review_policy p USING(owner_id,project_alias)
            WHERE r.owner_id=%s AND r.project_alias=%s AND r.task_id=%s ORDER BY r.review_no DESC LIMIT 1""",
            (AGENT_RETRY_LIMIT,self._owner,task['project_alias'],task['id']))
        row=cursor.fetchone()
        green=self._hosted_green(cursor,task) if task['latest_attempt_id'] else False
        value={'stage':'waiting_for_result','review_id':None,'feedback_id':None,'round':0,'max_rounds':AGENT_RETRY_LIMIT,
            'open_findings':0,'verified_findings':0,'latest_check_status':None,'toolchain':None,'handoff_reason':None,
            'source_attempt_id':str(task['latest_attempt_id']) if task['latest_attempt_id'] else None,
            'candidate_commit':None,'workspace_digest':None,'gate_manifest_sha256':None,'required_hosted_green':green,
            'can_accept':False,'can_repair':False,'can_retry':False,'auto_repair':flags['auto_repair'],'findings':[],
            'repair_task_id':None,'parent_task_id':None,
            'pipeline_stages':[],'pipeline_availability':'no_run','pipeline_phase':'unknown'}
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
        event=self._retry_projection(cursor,task)
        if event is not None:
            value['retry_event']=event
            value['can_repair']=task['status'] in {'completed','awaiting_review'} and row is not None and row['candidate_commit'] is not None and event['status']=='open' and event['used']<event['limit']
        value.update(self._pipeline_projection(cursor,task,row))
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

    def _control_record(self,cursor,task,expected_revision,key,action,intent,receipt,actor=None):
        actor=require_actor(actor,self._owner)
        # The control receipt gains the actor (section 4.2); the same dict is
        # stored, emitted as the checkpoint payload, and returned to the caller.
        receipt["actor"]=actor
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
        insert_event(cursor,self._owner,task['project_alias'],task['id'],'checkpoint',receipt,actor=actor)
        cursor.execute("SELECT pg_notify(%s,'')",(NOTIFY_CHANNEL,))

    def review_decide(self,*,project_alias,task_id,expected_revision,review_id,decision,agent_profile,note,idempotency_key,
                      findings=(),verified_finding_ids=(),recipe=None,resolve_recipe=None,_authority=None,actor=None):
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
            validator=getattr(_authority,'validate_review_decision',None)
            if validator is not None:validator(decision,review_id)
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
                if not isinstance(recipe,PinnedRecipe) or recipe.mode!='implement' or not native_policy_matches(recipe.resource_profile,recipe.backend,recipe.policy_digest):
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
                result['review_id']=str(self._append_review_retry(cursor,task,review,note,authority=_authority))
            elif decision=='escalate':
                cursor.execute("UPDATE forge_agent.coding_reviews SET status='handoff',reason=%s,updated_at=clock_timestamp() WHERE review_id=%s",(note,review_id))
            else:
                if task['status']=='completed':raise Rejected(409,'not_cancellable')
                cursor.execute("UPDATE forge_agent.coding_reviews SET status='cancelled',reason=%s WHERE review_id=%s",(note,review_id))
                cursor.execute("UPDATE forge_agent.coding_tasks SET status='cancelled',cancelled_at=clock_timestamp() WHERE owner_id=%s AND project_alias=%s AND id=%s",(self._owner,project_alias,task_id));result['status']='cancelled'
            self._control_record(cursor,task,expected_revision,idempotency_key,'review_decide',intent,result,actor=actor)
            return result

    def _append_review_retry(self,cursor,task,review,note,*,authority=None):
        cursor.execute("SELECT coalesce((SELECT(b.policy->>'max_infrastructure_retries')::integer FROM forge_agent.coding_review_budget b WHERE b.owner_id=%s AND b.project_alias=%s AND b.root_task_id=%s),p.max_infrastructure_retries) AS max_infrastructure_retries FROM forge_agent.coding_review_policy p WHERE owner_id=%s AND project_alias=%s",(self._owner,task['project_alias'],task['repair_root_task_id'] or task['id'],self._owner,task['project_alias']))
        policy=cursor.fetchone()
        from work_owner_reverification import BoundOwnerVerificationAuthority
        manual = isinstance(authority, BoundOwnerVerificationAuthority) and authority.store is self
        if not policy or not manual and review['infrastructure_retries']>=policy['max_infrastructure_retries']:
            raise Rejected(409,'not_reviewable')
        if manual:
            authority.validate_append(cursor,task,review)
        cursor.execute("""INSERT INTO forge_agent.coding_reviews(owner_id,project_alias,task_id,attempt_id,plan_decision_id,scope_plan_decision_id,reconciliation_receipt_id,
            base_revision,workspace_digest,parent_result_sha256,source_revision,review_no,round,stagnant_rounds,infrastructure_retries,previous_review_id,candidate_commit,summary,phase,owner_decision,verification,target_publication_id,source_publication_id,gate_manifest_sha256)
            SELECT owner_id,project_alias,task_id,attempt_id,plan_decision_id,scope_plan_decision_id,reconciliation_receipt_id,base_revision,workspace_digest,parent_result_sha256,%s,
             (SELECT coalesce(max(review_no),0)+1 FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s),round,stagnant_rounds,infrastructure_retries+1,review_id,candidate_commit,%s,phase,owner_decision,verification,target_publication_id,source_publication_id,gate_manifest_sha256
            FROM forge_agent.coding_reviews WHERE review_id=%s RETURNING review_id""",
            (task['revision']+1,self._owner,task['project_alias'],task['id'],note,review['review_id']))
        return cursor.fetchone()['review_id']

    def repair(self,*,project_alias,task_id,expected_revision,candidate_commit,agent_profile,note,findings,idempotency_key,recipe=None,resolve_recipe=None,actor=None):
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
            if resolve_recipe is not None:recipe=resolve_recipe()
            if not isinstance(recipe,PinnedRecipe) or recipe.mode!='implement' or not native_policy_matches(recipe.resource_profile,recipe.backend,recipe.policy_digest):
                raise Rejected(503,'unavailable')
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
                self._control_record(cursor,task,expected_revision,idempotency_key,'repair',intent,result,actor=actor)
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
            self._control_record(cursor,task,expected_revision,idempotency_key,'repair',intent,result,actor=actor)
            return result

    def publication_retry(self,*,project_alias,task_id,expected_revision,publication_id,candidate_commit,note,idempotency_key,actor=None):
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
            self._control_record(cursor,task,expected_revision,idempotency_key,'publication_retry',intent,result,actor=actor)
            return result
