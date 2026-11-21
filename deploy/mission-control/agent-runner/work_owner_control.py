"""Explicit owner admission for one exhausted, source-bound native code turn.

This port is never called by the automatic coordinator. It preserves the spent
repair ledger and old attempt/repair lineage; the next worker receives genuine
native workspace feedback, not a substituted candidate-tree receipt.
"""
import hashlib
import json
import re
from datetime import datetime
from uuid import UUID

from psycopg2.extras import Json
from queue_store import Rejected
from work_feedback import validate_findings
from work_peer_review import parse_review_target, parse_review_verdict
from work_config import sha256_canonical
from work_store import _resume_intent_digest

OWNER_CODE_RESUME = 'forge-owner-code-resume-v1'

def owner_source_key(workflow_id):
    return 'owner-code-resume:' + str(workflow_id)


def require_owner_capability(cursor):
    # A ready Python adapter must not admit work against an older database
    # whose separate merge-failure path can renew automatic repair allowance.
    signature = 'forge_agent.owner_continuation_capability()'
    cursor.execute('SELECT to_regprocedure(%s) AS available', (signature,))
    if cursor.fetchone()['available'] is None:
        raise Rejected(503, 'unavailable')
    cursor.execute("SELECT has_function_privilege(current_user,%s,'EXECUTE') AS allowed", (signature,))
    if not cursor.fetchone()['allowed']:
        raise Rejected(503, 'unavailable')
    cursor.execute('SELECT forge_agent.owner_continuation_capability() AS capability')
    value = cursor.fetchone()['capability']
    expected = dict(policy=OWNER_CODE_RESUME, schema_version=26, migration='coding_owner_continuation', same_task=True, repair_fenced=True)
    if (value != expected or type(value.get('schema_version')) is not int
            or value.get('same_task') is not True or value.get('repair_fenced') is not True):
        raise Rejected(503, 'unavailable')


def owner_continuation_used(cursor, workflow_id):
    cursor.execute("SELECT EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key=%s AND status='completed') AS used", (workflow_id, owner_source_key(workflow_id)))
    return cursor.fetchone()['used']


