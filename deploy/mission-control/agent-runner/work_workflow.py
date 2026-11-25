"""DB-only autonomous workflow coordinator, selected at API bootstrap.

Native tasks run through the ordinary worker. This coordinator owns delegated
state transitions; it has no process, filesystem-source or GitHub ports.
"""
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import datetime
import hashlib
import json
import logging
import select
import time
import threading
import re
from uuid import UUID,uuid4
import psycopg2
from psycopg2.extras import Json
from queue_store import Rejected
from work_config import PinnedRecipe,sha256_canonical,prepare_title
from work_policy import POLICY_ID,POLICY_DIGEST,NATIVE_POLICIES,native_policy_matches
from work_store import ADVISORY_LOCK_SALT
from work_retry_policy import AGENT_RETRY_LIMIT, budget_compatible_snapshot, effective_limit
from work_events import COORDINATOR_ACTOR, insert_event

LOG=logging.getLogger('forge-workflow')
_INHERIT_RETRY_EVENTS=object()
MAX_TARGET_BYTES=64*1024*1024+65536
NATIVE_DEADLINE_POLICY='forge-native-deadline-recovery-v1'
NATIVE_DEADLINE_RETRIES=1
NATIVE_DEADLINE_EVENT_POLICY='forge-native-deadline-event-recovery-v1'
PEER_WIRE_REPLAY_POLICY='forge-peer-wire-replay-v1'
PEER_CORRECTION_POLICY='forge-peer-correction-v1'
PEER_WIRE_ERROR='Peer review could not safely continue: PeerReviewError'
NATIVE_DEADLINE_NOTE=(
    'The previous writer stopped at its deadline; the reconciled workspace is preserved. '
    'Continue the SAME approved Plan. Inspect existing files and retained evidence, preserve completed work, '
    'finish focused checks and publish the final report promptly. Do not repeat full cold repository CI in '
    'this native turn: the independent verifier still runs ALL required fixed CI afterwards. '
    'Do not weaken checks, add skips, commit, push or change scope.')


def recipe_from_snapshot(snapshot,key):
    entry=snapshot['recipes'][key];value=entry['recipe']
    return PinnedRecipe(**value,digest=entry['digest'])


def _ready_for_recipe(rows,project_alias,profile_alias,mode,recipe):
    """Match each role to its own frozen execution identity, never another worker."""
    if recipe.version!=2 or not native_policy_matches(recipe.resource_profile,recipe.backend,recipe.policy_digest):
        return False
    return any(row.get('policy_digest')==recipe.policy_digest
        and any(x.get('alias')==project_alias for x in row['projects'])
        and any(x.get('alias')==profile_alias and x.get('backend')==recipe.backend
            and x.get('model')==recipe.model and mode in x.get('modes',[]) for x in row['profiles'])
        for row in rows)


@dataclass(frozen=True)
class BoundDelegatedAuthority:
    """Unserializable server-only capability valid inside one locked transaction."""
    store:object
    cursor:object
    workflow:dict
    action:dict
    review:dict
    verdict:dict

    def validate(self,cursor,task,kind):
        if cursor is not self.cursor or task['workflow_id']!=self.workflow['workflow_id'] or self.review['source_task_id']!=task['id'] or self.review['target_kind']!=kind:
            raise Rejected(403,'denied')
        cursor.execute("SELECT status,lease_token,lease_expires_at>clock_timestamp() AS valid FROM forge_agent.coding_workflow_actions WHERE action_id=%s FOR UPDATE",(self.action['action_id'],))
        current=cursor.fetchone()
        if not current or current['status']!='leased' or current['lease_token']!=self.action['lease_token'] or not current['valid']:
            raise Rejected(409,'reconcile_required')
        if task['status']!='awaiting_review' or task['revision']!=self.review['source_revision'] or task['latest_attempt_id']!=self.review['source_attempt_id']:
            raise Rejected(409,'result_mismatch')
        cursor.execute("SELECT enabled,phase FROM forge_agent.coding_workflows WHERE workflow_id=%s",(self.workflow['workflow_id'],))
        state=cursor.fetchone()
        if not state or not state['enabled'] or state['phase'] in {'cancelled','completed','escalated'}:
            raise Rejected(403,'denied')
        if self.verdict['source_result_sha256']!=self.review['source_result_sha256']:
            raise Rejected(409,'result_mismatch')

    def attestation(self):
        return {'workflow_id':str(self.workflow['workflow_id']),'workflow_review_id':str(self.review['workflow_review_id']),
                'reviewer_task_id':str(self.review['reviewer_task_id']),'reviewer_attempt_id':str(self.review['reviewer_attempt_id']),
                'reviewer_result_sha256':self.review['reviewer_result_sha256'],'policy_digest':self.workflow['policy_digest']}


@dataclass(frozen=True)
class BoundNativeRetryAuthority:
    """Server-only capability for one locked, receipt-bound deadline recovery."""
    store:object
    cursor:object
    workflow_id:object
    task_id:object
    attempt_id:object
    revision:int

    def validate(self,cursor,task,kind):
        if (cursor is not self.cursor or kind!='native_timeout' or task['id']!=self.task_id
            or task.get('workflow_id')!=self.workflow_id or task['latest_attempt_id']!=self.attempt_id
            or task['revision']!=self.revision or task['mode']!='implement'
            or task['status'] not in {'needs_reconcile','failed'}):
            raise Rejected(403,'denied')


@dataclass(frozen=True)
class BoundPreparationRetryAuthority:
    """One existing-budget check retry; never code-review approval authority."""
    store:object
    cursor:object
    workflow_id:object
    task_id:object
    attempt_id:object
    revision:int
    review_id:object

    def validate(self,cursor,task,kind):
        if (cursor is not self.cursor or kind!='code' or task['id']!=self.task_id
            or task.get('workflow_id')!=self.workflow_id or task['latest_attempt_id']!=self.attempt_id
            or task['revision']!=self.revision or task['mode']!='implement' or task['status']!='awaiting_review'):
            raise Rejected(403,'denied')

    def validate_review_decision(self,decision,review_id):
        if decision!='retry_infrastructure' or review_id!=self.review_id:
            raise Rejected(403,'denied')


