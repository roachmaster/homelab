"""DB-only, once-only native feedback delivery recovery through normal Resume."""
from dataclasses import dataclass
from datetime import datetime, timezone
import re
from uuid import UUID

from psycopg2.extras import Json
from queue_store import Rejected
from work_feedback import parse_feedback_json
from work_context_recovery_contract import POLICY, RECOVERY_SUMMARY, LEGACY_SUMMARY, make_recovery_note
from work_retry_policy import AGENT_RETRY_LIMIT
from work_events import CONTEXT_RECOVERY_ACTOR, insert_event

MAX_RECOVERIES = AGENT_RETRY_LIMIT
BACKOFF_SECONDS = 60


class FeedbackRecovery:
    POLICY_ID = POLICY
    FAILURE_SUMMARIES = (RECOVERY_SUMMARY, LEGACY_SUMMARY)
    ACTION_PREFIX = 'feedback-delivery-retry:'
    IDEMPOTENCY_PREFIX = 'feedback-delivery-'
    AUDIT_STAGE = 'feedback_delivery_recovered'
    RECOVERY_DESCRIPTION = 'Verified build feedback delivery resumed the same approved task.'
    # Both selected consumers account for the same logical feedback event.
    ACCOUNTING_PREFIXES = ('feedback-delivery-retry:%', 'peer-feedback-delivery-retry:%')

    def __init__(self, flow):
        self.flow = flow
        self.store = flow.store

    def _validate_failure_context(self, cursor, w, task, attempt, rec, feedback, packet, review, source):
        return (review['verification'].get('checks_passed') is False
            and review['verification'].get('integration_base') == feedback['integration_base']
            and isinstance(review['verification'].get('delivery_failure', {}).get('build'), dict))

    def retry(self, *, project_alias, task_id, expected_revision, reconciliation_receipt_id,
              idempotency_key):
        from work_workflow import BoundNativeRetryAuthority, NATIVE_POLICIES, recipe_from_snapshot, _ready_for_recipe
        self.store._require_project(project_alias)
        if (not isinstance(task_id, UUID) or not isinstance(reconciliation_receipt_id, UUID)
                or type(expected_revision) is not int or expected_revision < 1
                or not isinstance(idempotency_key, str) or not 1 <= len(idempotency_key) <= 128
                or any(c not in 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_.-' for c in idempotency_key)):
            raise Rejected(400, 'invalid_request')
        request = dict(project_alias=project_alias, task_id=str(task_id), expected_revision=expected_revision,
            reconciliation_receipt_id=str(reconciliation_receipt_id), idempotency_key=idempotency_key)
        with self.flow._transaction() as cursor:
            self.flow._project_lock(cursor, project_alias)
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE',
                (self.store.owner, project_alias, task_id))
            task = cursor.fetchone()
            if not task:
                raise Rejected(404, 'not_found')
            cursor.execute('SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND receipt_id=%s',
                (self.store.owner, project_alias, task_id, reconciliation_receipt_id))
            rec = cursor.fetchone()
            if not rec:
                raise Rejected(409, 'reconcile_required')
            key = self.ACTION_PREFIX + str(rec['attempt_id'])
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key=%s",
                (task['workflow_id'], key))
            previous = cursor.fetchone()
            if previous:
                if previous['intent'].get('request') != request:
                    raise Rejected(409, 'idempotency_conflict')
                receipt = previous['finish_receipt'] or {}
                if (previous['status'] != 'completed' or previous['intent'].get('policy') != self.POLICY_ID
                        or receipt.get('status') != self.AUDIT_STAGE or not isinstance(receipt.get('receipt'), dict)):
                    raise Rejected(409, 'reconcile_required')
                return dict(receipt['receipt'])
            if task['revision'] != expected_revision:
                raise Rejected(409, 'revision_conflict')
            cursor.execute('SELECT * FROM forge_agent.coding_workflows WHERE owner_id=%s AND project_alias=%s AND workflow_id=%s FOR UPDATE',
                (self.store.owner, project_alias, task['workflow_id']))
            w = cursor.fetchone()
            if (not w or w['phase'] not in {'implement', 'repair', 'escalated'}
                    or (w['phase'] == 'escalated' and w['escalation_reason'] != 'Agent stopped: needs_reconcile')
                    or (w['phase'] != 'escalated' and not w['enabled'])):
                raise Rejected(409, 'not_reviewable')
            self.flow._current_policy(w)
            p = w['snapshot']
            recipe = recipe_from_snapshot(p, 'author_implement')
            if any(profile not in self.flow.config.profiles or not self.flow.config.profiles[profile].execution_enabled
                    for profile in (p['author_profile'], p['reviewer_profile'])):
                raise Rejected(403, 'denied')
            if (not task['workflow_required'] or task['workflow_review_id'] is not None
                    or task['mode'] != 'implement' or task['status'] != 'needs_reconcile'
                    or task['cancelled_at'] is not None or task['latest_attempt_id'] != rec['attempt_id']
                    or rec['task_revision'] != task['revision'] or not task['feedback_id']
                    or not (task['plan_decision_id'] or task['scope_plan_decision_id'])
                    or task['agent_profile'] != p['author_profile'] or task['pinned_recipe'] != recipe.to_dict()
                    or task['recipe_digest'] != recipe.digest):
                raise Rejected(409, 'not_reviewable')
            cursor.execute('SELECT * FROM forge_agent.coding_attempts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s',
                (self.store.owner, project_alias, task_id, rec['attempt_id']))
            attempt = cursor.fetchone()
            if (not attempt or attempt['phase'] != 'stopped' or attempt['reservation_active']
                    or attempt['lease_expires_at'] is not None or attempt['outcome'] != 'needs_reconcile'
                    or attempt['summary'] not in self.FAILURE_SUMMARIES
                    or attempt['result_sha256'] is not None or attempt['result_byte_count'] is not None
                    or attempt['result_chunk_count'] is not None or attempt['finished_at'] is None
                    or attempt['published_at'] is None or attempt['feedback_id'] != task['feedback_id']
                    or attempt['feedback_sha256'] != task['feedback_sha256']
                    or attempt['resume_workspace_digest'] != rec['workspace_digest']
                    or attempt['pinned_recipe'] != recipe.to_dict() or attempt['recipe_digest'] != recipe.digest
                    or attempt['plan_decision_id'] != task['plan_decision_id']
                    or attempt['scope_plan_decision_id'] != task['scope_plan_decision_id']):
                raise Rejected(409, 'reconcile_required')
            if (not isinstance(attempt['outcome_digest'], str) or not re.fullmatch('[0-9a-f]{64}', attempt['outcome_digest'])
                    or attempt['publish_receipt'] != dict(result=None, status='needs_reconcile', outcome='needs_reconcile',
                        task_id=str(task_id), revision=expected_revision, attempt_id=str(attempt['attempt_id']),
                        project_alias=project_alias, outcome_digest=attempt['outcome_digest'])):
                raise Rejected(409, 'reconcile_required')
            proof = rec['stop_proof']
            try:
                checked = datetime.fromisoformat(proof['checked_at'].replace('Z', '+00:00'))
                valid_stop = (checked.tzinfo is not None and attempt['finished_at'] <= checked <= datetime.now(timezone.utc)
                    and rec['created_at'] >= attempt['finished_at'] and proof['scope_stopped'] is True
                    and proof['source'] == 'systemd' and proof['unit_name'] == attempt['unit_name'])
            except (ValueError, TypeError, KeyError, AttributeError):
                valid_stop = False
            if not valid_stop:
                raise Rejected(409, 'reconcile_required')
            cursor.execute("SELECT event_id FROM forge_agent.coding_events WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND event_kind='needs_reconcile' AND payload->>'attempt_id'=%s AND created_at>=%s ORDER BY sequence DESC LIMIT 1",
                (self.store.owner, project_alias, task_id, str(attempt['attempt_id']), attempt['finished_at']))
            witness = cursor.fetchone()
            if not witness:
                raise Rejected(409, 'not_reviewable')
            cursor.execute('SELECT * FROM forge_agent.coding_feedback WHERE feedback_id=%s AND owner_id=%s AND project_alias=%s AND task_id=%s',
                (task['feedback_id'], self.store.owner, project_alias, task_id))
            packet = cursor.fetchone()
            try:
                feedback = parse_feedback_json(packet['feedback_json'], task['feedback_sha256'])
                valid_packet = (packet['feedback_sha256'] == task['feedback_sha256']
                    and feedback['workspace_digest_kind'] == 'native_receipt' and feedback['disposition'] == 'repair'
                    and feedback['feedback_id'] == str(task['feedback_id']) and feedback['task_id'] == str(task_id)
                    and feedback['agent_profile'] == task['agent_profile']
                    and feedback['workspace_digest'] == rec['workspace_digest']
                    and feedback['plan_decision_id'] == str(task['plan_decision_id'] or task['scope_plan_decision_id'])
                    and feedback['integration_base'] == task['base_revision'])
            except (ValueError, TypeError, KeyError, AttributeError):
                valid_packet = False
            if not valid_packet:
                raise Rejected(409, 'result_mismatch')
            cursor.execute('SELECT * FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND review_id=%s',
                (self.store.owner, project_alias, task_id, packet['review_id']))
            review = cursor.fetchone()
            source = self.flow._artifact(cursor, task_id, packet['parent_attempt_id'])
            if (not review or review['status'] != 'repair' or review['feedback_id'] != task['feedback_id']
                    or review['attempt_id'] != packet['parent_attempt_id'] or review['parent_result_sha256'] != source['result_sha256']
                    or review['parent_result_sha256'] != feedback['parent_result_sha256']
                    or review['source_revision'] != feedback['source_revision']
                    or review['base_revision'] != task['base_revision']
                    or review['plan_decision_id'] != task['plan_decision_id']
                    or review['scope_plan_decision_id'] != task['scope_plan_decision_id']
                    or review['candidate_commit'] != feedback['candidate_commit']
                    or review['gate_manifest_sha256'] != feedback['gate_manifest_sha256']
                    or review['workspace_digest'] != rec['workspace_digest']
                    or review['verification'].get('cleanup_confirmed') is not True
                    or not self._validate_failure_context(cursor, w, task, attempt, rec,
                        feedback, packet, review, source)):
                raise Rejected(409, 'result_mismatch')
            cursor.execute("SELECT EXISTS(SELECT 1 FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id) WHERE t.workflow_id=%s AND(a.reservation_active OR a.lease_expires_at IS NOT NULL)) OR EXISTS(SELECT 1 FROM forge_agent.coding_tasks WHERE workflow_id=%s AND(status IN('queued','running','cancel_requested') OR(id<>%s AND status IN('failed','needs_reconcile','quota_blocked','blocked_auth','blocked_permission','cancelled')))) OR EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND status IN('pending','leased')) OR EXISTS(SELECT 1 FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(p.owner_id,p.project_alias,p.task_id) WHERE t.workflow_id=%s AND p.status IN('pending','checking','publishing')) OR EXISTS(SELECT 1 FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id) WHERE t.workflow_id=%s AND q.status IN('pending','checking')) AS busy",
                (w['workflow_id'], w['workflow_id'], task_id, w['workflow_id'], w['workflow_id'], w['workflow_id']))
            if cursor.fetchone()['busy']:
                raise Rejected(409, 'reconcile_required')
            cursor.execute("SELECT count(*) AS used,max(created_at) AS last,coalesce(max(created_at)<=clock_timestamp()-interval '60 seconds',true) AS ready FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key LIKE ANY(%s)",
                (w['workflow_id'], list(self.ACCOUNTING_PREFIXES)))
            budget = cursor.fetchone()
            events = self.flow.retry_events
            if (events is None and budget['used'] >= MAX_RECOVERIES) or not budget['ready']:
                raise Rejected(409, 'not_reviewable')
            if budget['used']:
                cursor.execute("SELECT %s<=clock_timestamp()-interval '60 seconds' AS ready", (attempt['finished_at'],))
                if not cursor.fetchone()['ready']:
                    raise Rejected(409, 'not_reviewable')
            event = None
            if events is not None:
                from work_retry_events import RetryScope
                scope = RetryScope(self.store.owner, project_alias, 'native_workflow', w['workflow_id'])
                event = events.observe_failure(cursor, scope, 'feedback_delivery', str(attempt['attempt_id']),
                    attempt['outcome_digest'], phase_id='feedback.' + str(task['feedback_id']))
                cursor.execute("SELECT count(*) AS used FROM forge_agent.coding_workflow_actions "
                    "WHERE workflow_id=%s AND kind='decide' AND status='completed' "
                    "AND source_key LIKE ANY(%s) AND intent->>'feedback_id'=%s",
                    (w['workflow_id'], list(self.ACCOUNTING_PREFIXES), str(task['feedback_id'])))
                prior_used = cursor.fetchone()['used']
                if event['used'] == 0 and prior_used:
                    event = events.adopt_legacy(cursor, event['event_id'], prior_used,
                        str(attempt['attempt_id']), attempt['outcome_digest'])
                if event['used'] >= event['limit']:
                    event = events.reserve(cursor, event['event_id'], key)
                    return dict(status='needs_attention', reason='feedback_delivery_event_exhausted',
                        workflow_id=str(w['workflow_id']), retry_event=event)
            cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase='repair',escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=%s", (w['workflow_id'],))
            self.flow._grant(cursor, w, p['author_profile'], 'implement')
            self.flow._grant(cursor, w, p['reviewer_profile'], 'review')
            cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND policy_digest=ANY(%s) AND observed_at>=clock_timestamp()-interval '90 seconds'",
                (self.store.owner, [x[0] for x in NATIVE_POLICIES.values()]))
            ready = cursor.fetchall()
            for role, mode, entry in ((p['author_profile'], 'implement', 'author_implement'), (p['reviewer_profile'], 'review', 'reviewer_review')):
                if not _ready_for_recipe(ready, project_alias, role, mode, recipe_from_snapshot(p, entry)):
                    raise Rejected(503, 'unavailable')
            note = make_recovery_note(workflow_id=w['workflow_id'], task_id=task_id,
                stopped_attempt_id=attempt['attempt_id'], feedback_id=task['feedback_id'],
                feedback_sha256=task['feedback_sha256'], feedback_parent_attempt_id=packet['parent_attempt_id'],
                workspace_digest=rec['workspace_digest'], reconciliation_receipt_id=rec['receipt_id'],
                base_revision=task['base_revision'])
            authority = BoundNativeRetryAuthority(self.store, cursor, w['workflow_id'], task_id, attempt['attempt_id'], task['revision'])
            if event is not None:
                event = events.reserve(cursor, event['event_id'], key)
                if event['status'] == 'exhausted':
                    raise Rejected(409, 'not_reviewable')
            receipt = self.store.resume(project_alias=project_alias, task_id=task_id, expected_revision=expected_revision,
                idempotency_key=idempotency_key, agent_profile=p['author_profile'], reconciliation_receipt_id=reconciliation_receipt_id,
                note=note, allowed_modes={'implement'}, resolve_recipe=lambda _: recipe, _authority=authority, actor=CONTEXT_RECOVERY_ACTOR)
            intent = dict(policy=self.POLICY_ID, request=request, source_attempt_id=str(attempt['attempt_id']),
                source_event_id=str(witness['event_id']), feedback_id=str(task['feedback_id']),
                feedback_sha256=task['feedback_sha256'], workspace_digest=rec['workspace_digest'], max_recoveries=MAX_RECOVERIES)
            if event is not None:
                intent['retry_event'] = event
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt) VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",
                (w['workflow_id'], self.store.owner, project_alias, key, Json(intent), Json({'status': self.AUDIT_STAGE, 'receipt': receipt})))
            insert_event(cursor, self.store.owner, project_alias, task_id, 'checkpoint',
                         dict(stage=self.AUDIT_STAGE, policy=self.POLICY_ID,
                    source_attempt_id=str(attempt['attempt_id']), source_event_id=str(witness['event_id']),
                    reconciliation_receipt_id=str(rec['receipt_id']), feedback_id=str(task['feedback_id']),
                    summary=self.RECOVERY_DESCRIPTION, receipt=receipt),
                         actor=CONTEXT_RECOVERY_ACTOR)
            return receipt

    def run_once(self):
        with self.store._transaction(read_only=True) as cursor:
            cursor.execute("""SELECT t.project_alias,t.id,t.revision,r.receipt_id,a.attempt_id
                FROM forge_agent.coding_tasks t JOIN forge_agent.coding_workflows w
                ON(w.owner_id,w.project_alias,w.workflow_id)=(t.owner_id,t.project_alias,t.workflow_id)
                JOIN forge_agent.coding_attempts a ON(a.owner_id,a.project_alias,a.task_id,a.attempt_id)=(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)
                JOIN forge_agent.coding_reconciliation_receipts r ON(r.owner_id,r.project_alias,r.task_id,r.attempt_id,r.task_revision)=(t.owner_id,t.project_alias,t.id,t.latest_attempt_id,t.revision)
                WHERE t.owner_id=%s AND t.project_alias=ANY(%s) AND t.workflow_required AND t.mode='implement'
                AND t.status='needs_reconcile' AND t.feedback_id IS NOT NULL AND a.outcome='needs_reconcile'
                AND a.summary=ANY(%s) AND a.phase='stopped' AND NOT a.reservation_active
                AND a.lease_expires_at IS NULL AND a.result_sha256 IS NULL
                AND w.phase IN('implement','repair','escalated') ORDER BY r.created_at DESC LIMIT 64""",
                (self.store.owner, list(self.store.allowed_projects), list(self.FAILURE_SUMMARIES)))
            candidates = cursor.fetchall()
        changed = False
        for task in candidates:
            try:
                receipt = self.retry(project_alias=task['project_alias'], task_id=task['id'], expected_revision=task['revision'],
                    reconciliation_receipt_id=task['receipt_id'], idempotency_key=self.IDEMPOTENCY_PREFIX + str(task['attempt_id']))
                changed = changed or receipt.get('status') == 'queued'
            except Rejected:
                pass
        return changed


@dataclass(frozen=True)
class FeedbackRecoveryFactory:
    flow: object

    def create(self):
        config = self.flow.config
        return FeedbackRecovery(self.flow) if config is not None and config.project_workflows else None