class NativeOwnerControl:
    """Ready narrow application port injected by the normal service factory."""
    def __init__(self, workflows):
        if not callable(getattr(workflows, '_transaction', None)):
            raise TypeError('Native owner control requires a ready workflow store')
        self.workflows = workflows

    def resume(self, command):
        flow = self.workflows
        store = flow.store
        store._require_project(command.project_alias)
        request = dict(project_alias=command.project_alias, task_id=str(command.task_id),
            expected_revision=command.expected_revision, agent_profile=command.agent_profile,
            reconciliation_receipt_id=str(command.reconciliation_receipt_id), note=command.note,
            idempotency_key=command.idempotency_key)
        digest = _resume_intent_digest(**{k: v for k, v in request.items() if k != 'idempotency_key'})
        with flow._transaction() as cursor:
            flow._project_lock(cursor, command.project_alias)
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE',
                (store.owner, command.project_alias, command.task_id))
            task = cursor.fetchone()
            if not task:
                raise Rejected(404, 'not_found')
            if not task['workflow_required'] or not task['workflow_id']:
                return None  # Ordinary manual Resume retains its original path.
            if (not isinstance(command.task_id, UUID) or not isinstance(command.reconciliation_receipt_id, UUID)
                or type(command.expected_revision) is not int or not 1 <= command.expected_revision < 2**53
                or not isinstance(command.idempotency_key, str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,128}', command.idempotency_key)
                or not isinstance(command.note, str) or not command.note.strip() or len(command.note.encode()) > 1536):
                raise Rejected(400, 'invalid_request')
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key=%s", (task['workflow_id'], owner_source_key(task['workflow_id'])))
            prior = cursor.fetchone()
            if prior:
                if (prior['intent'] or {}).get('request') != request:
                    raise Rejected(409, 'idempotency_conflict')
                saved = prior['finish_receipt'] or {}
                receipt = saved.get('receipt')
                expected = dict(project_alias=command.project_alias, task_id=str(task['id']),
                    base_revision=command.expected_revision, revision=command.expected_revision+1,
                    status='queued', recipe_digest=prior['intent'].get('recipe_digest'))
                if (prior['status'] != 'completed' or prior['owner_id'] != store.owner
                        or prior['project_alias'] != command.project_alias
                        or prior['intent'].get('policy') != OWNER_CODE_RESUME
                        or prior['intent'].get('workflow_id') != str(task['workflow_id'])
                        or saved.get('status') != 'owner_continued' or receipt != expected):
                    raise Rejected(409, 'reconcile_required')
                return dict(receipt)
            cursor.execute('SELECT * FROM forge_agent.coding_control_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND idempotency_key=%s',
                (store.owner, command.project_alias, command.task_id, command.idempotency_key))
            if cursor.fetchone():
                raise Rejected(409, 'idempotency_conflict')
            cursor.execute('SELECT * FROM forge_agent.coding_workflows WHERE owner_id=%s AND project_alias=%s AND workflow_id=%s FOR UPDATE',
                (store.owner, command.project_alias, task['workflow_id']))
            w = cursor.fetchone()
            if (not w or w['enabled'] or w['phase'] != 'escalated' or task['revision'] != command.expected_revision
                    or task['mode'] != 'implement' or task['status'] != 'awaiting_review'
                    or task['workflow_review_id'] is not None or task['cancelled_at'] is not None):
                raise Rejected(409, 'not_reviewable')
            try:
                flow._current_policy(w)
            except (KeyError, ValueError):
                raise Rejected(403, 'denied') from None
            from work_workflow import recipe_from_snapshot, _ready_for_recipe, NATIVE_POLICIES
            p = w['snapshot']
            recipe = recipe_from_snapshot(p, 'author_implement')
            if (command.agent_profile != p['author_profile'] or task['agent_profile'] != p['author_profile']
                    or task['pinned_recipe'] != recipe.to_dict() or task['recipe_digest'] != recipe.digest
                    or not (task['plan_decision_id'] or task['scope_plan_decision_id'])):
                raise Rejected(409, 'result_mismatch')
            cursor.execute("SELECT * FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s ORDER BY review_no DESC LIMIT 1 FOR UPDATE",
                (store.owner, command.project_alias, command.task_id))
            q = cursor.fetchone()
            if (not q or q['attempt_id'] != task['latest_attempt_id'] or q['source_revision'] != task['revision']
                    or q['status'] != 'awaiting_review' or q['base_revision'] != task['base_revision']
                    or q['plan_decision_id'] != task['plan_decision_id'] or q['scope_plan_decision_id'] != task['scope_plan_decision_id']
                    or q['verification'].get('checks_passed') is not True or q['verification'].get('cleanup_confirmed') is not True
                    or not q['candidate_commit'] or not q['gate_manifest_sha256']
                    or task['repair_source_review_id'] == q['review_id']):
                raise Rejected(409, 'result_mismatch')
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_reviews WHERE workflow_id=%s AND source_task_id=%s AND source_review_id=%s AND target_kind='code' AND status='decided' ORDER BY created_at DESC LIMIT 1 FOR UPDATE",
                (w['workflow_id'], task['id'], q['review_id']))
            peer = cursor.fetchone()
            if not peer:
                raise Rejected(409, 'not_reviewable')
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND workflow_review_id=%s AND kind='decide' AND status='completed'", (w['workflow_id'], peer['workflow_review_id']))
            decided = cursor.fetchone()
            handoff = decided['finish_receipt'] if decided else None
            if (not handoff or decided['finish_token'] is None or handoff.get('status') != 'escalated' or handoff.get('reason') != 'repair_budget_exhausted'
                    or handoff.get('review_id') != str(q['review_id'])
                    or handoff.get('workflow_review_id') != str(peer['workflow_review_id'])
                    or handoff.get('workflow_id') != str(w['workflow_id'])):
                raise Rejected(409, 'not_reviewable')
            target = parse_review_target(peer['target_json'], peer['target_sha256'])
            verdict = parse_review_verdict(peer['verdict_json'], target)
            if (verdict['decision'] != 'revise' or not 1 <= len(verdict['findings']) <= 3
                    or hashlib.sha256(peer['verdict_json'].encode()).hexdigest() != peer['verdict_sha256']
                    or peer['source_attempt_id'] != task['latest_attempt_id'] or peer['source_revision'] != task['revision']
                    or peer['source_result_sha256'] != q['parent_result_sha256']
                    or peer['candidate_commit'] != q['candidate_commit'] or peer['gate_manifest_sha256'] != q['gate_manifest_sha256']
                    or target['policy_digest'] != w['policy_digest'] or target['workflow_id'] != str(w['workflow_id'])
                    or target['workflow_review_id'] != str(peer['workflow_review_id']) or target['reviewer_task_id'] != str(peer['reviewer_task_id'])
                    or target['source_task_id'] != str(task['id']) or target['source_attempt_id'] != str(task['latest_attempt_id'])
                    or target['source_revision'] != task['revision'] or target['source_result_sha256'] != q['parent_result_sha256']
                    or target['author_profile'] != p['author_profile'] or target['reviewer_profile'] != p['reviewer_profile']
                    or target['integration_base'] != q['verification'].get('integration_base')
                    or target['review_id'] != str(q['review_id']) or target['workspace_digest'] != q['verification'].get('candidate_workspace_digest')
                    or target['candidate_commit'] != q['candidate_commit'] or target['gate_manifest_sha256'] != q['gate_manifest_sha256']):
                raise Rejected(409, 'result_mismatch')
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE',
                (store.owner, command.project_alias, peer['reviewer_task_id']))
            critic = cursor.fetchone()
            expected_critic = recipe_from_snapshot(p, 'reviewer_review')
            if (not critic or critic['status'] != 'completed' or critic['accepted_at'] is None
                    or critic['mode'] != 'review' or critic['workflow_review_id'] != peer['workflow_review_id']
                    or critic['agent_profile'] != p['reviewer_profile'] or critic['recipe_digest'] != expected_critic.digest
                    or critic['pinned_recipe'] != expected_critic.to_dict() or critic['latest_attempt_id'] != peer['reviewer_attempt_id']):
                raise Rejected(409, 'result_mismatch')
            cursor.execute('SELECT pinned_recipe,recipe_digest,workflow_id,workflow_review_id,review_target_sha256 FROM forge_agent.coding_attempts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s',
                (store.owner, command.project_alias, critic['id'], peer['reviewer_attempt_id']))
            critic_attempt = cursor.fetchone()
            if (not critic_attempt or critic_attempt['pinned_recipe'] != expected_critic.to_dict()
                    or critic_attempt['recipe_digest'] != expected_critic.digest or critic_attempt['workflow_id'] != w['workflow_id']
                    or critic_attempt['workflow_review_id'] != peer['workflow_review_id']
                    or critic_attempt['review_target_sha256'] != peer['target_sha256']):
                raise Rejected(409, 'result_mismatch')
            critic_result = flow._artifact(cursor, critic['id'], peer['reviewer_attempt_id'])
            source = flow._artifact(cursor, task['id'], task['latest_attempt_id'])
            if (critic_result['content'] != peer['verdict_json'] or critic_result['result_sha256'] != peer['reviewer_result_sha256']
                    or source['result_sha256'] != q['parent_result_sha256']):
                raise Rejected(409, 'result_mismatch')
            cursor.execute('SELECT * FROM forge_agent.coding_plan_decisions WHERE owner_id=%s AND project_alias=%s AND decision_id=%s',
                (store.owner, command.project_alias, task['plan_decision_id'] or task['scope_plan_decision_id']))
            plan = cursor.fetchone()
            if (not plan or plan['decision'] != 'resume' or target['plan_decision_id'] != str(plan['decision_id'])
                    or plan['plan_sha256'] != flow._artifact(cursor, plan['task_id'], plan['plan_attempt_id'])['result_sha256']
                    or target['approved_plan']['sha256'] != plan['plan_sha256']
                    or target['approved_plan']['attempt_id'] != str(plan['plan_attempt_id'])):
                raise Rejected(409, 'result_mismatch')
            cursor.execute('SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND receipt_id=%s',
                (store.owner, command.project_alias, task['id'], command.reconciliation_receipt_id))
            rec = cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_attempts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s',
                (store.owner, command.project_alias, task['id'], task['latest_attempt_id']))
            attempt = cursor.fetchone()
            proof = rec['stop_proof'] if rec else {}
            try:
                checked = datetime.fromisoformat(proof['checked_at'].replace('Z', '+00:00'))
            except (ValueError, TypeError, KeyError, AttributeError):
                raise Rejected(409, 'reconcile_required') from None
            if (not rec or not attempt or rec['attempt_id'] != task['latest_attempt_id'] or rec['task_revision'] != task['revision']
                    or rec['workspace_digest'] != q['workspace_digest'] or rec['receipt_id'] == q['reconciliation_receipt_id']
                    or rec['created_at'] <= peer['updated_at'] or checked.tzinfo is None or checked < peer['updated_at']
                    or proof.get('scope_stopped') is not True or proof.get('source') != 'systemd' or proof.get('unit_name') != attempt['unit_name']
                    or attempt['phase'] != 'stopped' or attempt['reservation_active'] or attempt['lease_expires_at'] is not None
                    or attempt['published_at'] is None or attempt['result_sha256'] != q['parent_result_sha256']
                    or attempt['recipe_digest'] != recipe.digest or attempt['pinned_recipe'] != recipe.to_dict()):
                raise Rejected(409, 'reconcile_required')
            cursor.execute("SELECT EXISTS(SELECT 1 FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id) WHERE t.workflow_id=%s AND(a.reservation_active OR a.lease_expires_at IS NOT NULL)) OR EXISTS(SELECT 1 FROM forge_agent.coding_tasks WHERE workflow_id=%s AND status IN('queued','running','cancel_requested')) OR EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND status IN('pending','leased')) OR EXISTS(SELECT 1 FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(p.owner_id,p.project_alias,p.task_id) WHERE t.workflow_id=%s AND p.status IN('pending','checking','publishing')) OR EXISTS(SELECT 1 FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id) WHERE t.workflow_id=%s AND q.status IN('pending','checking')) AS busy", (w['workflow_id'],)*5)
            if cursor.fetchone()['busy']:
                raise Rejected(409, 'reconcile_required')
            cursor.execute('SELECT * FROM forge_agent.coding_review_budget WHERE owner_id=%s AND project_alias=%s AND root_task_id=%s',
                (store.owner, command.project_alias, task['repair_root_task_id'] or task['id']))
            budget = cursor.fetchone()
            exhausted = flow._exhausted_code_budget(cursor, task, q)
            if not budget or not exhausted or handoff.get('budget') != exhausted or budget['policy'] != p['repair_policy']:
                raise Rejected(409, 'not_reviewable')
            require_owner_capability(cursor)
            cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase='implement',escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=%s", (w['workflow_id'],))
            for profile, mode in ((p['author_profile'], 'implement'), (p['reviewer_profile'], 'review')):
                if not flow.config.profiles[profile].execution_enabled:
                    raise Rejected(403, 'denied')
                flow._grant(cursor, w, profile, mode)
            cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND policy_digest=ANY(%s) AND observed_at>=clock_timestamp()-interval '90 seconds'", (store.owner, [x[0] for x in NATIVE_POLICIES.values()]))
            ready = cursor.fetchall()
            if any(not _ready_for_recipe(ready, command.project_alias, profile, mode, recipe_from_snapshot(p, entry))
                    for profile, mode, entry in ((p['author_profile'], 'implement', 'author_implement'), (p['reviewer_profile'], 'review', 'reviewer_review'))):
                raise Rejected(503, 'unavailable')
            cursor.execute("SELECT count(*) AS n FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND status IN('queued','running','cancel_requested')", (store.owner, command.project_alias))
            if cursor.fetchone()['n'] >= store.max_pending:
                raise Rejected(503, 'queue_full')
            findings = validate_findings(verdict['findings'], public=True)
            ledger = {x['finding_id']: x for x in q['findings'] if x.get('status', 'open') == 'open'}
            ledger.update({x['finding_id']: x for x in findings})
            ledger = validate_findings(list(ledger.values()), public=True)
            cursor.execute("UPDATE forge_agent.coding_reviews SET findings=%s,status='repair',summary=%s,updated_at=clock_timestamp() WHERE review_id=%s", (Json(ledger), verdict['summary'], q['review_id']))
            # Keep the old repair_source_review_id. This canonical SQL therefore
            # binds the feedback to q.workspace_digest and native_receipt.
            cursor.execute("SELECT forge_agent.coding_make_feedback(%s,%s,'repair',%s,%s) AS id", (q['review_id'], task['id'], verdict['summary'], Json(findings)))
            feedback_id = cursor.fetchone()['id']
            cursor.execute('SELECT feedback_sha256,feedback_json FROM forge_agent.coding_feedback WHERE feedback_id=%s', (feedback_id,))
            feedback = cursor.fetchone()
            raw_feedback = json.loads(feedback['feedback_json'])
            if (raw_feedback['workspace_digest'] != q['workspace_digest'] or raw_feedback.get('workspace_digest_kind') != 'native_receipt'
                    or raw_feedback['findings'] != findings):
                raise Rejected(409, 'result_mismatch')
            cursor.execute('UPDATE forge_agent.coding_reviews SET feedback_id=%s WHERE review_id=%s', (feedback_id, q['review_id']))
            context = 'Owner authorized ONE continuation; no automatic repair allowance renewed. Source review='+str(q['review_id'])+'; peer verdict sha256='+peer['verdict_sha256']+'; context only: '+command.note
            cursor.execute("""UPDATE forge_agent.coding_tasks SET status='queued',revision=revision+1,resume_workspace_digest=%s,
                feedback_id=%s,feedback_sha256=%s,legacy_resume_note=%s,auto_repair=false,
                workflow_feedback_review_id=NULL,workflow_feedback_sha256=NULL,updated_at=clock_timestamp()
                WHERE owner_id=%s AND project_alias=%s AND id=%s RETURNING revision""",
                (q['workspace_digest'], feedback_id, feedback['feedback_sha256'], context, store.owner, command.project_alias, task['id']))
            revision = cursor.fetchone()['revision']
            receipt = dict(project_alias=command.project_alias, task_id=str(task['id']), base_revision=command.expected_revision,
                revision=revision, status='queued', recipe_digest=recipe.digest)
            intent = dict(policy=OWNER_CODE_RESUME, request=request, workflow_id=str(w['workflow_id']), policy_digest=w['policy_digest'],
                review_id=str(q['review_id']), source_attempt_id=str(task['latest_attempt_id']), source_result_sha256=q['parent_result_sha256'],
                source_workspace_digest=q['workspace_digest'], candidate_commit=q['candidate_commit'], gate_manifest_sha256=q['gate_manifest_sha256'],
                verification_sha256=sha256_canonical(q['verification']), peer_review_id=str(peer['workflow_review_id']),
                target_sha256=peer['target_sha256'], verdict_sha256=peer['verdict_sha256'], reviewer_attempt_id=str(peer['reviewer_attempt_id']),
                reviewer_result_sha256=peer['reviewer_result_sha256'], plan_decision_id=str(plan['decision_id']), plan_sha256=plan['plan_sha256'],
                recipe_digest=recipe.digest, repair_rounds=budget['repair_rounds'], stagnant_rounds=budget['stagnant_rounds'],
                review_stagnant_rounds=q['stagnant_rounds'], infrastructure_retries=q['infrastructure_retries'],
                plan_revisions=w['plan_revisions'], review_round=q['round'], auto_repair=task['auto_repair'], repair_policy=budget['policy'])
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt) VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",
                (w['workflow_id'], store.owner, command.project_alias, owner_source_key(w['workflow_id']), Json(intent), Json(dict(status='owner_continued', receipt=receipt))))
            cursor.execute("INSERT INTO forge_agent.coding_control_receipts(owner_id,project_alias,task_id,action,idempotency_key,intent_digest,base_revision,result_revision,receipt_json) VALUES(%s,%s,%s,'resume',%s,%s,%s,%s,%s)",
                (store.owner, command.project_alias, task['id'], command.idempotency_key, digest, command.expected_revision, revision, Json(receipt)))
            payload = dict(receipt, policy=OWNER_CODE_RESUME, workflow_id=str(w['workflow_id']), review_id=str(q['review_id']),
                verdict_sha256=peer['verdict_sha256'], feedback_id=str(feedback_id), reconciliation_receipt_id=str(rec['receipt_id']),
                workspace_digest=q['workspace_digest'], spent_budget=exhausted, auto_repair=False)
            cursor.execute("INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload) SELECT %s,%s,%s,coalesce(max(sequence),0)+1,'resumed',%s FROM forge_agent.coding_events WHERE owner_id=%s AND project_alias=%s AND task_id=%s",
                (store.owner, command.project_alias, task['id'], Json(payload), store.owner, command.project_alias, task['id']))
            cursor.execute("SELECT pg_notify('forge_coding_jobs','')")
            return receipt


class NativeOwnerControlFactory:
    def __init__(self, config, catalog, store):
        self.config, self.catalog, self.store = config, catalog, store

    def create(self):
        from work_workflow import WorkflowStore
        return NativeOwnerControl(WorkflowStore(self.config, self.catalog, self.store))