class WorkflowStore:
    def __init__(self,config,catalog,store,*,retry_events=_INHERIT_RETRY_EVENTS):
        self.retry_events=getattr(store,'retry_events',None) if retry_events is _INHERIT_RETRY_EVENTS else retry_events
        self.config=config;self.catalog=catalog;self.store=store
        self.reviewer_correction = None
        self.feedback_recovery = None
        self.peer_feedback_recovery = None
        self.superseding_review = None
        self._retry_allowance_position = None
        self._manual_allowance_position = None

    @contextmanager
    def _transaction(self):
        with self.store._transaction() as cursor:
            cursor.execute("SET LOCAL lock_timeout='5s'")
            cursor.execute("SET LOCAL statement_timeout='10s'")
            yield cursor

    @contextmanager
    def listen(self):
        connection=self.store._connect_factory()
        try:
            connection.autocommit=True
            with connection.cursor() as cursor:cursor.execute("LISTEN forge_coding_jobs")
            yield connection
        finally:connection.close()

    @staticmethod
    def wait(listener,timeout,stop):
        deadline=time.monotonic()+max(0,timeout)
        while not stop.is_set():
            remaining=deadline-time.monotonic()
            if remaining<=0:return False
            if select.select([listener],[],[],min(1,remaining))[0]:
                listener.poll();notified=bool(listener.notifies);listener.notifies.clear()
                if notified:return True
        return False

    def _project_lock(self,cursor,project):
        cursor.execute("SELECT pg_advisory_xact_lock(hashtextextended(%s::text,%s::bigint))",(self.store.owner+"\n"+project,ADVISORY_LOCK_SALT))

    def _current_policy(self,workflow):
        policy=self.config.project_workflows.get(workflow['project_alias'])
        if policy is None:
            raise Rejected(403,'denied')
        current=policy.snapshot(self.catalog,self.config.repair_policy)
        if current['policy_digest']!=workflow['policy_digest'] and not budget_compatible_snapshot(workflow['snapshot'],current):
            raise Rejected(403,'denied')
        if not {'plan','review','implement'}<=self.config.allowed_modes:
            raise Rejected(403,'denied')
        return policy

    def _retry_limit(self,workflow,key):
        policy=self._current_policy(workflow)
        return effective_limit(workflow['snapshot'],key,policy.snapshot(self.catalog,self.config.repair_policy))

    def _repair_limit(self,workflow,key):
        self._current_policy(workflow)
        return self.config.repair_policy[key]

    def _apply_retry_allowances(self):
        # Only the explicitly selected common twenty-attempt policy widens old
        # ledgers. The database function journals the old/new allowance once;
        # snapshots, recipes, attempts and all spent counters remain unchanged.
        if self.retry_events is not None:return
        if any(self.config.repair_policy[k]!=AGENT_RETRY_LIMIT for k in
               ('max_rounds','max_stagnant_rounds','max_infrastructure_retries')):
            return
        with self._transaction() as cursor:
            cursor.execute("SELECT to_regprocedure('forge_agent.amend_agent_retry_policy(text,uuid,jsonb)') IS NOT NULL AS available")
            if not cursor.fetchone()['available']:
                return
            cursor.execute("""SELECT w.* FROM forge_agent.coding_workflows w
              JOIN forge_agent.coding_review_budget b ON(b.owner_id,b.project_alias,b.root_task_id)=(w.owner_id,w.project_alias,w.root_task_id)
              WHERE w.owner_id=%s AND w.project_alias=ANY(%s) AND w.phase NOT IN('completed','cancelled')
              AND ((w.snapshot->>'max_plan_revisions')::integer<20 OR (w.snapshot->>'max_code_revisions')::integer<20
                OR b.policy->'max_rounds'<>'20'::jsonb OR b.policy->'max_stagnant_rounds'<>'20'::jsonb OR b.policy->'max_infrastructure_retries'<>'20'::jsonb)
              AND NOT EXISTS(SELECT FROM forge_agent.coding_events e WHERE(e.owner_id,e.project_alias,e.task_id)=(w.owner_id,w.project_alias,w.root_task_id)
                AND e.event_kind='checkpoint' AND e.payload->>'stage'='retry_policy_updated'
                AND e.payload->>'policy'='forge-agent-retry-v1' AND e.payload->>'scope_id'=w.workflow_id::text)
              AND (%s::timestamptz IS NULL OR (w.created_at,w.workflow_id)>(%s::timestamptz,%s::uuid))
              ORDER BY w.created_at,w.workflow_id LIMIT 64""",(self.store.owner,list(self.store.allowed_projects)) +
                ((self._retry_allowance_position[0],*self._retry_allowance_position) if self._retry_allowance_position else (None,None,None)))
            rows=cursor.fetchall()
            self._retry_allowance_position=(rows[-1]['created_at'],rows[-1]['workflow_id']) if len(rows)==64 else None
            for w in rows:
                try:
                    policy=self._current_policy(w)
                except Rejected:
                    continue
                if policy.max_plan_revisions!=AGENT_RETRY_LIMIT:
                    continue
                if any(not self.config.profiles[p].execution_enabled for p in (policy.author_profile,policy.reviewer_profile)):
                    continue
                self._project_lock(cursor,w['project_alias'])
                self._amend_retry_allowance(cursor,w['project_alias'],w['root_task_id'])
            cursor.execute("""SELECT t.project_alias,t.id,t.created_at,t.agent_profile,t.mode,b.policy FROM forge_agent.coding_tasks t
              JOIN forge_agent.coding_review_budget b ON(b.owner_id,b.project_alias,b.root_task_id)=(t.owner_id,t.project_alias,t.id)
              WHERE t.owner_id=%s AND t.project_alias=ANY(%s) AND t.workflow_id IS NULL
              AND (t.status NOT IN('completed','cancelled') OR EXISTS(SELECT FROM forge_agent.coding_tasks child
                WHERE(child.owner_id,child.project_alias,child.repair_root_task_id)=(t.owner_id,t.project_alias,t.id)
                AND child.status NOT IN('completed','cancelled')))
              AND b.policy IS DISTINCT FROM %s
              AND (b.policy-ARRAY['max_rounds','max_stagnant_rounds','max_infrastructure_retries'])=
                (%s::jsonb-ARRAY['max_rounds','max_stagnant_rounds','max_infrastructure_retries'])
              AND (%s::timestamptz IS NULL OR (t.created_at,t.id)>(%s::timestamptz,%s::uuid))
              ORDER BY t.created_at,t.id LIMIT 64""",(self.store.owner,list(self.store.allowed_projects),
                Json(dict(self.config.repair_policy)),Json(dict(self.config.repair_policy))) +
                ((self._manual_allowance_position[0],*self._manual_allowance_position) if self._manual_allowance_position else (None,None,None)))
            manual_rows=cursor.fetchall()
            self._manual_allowance_position=(manual_rows[-1]['created_at'],manual_rows[-1]['id']) if len(manual_rows)==64 else None
            for row in manual_rows:
                profile=self.config.profiles.get(row['agent_profile'])
                if profile is None or not profile.execution_enabled or row['mode'] not in self.config.allowed_modes:
                    continue
                self._project_lock(cursor,row['project_alias'])
                self._amend_retry_allowance(cursor,row['project_alias'],row['id'])

    def _amend_retry_allowance(self,cursor,project,root):
        # A concurrent grant revocation or historical orphan must not roll back
        # unrelated amendments. Keep grants private to the narrow SQL port.
        cursor.execute('SAVEPOINT retry_allowance_scope')
        try:
            cursor.execute('SELECT forge_agent.amend_agent_retry_policy(%s,%s,%s)',
                (project,root,Json(dict(self.config.repair_policy))))
        except psycopg2.Error as error:
            cursor.execute('ROLLBACK TO SAVEPOINT retry_allowance_scope')
            if error.pgcode not in {'P0009','P0006'}:
                raise
        finally:
            cursor.execute('RELEASE SAVEPOINT retry_allowance_scope')

    def _grant(self,cursor,workflow,profile,mode):
        self._current_policy(workflow)
        if profile not in self.config.profiles:
            raise Rejected(403,'denied')
        cursor.execute("SELECT forge_agent.coding_workflow_role_granted(%s,%s,%s) AS allowed",(workflow['workflow_id'],profile,mode))
        if not cursor.fetchone()['allowed']:raise Rejected(403,'denied')

    def _phase(self,cursor,w,phase):
        cursor.execute("UPDATE forge_agent.coding_workflows SET phase=%s,updated_at=clock_timestamp() WHERE workflow_id=%s AND enabled AND phase NOT IN('completed','cancelled','escalated') AND phase IS DISTINCT FROM %s",(phase,w['workflow_id'],phase))

    def _escalate(self,cursor,w,reason):
        reason=reason[:2048]
        cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=false,phase='escalated',escalation_reason=%s,updated_at=clock_timestamp() WHERE workflow_id=%s AND phase NOT IN('completed','cancelled')",(reason,w['workflow_id']))
        cursor.execute("SELECT id,project_alias,status,revision FROM forge_agent.coding_tasks WHERE workflow_id=%s AND status IN('queued','running') FOR UPDATE",(w['workflow_id'],))
        affected=cursor.fetchall()
        cursor.execute("UPDATE forge_agent.coding_tasks SET cancelled_at=CASE WHEN status='queued' THEN clock_timestamp() ELSE NULL END,status=CASE WHEN status='queued' THEN 'cancelled' ELSE 'cancel_requested' END,revision=revision+1,updated_at=clock_timestamp() WHERE workflow_id=%s AND status IN('queued','running')",(w['workflow_id'],))
        cursor.execute("UPDATE forge_agent.coding_workflow_actions SET status='handoff',lease_token=NULL,lease_expires_at=NULL,updated_at=clock_timestamp() WHERE workflow_id=%s AND status IN('pending','leased')",(w['workflow_id'],))
        # The escalation reason stays on the workflow row; each affected task
        # also gets one event carrying the escalating actor and cause (4.2).
        for affected_task in affected:
            affected_target='cancelled' if affected_task['status']=='queued' else 'cancel_requested'
            insert_event(cursor,self.store.owner,affected_task['project_alias'],affected_task['id'],affected_target,
                         {'base_revision':affected_task['revision'],'revision':affected_task['revision']+1,
                          'workflow_id':str(w['workflow_id'])},actor=COORDINATOR_ACTOR,cause='workflow_cancelled',
                         cause_detail=reason)

    @staticmethod
    def _native_deadline_phase(task_id):
        return 'native_execution.deadline.'+str(task_id)

    def _native_deadline_event(self,cursor,w,task,attempt,rec):
        """Retain the legacy stopped-action ledger in the selected retry port.

        Late ingestion reserves only an already completed, control-receipt-bound
        action. It never queues its writer again. Stable observation/action keys
        make the import idempotent, including after the old event has resolved.
        """
        from work_retry_events import RetryScope
        scope=RetryScope(self.store.owner,task['project_alias'],'native_workflow',w['workflow_id'])
        phase=self._native_deadline_phase(task['id'])
        proof=dict(attempt_id=str(attempt['attempt_id']),reconciliation_receipt_id=str(rec['receipt_id']),
            workspace_digest=rec['workspace_digest'],plan_decision_id=str(task['plan_decision_id'] or task['scope_plan_decision_id']),
            recipe_digest=task['recipe_digest'])
        event=self.retry_events.observe_failure(cursor,scope,'infrastructure',str(attempt['attempt_id']),
            sha256_canonical(proof),phase_id=phase)
        cursor.execute("""SELECT x.* FROM forge_agent.coding_workflow_actions x WHERE x.workflow_id=%s
          AND x.owner_id=%s AND x.project_alias=%s AND x.kind='decide' AND x.status='completed'
          AND x.source_key LIKE 'native-deadline-retry:%%' AND x.intent->>'policy'=%s
          AND x.intent->>'task_id'=%s AND NOT EXISTS(
            SELECT FROM forge_agent.coding_attempts p WHERE(p.owner_id,p.project_alias,p.task_id)=(x.owner_id,x.project_alias,%s)
            AND p.phase='stopped' AND p.result_sha256 IS NOT NULL AND NOT p.reservation_active AND p.lease_expires_at IS NULL
            AND p.published_at>x.created_at AND p.published_at<%s
            AND EXISTS(SELECT FROM forge_agent.coding_reconciliation_receipts r WHERE(r.owner_id,r.project_alias,r.task_id,r.attempt_id)=(p.owner_id,p.project_alias,p.task_id,p.attempt_id)))
          ORDER BY x.created_at,x.action_id""",
            (w['workflow_id'],self.store.owner,task['project_alias'],NATIVE_DEADLINE_POLICY,str(task['id']),task['id'],attempt['started_at']))
        legacy=cursor.fetchall()
        if len(legacy)>NATIVE_DEADLINE_RETRIES:raise Rejected(409,'reconcile_required')
        for action in legacy:
            intent=action['intent'];finish=action['finish_receipt'] or {}
            if not isinstance(intent,dict) or not isinstance(finish,dict):raise Rejected(409,'reconcile_required')
            try:
                for field in ('source_attempt_id','reconciliation_receipt_id'):
                    if str(UUID(intent[field]))!=intent[field]:raise ValueError('Noncanonical retained identity')
            except (ValueError,TypeError,KeyError,AttributeError):raise Rejected(409,'reconcile_required') from None
            if (type(intent.get('source_revision')) is not int or intent['source_revision']<1
                or intent.get('max_native_deadline_retries')!=NATIVE_DEADLINE_RETRIES
                or action['source_key']!='native-deadline-retry:'+str(intent.get('source_attempt_id'))
                or finish.get('status')!='native_deadline_retried' or not isinstance(finish.get('receipt'),dict)):
                raise Rejected(409,'reconcile_required')
            cursor.execute("""SELECT r.attempt_id,r.task_revision,r.workspace_digest,c.receipt_json
              FROM forge_agent.coding_reconciliation_receipts r
              JOIN forge_agent.coding_control_receipts c ON(c.owner_id,c.project_alias,c.task_id)=(r.owner_id,r.project_alias,r.task_id)
              WHERE(r.owner_id,r.project_alias,r.task_id,r.receipt_id)=(%s,%s,%s,%s)
              AND c.action='resume' AND c.idempotency_key=%s AND c.base_revision=r.task_revision
              AND c.result_revision=r.task_revision+1""",
                (self.store.owner,task['project_alias'],task['id'],intent.get('reconciliation_receipt_id'),intent.get('idempotency_key')))
            retained=cursor.fetchone()
            if (not retained or str(retained['attempt_id'])!=intent.get('source_attempt_id')
                or retained['task_revision']!=intent.get('source_revision')
                or retained['workspace_digest']!=intent.get('workspace_digest')
                or retained['receipt_json']!=finish['receipt']
                or finish['receipt'].get('task_id')!=str(task['id']) or finish['receipt'].get('status')!='queued'):
                raise Rejected(409,'reconcile_required')
            observed=self.retry_events.observe_failure(cursor,scope,'infrastructure',
                'native-deadline-legacy:'+str(action['action_id']),sha256_canonical(dict(intent=intent,finish=finish)),phase_id=phase)
            if observed['event_id']!=event['event_id'] and observed['status']!='resolved':
                raise Rejected(409,'reconcile_required')
            self.retry_events.reserve(cursor,observed['event_id'],action['source_key'])
        return self.retry_events.get(cursor,event['event_id'])

    def retry_native_timeout(self,*,project_alias,task_id,expected_revision,reconciliation_receipt_id,idempotency_key,note=''):
        """Supported private operator/automatic ingress; never a browser override.

        The selected retry port accounts up to twenty recoveries in the same
        unresolved native-execution deadline event. Historical configurations
        without that port retain their one-shot allowance. Plan/code counters
        and verifier infrastructure retry episodes are independent.
        """
        self.store._require_project(project_alias)
        if (not isinstance(task_id,UUID) or not isinstance(reconciliation_receipt_id,UUID)
            or type(expected_revision) is not int or expected_revision<1
            or not isinstance(idempotency_key,str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,128}',idempotency_key)
            or not isinstance(note,str) or len(note.encode())>1024):
            raise Rejected(400,'invalid_request')
        with self._transaction() as cursor:
            self._project_lock(cursor,project_alias)
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE',
                           (self.store.owner,project_alias,task_id))
            task=cursor.fetchone()
            if not task:raise Rejected(404,'not_found')
            cursor.execute('SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND receipt_id=%s',
                           (self.store.owner,project_alias,task_id,reconciliation_receipt_id))
            rec=cursor.fetchone()
            if not rec:raise Rejected(409,'reconcile_required')
            key='native-deadline-retry:'+str(rec['attempt_id'])
            intent={'policy':NATIVE_DEADLINE_POLICY,'max_native_deadline_retries':NATIVE_DEADLINE_RETRIES,
                'task_id':str(task_id),'source_attempt_id':str(rec['attempt_id']),'source_revision':expected_revision,
                'reconciliation_receipt_id':str(reconciliation_receipt_id),'workspace_digest':rec['workspace_digest'],
                'idempotency_key':idempotency_key,'note':note}
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key=%s",
                           (task['workflow_id'],key))
            event_intent=dict(intent,policy=NATIVE_DEADLINE_EVENT_POLICY,
                max_native_deadline_retries=AGENT_RETRY_LIMIT,phase_id=self._native_deadline_phase(task_id))
            previous=cursor.fetchone()
            if previous:
                if previous['intent'] not in (intent,event_intent):raise Rejected(409,'idempotency_conflict')
                if previous['status']!='completed' or not previous['finish_receipt']:raise Rejected(409,'reconcile_required')
                return dict(previous['finish_receipt']['receipt'])
            if task['revision']!=expected_revision:raise Rejected(409,'revision_conflict')
            from work_owner_control import owner_continuation_used
            if owner_continuation_used(cursor, task['workflow_id']):raise Rejected(409,'not_reviewable')
            cursor.execute('SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s AND owner_id=%s AND project_alias=%s FOR UPDATE',
                           (task['workflow_id'],self.store.owner,project_alias))
            w=cursor.fetchone()
            if (not w or w['phase'] not in {'implement','repair','escalated'}
                or w['phase']!='escalated' and not w['enabled']
                or w['phase']=='escalated' and w['escalation_reason'] not in {
                    'Agent stopped: needs_reconcile','Agent stopped: failed'}):raise Rejected(409,'not_reviewable')
            p=w['snapshot'];self._current_policy(w)
            recipe=recipe_from_snapshot(p,'author_implement')
            if (not task['workflow_required'] or task['workflow_review_id'] is not None or task['mode']!='implement'
                or task['status'] not in {'needs_reconcile','failed'} or task['cancelled_at'] is not None
                or task['latest_attempt_id']!=rec['attempt_id'] or rec['task_revision']!=task['revision']
                or not(task['plan_decision_id'] or task['scope_plan_decision_id'])
                or task['agent_profile']!=p['author_profile'] or task['recipe_digest']!=recipe.digest
                or task['pinned_recipe']!=recipe.to_dict()):raise Rejected(409,'not_reviewable')
            cursor.execute('SELECT * FROM forge_agent.coding_attempts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s',
                           (self.store.owner,project_alias,task_id,rec['attempt_id']))
            attempt=cursor.fetchone()
            proof=rec['stop_proof']
            try:checked=datetime.fromisoformat(proof['checked_at'].replace('Z','+00:00'))
            except (ValueError,TypeError,KeyError,AttributeError):raise Rejected(409,'reconcile_required') from None
            if (not attempt or attempt['phase']!='stopped' or attempt['reservation_active'] or attempt['lease_expires_at'] is not None
                or attempt['result_sha256'] is not None or attempt['finished_at'] is None
                or attempt['finished_at']<attempt['deadline_at'] or rec['created_at']<attempt['deadline_at']
                or checked.tzinfo is None or checked<attempt['deadline_at']
                or attempt['pinned_recipe']!=recipe.to_dict() or attempt['recipe_digest']!=recipe.digest
                or proof.get('scope_stopped') is not True or proof.get('source')!='systemd'
                or proof.get('unit_name')!=attempt['unit_name']):raise Rejected(409,'reconcile_required')
            cursor.execute("SELECT EXISTS(SELECT 1 FROM forge_agent.coding_events WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND event_kind='needs_reconcile' AND payload->>'attempt_id'=%s AND payload->>'reason' IN('deadline_exceeded','stopped_without_publication') AND created_at>=%s) AS witnessed",
                (self.store.owner,project_alias,task_id,str(attempt['attempt_id']),attempt['deadline_at']))
            if not cursor.fetchone()['witnessed']:raise Rejected(409,'not_reviewable')
            cursor.execute("SELECT EXISTS(SELECT 1 FROM forge_agent.coding_events WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND created_at>=%s AND (event_kind IN('quota_blocked','blocked_auth','blocked_permission','cancel_requested','cancelled') OR payload->>'status' IN('quota_blocked','blocked_auth','blocked_permission','cancel_requested','cancelled'))) AS blocked",
                (self.store.owner,project_alias,task_id,attempt['started_at']))
            if cursor.fetchone()['blocked']:raise Rejected(409,'not_reviewable')
            cursor.execute("SELECT EXISTS(SELECT 1 FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id) WHERE t.workflow_id=%s AND(a.reservation_active OR a.lease_expires_at IS NOT NULL)) AS writer",(w['workflow_id'],))
            if cursor.fetchone()['writer']:raise Rejected(409,'reconcile_required')
            cursor.execute("SELECT EXISTS(SELECT 1 FROM forge_agent.coding_tasks WHERE workflow_id=%s AND status IN('queued','running','cancel_requested')) OR EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND status IN('pending','leased')) OR EXISTS(SELECT 1 FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(p.owner_id,p.project_alias,p.task_id) WHERE t.workflow_id=%s AND p.status IN('pending','checking','publishing')) OR EXISTS(SELECT 1 FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id) WHERE t.workflow_id=%s AND q.status IN('pending','checking')) AS busy",
                           (w['workflow_id'],)*4)
            if cursor.fetchone()['busy']:raise Rejected(409,'reconcile_required')
            cursor.execute("SELECT EXISTS(SELECT 1 FROM forge_agent.coding_tasks WHERE workflow_id=%s AND id<>%s AND status IN('failed','needs_reconcile','quota_blocked','blocked_auth','blocked_permission','cancelled')) AS sibling_failure",
                           (w['workflow_id'],task_id))
            if cursor.fetchone()['sibling_failure']:raise Rejected(409,'not_reviewable')
            cursor.execute("SELECT count(*) AS used FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key LIKE 'native-deadline-retry:%%'",(w['workflow_id'],))
            prior_deadlines=cursor.fetchone()['used']
            if self.retry_events is None and prior_deadlines>=NATIVE_DEADLINE_RETRIES:
                raise Rejected(409,'not_reviewable')
            event=None
            if self.retry_events is not None:
                event=self._native_deadline_event(cursor,w,task,attempt,rec)
                if event['used']>=event['limit']:
                    event=self.retry_events.reserve(cursor,event['event_id'],key)
                    self._escalate(cursor,w,'Native execution deadline event exhausted its twenty retry attempts. The stopped workspace and approved Plan are retained.')
                    return dict(status='needs_attention',reason='native_deadline_event_exhausted',
                        workflow_id=str(w['workflow_id']),retry_event=event)
            cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase='implement',escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=%s",(w['workflow_id'],))
            self._grant(cursor,w,p['author_profile'],'implement');self._grant(cursor,w,p['reviewer_profile'],'review')
            cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND policy_digest=ANY(%s) AND observed_at>=clock_timestamp()-interval '90 seconds'",
                           (self.store.owner,[x[0] for x in NATIVE_POLICIES.values()]))
            ready=cursor.fetchall()
            for role,mode,entry in ((p['author_profile'],'implement','author_implement'),(p['reviewer_profile'],'review','reviewer_review')):
                required=recipe_from_snapshot(p,entry)
                if not _ready_for_recipe(ready,project_alias,role,mode,required):
                    raise Rejected(503,'unavailable')
            authority=BoundNativeRetryAuthority(self.store,cursor,w['workflow_id'],task_id,attempt['attempt_id'],task['revision'])
            resume_note=NATIVE_DEADLINE_NOTE+('\nOperator context: '+note if note else '')
            if event is not None:
                event=self.retry_events.reserve(cursor,event['event_id'],key)
                if event['status']=='exhausted':raise Rejected(409,'not_reviewable')
                intent=event_intent
            receipt=self.store.resume(project_alias=project_alias,task_id=task_id,expected_revision=expected_revision,
                idempotency_key=idempotency_key,agent_profile=p['author_profile'],reconciliation_receipt_id=reconciliation_receipt_id,
                note=resume_note,allowed_modes={'implement'},resolve_recipe=lambda _:recipe,_authority=authority,actor=COORDINATOR_ACTOR)
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt) VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",
                (w['workflow_id'],self.store.owner,project_alias,key,Json(intent),Json(dict(status='native_deadline_retried',receipt=receipt,**({'retry_event':event} if event is not None else {})))))
            return receipt

    def _recover_native_timeouts(self):
        """Existing durable-event scan retries only reconciled deadline stops."""
        with self.store._transaction(read_only=True) as cursor:
            cursor.execute("""SELECT t.project_alias,t.id,t.revision,r.receipt_id,a.attempt_id
              FROM forge_agent.coding_tasks t JOIN forge_agent.coding_workflows w
              ON(w.owner_id,w.project_alias,w.workflow_id)=(t.owner_id,t.project_alias,t.workflow_id)
              JOIN forge_agent.coding_attempts a ON(a.owner_id,a.project_alias,a.task_id,a.attempt_id)=(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)
              JOIN forge_agent.coding_reconciliation_receipts r ON(r.owner_id,r.project_alias,r.task_id,r.attempt_id,r.task_revision)=(t.owner_id,t.project_alias,t.id,t.latest_attempt_id,t.revision)
              WHERE t.owner_id=%s AND t.project_alias=ANY(%s) AND t.workflow_required AND t.mode='implement'
              AND t.status IN('needs_reconcile','failed') AND w.phase IN('implement','repair','escalated')
              AND(w.enabled OR w.phase='escalated')
              AND(w.phase<>'escalated' OR w.escalation_reason IN('Agent stopped: needs_reconcile','Agent stopped: failed'))
              AND t.cancelled_at IS NULL AND t.workflow_review_id IS NULL AND a.result_sha256 IS NULL
              AND a.phase='stopped' AND NOT a.reservation_active AND a.lease_expires_at IS NULL
              AND a.finished_at>=a.deadline_at AND r.created_at>=a.deadline_at
              AND a.pinned_recipe->>'policy_digest'=ANY(%s)
              AND t.agent_profile=w.snapshot->>'author_profile'
              AND t.recipe_digest=w.snapshot->'recipes'->'author_implement'->>'digest'
              AND EXISTS(SELECT 1 FROM forge_agent.coding_events e WHERE(e.owner_id,e.project_alias,e.task_id)=(t.owner_id,t.project_alias,t.id)
                AND e.event_kind='needs_reconcile' AND e.payload->>'attempt_id'=a.attempt_id::text
                AND e.payload->>'reason' IN('deadline_exceeded','stopped_without_publication') AND e.created_at>=a.deadline_at)
              AND NOT EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions x WHERE x.workflow_id=w.workflow_id
                 AND x.kind='decide' AND (x.source_key='native-deadline-retry:'||a.attempt_id::text
                   OR(%s AND x.source_key LIKE 'native-deadline-retry:%%')))
              ORDER BY r.created_at DESC LIMIT 64""",(self.store.owner,list(self.store.allowed_projects),[x[0] for x in NATIVE_POLICIES.values()],self.retry_events is None))
            candidates=cursor.fetchall()
        for task in candidates:
            try:
                self.retry_native_timeout(project_alias=task['project_alias'],task_id=task['id'],expected_revision=task['revision'],
                    reconciliation_receipt_id=task['receipt_id'],idempotency_key='native-deadline-'+str(task['attempt_id']))
            except Rejected:
                # Missing proof, revoked scope, budget exhaustion and real
                # exceptions retain the existing escalation behavior.
                pass

    def _verify_peer_recovery_source(self,cursor,project_alias,w,review,task,critic,target):
        """Shared source binding, review-row and quiescence checks for peer recovery.

        Used by both the compatibility replay and the transcription-defect
        correction. Returns the awaiting-review coding_reviews row.
        """
        if (target['reviewer_task_id']!=str(critic['id']) or target['source_task_id']!=str(task['id'])
            or target['source_attempt_id']!=str(task['latest_attempt_id']) or target['source_revision']!=task['revision']
            or target['workflow_id']!=str(w['workflow_id']) or target['workflow_review_id']!=str(review['workflow_review_id'])
            or target['policy_digest']!=w['policy_digest'] or target['plan_decision_id']!=str(task['plan_decision_id'] or task['scope_plan_decision_id'])
            or target['review_id']!=str(review['source_review_id'])):raise Rejected(409,'result_mismatch')
        cursor.execute('SELECT * FROM forge_agent.coding_reviews WHERE review_id=%s FOR UPDATE',(review['source_review_id'],));q=cursor.fetchone()
        if (not q or q['owner_id']!=self.store.owner or q['project_alias']!=project_alias or q['task_id']!=task['id']
            or q['attempt_id']!=task['latest_attempt_id'] or q['status']!='awaiting_review'
            or q['candidate_commit']!=target['candidate_commit'] or q['gate_manifest_sha256']!=target['gate_manifest_sha256']
            or q['verification'].get('checks_passed') is not True or q['verification'].get('cleanup_confirmed') is not True
            or q['verification'].get('candidate_workspace_digest')!=target['workspace_digest']
            or q['verification'].get('integration_base')!=target['integration_base']
            or {'checks_passed':True,'toolchain':q['verification'].get('toolchain','Not reported'),
                'checks':[{k:x[k] for k in ('check_id','status','head_sha')} for x in q['verification'].get('checks',[])]}!=target['verification']):
            raise Rejected(409,'not_reviewable')
        cursor.execute("SELECT EXISTS(SELECT 1 FROM forge_agent.coding_tasks WHERE workflow_id=%s AND status IN('queued','running','cancel_requested','failed','needs_reconcile','quota_blocked','blocked_auth','blocked_permission','cancelled')) OR EXISTS(SELECT 1 FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id) WHERE t.workflow_id=%s AND(a.reservation_active OR a.lease_expires_at IS NOT NULL)) OR EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND status IN('pending','leased')) OR EXISTS(SELECT 1 FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(p.owner_id,p.project_alias,p.task_id) WHERE t.workflow_id=%s) OR EXISTS(SELECT 1 FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id) WHERE t.workflow_id=%s AND q.status IN('pending','checking')) AS busy",(w['workflow_id'],)*5)
        if cursor.fetchone()['busy']:raise Rejected(409,'reconcile_required')
        return q

    def _correct_peer_verdict(self,cursor,w,review,task,critic,target,defect,*,reviewer_attempt_id,reviewer_result_sha256,correction_key,correction_intent,validate_target):
        """Supersede a transcription-defective review with a fresh reviewer round.

        The defective verdict is never applied. The old row keeps its target
        and verdict bytes untouched and only flips to 'superseded'; the new
        row reuses the source binding at the next canonical generation, under
        a fresh target built by ordinary admission. Only an M35 root may be
        superseded, enforced atomically under the row lock held by the caller.
        The repair budget is not consumed here: it still gates the revision
        that the fresh review may request.
        """
        # The selected canonical provider owns generation/event-budget
        # accounting. This compatibility helper must not preempt it or admit
        # another correction after its event budget is exhausted. Historical
        # configurations without retry events retain their one-shot correction.
        if self.superseding_review is not None and self.retry_events is not None:
            raise Rejected(409,'not_reviewable')
        if ('supersedes_workflow_review_id' not in review
            or review['supersedes_workflow_review_id'] is not None
            or type(review.get('review_generation')) is not int
            or review['review_generation'] != 0
            or review.get('supersedes_review_id') is not None):
            raise Rejected(409,'not_reviewable')
        self._verify_peer_recovery_source(cursor,review['project_alias'],w,review,task,critic,target)
        # M33 validates the exact escalated parent and next generation at
        # INSERT. Keep M35's terminal status change in this same transaction,
        # after that canonical lineage proof; main's independent path stays
        # escalated and does not enter this branch-only correction helper.
        cursor.execute("UPDATE forge_agent.coding_workflow_reviews SET status='escalated',updated_at=clock_timestamp() WHERE workflow_review_id=%s",(review['workflow_review_id'],))
        generation=review['review_generation']+1
        cursor.execute("""INSERT INTO forge_agent.coding_workflow_reviews(workflow_id,owner_id,project_alias,target_kind,source_task_id,source_attempt_id,source_result_sha256,source_revision,source_review_id,candidate_commit,gate_manifest_sha256,supersedes_workflow_review_id,review_generation,supersedes_review_id)
          VALUES(%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s) RETURNING workflow_review_id""",
          (review['workflow_id'],review['owner_id'],review['project_alias'],review['target_kind'],review['source_task_id'],review['source_attempt_id'],review['source_result_sha256'],review['source_revision'],review['source_review_id'],review['candidate_commit'],review['gate_manifest_sha256'],review['workflow_review_id'],generation,review['workflow_review_id']))
        new_id=cursor.fetchone()['workflow_review_id']
        cursor.execute("UPDATE forge_agent.coding_workflow_reviews SET status='superseded',updated_at=clock_timestamp() WHERE workflow_review_id=%s",(review['workflow_review_id'],))
        cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,workflow_review_id,kind,source_key) VALUES(%s,%s,%s,%s,'review',%s) RETURNING action_id",(w['workflow_id'],self.store.owner,review['project_alias'],new_id,str(new_id)))
        review_action_id=cursor.fetchone()['action_id']
        new_review=dict(review);new_review.update(workflow_review_id=new_id,target_json=None,target_sha256=None,reviewer_task_id=None,reviewer_attempt_id=None,reviewer_result_sha256=None,verdict_json=None,verdict_sha256=None,status='pending',supersedes_workflow_review_id=review['workflow_review_id'],review_generation=generation,supersedes_review_id=review['workflow_review_id'])
        if 'diagnostics' in review:
            new_review['diagnostics']=None
        # Un-escalate BEFORE admitting the fresh reviewer: the role grant
        # (coding_workflow_role_granted) only allows a workflow whose phase
        # is not completed/cancelled/escalated, so admitting while still
        # escalated is always denied. The caller's gates already ran
        # against the locked rows, so this re-enable is the correction.
        cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase='code_review',escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=%s",(w['workflow_id'],))
        # The correction key ('peer-correction-<id>:1') is the workflow
        # action's source_key, but a task idempotency_key forbids ':' --
        # derive the task key with the version separator as '-'.
        admission=self._admit_reviewer(cursor,w,new_review,task,validate_target,idempotency_key=correction_key.replace(':', '-'))
        cursor.execute("UPDATE forge_agent.coding_workflow_actions SET status='completed',finish_receipt=%s WHERE action_id=%s",(Json(admission),review_action_id))
        note=('Peer review superseded by correction '+str(new_id)+': transcription defect '+str(defect['check'])
              +'; the defective verdict was never applied.')
        cursor.execute("UPDATE forge_agent.coding_tasks SET status='completed',accepted_at=clock_timestamp(),revision=revision+1,updated_at=clock_timestamp() WHERE owner_id=%s AND project_alias=%s AND id=%s",(self.store.owner,review['project_alias'],critic['id']))
        insert_event(cursor,self.store.owner,review['project_alias'],critic['id'],'completed',
                     {'note':note,'superseding_workflow_review_id':str(new_id)},actor=COORDINATOR_ACTOR)
        p=w['snapshot']
        self._grant(cursor,w,p['author_profile'],'implement');self._grant(cursor,w,p['reviewer_profile'],'review')
        cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND policy_digest=ANY(%s) AND observed_at>=clock_timestamp()-interval '90 seconds'",(self.store.owner,[x[0] for x in NATIVE_POLICIES.values()]));ready=cursor.fetchall()
        for profile,mode,entry in ((p['author_profile'],'implement','author_implement'),(p['reviewer_profile'],'review','reviewer_review')):
            recipe=recipe_from_snapshot(p,entry)
            if not _ready_for_recipe(ready,review['project_alias'],profile,mode,recipe):
                raise Rejected(503,'unavailable')
        receipt={'status':'peer_verdict_correction_queued','workflow_id':str(w['workflow_id']),'workflow_review_id':str(review['workflow_review_id']),
                 'superseding_workflow_review_id':str(new_id),'reviewer_task_id':str(admission['task_id']),
                 'source_task_id':str(task['id']),'reviewer_attempt_id':str(reviewer_attempt_id),
                 'reviewer_result_sha256':reviewer_result_sha256,'target_sha256':review['target_sha256'],
                 'defect':defect}
        cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt) VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",
                       (w['workflow_id'],self.store.owner,review['project_alias'],correction_key,Json(correction_intent),Json({'status':'peer_verdict_correction_queued','receipt':receipt})))
        cursor.execute("SELECT pg_notify('forge_coding_jobs','')")
        return receipt

    def retry_peer_verdict(self,*,project_alias,workflow_review_id,expected_revision,
                           reviewer_attempt_id,reviewer_result_sha256,target_sha256,idempotency_key):
        """Replay one exact saved code revision through the ordinary controller.

        This private factory-owned compatibility recovery never creates a verdict
        or approves code. The original result bytes remain the source authority.
        A verdict that fails parsing only on context_sha256 equality instead
        takes the superseding-review correction path.
        """
        from work_peer_review import classify_verdict_defect,parse_review_target,parse_review_verdict,validate_review_target
        self.store._require_project(project_alias)
        if (not isinstance(workflow_review_id,UUID) or not isinstance(reviewer_attempt_id,UUID)
            or type(expected_revision) is not int or expected_revision<1
            or not isinstance(idempotency_key,str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,128}',idempotency_key)
            or any(not isinstance(v,str) or not re.fullmatch(r'[0-9a-f]{64}',v)
                   for v in (reviewer_result_sha256,target_sha256))):raise Rejected(400,'invalid_request')
        with self._transaction() as cursor:
            self._project_lock(cursor,project_alias)
            cursor.execute('SELECT * FROM forge_agent.coding_workflow_reviews WHERE owner_id=%s AND project_alias=%s AND workflow_review_id=%s',
                           (self.store.owner,project_alias,workflow_review_id));r=cursor.fetchone()
            if not r:raise Rejected(404,'not_found')
            correction_key='peer-correction-'+str(workflow_review_id)+':1'
            correction_intent={'policy':PEER_CORRECTION_POLICY,'workflow_review_id':str(workflow_review_id),
                    'source_revision':expected_revision,'reviewer_attempt_id':str(reviewer_attempt_id),
                    'reviewer_result_sha256':reviewer_result_sha256,'target_sha256':target_sha256,'idempotency_key':idempotency_key}
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key=%s",(r['workflow_id'],correction_key));prior=cursor.fetchone()
            if prior:
                if prior['intent']!=correction_intent:raise Rejected(409,'idempotency_conflict')
                if prior['status']!='completed' or not prior['finish_receipt']:raise Rejected(409,'reconcile_required')
                return dict(prior['finish_receipt']['receipt'])
            key='peer-verdict-replay:'+str(workflow_review_id)+':'+str(reviewer_attempt_id)
            intent={'policy':PEER_WIRE_REPLAY_POLICY,'max_replays':1,'workflow_review_id':str(workflow_review_id),
                    'source_revision':expected_revision,'reviewer_attempt_id':str(reviewer_attempt_id),
                    'reviewer_result_sha256':reviewer_result_sha256,'target_sha256':target_sha256,'idempotency_key':idempotency_key}
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key=%s",(r['workflow_id'],key));prior=cursor.fetchone()
            if prior:
                if prior['intent']!=intent:raise Rejected(409,'idempotency_conflict')
                if prior['status']!='completed' or not prior['finish_receipt']:raise Rejected(409,'reconcile_required')
                return dict(prior['finish_receipt']['receipt'])
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE',
                           (self.store.owner,project_alias,r['source_task_id']));task=cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE',
                           (self.store.owner,project_alias,r['reviewer_task_id']));critic=cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s FOR UPDATE',(workflow_review_id,));review=dict(cursor.fetchone())
            cursor.execute('SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s FOR UPDATE',(r['workflow_id'],));w=cursor.fetchone()
            self._current_policy(w);p=w['snapshot']
            if (w['phase']!='escalated' or w['enabled'] or w['escalation_reason']!=PEER_WIRE_ERROR
                or review['status']!='escalated' or review['target_kind']!='code' or review['target_sha256']!=target_sha256
                or not task or not critic or task['workflow_id']!=w['workflow_id'] or critic['workflow_id']!=w['workflow_id']
                or task['revision']!=expected_revision or task['revision']!=review['source_revision']
                or task['latest_attempt_id']!=review['source_attempt_id'] or task['status']!='awaiting_review'
                or task['mode']!='implement' or task['workflow_review_id'] is not None or not task['workflow_required']
                or critic['status']!='awaiting_review' or critic['mode']!='review' or not critic['workflow_required']
                or critic['latest_attempt_id']!=reviewer_attempt_id or critic['workflow_review_id']!=workflow_review_id
                or critic['review_target_sha256']!=target_sha256 or task['cancelled_at'] is not None or critic['cancelled_at'] is not None):
                raise Rejected(409,'not_reviewable')
            for row,profile,entry in ((task,p['author_profile'],'author_implement'),(critic,p['reviewer_profile'],'reviewer_review')):
                recipe=recipe_from_snapshot(p,entry)
                if row['agent_profile']!=profile or row['pinned_recipe']!=recipe.to_dict() or row['recipe_digest']!=recipe.digest:
                    raise Rejected(409,'result_mismatch')
                cursor.execute('SELECT phase,reservation_active,lease_expires_at,published_at,unit_name,pinned_recipe,recipe_digest FROM forge_agent.coding_attempts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s',
                               (self.store.owner,project_alias,row['id'],row['latest_attempt_id']));attempt=cursor.fetchone()
                cursor.execute('SELECT stop_proof FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s AND task_revision=%s',
                               (self.store.owner,project_alias,row['id'],row['latest_attempt_id'],row['revision']));rec=cursor.fetchone()
                if (not attempt or attempt['phase']!='stopped' or attempt['reservation_active'] or attempt['lease_expires_at'] is not None
                    or not attempt['published_at'] or attempt['pinned_recipe']!=recipe.to_dict() or attempt['recipe_digest']!=recipe.digest
                    or not rec or rec['stop_proof'].get('scope_stopped') is not True or rec['stop_proof'].get('source')!='systemd'
                    or rec['stop_proof'].get('unit_name')!=attempt['unit_name']):
                    raise Rejected(409,'reconcile_required')
            source=self._artifact(cursor,task['id'],task['latest_attempt_id']);result=self._artifact(cursor,critic['id'],reviewer_attempt_id)
            if source['result_sha256']!=review['source_result_sha256'] or result['result_sha256']!=reviewer_result_sha256:
                raise Rejected(409,'result_mismatch')
            try:
                target=parse_review_target(review['target_json'],target_sha256)
            except (ValueError,KeyError,TypeError):raise Rejected(409,'result_mismatch') from None
            defect=classify_verdict_defect(result['content'],target)
            if defect is not None:
                return self._correct_peer_verdict(cursor,w,review,task,critic,target,defect,
                    reviewer_attempt_id=reviewer_attempt_id,reviewer_result_sha256=reviewer_result_sha256,
                    correction_key=correction_key,correction_intent=correction_intent,validate_target=validate_review_target)
            # Plan §3.3: a fully valid verdict is applied, whatever its
            # decision or finding categories. The correctness-category gate
            # was written for one finite incompatibility and is dropped.
            try:
                parse_review_verdict(result['content'],target)
            except (ValueError,KeyError,TypeError):raise Rejected(409,'result_mismatch') from None
            q=self._verify_peer_recovery_source(cursor,project_alias,w,review,task,critic,target)
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND workflow_review_id=%s AND kind='decide' AND source_key=%s FOR UPDATE",
                           (w['workflow_id'],workflow_review_id,str(reviewer_attempt_id)+':'+reviewer_result_sha256));action=cursor.fetchone()
            if not action or action['status']!='handoff' or action['finish_receipt'] is not None or action['intent'] is not None:
                raise Rejected(409,'not_reviewable')
            cursor.execute("SELECT count(*) AS used FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key LIKE 'peer-verdict-replay:%%'",(w['workflow_id'],))
            if cursor.fetchone()['used']:raise Rejected(409,'not_reviewable')
            cursor.execute('SELECT repair_rounds,stagnant_rounds,policy FROM forge_agent.coding_review_budget WHERE owner_id=%s AND project_alias=%s AND root_task_id=%s',
                           (self.store.owner,project_alias,task['repair_root_task_id'] or task['id']));budget=cursor.fetchone()
            if self.retry_events is None and (q['round']>=self._retry_limit(w,'max_code_revisions') or budget and (budget['repair_rounds']>=self._retry_limit(w,'max_code_revisions')
                or budget['stagnant_rounds']>=budget['policy']['max_stagnant_rounds'])):raise Rejected(409,'not_reviewable')
            cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase='code_review',escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=%s",(w['workflow_id'],))
            self._grant(cursor,w,p['author_profile'],'implement');self._grant(cursor,w,p['reviewer_profile'],'review')
            cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND policy_digest=ANY(%s) AND observed_at>=clock_timestamp()-interval '90 seconds'",(self.store.owner,[x[0] for x in NATIVE_POLICIES.values()]));ready=cursor.fetchall()
            for profile,mode,entry in ((p['author_profile'],'implement','author_implement'),(p['reviewer_profile'],'review','reviewer_review')):
                recipe=recipe_from_snapshot(p,entry)
                if not _ready_for_recipe(ready,project_alias,profile,mode,recipe):
                    raise Rejected(503,'unavailable')
            cursor.execute("UPDATE forge_agent.coding_workflow_reviews SET status='reviewing',updated_at=clock_timestamp() WHERE workflow_review_id=%s",(workflow_review_id,))
            cursor.execute("UPDATE forge_agent.coding_workflow_actions SET status='pending',worker_id=NULL,lease_token=NULL,lease_expires_at=NULL,updated_at=clock_timestamp() WHERE action_id=%s",(action['action_id'],))
            receipt={'status':'peer_verdict_replay_queued','workflow_id':str(w['workflow_id']),'workflow_review_id':str(workflow_review_id),
                     'source_task_id':str(task['id']),'reviewer_attempt_id':str(reviewer_attempt_id),'reviewer_result_sha256':reviewer_result_sha256,
                     'target_sha256':target_sha256,'action_id':str(action['action_id'])}
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt) VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",
                           (w['workflow_id'],self.store.owner,project_alias,key,Json(intent),Json({'status':'peer_verdict_replay_queued','receipt':receipt})))
            cursor.execute("SELECT pg_notify('forge_coding_jobs','')")
            return receipt

    def _recover_peer_verdicts(self):
        with self.store._transaction(read_only=True) as cursor:
            cursor.execute("""SELECT r.workflow_review_id,r.target_sha256,r.source_revision,t.project_alias,a.attempt_id,a.result_sha256
              FROM forge_agent.coding_workflow_reviews r JOIN forge_agent.coding_workflows w USING(workflow_id)
              JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(r.owner_id,r.project_alias,r.reviewer_task_id)
              JOIN forge_agent.coding_attempts a ON(a.owner_id,a.project_alias,a.task_id,a.attempt_id)=(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)
              WHERE r.owner_id=%s AND r.project_alias=ANY(%s) AND r.target_kind='code' AND r.status='escalated'
              AND w.phase='escalated' AND NOT w.enabled AND w.escalation_reason=%s AND t.status='awaiting_review'
              AND a.phase='stopped' AND NOT a.reservation_active AND a.lease_expires_at IS NULL AND a.result_sha256 IS NOT NULL
              AND NOT EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions x WHERE x.workflow_id=w.workflow_id AND x.source_key LIKE 'peer-verdict-replay:%%')
              ORDER BY r.updated_at DESC LIMIT 64""",(self.store.owner,list(self.store.allowed_projects),PEER_WIRE_ERROR));rows=cursor.fetchall()
        for row in rows:
            try:self.retry_peer_verdict(project_alias=row['project_alias'],workflow_review_id=row['workflow_review_id'],
                expected_revision=row['source_revision'],reviewer_attempt_id=row['attempt_id'],reviewer_result_sha256=row['result_sha256'],
                target_sha256=row['target_sha256'],idempotency_key='peer-wire-'+str(row['workflow_review_id']))
            except Rejected:pass

    def retry_preparation(self,*,project_alias,task_id,expected_revision,review_id,
                          reconciliation_receipt_id,idempotency_key):
        """Retry the known linked-base preparation defect using existing infra budget."""
        self.store._require_project(project_alias)
        if (any(not isinstance(value,UUID) for value in (task_id,review_id,reconciliation_receipt_id))
            or type(expected_revision) is not int or expected_revision<1
            or not isinstance(idempotency_key,str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,128}',idempotency_key)):
            raise Rejected(400,'invalid_request')
        note='Retry exact stopped candidate preparation after the linked-base import fix; reconciliation='+str(reconciliation_receipt_id)
        intent=sha256_canonical({'action':'review_decide','project_alias':project_alias,'task_id':str(task_id),
            'expected_revision':expected_revision,'review_id':str(review_id),'decision':'retry_infrastructure',
            'agent_profile':None,'note':note,'findings':[],'verified_finding_ids':[]})
        with self._transaction() as cursor:
            self._project_lock(cursor,project_alias)
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE',
                           (self.store.owner,project_alias,task_id));task=cursor.fetchone()
            if not task:raise Rejected(404,'not_found')
            cursor.execute('SELECT * FROM forge_agent.coding_control_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND idempotency_key=%s',
                           (self.store.owner,project_alias,task_id,idempotency_key));prior=cursor.fetchone()
            if prior:
                if prior['action']!='review_decide' or prior['intent_digest']!=intent:raise Rejected(409,'idempotency_conflict')
                return dict(prior['receipt_json'])
            from work_owner_control import owner_continuation_used
            if owner_continuation_used(cursor, task['workflow_id']):raise Rejected(409,'not_reviewable')
            cursor.execute('SELECT * FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND review_id=%s FOR UPDATE',
                           (self.store.owner,project_alias,task_id,review_id));q=cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_workflows WHERE owner_id=%s AND project_alias=%s AND workflow_id=%s FOR UPDATE',
                           (self.store.owner,project_alias,task['workflow_id']));w=cursor.fetchone()
            if not w:raise Rejected(409,'not_reviewable')
            self._current_policy(w);p=w['snapshot'];recipe=recipe_from_snapshot(p,'author_implement')
            if (w['phase']!='escalated' or w['enabled'] or w['escalation_reason']!='publication_command_failed'
                or task['revision']!=expected_revision or task['status']!='awaiting_review' or task['mode']!='implement'
                or not task['workflow_required'] or task['workflow_review_id'] is not None or task['cancelled_at'] is not None
                or task['agent_profile']!=p['author_profile'] or task['pinned_recipe']!=recipe.to_dict() or task['recipe_digest']!=recipe.digest
                or not task['parent_task_id'] or not task['repair_source_review_id'] or not task['scope_plan_decision_id']
                or not q or q['attempt_id']!=task['latest_attempt_id'] or q['source_revision']!=task['revision']
                or q['base_revision']!=task['base_revision'] or q['plan_decision_id']!=task['plan_decision_id']
                or q['scope_plan_decision_id']!=task['scope_plan_decision_id']
                or q['phase']!='local' or q['status']!='handoff' or q['reason']!='publication_command_failed'
                or q['candidate_commit'] is not None or q['reconciliation_receipt_id']!=reconciliation_receipt_id
                or q['verification'].get('checks_passed') is not False or q['verification'].get('checks')!=[]
                or q['verification'].get('cleanup_confirmed') is not True
                or q['infrastructure_retries']>=self._repair_limit(w,'max_infrastructure_retries')):
                raise Rejected(409,'not_reviewable')
            failure=q['finish_receipt'] or {};outcome=failure.get('outcome',{});verified=outcome.get('verification',{})
            if (q['finish_token'] is None or failure.get('review_id')!=str(review_id) or failure.get('status')!='handoff'
                or outcome.get('reason')!='publication_command_failed' or outcome.get('candidate_commit') is not None
                or verified.get('checks_passed') is not False or verified.get('checks')!=[]
                or verified.get('cleanup_confirmed') is not True):raise Rejected(409,'not_reviewable')
            cursor.execute('SELECT * FROM forge_agent.coding_reviews WHERE review_id=%s',(task['repair_source_review_id'],));parent=cursor.fetchone()
            if (not parent or parent['owner_id']!=self.store.owner or parent['project_alias']!=project_alias
                or parent['task_id']!=task['parent_task_id'] or parent['candidate_commit']!=task['base_revision']
                or task['repair_candidate_commit']!=task['base_revision']):raise Rejected(409,'result_mismatch')
            cursor.execute('SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND receipt_id=%s',
                           (self.store.owner,project_alias,task_id,reconciliation_receipt_id));rec=cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_attempts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s',
                           (self.store.owner,project_alias,task_id,task['latest_attempt_id']));attempt=cursor.fetchone()
            if (not rec or rec['attempt_id']!=task['latest_attempt_id'] or rec['task_revision']!=task['revision']
                or rec['workspace_digest']!=q['workspace_digest'] or not attempt or attempt['phase']!='stopped'
                or attempt['reservation_active'] or attempt['lease_expires_at'] is not None or not attempt['published_at']
                or attempt['result_sha256']!=q['parent_result_sha256'] or attempt['pinned_recipe']!=recipe.to_dict()
                or attempt['recipe_digest']!=recipe.digest or rec['stop_proof'].get('scope_stopped') is not True
                or rec['stop_proof'].get('source')!='systemd' or rec['stop_proof'].get('unit_name')!=attempt['unit_name']):
                raise Rejected(409,'reconcile_required')
            self._artifact(cursor,task_id,task['latest_attempt_id'])
            cursor.execute("SELECT EXISTS(SELECT 1 FROM forge_agent.coding_tasks WHERE workflow_id=%s AND status IN('queued','running','cancel_requested','failed','needs_reconcile','quota_blocked','blocked_auth','blocked_permission','cancelled')) OR EXISTS(SELECT 1 FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id) WHERE t.workflow_id=%s AND(a.reservation_active OR a.lease_expires_at IS NOT NULL)) OR EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND status IN('pending','leased')) OR EXISTS(SELECT 1 FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(p.owner_id,p.project_alias,p.task_id) WHERE t.workflow_id=%s AND p.status IN('pending','checking','publishing')) OR EXISTS(SELECT 1 FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id) WHERE t.workflow_id=%s AND q.status IN('pending','checking')) AS busy",(w['workflow_id'],)*5)
            if cursor.fetchone()['busy']:raise Rejected(409,'reconcile_required')
            cursor.execute('SELECT max(review_no) AS latest FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s',
                           (self.store.owner,project_alias,task_id))
            if cursor.fetchone()['latest']!=q['review_no']:raise Rejected(409,'not_reviewable')
            cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase='checks',escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=%s",(w['workflow_id'],))
            self._grant(cursor,w,p['author_profile'],'implement');self._grant(cursor,w,p['reviewer_profile'],'review')
            cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND policy_digest=ANY(%s) AND observed_at>=clock_timestamp()-interval '90 seconds'",(self.store.owner,[x[0] for x in NATIVE_POLICIES.values()]));ready=cursor.fetchall()
            for profile,mode,entry in ((p['author_profile'],'implement','author_implement'),(p['reviewer_profile'],'review','reviewer_review')):
                required=recipe_from_snapshot(p,entry)
                if not _ready_for_recipe(ready,project_alias,profile,mode,required):raise Rejected(503,'unavailable')
            authority=BoundPreparationRetryAuthority(self.store,cursor,w['workflow_id'],task_id,task['latest_attempt_id'],task['revision'],review_id)
            return self.store.review_decide(project_alias=project_alias,task_id=task_id,expected_revision=expected_revision,
                review_id=review_id,decision='retry_infrastructure',agent_profile=None,note=note,idempotency_key=idempotency_key,_authority=authority,actor=COORDINATOR_ACTOR)

    def _recover_preparations(self):
        with self.store._transaction(read_only=True) as cursor:
            cursor.execute("""SELECT t.project_alias,t.id,t.revision,q.review_id,q.reconciliation_receipt_id FROM forge_agent.coding_tasks t
              JOIN forge_agent.coding_workflows w USING(workflow_id) JOIN forge_agent.coding_reviews q
              ON(q.owner_id,q.project_alias,q.task_id,q.attempt_id)=(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)
              WHERE t.owner_id=%s AND t.project_alias=ANY(%s) AND t.workflow_required AND t.parent_task_id IS NOT NULL
              AND t.mode='implement' AND t.status='awaiting_review' AND t.cancelled_at IS NULL
              AND w.phase='escalated' AND NOT w.enabled AND w.escalation_reason='publication_command_failed'
              AND q.status='handoff' AND q.phase='local' AND q.reason='publication_command_failed' AND q.candidate_commit IS NULL
              AND q.verification->'checks_passed'='false'::jsonb AND q.verification->'checks'='[]'::jsonb
              AND q.verification->'cleanup_confirmed'='true'::jsonb AND q.source_revision=t.revision
              AND q.finish_token IS NOT NULL AND q.finish_receipt->>'status'='handoff'
              AND q.finish_receipt->'outcome'->>'reason'='publication_command_failed'
              AND q.infrastructure_retries<coalesce((SELECT(b.policy->>'max_infrastructure_retries')::integer FROM forge_agent.coding_review_budget b WHERE(b.owner_id,b.project_alias,b.root_task_id)=(w.owner_id,w.project_alias,w.root_task_id)),(w.snapshot->'repair_policy'->>'max_infrastructure_retries')::integer)
              ORDER BY q.updated_at DESC LIMIT 64""",(self.store.owner,list(self.store.allowed_projects)));rows=cursor.fetchall()
        for row in rows:
            try:self.retry_preparation(project_alias=row['project_alias'],task_id=row['id'],expected_revision=row['revision'],
                review_id=row['review_id'],reconciliation_receipt_id=row['reconciliation_receipt_id'],idempotency_key='prepare-retry-'+str(row['review_id']))
            except Rejected:pass

    def defer_source_readiness(self,*,project_alias,task_id,expected_revision,review_id,
                               reconciliation_receipt_id,idempotency_key):
        """Once deliver unstarted checks after a newer trusted source-ready receipt.

        This retains spent infrastructure/repair counters and the task revision;
        it does not resume a model, approve source, or retry an executed check.
        """
        self.store._require_project(project_alias)
        if (any(not isinstance(value,UUID) for value in (task_id,review_id,reconciliation_receipt_id))
            or type(expected_revision) is not int or expected_revision<1
            or not isinstance(idempotency_key,str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,128}',idempotency_key)):
            raise Rejected(400,'invalid_request')
        intent={'policy':'forge-source-readiness-delivery-v1','max_deliveries':1,'task_id':str(task_id),
                'source_revision':expected_revision,'review_id':str(review_id),'reconciliation_receipt_id':str(reconciliation_receipt_id),
                'idempotency_key':idempotency_key}
        key='source-readiness-delivery:'+str(review_id)
        with self._transaction() as cursor:
            self._project_lock(cursor,project_alias)
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE',
                           (self.store.owner,project_alias,task_id));task=cursor.fetchone()
            if not task:raise Rejected(404,'not_found')
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key=%s",(task['workflow_id'],key));prior=cursor.fetchone()
            if prior:
                if prior['intent']!=intent:raise Rejected(409,'idempotency_conflict')
                if prior['status']!='completed' or not prior['finish_receipt']:raise Rejected(409,'reconcile_required')
                return dict(prior['finish_receipt']['receipt'])
            from work_owner_control import owner_continuation_used
            if owner_continuation_used(cursor, task['workflow_id']):raise Rejected(409,'not_reviewable')
            cursor.execute('SELECT * FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND review_id=%s FOR UPDATE',
                           (self.store.owner,project_alias,task_id,review_id));q=cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_workflows WHERE owner_id=%s AND project_alias=%s AND workflow_id=%s FOR UPDATE',
                           (self.store.owner,project_alias,task['workflow_id']));w=cursor.fetchone()
            if not w:raise Rejected(409,'not_reviewable')
            self._current_policy(w);p=w['snapshot'];recipe=recipe_from_snapshot(p,'author_implement')
            if (w['phase']!='escalated' or w['enabled'] or not q or w['escalation_reason']!=q['reason']
                or q['reason'] not in {'review_unavailable','workspace_busy'} or q['phase']!='local' or q['status']!='handoff'
                or q['candidate_commit'] is not None or q['source_revision']!=expected_revision
                or q['attempt_id']!=task['latest_attempt_id'] or q['base_revision']!=task['base_revision']
                or q['plan_decision_id']!=task['plan_decision_id'] or q['scope_plan_decision_id']!=task['scope_plan_decision_id']
                or task['revision']!=expected_revision or task['status']!='awaiting_review' or task['mode']!='implement'
                or not task['workflow_required'] or task['workflow_review_id'] is not None or task['cancelled_at'] is not None
                or not task['parent_task_id'] or not task['scope_plan_decision_id']
                or task['agent_profile']!=p['author_profile'] or task['pinned_recipe']!=recipe.to_dict() or task['recipe_digest']!=recipe.digest
                or q['infrastructure_retries'] not in {p['repair_policy']['max_infrastructure_retries'],self._repair_limit(w,'max_infrastructure_retries')} or q['infrastructure_retries']<1):
                raise Rejected(409,'not_reviewable')
            failure=q['finish_receipt'] or {};outcome=failure.get('outcome',{});verification=outcome.get('verification',{})
            typed_busy=(q['reason']=='workspace_busy' and verification.get('failure_stage')=='snapshot'
                        and verification.get('exception_type')=='WorkspaceBusyError')
            legacy_busy=(q['reason']=='review_unavailable' and 'failure_stage' not in verification
                         and 'exception_type' not in verification)
            if (q['finish_token'] is None or failure.get('review_id')!=str(review_id) or failure.get('status')!='handoff'
                or outcome.get('reason')!=q['reason'] or outcome.get('candidate_commit') is not None
                or not(typed_busy or legacy_busy) or verification.get('checks_passed') is not False
                or verification.get('checks')!=[] or verification.get('cleanup_confirmed') is not True
                or q['verification'].get('checks_passed') is not False or q['verification'].get('checks')!=[]
                or q['verification'].get('cleanup_confirmed') is not True):raise Rejected(409,'not_reviewable')
            cursor.execute('SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND receipt_id=%s',
                           (self.store.owner,project_alias,task_id,reconciliation_receipt_id));rec=cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_attempts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s',
                           (self.store.owner,project_alias,task_id,task['latest_attempt_id']));attempt=cursor.fetchone()
            if (not rec or rec['attempt_id']!=task['latest_attempt_id'] or rec['task_revision']!=expected_revision
                or rec['created_at']<=q['updated_at'] or rec['workspace_digest']!=q['workspace_digest']
                or rec['receipt_id']==q['reconciliation_receipt_id'] or not attempt or attempt['phase']!='stopped'
                or attempt['reservation_active'] or attempt['lease_expires_at'] is not None or not attempt['published_at']
                or attempt['result_sha256']!=q['parent_result_sha256'] or attempt['pinned_recipe']!=recipe.to_dict()
                or attempt['recipe_digest']!=recipe.digest or rec['stop_proof'].get('scope_stopped') is not True
                or rec['stop_proof'].get('source')!='systemd' or rec['stop_proof'].get('unit_name')!=attempt['unit_name']):
                raise Rejected(409,'reconcile_required')
            self._artifact(cursor,task_id,task['latest_attempt_id'])
            cursor.execute("SELECT EXISTS(SELECT 1 FROM forge_agent.coding_tasks WHERE workflow_id=%s AND status IN('queued','running','cancel_requested','failed','needs_reconcile','quota_blocked','blocked_auth','blocked_permission','cancelled')) OR EXISTS(SELECT 1 FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id) WHERE t.workflow_id=%s AND(a.reservation_active OR a.lease_expires_at IS NOT NULL)) OR EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND status IN('pending','leased')) OR EXISTS(SELECT 1 FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(p.owner_id,p.project_alias,p.task_id) WHERE t.workflow_id=%s AND p.status IN('pending','checking','publishing')) OR EXISTS(SELECT 1 FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id) WHERE t.workflow_id=%s AND q.status IN('pending','checking')) AS busy",(w['workflow_id'],)*5)
            if cursor.fetchone()['busy']:raise Rejected(409,'reconcile_required')
            cursor.execute('SELECT max(review_no) AS latest FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s',
                           (self.store.owner,project_alias,task_id))
            if cursor.fetchone()['latest']!=q['review_no']:raise Rejected(409,'not_reviewable')
            cursor.execute("SELECT count(*) AS used FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND source_key LIKE 'source-readiness-delivery:%%'",(w['workflow_id'],))
            if cursor.fetchone()['used']:raise Rejected(409,'not_reviewable')
            cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase='checks',escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=%s",(w['workflow_id'],))
            self._grant(cursor,w,p['author_profile'],'implement');self._grant(cursor,w,p['reviewer_profile'],'review')
            cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND policy_digest=ANY(%s) AND observed_at>=clock_timestamp()-interval '90 seconds'",(self.store.owner,[x[0] for x in NATIVE_POLICIES.values()]));ready=cursor.fetchall()
            for profile,mode,entry in ((p['author_profile'],'implement','author_implement'),(p['reviewer_profile'],'review','reviewer_review')):
                required=recipe_from_snapshot(p,entry)
                if not _ready_for_recipe(ready,project_alias,profile,mode,required):raise Rejected(503,'unavailable')
            cursor.execute("""INSERT INTO forge_agent.coding_reviews(owner_id,project_alias,task_id,attempt_id,plan_decision_id,scope_plan_decision_id,reconciliation_receipt_id,
              base_revision,workspace_digest,parent_result_sha256,source_revision,review_no,round,stagnant_rounds,infrastructure_retries,previous_review_id,
              findings,verified_finding_ids,verification,summary,phase,owner_decision)
              SELECT owner_id,project_alias,task_id,attempt_id,plan_decision_id,scope_plan_decision_id,%s,
              base_revision,workspace_digest,parent_result_sha256,source_revision,review_no+1,round,stagnant_rounds,infrastructure_retries,review_id,
              findings,verified_finding_ids,verification-'failure_stage'-'exception_type',%s,phase,owner_decision
              FROM forge_agent.coding_reviews WHERE review_id=%s RETURNING review_id""",
              (reconciliation_receipt_id,'Deliver stopped source checks after trusted resource-readiness reconciliation.',review_id))
            successor=cursor.fetchone()['review_id']
            receipt={'status':'source_readiness_delivered','project_alias':project_alias,'task_id':str(task_id),'revision':expected_revision,
                     'source_review_id':str(review_id),'review_id':str(successor),'reconciliation_receipt_id':str(reconciliation_receipt_id),
                     'infrastructure_retries':q['infrastructure_retries'],'delivery_count':1}
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt) VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",
                           (w['workflow_id'],self.store.owner,project_alias,key,Json(intent),Json({'status':'source_readiness_delivered','receipt':receipt})))
            insert_event(cursor,self.store.owner,project_alias,task_id,'checkpoint',receipt,actor=COORDINATOR_ACTOR)
            cursor.execute("SELECT pg_notify('forge_coding_jobs','')")
            return receipt

    def _recover_source_readiness(self):
        with self.store._transaction(read_only=True) as cursor:
            cursor.execute("""SELECT t.project_alias,t.id,t.revision,q.review_id,r.receipt_id FROM forge_agent.coding_tasks t
              JOIN forge_agent.coding_workflows w USING(workflow_id) JOIN forge_agent.coding_reviews q
              ON(q.owner_id,q.project_alias,q.task_id,q.attempt_id)=(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)
              JOIN forge_agent.coding_reconciliation_receipts r ON(r.owner_id,r.project_alias,r.task_id,r.attempt_id,r.task_revision)=(t.owner_id,t.project_alias,t.id,t.latest_attempt_id,t.revision)
              WHERE t.owner_id=%s AND t.project_alias=ANY(%s) AND t.workflow_required AND t.parent_task_id IS NOT NULL
              AND t.status='awaiting_review' AND t.cancelled_at IS NULL AND w.phase='escalated' AND NOT w.enabled
              AND q.phase='local' AND q.status='handoff' AND q.candidate_commit IS NULL AND q.source_revision=t.revision
              AND q.reason IN('review_unavailable','workspace_busy') AND w.escalation_reason=q.reason
              AND q.infrastructure_retries IN((w.snapshot->'repair_policy'->>'max_infrastructure_retries')::integer,
                coalesce((SELECT(b.policy->>'max_infrastructure_retries')::integer FROM forge_agent.coding_review_budget b WHERE(b.owner_id,b.project_alias,b.root_task_id)=(w.owner_id,w.project_alias,w.root_task_id)),(w.snapshot->'repair_policy'->>'max_infrastructure_retries')::integer))
              AND q.infrastructure_retries>0 AND r.created_at>q.updated_at AND r.workspace_digest=q.workspace_digest
              AND q.finish_token IS NOT NULL AND q.finish_receipt->>'status'='handoff'
              AND q.finish_receipt->'outcome'->'verification'->'checks'='[]'::jsonb
              AND NOT EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions a WHERE a.workflow_id=w.workflow_id AND a.source_key LIKE 'source-readiness-delivery:%%')
              ORDER BY r.created_at DESC LIMIT 64""",(self.store.owner,list(self.store.allowed_projects)));rows=cursor.fetchall()
        for row in rows:
            try:self.defer_source_readiness(project_alias=row['project_alias'],task_id=row['id'],expected_revision=row['revision'],review_id=row['review_id'],
                reconciliation_receipt_id=row['receipt_id'],idempotency_key='source-ready-'+str(row['review_id']))
            except Rejected:pass

    def recover(self,cursor):
        """Authoritative replay fills missed callbacks using immutable source keys."""
        cursor.execute("SELECT * FROM forge_agent.coding_workflows WHERE owner_id=%s AND phase NOT IN('completed','cancelled','escalated') ORDER BY created_at",(self.store.owner,))
        for w in cursor.fetchall():
            try:self._current_policy(w)
            except Rejected:self._escalate(cursor,w,'Workflow delegation or role grant is no longer current.');continue
            cursor.execute("SELECT t.*,a.phase AS attempt_phase,a.result_sha256,a.reservation_active,a.lease_expires_at,a.published_at FROM forge_agent.coding_tasks t LEFT JOIN forge_agent.coding_attempts a ON(a.owner_id,a.project_alias,a.task_id,a.attempt_id)=(t.owner_id,t.project_alias,t.id,t.latest_attempt_id) WHERE t.owner_id=%s AND t.project_alias=%s AND t.workflow_id=%s ORDER BY t.created_at",(self.store.owner,w['project_alias'],w['workflow_id']))
            tasks=cursor.fetchall()
            for t in tasks:
                if t['status'] in {'quota_blocked','blocked_auth','blocked_permission','needs_reconcile','failed'}:
                    self._escalate(cursor,w,'Agent stopped: '+t['status']);break
                if t['status']=='cancelled':
                    self._escalate(cursor,w,'Workflow task was cancelled.');break
                if t['status'] in {'queued','running'}:
                    if t['workflow_review_id'] is not None:
                        cursor.execute("SELECT target_kind FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s",(t['workflow_review_id'],));peer=cursor.fetchone()
                        if peer:self._phase(cursor,w,'code_review' if peer['target_kind']=='code' else 'plan_review')
                    elif t['mode']=='plan':self._phase(cursor,w,'plan_revision' if w['plan_revisions'] else 'plan')
                    elif t['mode']=='implement':self._phase(cursor,w,'repair' if t['feedback_id'] else 'implement')
                if t['status']!='awaiting_review' or t['attempt_phase']!='stopped' or t['reservation_active'] or t['lease_expires_at'] is not None or not t['published_at'] or not t['result_sha256']:continue
                cursor.execute("SELECT EXISTS(SELECT 1 FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s) AS ready",(self.store.owner,w['project_alias'],t['id'],t['latest_attempt_id']))
                if not cursor.fetchone()['ready']:continue
                if t['workflow_review_id'] is not None:
                    cursor.execute("SELECT target_kind,status FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s",(t['workflow_review_id'],))
                    peer=cursor.fetchone()
                    if peer and peer['status']=='reviewing':self._phase(cursor,w,'code_review' if peer['target_kind']=='code' else 'plan_review')
                    cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,workflow_review_id,kind,source_key) SELECT workflow_id,owner_id,project_alias,workflow_review_id,'decide',%s FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s AND status='reviewing' ON CONFLICT DO NOTHING",(str(t['latest_attempt_id'])+':'+t['result_sha256'],t['workflow_review_id']))
                    continue
                if t['mode']=='plan':self._queue_review(cursor,w,t,None)
                elif t['mode']=='implement':
                    if self.retry_events is not None:
                        from work_retry_events import RetryScope
                        self.retry_events.resolve_verified(cursor,RetryScope(self.store.owner,w['project_alias'],'native_workflow',w['workflow_id']),
                            'infrastructure','native-artifact:'+str(t['latest_attempt_id']),t['result_sha256'],
                            phase_id=self._native_deadline_phase(t['id']))
                    cursor.execute("SELECT * FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s AND phase='local' ORDER BY created_at DESC LIMIT 1",(self.store.owner,w['project_alias'],t['id'],t['latest_attempt_id']))
                    q=cursor.fetchone()
                    if q and q['status']=='awaiting_review' and q['verification'].get('checks_passed') is True and q['verification'].get('cleanup_confirmed') is True:
                        self._queue_review(cursor,w,t,q)
                    elif q and q['status'] in {'handoff','needs_reconcile','revise_plan'}:self._escalate(cursor,w,q['reason'] or q['status'])
                    elif q and q['status'] in {'pending','checking','retry_infrastructure'}:self._phase(cursor,w,'checks')
                    elif q and q['status']=='repair':self._phase(cursor,w,'repair')
                    elif q and q['status']=='approved':
                        if q['repair_task_id'] is not None:
                            self._phase(cursor,w,'repair')
                            continue
                        cursor.execute("SELECT publication_id,status FROM forge_agent.coding_publications WHERE review_id=%s ORDER BY created_at DESC LIMIT 1",(q['review_id'],));publication=cursor.fetchone()
                        if publication and publication['status']=='published':
                            cursor.execute("SELECT EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions WHERE source_publication_id=%s AND kind='merge' AND status IN('pending','leased')) AS active",(publication['publication_id'],))
                            self._phase(cursor,w,'merging' if cursor.fetchone()['active'] else 'hosted')
                        elif publication and publication['status']=='failed':self._escalate(cursor,w,'Trusted publication stopped; retained source and remote metadata require reconciliation.')
                        else:self._phase(cursor,w,'publishing')
            cursor.execute("SELECT enabled,phase FROM forge_agent.coding_workflows WHERE workflow_id=%s",(w['workflow_id'],));active=cursor.fetchone()
            if not active or not active['enabled'] or active['phase'] in {'completed','cancelled','escalated'}:continue
            cursor.execute("""SELECT p.*,q.candidate_commit,q.gate_manifest_sha256 FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)=(p.owner_id,p.project_alias,p.task_id,p.attempt_id)
              JOIN forge_agent.coding_reviews q ON q.review_id=p.review_id WHERE t.workflow_id=%s AND p.status='published' AND q.status='approved' AND q.repair_task_id IS NULL AND q.owner_decision->>'closed_by'='peer_reviewer'
              AND (SELECT h.required_green FROM forge_agent.coding_hosted_checks h WHERE h.publication_id=p.publication_id AND h.candidate_commit=p.commit_sha ORDER BY h.observed_at DESC,h.observation_id DESC LIMIT 1) IS TRUE ORDER BY p.created_at DESC""",(w['workflow_id'],))
            for p in cursor.fetchall():
                if not w['snapshot']['auto_merge']:continue
                cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,source_publication_id) VALUES(%s,%s,%s,'merge',%s,%s) ON CONFLICT DO NOTHING",(w['workflow_id'],self.store.owner,w['project_alias'],str(p['publication_id'])+':'+p['commit_sha'],p['publication_id']))
                cursor.execute("UPDATE forge_agent.coding_workflows SET phase='merging',updated_at=clock_timestamp() WHERE workflow_id=%s AND phase NOT IN('cancelled','escalated','completed')",(w['workflow_id'],))

    def _queue_review(self,cursor,w,t,q):
        cursor.execute("""INSERT INTO forge_agent.coding_workflow_reviews(workflow_id,owner_id,project_alias,target_kind,source_task_id,source_attempt_id,source_result_sha256,source_revision,source_review_id,candidate_commit,gate_manifest_sha256)
          VALUES(%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s) ON CONFLICT DO NOTHING RETURNING workflow_review_id""",
          (w['workflow_id'],self.store.owner,w['project_alias'],'code' if q else 'plan',t['id'],t['latest_attempt_id'],t['result_sha256'],t['revision'],q['review_id'] if q else None,q['candidate_commit'] if q else None,q['gate_manifest_sha256'] if q else None))
        created=cursor.fetchone()
        if created is None:return
        rid=created['workflow_review_id']
        cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,workflow_review_id,kind,source_key) VALUES(%s,%s,%s,%s,'review',%s)",(w['workflow_id'],self.store.owner,w['project_alias'],rid,str(rid)))
        cursor.execute("UPDATE forge_agent.coding_workflows SET phase=%s,updated_at=clock_timestamp() WHERE workflow_id=%s",('code_review' if q else 'plan_review',w['workflow_id']))

    def claim(self,worker_id):
        self._apply_retry_allowances()
        if self.feedback_recovery is not None:
            self.feedback_recovery.run_once()
        if self.peer_feedback_recovery is not None:
            self.peer_feedback_recovery.run_once()
        self._recover_source_readiness()
        self._recover_preparations()
        self._recover_peer_verdicts()
        if self.superseding_review is not None:
            self.superseding_review.recover()
        self._recover_native_timeouts()
        with self._transaction() as cursor:
            for project in sorted(self.store.allowed_projects):self._project_lock(cursor,project)
            self.recover(cursor)
            cursor.execute("""SELECT a.* FROM forge_agent.coding_workflow_actions a JOIN forge_agent.coding_workflows w ON w.workflow_id=a.workflow_id WHERE a.owner_id=%s AND a.kind IN('review','decide') AND w.enabled AND w.phase NOT IN('completed','cancelled','escalated') AND(a.status='pending' OR(a.status='leased' AND a.lease_expires_at<=clock_timestamp())) ORDER BY a.created_at FOR UPDATE OF a SKIP LOCKED LIMIT 1""",(self.store.owner,))
            action=cursor.fetchone()
            if not action:return None
            cursor.execute("UPDATE forge_agent.coding_workflow_actions SET status='leased',worker_id=%s,lease_token=gen_random_uuid(),lease_expires_at=clock_timestamp()+interval '60 seconds',updated_at=clock_timestamp() WHERE action_id=%s RETURNING *",(worker_id,action['action_id']))
            return dict(cursor.fetchone())

    def _artifact(self,cursor,task_id,attempt_id):
        cursor.execute("SELECT a.result_sha256,a.result_byte_count,a.phase,a.reservation_active,a.lease_expires_at,string_agg(c.content,'' ORDER BY c.chunk_index) AS content FROM forge_agent.coding_attempts a JOIN forge_agent.coding_result_chunks c ON(c.owner_id,c.project_alias,c.task_id,c.attempt_id)=(a.owner_id,a.project_alias,a.task_id,a.attempt_id) WHERE a.owner_id=%s AND a.task_id=%s AND a.attempt_id=%s GROUP BY a.owner_id,a.project_alias,a.task_id,a.attempt_id",(self.store.owner,task_id,attempt_id))
        a=cursor.fetchone()
        if not a or a['phase']!='stopped' or a['reservation_active'] or a['lease_expires_at'] is not None or a['content'] is None or len(a['content'].encode())!=a['result_byte_count'] or hashlib.sha256(a['content'].encode()).hexdigest()!=a['result_sha256']:raise Rejected(409,'result_mismatch')
        return a

    def process(self,job):
        from work_peer_review import PeerReviewError,validate_review_target,parse_review_target,validate_live_review_verdict
        with self._transaction() as cursor:
            self._project_lock(cursor,job["project_alias"])
            # Task -> peer review -> action is shared with owner/cancel paths.
            cursor.execute("SELECT r.source_task_id,w.root_task_id FROM forge_agent.coding_workflow_reviews r JOIN forge_agent.coding_workflows w USING(workflow_id) WHERE r.workflow_review_id=%s",(job['workflow_review_id'],));identity=cursor.fetchone()
            if not identity:return
            if identity['root_task_id']!=identity['source_task_id']:
                cursor.execute("SELECT id FROM forge_agent.coding_tasks WHERE owner_id=%s AND id=%s FOR UPDATE",(self.store.owner,identity['root_task_id']))
            cursor.execute("SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND id=%s FOR UPDATE",(self.store.owner,identity['source_task_id']));task=cursor.fetchone()
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s FOR UPDATE",(job['workflow_review_id'],));review=dict(cursor.fetchone())
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_actions WHERE action_id=%s FOR UPDATE",(job['action_id'],));action=cursor.fetchone()
            if not action or action['status']!='leased' or action['lease_token']!=job['lease_token']:return
            cursor.execute("SELECT %s>clock_timestamp() AS valid",(action['lease_expires_at'],))
            if not cursor.fetchone()['valid']:raise Rejected(409,'reconcile_required')
            cursor.execute("SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s FOR UPDATE",(action['workflow_id'],));w=cursor.fetchone()
            cursor.execute('SAVEPOINT workflow_action')
            try:
                self._current_policy(w)
                if not w['enabled'] or w['phase'] in {'cancelled','escalated','completed'}:raise Rejected(403,'denied')
                if task['revision']!=review['source_revision'] or task['latest_attempt_id']!=review['source_attempt_id'] or task['status']!='awaiting_review':raise Rejected(409,'result_mismatch')
                if action['kind']=='review':
                    receipt=self._admit_reviewer(cursor,w,review,task,validate_review_target)
                else:
                    target=parse_review_target(review['target_json'],review['target_sha256'])
                    receipt=self._apply_verdict(cursor,w,review,task,action,target,validate_live_review_verdict)
            except (Rejected,ValueError,KeyError) as error:
                cursor.execute('ROLLBACK TO SAVEPOINT workflow_action')
                if isinstance(error,Rejected) and (error.code=='reconcile_required' or error.status>=500):return
                self._escalate(cursor,w,'Peer review could not safely continue: '+(getattr(error,'code',None) or type(error).__name__))
                # The locked SELECT * row identifies whether this additive
                # field is installed. Historical schemas still escalate safely
                # without issuing SQL against a column they do not have.
                if 'diagnostics' in review:
                    diagnostics=None
                    if isinstance(error,PeerReviewError) and error.check is not None:
                        diagnostics={"check":error.check,"detail":error.detail}
                    cursor.execute("UPDATE forge_agent.coding_workflow_reviews SET status='escalated',diagnostics=%s,updated_at=clock_timestamp() WHERE workflow_review_id=%s",(Json(diagnostics) if diagnostics is not None else None,review['workflow_review_id']))
                else:
                    cursor.execute("UPDATE forge_agent.coding_workflow_reviews SET status='escalated',updated_at=clock_timestamp() WHERE workflow_review_id=%s",(review['workflow_review_id'],))
                return
            cursor.execute("UPDATE forge_agent.coding_workflow_actions SET status='completed',finish_token=%s,finish_receipt=%s,lease_token=NULL,lease_expires_at=NULL,updated_at=clock_timestamp() WHERE action_id=%s",(action['lease_token'],Json(receipt),action['action_id']))

    def _admit_reviewer(self,cursor,w,review,task,validate_target,*,idempotency_key=None):
        p=w['snapshot'];self._grant(cursor,w,p['reviewer_profile'],'review')
        source=self._artifact(cursor,task['id'],review['source_attempt_id']);reviewer_id=uuid4()
        cursor.execute("SELECT prompt,acceptance_criteria FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s",(self.store.owner,w['project_alias'],w['root_task_id']));assignment=cursor.fetchone()
        if assignment is None:raise Rejected(409,'result_mismatch')
        target={'version':1,'workflow_id':str(w['workflow_id']),'workflow_review_id':str(review['workflow_review_id']),'policy_digest':w['policy_digest'],
            'target_kind':review['target_kind'],'reviewer_task_id':str(reviewer_id),'author_profile':p['author_profile'],'reviewer_profile':p['reviewer_profile'],
            'source_task_id':str(task['id']),'source_attempt_id':str(review['source_attempt_id']),'source_result_sha256':review['source_result_sha256'],
            'source_revision':review['source_revision'],'base_revision':review['candidate_commit'] or task['base_revision'],'plan_revision':w['plan_revisions'],
            'plan_decision_id':None,'review_id':str(review['source_review_id']) if review['source_review_id'] else None,'candidate_commit':review['candidate_commit'],
            'gate_manifest_sha256':review['gate_manifest_sha256'],'integration_base':None,'workspace_digest':None,'source_text':source['content'],'request_prompt':assignment['prompt'],'acceptance_criteria':assignment['acceptance_criteria'],
            'findings':[],'verification':None,'approved_plan':None}
        if review['target_kind']=='code':
            cursor.execute("SELECT * FROM forge_agent.coding_reviews WHERE review_id=%s FOR UPDATE",(review['source_review_id'],));q=cursor.fetchone()
            if q['status']!='awaiting_review' or q['verification'].get('checks_passed') is not True or q['verification'].get('cleanup_confirmed') is not True:raise Rejected(409,'not_reviewable')
            cursor.execute("SELECT * FROM forge_agent.coding_plan_decisions WHERE owner_id=%s AND project_alias=%s AND decision_id=%s",(self.store.owner,w['project_alias'],task['plan_decision_id'] or task['scope_plan_decision_id']));plan=cursor.fetchone()
            if not plan or plan['decision']!='resume':raise Rejected(409,'not_reviewable')
            plan_source=self._artifact(cursor,plan['task_id'],plan['plan_attempt_id'])
            target.update(plan_decision_id=str(plan['decision_id']),integration_base=q['verification']['integration_base'],workspace_digest=q['verification']['candidate_workspace_digest'],findings=[{**x,'status':'verified' if x['finding_id'] in q['verified_finding_ids'] else x.get('status','open')} for x in q['findings']],
                verification={'checks_passed':True,'toolchain':q['verification'].get('toolchain','Not reported'),
                    'checks':[{'check_id':x['check_id'],'status':x['status'],'head_sha':x['head_sha']} for x in q['verification'].get('checks',[])]},
                approved_plan={'attempt_id':str(plan['plan_attempt_id']),'sha256':plan['plan_sha256'],'content':plan_source['content']})
        target=validate_target(target);raw=json.dumps(target,ensure_ascii=False,separators=(',',':'),allow_nan=False)
        if len(raw.encode())>MAX_TARGET_BYTES:raise ValueError('target_resource_bound')
        digest=hashlib.sha256(raw.encode()).hexdigest();recipe=recipe_from_snapshot(p,'reviewer_review')
        cursor.execute("SELECT count(*) AS n FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND status IN('queued','running','cancel_requested')",(self.store.owner,w['project_alias']))
        if cursor.fetchone()['n']>=self.store.max_pending:raise Rejected(503,'queue_full')
        prompt='Independently review the immutable agent-review.json target. Return exactly the specified source-bound JSON verdict. Do not modify source, execute builds, or implement fixes.'
        if review.get('supersedes_review_id') is not None:
            prompt+=' Copy context_sha256 exactly as '+digest+' (64 lowercase hexadecimal characters). Check every identity against the supplied target before returning JSON.'
        receipt={'task_id':str(reviewer_id),'project_alias':w['project_alias'],'revision':1,'status':'queued','recipe_digest':recipe.digest}
        cursor.execute("""INSERT INTO forge_agent.coding_tasks(owner_id,project_alias,id,idempotency_key,intent_digest,base_revision,mode,agent_profile,title,prompt,acceptance_criteria,pinned_recipe,recipe_digest,status,revision,auto_repair,review_required,workflow_required,workflow_id,workflow_review_id,review_target_sha256)
          VALUES(%s,%s,%s,%s,%s,%s,'review',%s,%s,%s,%s,%s,%s,'queued',1,false,false,true,%s,%s,%s)""",
          (self.store.owner,w['project_alias'],reviewer_id,idempotency_key or 'peer-'+str(review['workflow_review_id']),sha256_canonical(target),target['base_revision'],p['reviewer_profile'],prepare_title('Review '+task['title']),prompt,'Return a strict source-bound verdict only.',Json(recipe.to_dict()),recipe.digest,w['workflow_id'],review['workflow_review_id'],digest))
        cursor.execute("UPDATE forge_agent.coding_workflow_reviews SET target_json=%s,target_sha256=%s,reviewer_task_id=%s,status='reviewing',updated_at=clock_timestamp() WHERE workflow_review_id=%s",(raw,digest,reviewer_id,review['workflow_review_id']))
        insert_event(cursor,self.store.owner,w['project_alias'],reviewer_id,'queued',receipt,actor=COORDINATOR_ACTOR,first=True)
        cursor.execute("SELECT pg_notify('forge_coding_jobs','')")
        return receipt

    def _code_review_source(self,cursor,review,task,authority):
        authority.validate(cursor,task,'code')
        cursor.execute("SELECT * FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND review_id=%s FOR UPDATE",
            (self.store.owner,task['project_alias'],task['id'],review['source_review_id']))
        source=cursor.fetchone()
        if (not source or source['status']!='awaiting_review' or source['attempt_id']!=task['latest_attempt_id']
            or source['plan_decision_id']!=task['plan_decision_id'] or source['scope_plan_decision_id']!=task['scope_plan_decision_id']
            or source['candidate_commit']!=review['candidate_commit'] or source['gate_manifest_sha256']!=review['gate_manifest_sha256']
            or source['verification'].get('checks_passed') is not True or source['verification'].get('cleanup_confirmed') is not True):
            raise Rejected(409,'result_mismatch')
        return source

    def _exhausted_code_budget(self,cursor,task,source):
        # Read under the existing project/root/source locks. The API has no
        # budget UPDATE grant; canonical repair admission retains its own row
        # lock and final recheck. Success cannot reset the frozen ledger.
        cursor.execute("SELECT repair_rounds,stagnant_rounds,policy FROM forge_agent.coding_review_budget WHERE owner_id=%s AND project_alias=%s AND root_task_id=%s",
            (self.store.owner,task['project_alias'],task['repair_root_task_id'] or task['id']))
        budget=cursor.fetchone()
        if budget is None:return None
        evidence={'repair_rounds':budget['repair_rounds'],'max_rounds':budget['policy']['max_rounds'],
            'stagnant_rounds':max(budget['stagnant_rounds'],source['stagnant_rounds']),
            'max_stagnant_rounds':budget['policy']['max_stagnant_rounds']}
        if evidence['repair_rounds']>=evidence['max_rounds'] or evidence['stagnant_rounds']>=evidence['max_stagnant_rounds']:
            return evidence
        return None

    def _retry_scope(self,w):
        from work_retry_events import RetryScope
        return RetryScope(self.store.owner,w['project_alias'],'native_workflow',UUID(str(w['workflow_id'])))

    def _code_handoff(self,cursor,w,review,reason,budget=None):
        note=('Independent code review requested revisions; the frozen repair budget is exhausted.'
            if reason=='repair_budget_exhausted' else 'The source-bound code decision was rejected by a repair/publication guard; retained verdict requires reconciliation.')
        self._escalate(cursor,w,note)
        receipt={'status':'escalated','reason':reason,'workflow_id':str(w['workflow_id']),
            'workflow_review_id':str(review['workflow_review_id']),'review_id':str(review['source_review_id'])}
        if budget is not None:receipt['budget']=budget
        return receipt

    def _apply_verdict(self,cursor,w,review,task,action,target,parse_verdict):
        p=w['snapshot'];self._grant(cursor,w,p['reviewer_profile'],'review')
        cursor.execute("SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE",(self.store.owner,w['project_alias'],review['reviewer_task_id']));critic=cursor.fetchone()
        expected=recipe_from_snapshot(p,'reviewer_review')
        if not critic or critic['mode']!='review' or critic['agent_profile']!=p['reviewer_profile'] or critic['recipe_digest']!=expected.digest or critic['workflow_review_id']!=review['workflow_review_id'] or critic['status']!='awaiting_review':raise Rejected(409,'result_mismatch')
        result=self._artifact(cursor,critic['id'],critic['latest_attempt_id'])
        from work_peer_review import IncompleteApprovalError, PeerReviewError
        try:
            verdict=parse_verdict(result['content'],target)
        except IncompleteApprovalError as error:
            if self.reviewer_correction is None:raise Rejected(503,'unavailable')
            return self.reviewer_correction.correct(cursor,w,review,task,critic,result,error,action=action)
        except PeerReviewError as error:
            if self.superseding_review is not None:
                receipt=self.superseding_review.correct(cursor,w,review,task,critic,result,error,action=action)
                if receipt is not None:return receipt
            raise
        cursor.execute("UPDATE forge_agent.coding_workflow_reviews SET reviewer_attempt_id=%s,reviewer_result_sha256=%s,verdict_json=%s,verdict_sha256=%s WHERE workflow_review_id=%s",(critic['latest_attempt_id'],result['result_sha256'],result['content'],hashlib.sha256(result['content'].encode()).hexdigest(),review['workflow_review_id']))
        review.update(reviewer_attempt_id=critic['latest_attempt_id'],reviewer_result_sha256=result['result_sha256'])
        authority=BoundDelegatedAuthority(self.store,cursor,w,action,review,verdict)
        decision=verdict['decision'];key='peer-'+str(review['workflow_review_id'])
        retry_event=None;event_exhausted=False
        if self.retry_events is not None:
            scope=self._retry_scope(w);kind='planning' if review['target_kind']=='plan' else 'review'
            evidence=hashlib.sha256(result['content'].encode()).hexdigest();source=str(review['workflow_review_id'])
            family=(self.superseding_review.root_review_id(cursor,review)
                if self.superseding_review is not None and review.get('supersedes_review_id') is not None else review['workflow_review_id'])
            self.retry_events.resolve_verified(cursor,scope,'reviewer_correction',source,evidence,phase_id='reviewer_correction.'+str(family))
            if decision=='approve':self.retry_events.resolve_verified(cursor,scope,kind,source,evidence)
            elif decision=='revise':
                retry_event=self.retry_events.observe_failure(cursor,scope,kind,source,evidence)
                if review['target_kind']=='plan':retry_event=self.retry_events.reserve(cursor,retry_event['event_id'],'plan-revision.'+source)
                event_exhausted=retry_event['status']=='exhausted'
                if review['target_kind']=='code' and not event_exhausted:
                    self.retry_events.bind_review(cursor,review['source_review_id'],retry_event['event_id'],'peer-revision.'+source)

        if decision=='escalate' or event_exhausted or(review['target_kind']=='plan' and decision=='revise' and self.retry_events is None and w['plan_revisions']>=self._retry_limit(w,'max_plan_revisions')):
            self._escalate(cursor,w,verdict['summary']);receipt={'status':'escalated','workflow_id':str(w['workflow_id'])}
        elif review['target_kind']=='plan':
            mode='implement' if decision=='approve' else 'plan';self._grant(cursor,w,p['author_profile'],mode)
            note=verdict['summary']
            if verdict['findings']:note+='\n'+json.dumps(verdict['findings'],ensure_ascii=False,separators=(',',':'))
            if len(note.encode())>2048:note=verdict['summary'] # full source-bound verdict remains in readonly Plan feedback below
            receipt=self.store.plan_decide(project_alias=w['project_alias'],task_id=task['id'],expected_revision=task['revision'],plan_attempt_id=review['source_attempt_id'],plan_sha256=review['source_result_sha256'],decision='resume' if decision=='approve' else 'revise',agent_profile=p['author_profile'],idempotency_key=key,note=note,resolve_recipe=lambda _:recipe_from_snapshot(p,'author_implement' if decision=='approve' else 'author_plan'),auto_repair=True,_authority=authority,actor=COORDINATOR_ACTOR)
            cursor.execute("UPDATE forge_agent.coding_workflows SET phase=%s,plan_revisions=plan_revisions+%s,updated_at=clock_timestamp() WHERE workflow_id=%s",('implement' if decision=='approve' else 'plan_revision',0 if decision=='approve' else 1,w['workflow_id']))
        else:
            source=self._code_review_source(cursor,review,task,authority)
            budget=self._exhausted_code_budget(cursor,task,source) if decision=='revise' and self.retry_events is None else None
            from work_owner_control import owner_continuation_used
            owner_spent = decision == 'revise' and owner_continuation_used(cursor, w['workflow_id'])
            if owner_spent:
                authority.validate(cursor,task,'code')
                receipt=self._code_handoff(cursor,w,review,'owner_continuation_exhausted',budget)
            elif budget is not None:
                authority.validate(cursor,task,'code')
                receipt=self._code_handoff(cursor,w,review,'repair_budget_exhausted',budget)
            else:
                if decision=='revise':self._grant(cursor,w,p['author_profile'],'implement')
                # A delegated transition shares this transaction. Catch only its
                # typed SQL guard rejection here, before the outer transaction
                # maps it and loses the genuine immutable peer verdict.
                cursor.execute('SAVEPOINT peer_code_decision')
                try:
                    receipt=self.store.review_decide(project_alias=w['project_alias'],task_id=task['id'],expected_revision=task['revision'],review_id=review['source_review_id'],decision='approve' if decision=='approve' else 'repair',agent_profile=p['author_profile'],note=verdict['summary'],idempotency_key=key,findings=verdict['findings'],verified_finding_ids=verdict['attested_finding_ids'],resolve_recipe=lambda:recipe_from_snapshot(p,'author_implement'),_authority=authority,actor=COORDINATOR_ACTOR)
                except psycopg2.Error as error:
                    if error.pgcode!='P0009':raise
                    cursor.execute('ROLLBACK TO SAVEPOINT peer_code_decision')
                    authority.validate(cursor,task,'code')
                    receipt=self._code_handoff(cursor,w,review,'code_transition_not_reviewable')
                cursor.execute('RELEASE SAVEPOINT peer_code_decision')
                if receipt.get('status')!='escalated':
                    cursor.execute("UPDATE forge_agent.coding_workflows SET phase=%s,updated_at=clock_timestamp() WHERE workflow_id=%s",('publishing' if decision=='approve' else 'repair',w['workflow_id']))
        if receipt.get('status')!='escalated' and decision in {'approve','revise'} and not (review['target_kind']=='plan' and decision=='revise' and self.retry_events is None and w['plan_revisions']>=self._retry_limit(w,'max_plan_revisions')):
            next_task=UUID(receipt['repair_task_id']) if receipt.get('repair_task_id') else task['id']
            cursor.execute("UPDATE forge_agent.coding_tasks SET workflow_feedback_review_id=%s,workflow_feedback_sha256=%s WHERE owner_id=%s AND project_alias=%s AND id=%s",(review['workflow_review_id'],hashlib.sha256(result['content'].encode()).hexdigest(),self.store.owner,w['project_alias'],next_task))
        cursor.execute("UPDATE forge_agent.coding_workflow_reviews SET status='decided',updated_at=clock_timestamp() WHERE workflow_review_id=%s",(review['workflow_review_id'],))
        cursor.execute("UPDATE forge_agent.coding_tasks SET status='completed',accepted_at=clock_timestamp(),revision=revision+1,updated_at=clock_timestamp() WHERE owner_id=%s AND project_alias=%s AND id=%s",(self.store.owner,w['project_alias'],critic['id']))
        return receipt


