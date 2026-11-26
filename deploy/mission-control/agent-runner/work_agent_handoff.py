"""Source-bound, one-way author delegation after a published AGY quota stop.

The original workflow and all historical attempts remain immutable. A completed
action delegates future author turns to a configured Codex profile; it neither
approves work nor changes the independent reviewer or any retry allowance.
"""

from copy import deepcopy
from dataclasses import dataclass
from datetime import UTC, datetime
from uuid import UUID

from psycopg2.extras import Json

from queue_store import Rejected
from work_config import sha256_canonical, validate_identifier
from work_events import COORDINATOR_ACTOR, insert_event
from work_quota_diagnostics import parse_quota_summary


POLICY = "forge-provider-author-handoff-v1"
PREFIX = "provider-author-handoff:"
_ADMISSION = object()


def changed_snapshot(original, catalog):
    """Substitute only future author recipes, preserving delegated scope."""
    selected = deepcopy(original)
    for entry, mode in (("author_plan", "plan"), ("author_implement", "implement")):
        try:
            alias, recipe = catalog.resolve_handoff(original["author_profile"], mode)
        except (KeyError, ValueError):
            raise Rejected(403, "denied") from None
        if original["recipes"][entry]["recipe"]["backend"] != "antigravity":
            raise Rejected(403, "denied")
        selected["author_profile"] = alias
        selected["recipes"][entry] = {"recipe": recipe.to_dict(), "digest": recipe.digest}
    if selected["author_profile"] == original["reviewer_profile"]:
        raise Rejected(403, "denied")
    selected.pop("policy_digest")
    selected["policy_digest"] = sha256_canonical(selected)
    return selected


@dataclass(frozen=True)
class AuthorHandoff:
    project_alias: str
    task_id: UUID
    expected_revision: int
    source_attempt_id: UUID | None
    quota_source_attempt_id: UUID

    def __post_init__(self):
        validate_identifier(self.project_alias, "project alias")
        if (not isinstance(self.task_id, UUID)
                or not isinstance(self.quota_source_attempt_id, UUID)
                or self.source_attempt_id is not None and not isinstance(self.source_attempt_id, UUID)
                or type(self.expected_revision) is not int
                or not 1 <= self.expected_revision < 2**53 - 1):
            raise ValueError("Invalid author handoff")


@dataclass(frozen=True)
class BoundAuthorHandoffAuthority:
    store: object
    cursor: object
    workflow_id: UUID
    task_id: UUID
    source_attempt_id: UUID | None
    revision: int
    source_status: str
    target_profile: str
    recipe_digest: str
    workspace_digest: str | None
    admission: object

    def validate(self, cursor, task, kind):
        if (self.admission is not _ADMISSION or cursor is not self.cursor
                or kind != "author_handoff" or task["workflow_id"] != self.workflow_id
                or task["id"] != self.task_id or task["revision"] != self.revision
                or task["latest_attempt_id"] != self.source_attempt_id
                or task["status"] != self.source_status
                or task["mode"] not in {"plan", "implement"}
                or self.source_status not in {"queued", "quota_blocked"}):
            raise Rejected(403, "denied")