class WorkflowCoordinator:
    def __init__(self,store,*,worker_id='workflow-coordinator',interval_seconds=30,external_maintenance=None,external_workflow=None,delivery_repair=None,clock=time.monotonic):
        if external_maintenance is not None and not callable(external_maintenance):
            raise TypeError('External maintenance requires an injected callback')
        self.store=store;self.worker_id=worker_id;self.interval_seconds=interval_seconds;self.stop=threading.Event();self._thread=None
        if delivery_repair is not None and not callable(getattr(delivery_repair,'run_once',None)):
            raise TypeError('Delivery repair requires a ready run_once port')
        self._delivery_repair=delivery_repair
        self._external_workflow=external_workflow;self._external_maintenance=external_maintenance;self._clock=clock;self._next_external=float('-inf')
    def run_once(self):
        if self._external_maintenance is not None and self._clock()>=self._next_external:
            self._next_external=self._clock()+30
            self._external_maintenance()
        external_changed=self._external_workflow.run_once() if self._external_workflow is not None else False
        job=self.store.claim(self.worker_id)
        if job:self.store.process(job)
        repaired=self._delivery_repair.run_once() if self._delivery_repair is not None else False
        return job is not None or external_changed or repaired
    def retry_native_timeout(self,**request):
        return self.store.retry_native_timeout(**request)
    def retry_peer_verdict(self,**request):
        return self.store.retry_peer_verdict(**request)
    def retry_preparation(self,**request):
        return self.store.retry_preparation(**request)
    def defer_source_readiness(self,**request):
        return self.store.defer_source_readiness(**request)
    def run(self):
        while not self.stop.is_set():
            try:
                # Subscribe before scanning so a commit cannot fall between scan and wait.
                with self.store.listen() as listener:
                    while not self.stop.is_set():
                        while not self.stop.is_set() and self.run_once():pass
                        self.store.wait(listener,self.interval_seconds,self.stop)
            except Exception:
                LOG.exception('Workflow coordination unavailable')
                self.stop.wait(1)
    def start(self):
        if self._thread is not None:raise RuntimeError('Workflow coordinator already started')
        self._thread=threading.Thread(target=self.run,name='forge-workflow',daemon=True);self._thread.start()
    def close(self):
        self.stop.set()
        if self._thread:self._thread.join(timeout=15)
        if self._thread and self._thread.is_alive():raise RuntimeError('Workflow coordinator did not quiesce')


class WorkflowCoordinatorFactory:
    def __init__(self,config,catalog,store,*,external_maintenance=None,external_workflow=None,delivery_repair=None,retry_events=_INHERIT_RETRY_EVENTS):
        self.config=config;self.catalog=catalog;self.store=store;self.external_maintenance=external_maintenance;self.external_workflow=external_workflow;self.delivery_repair=delivery_repair;self.retry_events=getattr(store,'retry_events',None) if retry_events is _INHERIT_RETRY_EVENTS else retry_events
    def create(self):
        return WorkflowCoordinator(WorkflowStoreFactory(self.config,self.catalog,self.store,retry_events=self.retry_events).create(),external_maintenance=self.external_maintenance,external_workflow=self.external_workflow,delivery_repair=self.delivery_repair)


class WorkflowStoreFactory:
    """Composition selects and injects the reviewer correction application port."""
    def __init__(self,config,catalog,store,*,reviewer_correction_factory=None,feedback_recovery_factory=None,peer_feedback_recovery_factory=None,superseding_review_factory=None,retry_events=_INHERIT_RETRY_EVENTS):
        self.config,self.catalog,self.store=config,catalog,store
        self.retry_events=getattr(store,'retry_events',None) if retry_events is _INHERIT_RETRY_EVENTS else retry_events
        self.reviewer_correction_factory=reviewer_correction_factory
        self.feedback_recovery_factory=feedback_recovery_factory
        self.peer_feedback_recovery_factory=peer_feedback_recovery_factory
        self.superseding_review_factory=superseding_review_factory
    def create(self):
        from work_reviewer_correction import ReviewerCorrectionFactory
        from work_context_recovery import FeedbackRecoveryFactory
        from work_peer_feedback_recovery import PeerFeedbackRecoveryFactory
        from work_superseding_review import SupersedingReviewFactory
        flow=WorkflowStore(self.config,self.catalog,self.store,retry_events=self.retry_events)
        factory=self.reviewer_correction_factory or ReviewerCorrectionFactory
        flow.reviewer_correction=factory(flow).create()
        if not all(callable(getattr(flow.reviewer_correction, name, None)) for name in ('correct','resume')):
            raise TypeError('Reviewer correction requires a ready correction/control port')
        flow.feedback_recovery=(self.feedback_recovery_factory or FeedbackRecoveryFactory)(flow).create()
        if flow.feedback_recovery is not None and not callable(getattr(flow.feedback_recovery,'run_once',None)):
            raise TypeError('Feedback recovery requires a ready recovery port')
        flow.peer_feedback_recovery=(self.peer_feedback_recovery_factory or PeerFeedbackRecoveryFactory)(flow).create()
        if flow.peer_feedback_recovery is not None and not callable(getattr(flow.peer_feedback_recovery,'run_once',None)):
            raise TypeError('Peer feedback recovery requires a ready recovery port')
        flow.superseding_review=(self.superseding_review_factory or SupersedingReviewFactory)(flow).create()
        if not all(callable(getattr(flow.superseding_review,name,None)) for name in ('recover','correct','root_review_id')):
            raise TypeError('Superseding review requires a ready source-bound recovery port')
        return flow