class AgentHandoffProvider:
    """Ready application port. Current-state scans are durable; NOTIFY is a hint."""

    def __init__(self, flow, source_policy):
        self.flow, self.source_policy = flow, source_policy
        self._scan_position = None

    def handles_quota(self, workflow, task):
        if (task.get("attempt_outcome") != "quota_blocked" or task["mode"] not in {"plan", "implement"}
                or task["workflow_review_id"] is not None):
            return False
        with self.flow.store._transaction(read_only=True) as cursor:
            source = self.source_policy(workflow, cursor=cursor)
            selected = self.effective(workflow, source, cursor=cursor)
        return (selected["author_profile"] != source["author_profile"]
                and task["agent_profile"] == selected["author_profile"])

    def run_once(self):
        flow = self.flow
        selected_profiles = [p.alias for p in flow.config.profiles.values()
                             if getattr(p, "quota_handoff_profile", None)]
        if not selected_profiles:
            return
        with flow.store._transaction(read_only=True) as cursor:
            cursor.execute("SELECT to_regprocedure('forge_agent.coding_workflow_author_handoff(uuid)') IS NOT NULL AS available")
            if not cursor.fetchone()["available"]:
                return
            cursor.execute("""
                SELECT t.id,t.project_alias,t.revision,t.latest_attempt_id,t.agent_profile,t.updated_at,
                       t.pinned_recipe->>'model' AS model
                FROM forge_agent.coding_tasks t JOIN forge_agent.coding_workflows w
                  ON (w.owner_id,w.project_alias,w.workflow_id)=(t.owner_id,t.project_alias,t.workflow_id)
                WHERE t.owner_id=%s AND t.project_alias=ANY(%s)
                  AND t.agent_profile=ANY(%s)
                  AND t.status IN ('quota_blocked','queued') AND t.cancelled_at IS NULL
                  AND t.mode IN ('plan','implement') AND t.workflow_review_id IS NULL
                  AND w.enabled AND w.phase IN ('plan','plan_revision','implement','repair')
                  AND NOT EXISTS (SELECT FROM forge_agent.coding_workflow_actions a
                    WHERE a.workflow_id=w.workflow_id AND a.source_key LIKE 'provider-author-handoff:%%')
                  AND (%s::timestamptz IS NULL OR (t.updated_at,t.id)>(%s::timestamptz,%s::uuid))
                ORDER BY t.updated_at,t.id LIMIT 64
            """, (flow.store.owner, list(flow.store.allowed_projects), selected_profiles)
                  + ((self._scan_position[0], *self._scan_position) if self._scan_position else (None, None, None)))
            rows = cursor.fetchall()
        self._scan_position = (rows[-1]["updated_at"], rows[-1]["id"]) if len(rows) == 64 else None
        for row in rows:
            profile = flow.catalog.profile(row["agent_profile"])
            if not profile or not getattr(profile, "quota_handoff_profile", None):
                continue
            with flow.store._transaction(read_only=True) as cursor:
                quota = self._quota_source(cursor, row["project_alias"], row["agent_profile"], row["model"])
            if quota is None:
                continue
            command = AuthorHandoff(row["project_alias"], row["id"], row["revision"],
                                    row["latest_attempt_id"], quota["attempt_id"])
            try:
                self.handoff(command)
            except Rejected as error:
                if error.status not in {403, 409, 503}:
                    raise

    def _quota_source(self, cursor, project, profile, model):
        # A real successful turn closes an availability episode. Neither a
        # model's prose nor a stale local cooldown file can establish one.
        cursor.execute("""
            SELECT a.* FROM forge_agent.coding_attempts a
            WHERE a.owner_id=%s AND a.project_alias=%s AND a.agent_profile=%s
              AND a.pinned_recipe->>'backend'='antigravity' AND a.pinned_recipe->>'model'=%s
              AND a.outcome='quota_blocked' AND a.phase='stopped'
              AND a.published_at IS NOT NULL AND a.finished_at IS NOT NULL
              AND NOT a.reservation_active AND a.lease_expires_at IS NULL
              AND EXISTS (SELECT FROM forge_agent.coding_reconciliation_receipts stopped
                WHERE (stopped.owner_id,stopped.project_alias,stopped.task_id,stopped.attempt_id)=
                      (a.owner_id,a.project_alias,a.task_id,a.attempt_id)
                  AND stopped.stop_proof->>'scope_stopped'='true'
                  AND stopped.stop_proof->>'source'='systemd'
                  AND stopped.stop_proof->>'unit_name'=a.unit_name)
              AND NOT EXISTS (SELECT FROM forge_agent.coding_attempts succeeded
                WHERE succeeded.owner_id=a.owner_id AND succeeded.project_alias=a.project_alias
                  AND succeeded.agent_profile=a.agent_profile
                  AND succeeded.pinned_recipe->>'model'=a.pinned_recipe->>'model'
                  AND succeeded.outcome='awaiting_review' AND succeeded.published_at IS NOT NULL
                  AND succeeded.finished_at>a.finished_at)
            ORDER BY a.finished_at DESC,a.attempt_id DESC LIMIT 1
        """, (self.flow.store.owner, project, profile, model))
        row = cursor.fetchone()
        return row if row and parse_quota_summary(row["summary"]) is not None else None

    def effective(self, workflow, source_snapshot, *, cursor):
        cursor.execute("""
            SELECT source_key,intent,finish_receipt FROM forge_agent.coding_workflow_actions
            WHERE owner_id=%s AND project_alias=%s AND workflow_id=%s AND kind='decide'
              AND status='completed' AND source_key LIKE 'provider-author-handoff:%%'
            ORDER BY created_at,action_id LIMIT 2
        """, (self.flow.store.owner, workflow["project_alias"], workflow["workflow_id"]))
        rows = cursor.fetchall()
        if not rows:
            return source_snapshot
        if len(rows) != 1:
            raise Rejected(409, "reconcile_required")
        row = rows[0]
        intent, finish = row["intent"], row["finish_receipt"]
        try:
            expected = changed_snapshot(source_snapshot, self.flow.catalog)
            valid = (intent["policy"] == POLICY and intent["owner"] == self.flow.store.owner
                and intent["project_alias"] == workflow["project_alias"]
                and intent["workflow_id"] == str(workflow["workflow_id"])
                and intent["original_snapshot_sha256"] == workflow["policy_digest"]
                and intent["source_snapshot"] == source_snapshot
                and intent["source_snapshot_sha256"] == source_snapshot["policy_digest"]
                and intent["source_profile"] == source_snapshot["author_profile"]
                and intent["source_status"] in {"queued", "quota_blocked"}
                and intent["source_mode"] in {"plan", "implement"}
                and intent["source_recipe_digest"] == source_snapshot["recipes"]["author_" + intent["source_mode"]]["digest"]
                and intent["target_profile"] == expected["author_profile"]
                and intent["effective_snapshot"] == expected
                and row["source_key"] == PREFIX + str(UUID(intent["task_id"]))
                and finish["status"] == "provider_author_handed_off"
                and finish["intent_sha256"] == sha256_canonical(intent)
                and finish["receipt"]["task_id"] == intent["task_id"]
                and finish["receipt"]["revision"] == intent["source_revision"] + 1
                and finish["receipt"]["base_revision"] == intent["source_revision"]
                and finish["receipt"]["project_alias"] == workflow["project_alias"]
                and finish["receipt"]["recipe_digest"] == expected["recipes"]["author_" + intent["source_mode"]]["digest"]
                and finish["receipt"]["status"] == "queued")
        except (KeyError, ValueError, TypeError, AttributeError):
            raise Rejected(409, "reconcile_required") from None
        if not valid:
            raise Rejected(409, "reconcile_required")
        return expected

    def handoff(self, command):
        if not isinstance(command, AuthorHandoff):
            raise TypeError("Expected typed author handoff")
        from work_workflow import NATIVE_POLICIES, _ready_for_recipe, recipe_from_snapshot
        flow, store = self.flow, self.flow.store
        store._require_project(command.project_alias)
        request = {k: str(v) if isinstance(v, UUID) else v for k, v in vars(command).items()}
        key = PREFIX + str(command.task_id)
        with flow._transaction() as cursor:
            flow._project_lock(cursor, command.project_alias)
            cursor.execute("SELECT * FROM forge_agent.coding_tasks WHERE (owner_id,project_alias,id)=(%s,%s,%s) FOR UPDATE",
                           (store.owner, command.project_alias, command.task_id))
            task = cursor.fetchone()
            if task is None:
                raise Rejected(404, "not_found")
            cursor.execute("SELECT * FROM forge_agent.coding_workflows WHERE (owner_id,project_alias,workflow_id)=(%s,%s,%s) FOR UPDATE",
                           (store.owner, command.project_alias, task["workflow_id"]))
            workflow = cursor.fetchone()
            if workflow is None:
                raise Rejected(409, "not_reviewable")
            cursor.execute("SELECT intent,finish_receipt FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND source_key=%s",
                           (workflow["workflow_id"], key))
            old = cursor.fetchone()
            if old:
                if old["intent"].get("request") != request:
                    raise Rejected(409, "idempotency_conflict")
                return dict(old["finish_receipt"]["receipt"])
            flow._current_policy(workflow)
            source = self.source_policy(workflow, cursor=cursor)
            if (not workflow["enabled"] or workflow["phase"] not in {"plan", "plan_revision", "implement", "repair"}
                    or task["status"] not in {"queued", "quota_blocked"} or task["cancelled_at"] is not None
                    or task["revision"] != command.expected_revision
                    or task["latest_attempt_id"] != command.source_attempt_id
                    or task["mode"] not in {"plan", "implement"} or not task["workflow_required"]
                    or task["workflow_review_id"] is not None or task["agent_profile"] != source["author_profile"]):
                raise Rejected(409, "not_reviewable")
            cursor.execute("SELECT 1 FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND source_key LIKE 'provider-author-handoff:%%'",
                           (workflow["workflow_id"],))
            if cursor.fetchone():
                raise Rejected(409, "not_reviewable")
            recipe = recipe_from_snapshot(source, "author_" + task["mode"])
            if task["recipe_digest"] != recipe.digest or task["pinned_recipe"] != recipe.to_dict():
                raise Rejected(409, "result_mismatch")
            quota = self._quota_source(cursor, command.project_alias, task["agent_profile"], recipe.model)
            if not quota or quota["attempt_id"] != command.quota_source_attempt_id:
                raise Rejected(409, "reconcile_required")
            cursor.execute("""
                SELECT EXISTS(SELECT FROM forge_agent.coding_tasks WHERE workflow_id=%s AND
                  (cancelled_at IS NOT NULL OR status IN ('running','cancelled','cancel_requested')
                   OR id<>%s AND status='queued'))
                OR EXISTS(SELECT FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t
                  ON (t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id)
                  WHERE t.workflow_id=%s AND (a.reservation_active OR a.lease_expires_at IS NOT NULL))
                OR EXISTS(SELECT FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND status IN ('pending','leased'))
                OR EXISTS(SELECT FROM forge_agent.coding_workflow_reviews WHERE workflow_id=%s AND status IN ('pending','reviewing'))
                OR EXISTS(SELECT FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t
                  ON (t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id)
                  WHERE t.workflow_id=%s AND q.status IN ('pending','checking'))
                OR EXISTS(SELECT FROM forge_agent.coding_publications q JOIN forge_agent.coding_tasks t
                  ON (t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id)
                  WHERE t.workflow_id=%s AND q.status IN ('pending','checking','publishing')) AS busy
            """, (workflow["workflow_id"], task["id"], *([workflow["workflow_id"]] * 5)))
            if cursor.fetchone()["busy"]:
                raise Rejected(409, "reconcile_required")
            rec = None
            if task["latest_attempt_id"] is not None:
                cursor.execute("SELECT * FROM forge_agent.coding_attempts WHERE (owner_id,project_alias,task_id,attempt_id)=(%s,%s,%s,%s)",
                    (store.owner, command.project_alias, task["id"], task["latest_attempt_id"]))
                attempt = cursor.fetchone()
                cursor.execute("SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE (owner_id,project_alias,task_id,attempt_id)=(%s,%s,%s,%s) ORDER BY created_at DESC,receipt_id DESC LIMIT 1",
                    (store.owner, command.project_alias, task["id"], task["latest_attempt_id"]))
                rec = cursor.fetchone()
                if (not attempt or not rec or attempt["phase"] != "stopped" or not attempt["published_at"]
                        or not attempt["finished_at"] or rec["created_at"] < attempt["finished_at"]
                        or attempt["agent_profile"] != source["author_profile"]
                        or attempt["reservation_active"] or attempt["lease_expires_at"] is not None
                        or rec["stop_proof"].get("scope_stopped") is not True
                        or rec["stop_proof"].get("source") != "systemd"
                        or rec["stop_proof"].get("unit_name") != attempt["unit_name"]):
                    raise Rejected(409, "reconcile_required")
                try:
                    checked = datetime.fromisoformat(rec["stop_proof"]["checked_at"].replace("Z", "+00:00"))
                    valid_stop = checked.tzinfo is not None and attempt["started_at"] <= checked <= datetime.now(UTC)
                except (KeyError, ValueError, TypeError, AttributeError):
                    valid_stop = False
                if not valid_stop:
                    raise Rejected(409, "reconcile_required")
                if task["status"] == "quota_blocked":
                    if (attempt["outcome"] != "quota_blocked" or rec["task_revision"] != task["revision"]
                            or attempt["recipe_digest"] != recipe.digest
                            or parse_quota_summary(attempt["summary"]) is None):
                        raise Rejected(409, "reconcile_required")
                elif (rec["task_revision"] >= task["revision"]
                        or task["resume_workspace_digest"] != rec["workspace_digest"]):
                    raise Rejected(409, "reconcile_required")
            elif task["status"] != "queued" or task["resume_workspace_digest"] is not None:
                raise Rejected(409, "reconcile_required")
            plan_id = task["plan_decision_id"] or task["scope_plan_decision_id"]
            if task["mode"] == "implement":
                cursor.execute("SELECT 1 FROM forge_agent.coding_plan_decisions WHERE (owner_id,project_alias,task_id,decision_id)=(%s,%s,%s,%s) AND decision='resume'",
                    (store.owner, command.project_alias, workflow["root_task_id"], plan_id))
                if cursor.fetchone() is None:
                    raise Rejected(409, "result_mismatch")
            selected = changed_snapshot(source, flow.catalog)
            cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND observed_at>=clock_timestamp()-interval '90 seconds' AND policy_digest=ANY(%s)",
                           (store.owner, [x[0] for x in NATIVE_POLICIES.values()]))
            ready = cursor.fetchall()
            for alias, mode, entry in ((selected["author_profile"], "plan", "author_plan"),
                    (selected["author_profile"], "implement", "author_implement"),
                    (selected["reviewer_profile"], "review", "reviewer_review")):
                if not _ready_for_recipe(ready, command.project_alias, alias, mode, recipe_from_snapshot(selected, entry)):
                    raise Rejected(503, "unavailable")
            target_recipe = recipe_from_snapshot(selected, "author_" + task["mode"])
            receipt = dict(project_alias=command.project_alias, task_id=str(task["id"]),
                base_revision=task["revision"], revision=task["revision"] + 1, status="queued", recipe_digest=target_recipe.digest)
            intent = dict(policy=POLICY, owner=store.owner, project_alias=command.project_alias,
                workflow_id=str(workflow["workflow_id"]), task_id=str(task["id"]), source_status=task["status"],
                source_revision=task["revision"], source_attempt_id=str(task["latest_attempt_id"]) if task["latest_attempt_id"] else None,
                quota_source_attempt_id=str(quota["attempt_id"]), source_profile=task["agent_profile"],
                target_profile=selected["author_profile"], source_mode=task["mode"], source_model=recipe.model,
                source_recipe_digest=recipe.digest, source_snapshot=source, source_snapshot_sha256=source["policy_digest"],
                original_snapshot_sha256=workflow["policy_digest"], effective_snapshot=selected,
                reconciliation_receipt_id=str(rec["receipt_id"]) if rec else None,
                source_workspace_digest=rec["workspace_digest"] if rec else None,
                source_resume_workspace_digest=task["resume_workspace_digest"],
                feedback_id=str(task["feedback_id"]) if task["feedback_id"] else None,
                feedback_sha256=task["feedback_sha256"],
                plan_decision_id=str(task["plan_decision_id"]) if task["plan_decision_id"] else None,
                scope_plan_decision_id=str(task["scope_plan_decision_id"]) if task["scope_plan_decision_id"] else None,
                request=request)
            finish = dict(status="provider_author_handed_off", intent_sha256=sha256_canonical(intent), receipt=receipt)
            # SQL delegation and task mutation become visible together, never
            # granting a temporary profile outside this locked transaction.
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt) VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",
                (workflow["workflow_id"], store.owner, command.project_alias, key, Json(intent), Json(finish)))
            for alias, mode in ((selected["author_profile"], task["mode"]), (selected["reviewer_profile"], "review")):
                cursor.execute("SELECT forge_agent.coding_workflow_role_granted(%s,%s,%s) AS allowed",
                               (workflow["workflow_id"], alias, mode))
                if not cursor.fetchone()["allowed"]:
                    raise Rejected(403, "denied")
            authority = BoundAuthorHandoffAuthority(store, cursor, workflow["workflow_id"], task["id"],
                task["latest_attempt_id"], task["revision"], task["status"], selected["author_profile"],
                target_recipe.digest, rec["workspace_digest"] if rec else None, _ADMISSION)
            actual = store.handoff_author(command, target_profile=selected["author_profile"],
                recipe=target_recipe, workspace_digest=rec["workspace_digest"] if rec else None, _authority=authority)
            if actual != receipt:
                raise Rejected(409, "result_mismatch")
            insert_event(cursor, store.owner, command.project_alias, task["id"], "checkpoint",
                dict(stage="provider_author_handoff", source_profile=task["agent_profile"],
                    target_profile=selected["author_profile"], model=target_recipe.model,
                    quota_source_attempt_id=str(quota["attempt_id"]), old_recipe_digest=recipe.digest,
                    new_recipe_digest=target_recipe.digest, policy=POLICY), actor=COORDINATOR_ACTOR)
            cursor.execute("SELECT pg_notify('forge_coding_jobs','')")
            return actual


class AgentHandoffFactory:
    def __init__(self, flow, source_policy):
        self.flow, self.source_policy = flow, source_policy

    def create(self):
        if not callable(self.source_policy):
            raise TypeError("Expected selected source execution-policy provider")
        return AgentHandoffProvider(self.flow, self.source_policy)
