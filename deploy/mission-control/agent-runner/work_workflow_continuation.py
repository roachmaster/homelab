"""Private, explicitly selected recovery of the default-role rollover incident.

No HTTP authority and no implementation approval. A locked source fingerprint
admits one new Plan in the original workflow, retaining all spent allowances.
"""
from dataclasses import dataclass
from uuid import UUID, uuid4
import re

from psycopg2.extras import Json
from queue_store import Rejected
from work_config import prepare_title, sha256_canonical
from work_events import insert_event, operator_actor, COORDINATOR_ACTOR

POLICY = 'forge-policy-incident-continuation-v1'
PREFIX = 'policy-continuation:'
INCIDENT = 'Workflow delegation or role grant is no longer current.'


def task_binding(task):
    return {key: str(task[key]) if task[key] is not None else None
            for key in ('id', 'revision', 'status', 'latest_attempt_id')}


def retained_sources(cursor, workflow_id):
    cursor.execute("""SELECT finish_receipt FROM forge_agent.coding_workflow_actions
      WHERE workflow_id=%s AND kind='decide' AND status='completed' AND source_key LIKE %s""",
      (workflow_id, PREFIX + '%'))
    return [binding for row in cursor.fetchall()
            for binding in row['finish_receipt']['receipt']['retained_sources']]


def is_retained_source(task, bindings):
    return task_binding(task) in bindings



@dataclass(frozen=True)
class PolicyIncidentContinuation:
    project_alias: str
    workflow_id: UUID
    source_sha256: str
    idempotency_key: UUID
    note: str

    def __post_init__(self):
        if (not isinstance(self.project_alias, str)
                or not re.fullmatch(r'[A-Za-z0-9_.-]{1,64}', self.project_alias)
                or not isinstance(self.workflow_id, UUID)
                or not isinstance(self.idempotency_key, UUID)
                or not isinstance(self.source_sha256, str)
                or not re.fullmatch(r'[0-9a-f]{64}', self.source_sha256)
                or not isinstance(self.note, str) or not self.note.strip()
                or len(self.note.encode()) > 1024
                or any(ord(c) < 32 for c in self.note)):
            raise ValueError('Invalid incident continuation')


@dataclass(frozen=True)
class UnstartedPlanResume:
    project_alias:str
    task_id:UUID
    attempt_id:UUID
    expected_revision:int
    reconciliation_receipt_id:UUID
    idempotency_key:UUID
    deployed_fix_revision:str

    def __post_init__(self):
        if (not isinstance(self.project_alias,str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,64}',self.project_alias)
                or any(not isinstance(v,UUID) for v in (self.task_id,self.attempt_id,self.reconciliation_receipt_id,self.idempotency_key))
                or type(self.expected_revision) is not int or not 1<=self.expected_revision<2**53
                or not isinstance(self.deployed_fix_revision,str) or not re.fullmatch('[0-9a-f]{40}',self.deployed_fix_revision)):
            raise ValueError('Invalid unstarted Plan resume')


@dataclass(frozen=True)
class ApprovedPlanDirectionResume(UnstartedPlanResume):
    """Exact stopped first implementation after a recovered and approved Plan."""


class WorkflowContinuation:
    """Operator-owned port; composition supplies the ready workflow store."""
    def __init__(self, flow, retry_evidence):
        if not callable(getattr(retry_evidence, 'read', None)):
            raise TypeError('Incident recovery requires a ready historical retry evidence port')
        self.flow, self.retry_evidence = flow, retry_evidence

    def _lock(self, cursor, project, workflow_id):
        flow = self.flow
        flow.store._require_project(project)
        flow._project_lock(cursor, project)
        cursor.execute('''SELECT root_task_id FROM forge_agent.coding_workflows
          WHERE workflow_id=%s AND owner_id=%s AND project_alias=%s''',
          (workflow_id, flow.store.owner, project))
        row = cursor.fetchone()
        if row is None:
            raise Rejected(404, 'not_found')
        # Match operator cancellation's root-before-workflow lock order.
        cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE id=%s FOR UPDATE',
                       (row['root_task_id'],))
        root = cursor.fetchone()
        cursor.execute('SELECT id FROM forge_agent.coding_tasks WHERE workflow_id=%s ORDER BY id FOR UPDATE',
                       (workflow_id,))
        cursor.fetchall()
        cursor.execute('SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s FOR UPDATE',
                       (workflow_id,))
        return root, cursor.fetchone()

    def _source(self, cursor, w):
        """Bind exact historical source, including cancellation and stop evidence."""
        result = {'workflow': {k: w[k] for k in ('snapshot', 'policy_digest', 'phase',
                    'enabled', 'escalation_reason', 'plan_revisions')},
                  'effective': self.flow._snapshot(w)}
        # SQL JSON supplies a stable representation of UUIDs and timestamps.
        for name, query in (
            ('tasks', 'SELECT t.* FROM forge_agent.coding_tasks t WHERE workflow_id=%s ORDER BY id'),
            ('attempts', '''SELECT a.* FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t
              ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id)
              WHERE t.workflow_id=%s ORDER BY a.attempt_id'''),
            ('stops', '''SELECT r.* FROM forge_agent.coding_reconciliation_receipts r
              JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(r.owner_id,r.project_alias,r.task_id)
              WHERE t.workflow_id=%s ORDER BY r.receipt_id'''),
            ('events', '''SELECT e.* FROM forge_agent.coding_events e JOIN forge_agent.coding_tasks t
              ON(t.owner_id,t.project_alias,t.id)=(e.owner_id,e.project_alias,e.task_id)
              WHERE t.workflow_id=%s ORDER BY e.task_id,e.sequence'''),
            ('controls', '''SELECT c.* FROM forge_agent.coding_control_receipts c JOIN forge_agent.coding_tasks t
              ON(t.owner_id,t.project_alias,t.id)=(c.owner_id,c.project_alias,c.task_id)
              WHERE t.workflow_id=%s ORDER BY c.task_id,c.idempotency_key'''),
        ):
            cursor.execute('SELECT coalesce(jsonb_agg(to_jsonb(s)),\'[]\'::jsonb) AS value FROM (' + query + ') s',
                           (w['workflow_id'],))
            result[name] = cursor.fetchone()['value']
        result['retry_events'] = self.retry_evidence.read(w['owner_id'], w['project_alias'], w['workflow_id'])
        cursor.execute('''SELECT to_jsonb(b) AS value FROM forge_agent.coding_review_budget b
          WHERE owner_id=%s AND project_alias=%s AND root_task_id=%s''',
          (w['owner_id'], w['project_alias'], w['root_task_id']))
        row = cursor.fetchone()
        result['repair_budget'] = row['value'] if row else None
        return result

    def inspect(self, project, workflow_id):
        with self.flow._transaction() as cursor:
            _root, w = self._lock(cursor, project, workflow_id)
            source = self._source(cursor, w)
            return {'policy': POLICY, 'workflow_id': str(workflow_id),
                    'source_sha256': sha256_canonical(source),
                    'retained_sources': [task_binding(t) for t in source['tasks']]}

    def resume_approved_plan_direction(self,command):
        """Retry only the first implementation misdirected by an old Plan note."""
        from work_workflow import BoundPlanDirectionAuthority,recipe_from_snapshot,_ready_for_recipe
        from work_policy import NATIVE_POLICIES
        from work_peer_review import parse_review_target,parse_review_verdict
        from work_agent_branch import OWNED_FEATURE_BRANCHES
        if type(command) is not ApprovedPlanDirectionResume:raise Rejected(400,'invalid_request')
        flow,store=self.flow,self.flow.store
        intent={'policy':'forge-approved-plan-direction-v1',**{k:str(v) for k,v in vars(command).items()}}
        key='approved-plan-direction:'+str(command.attempt_id)
        with flow._transaction() as cursor:
            store._require_project(command.project_alias);flow._project_lock(cursor,command.project_alias)
            cursor.execute('SELECT workflow_id FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s',
                (store.owner,command.project_alias,command.task_id));found=cursor.fetchone()
            if not found or found['workflow_id'] is None:raise Rejected(404,'not_found')
            root,w=self._lock(cursor,command.project_alias,found['workflow_id'])
            cursor.execute("SELECT intent,finish_receipt FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key=%s",(w['workflow_id'],key));prior=cursor.fetchone()
            if prior:
                if prior['intent']!=intent:raise Rejected(409,'idempotency_conflict')
                return prior['finish_receipt']['receipt']
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE id=%s FOR UPDATE',(command.task_id,));task=cursor.fetchone()
            old_note='The prior Plan claim was rejected before native launch. The pristine pinned workspace was materialized and reconciled after that failure. Produce the fresh Plan; independent review remains required.'
            if (w['enabled'] or w['phase']!='escalated' or task['mode']!='implement'
                    or task['status'] not in {'awaiting_review','needs_reconcile'} or task['cancelled_at'] is not None
                    or task['revision']!=command.expected_revision or task['latest_attempt_id']!=command.attempt_id
                    or task['parent_task_id']!=root['id'] or task['repair_root_task_id']!=root['id']
                    or task['legacy_resume_note']!=old_note or task['plan_decision_id'] is None
                    or any(task[k] is not None for k in ('scope_plan_decision_id','workflow_review_id','feedback_id','repair_source_review_id'))):
                raise Rejected(409,'not_plan_direction_defect')
            cursor.execute("SELECT action_id FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND status='completed' AND source_key LIKE %s AND finish_receipt->'receipt'->>'task_id'=%s",(w['workflow_id'],PREFIX+'%',str(task['id'])))
            if cursor.fetchone() is None:raise Rejected(409,'source_changed')
            cursor.execute("SELECT intent FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND status='completed' AND source_key LIKE 'unstarted-plan-resume:%%' AND finish_receipt->'receipt'->>'task_id'=%s",(w['workflow_id'],str(task['id'])));startup=cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_attempts WHERE task_id=%s ORDER BY attempt_no',(task['id'],));attempts=cursor.fetchall()
            if (not startup or len(attempts)!=3 or str(attempts[0]['attempt_id'])!=startup['intent']['attempt_id']
                    or attempts[-1]['attempt_id']!=command.attempt_id or any(a['phase']!='stopped' or a['reservation_active'] or a['lease_expires_at'] is not None for a in attempts)):
                raise Rejected(409,'reconcile_required')
            attempt=attempts[-1]
            if attempt['legacy_resume_note']!=old_note:raise Rejected(409,'source_changed')
            cursor.execute('SELECT * FROM forge_agent.coding_plan_decisions WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND decision_id=%s',(store.owner,command.project_alias,task['id'],task['plan_decision_id']));plan=cursor.fetchone()
            if (not plan or plan['decision']!='resume' or plan['plan_attempt_id']!=attempts[1]['attempt_id']
                    or plan['base_revision']!=task['base_revision'] or plan['agent_profile']!=task['agent_profile']
                    or plan['plan_sha256']!=flow._artifact(cursor,task['id'],plan['plan_attempt_id'])['result_sha256']):
                raise Rejected(409,'plan_changed')
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_reviews WHERE workflow_id=%s AND target_kind='plan' AND source_task_id=%s AND source_attempt_id=%s AND status='decided'",(w['workflow_id'],task['id'],plan['plan_attempt_id']));peers=cursor.fetchall()
            if len(peers)!=1:raise Rejected(409,'plan_changed')
            peer=peers[0]
            try:
                target=parse_review_target(peer['target_json'],peer['target_sha256'])
                verdict=parse_review_verdict(peer['verdict_json'],target)
                if verdict['decision']!='approve' or target['source_result_sha256']!=plan['plan_sha256']:raise ValueError()
            except (ValueError,KeyError,TypeError):raise Rejected(409,'plan_changed') from None
            cursor.execute('SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE task_id=%s AND receipt_id=%s',(task['id'],command.reconciliation_receipt_id));rec=cursor.fetchone()
            cursor.execute('SELECT workspace_digest FROM forge_agent.coding_reconciliation_receipts WHERE task_id=%s AND attempt_id=%s AND task_revision=%s',(task['id'],plan['plan_attempt_id'],plan['source_revision']));plan_rec=cursor.fetchone()
            if (not rec or not plan_rec or rec['attempt_id']!=command.attempt_id or rec['task_revision']!=task['revision']
                    or rec['stop_proof'].get('scope_stopped') is not True
                    or rec['stop_proof'].get('source')!='systemd' or rec['stop_proof'].get('unit_name')!=attempt['unit_name']):raise Rejected(409,'reconcile_required')
            cursor.execute('SELECT * FROM forge_agent.coding_reviews WHERE task_id=%s AND attempt_id=%s FOR UPDATE',(task['id'],command.attempt_id));reviews=cursor.fetchall();review=None
            if task['status']=='awaiting_review':
                if len(reviews)!=1:raise Rejected(409,'review_changed')
                review=reviews[0];outcome=(review['finish_receipt'] or {}).get('outcome',{})
                if (rec['workspace_digest']!=plan_rec['workspace_digest']
                        or w['escalation_reason']!='no_source_changes' or review['status']!='handoff' or review['reason']!='no_source_changes'
                        or review['candidate_commit'] is not None or review['phase']!='local' or review['feedback_id'] is not None
                        or review['attempt_id']!=attempt['attempt_id'] or review['source_revision']!=task['revision']
                        or review['plan_decision_id']!=plan['decision_id'] or review['reconciliation_receipt_id']!=rec['receipt_id']
                        or review['workspace_digest']!=rec['workspace_digest'] or review['finish_token'] is None
                        or review['finish_receipt'].get('review_id')!=str(review['review_id'])
                        or review['finish_receipt'].get('status')!='handoff'
                        or outcome.get('reason')!='no_source_changes' or outcome.get('candidate_commit') is not None
                        or outcome.get('verification',{}).get('cleanup_confirmed') is not True
                        or outcome.get('verification',{}).get('checks_passed') is not False
                        or outcome.get('verification',{}).get('checks')!=[]
                        or review['verification'].get('cleanup_confirmed') is not True
                        or review['verification'].get('checks_passed') is not False or review['verification'].get('checks')!=[]):raise Rejected(409,'review_changed')
                if flow._artifact(cursor,task['id'],attempt['attempt_id'])['result_sha256']!=review['parent_result_sha256']:raise Rejected(409,'result_mismatch')
            elif (reviews or w['escalation_reason']!='Agent stopped: needs_reconcile'
                    or attempt['result_sha256'] is not None or attempt['published_at'] is not None
                    or attempt['summary']!='Worker shutdown requires review before any replacement attempt.'):
                raise Rejected(409,'not_plan_direction_defect')
            cursor.execute("""SELECT EXISTS(SELECT 1 FROM forge_agent.coding_tasks WHERE workflow_id=%s AND status IN('queued','running','cancel_requested'))
              OR EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND status IN('pending','leased'))
              OR EXISTS(SELECT 1 FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t USING(owner_id,project_alias) WHERE t.id=p.task_id AND t.workflow_id=%s)
              OR EXISTS(SELECT 1 FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t ON t.id=q.task_id WHERE t.workflow_id=%s AND q.status IN('pending','checking'))
              OR EXISTS(SELECT 1 FROM forge_agent.coding_control_receipts c JOIN forge_agent.coding_tasks t ON t.id=c.task_id WHERE t.workflow_id=%s AND c.action='cancel')
              OR EXISTS(SELECT 1 FROM forge_agent.coding_events e JOIN forge_agent.coding_tasks t ON t.id=e.task_id WHERE t.workflow_id=%s AND e.event_kind IN('cancelled','cancel_requested') AND(e.payload->>'cause'='operator_request' OR e.payload->'actor'->>'kind'='operator')) AS busy""",(w['workflow_id'],)*6)
            if cursor.fetchone()['busy']:raise Rejected(409,'reconcile_required')
            flow._current_policy(w);p=flow._snapshot(w);recipe=recipe_from_snapshot(p,'author_implement')
            if (p['author_profile'] not in OWNED_FEATURE_BRANCHES or flow.config.profiles[p['author_profile']].branch!=OWNED_FEATURE_BRANCHES[p['author_profile']]
                    or task['agent_profile']!=p['author_profile'] or task['pinned_recipe']!=recipe.to_dict() or task['recipe_digest']!=recipe.digest
                    or attempt['pinned_recipe']!=recipe.to_dict() or attempt['recipe_digest']!=recipe.digest):raise Rejected(409,'source_changed')
            if flow.retry_events is None:raise Rejected(503,'unavailable')
            if any(e['status'] in ('open','exhausted') or e['used']>=e['attempt_limit'] for e in self.retry_evidence.read(store.owner,w['project_alias'],w['workflow_id'])):raise Rejected(409,'retry_exhausted')
            source=sha256_canonical({'plan':str(plan['decision_id']),'attempt':str(attempt['attempt_id']),'result':attempt['result_sha256'],'review':str(review['review_id']) if review else None,'reconciliation':str(rec['receipt_id']),'fix':command.deployed_fix_revision})
            episode=flow.retry_events.observe_failure(cursor,flow._retry_scope(w),'feedback_delivery',str(attempt['attempt_id']),source,phase_id='approved-plan-direction')
            charged=flow.retry_events.reserve(cursor,episode['event_id'],key)
            if charged['status']!='open' or charged['used']!=1:raise Rejected(409,'retry_exhausted')
            cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase='implement',escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=%s",(w['workflow_id'],))
            for profile,mode in ((p['author_profile'],'implement'),(p['reviewer_profile'],'review')):flow._grant(cursor,w,profile,mode)
            cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND policy_digest=ANY(%s) AND observed_at>=clock_timestamp()-interval '90 seconds'",(store.owner,[v[0] for v in NATIVE_POLICIES.values()]));ready=cursor.fetchall()
            for profile,mode,entry in ((p['author_profile'],'implement','author_implement'),(p['reviewer_profile'],'review','reviewer_review')):
                if not _ready_for_recipe(ready,w['project_alias'],profile,mode,recipe_from_snapshot(p,entry)):raise Rejected(503,'unavailable')
            authority=BoundPlanDirectionAuthority(store,cursor,w['workflow_id'],task['id'],attempt['attempt_id'],task['revision'],review['review_id'] if review else None)
            receipt=store.resume(project_alias=w['project_alias'],task_id=task['id'],expected_revision=task['revision'],idempotency_key='plan-direction-'+str(command.idempotency_key),agent_profile=p['author_profile'],reconciliation_receipt_id=rec['receipt_id'],note='Implement the saved independently approved Plan. The earlier startup recovery note applied only before Plan approval and is historical now. Preserve scope and report actual changes; normal independent checks and review remain required.',allowed_modes={'implement'},resolve_recipe=lambda _:recipe,_authority=authority,actor=operator_actor(store.owner))
            audit={**receipt,'plan_decision_id':str(plan['decision_id']),'source_attempt_id':str(attempt['attempt_id']),'review_id':str(review['review_id']) if review else None,'reconciliation_receipt_id':str(rec['receipt_id']),'retry_event_id':str(episode['event_id']),'retry_charge':charged,'retained_plan_revisions':w['plan_revisions']}
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt) VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",(w['workflow_id'],store.owner,w['project_alias'],key,Json(intent),Json({'receipt':audit})))
            insert_event(cursor,store.owner,w['project_alias'],root['id'],'checkpoint',{'workflow_id':str(w['workflow_id']),'phase':'implement','related_task_id':str(task['id']),'summary':'The approved implementation is queued after correcting stale Plan-only retry direction. The same Plan, task, historical results and budgets are retained; independent code review is still required.'},actor=COORDINATOR_ACTOR)
            return audit

    def resume_unstarted_plan(self,command):
        """Resume the same initial Plan after audited post-failure materialization.

        The operator supplies genuine systemd stop and actual Git workspace
        evidence through the ordinary reconciler first. This port cannot invent
        workspace evidence or resume an attempt that produced output.
        """
        from work_workflow import BoundUnstartedPlanAuthority,recipe_from_snapshot,_ready_for_recipe
        from work_policy import NATIVE_POLICIES
        if not isinstance(command,UnstartedPlanResume):raise Rejected(400,'invalid_request')
        flow,store=self.flow,self.flow.store
        intent={'policy':'forge-unstarted-plan-resume-v1',**{k:str(v) for k,v in vars(command).items()}}
        key='unstarted-plan-resume:'+str(command.attempt_id)
        with flow._transaction() as cursor:
            store._require_project(command.project_alias);flow._project_lock(cursor,command.project_alias)
            cursor.execute('SELECT workflow_id FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s',
                (store.owner,command.project_alias,command.task_id));found=cursor.fetchone()
            if not found or found['workflow_id'] is None:raise Rejected(404,'not_found')
            root,w=self._lock(cursor,command.project_alias,found['workflow_id'])
            cursor.execute("SELECT intent,finish_receipt FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND source_key=%s AND kind='decide'",
                (w['workflow_id'],key));previous=cursor.fetchone()
            if previous:
                if previous['intent']!=intent:raise Rejected(409,'idempotency_conflict')
                return previous['finish_receipt']['receipt']
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE id=%s FOR UPDATE',(command.task_id,));task=cursor.fetchone()
            if (w['enabled'] or w['phase']!='escalated' or w['escalation_reason']!='Agent stopped: needs_reconcile'
                    or task['mode']!='plan' or task['status']!='needs_reconcile' or task['cancelled_at'] is not None
                    or task['revision']!=command.expected_revision or task['latest_attempt_id']!=command.attempt_id
                    or task['parent_task_id']!=root['id'] or task['repair_root_task_id']!=root['id']
                    or any(task[k] is not None for k in ('plan_decision_id','scope_plan_decision_id','workflow_review_id','feedback_id'))):
                raise Rejected(409,'not_unstarted_plan')
            cursor.execute("SELECT action_id,finish_receipt FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND status='completed' AND source_key LIKE %s AND finish_receipt->'receipt'->>'task_id'=%s",
                (w['workflow_id'],PREFIX+'%',str(task['id'])));source=cursor.fetchone()
            if not source:raise Rejected(409,'source_changed')
            cursor.execute('SELECT * FROM forge_agent.coding_attempts WHERE task_id=%s ORDER BY attempt_no',(task['id'],));attempts=cursor.fetchall()
            if (len(attempts)!=1 or attempts[0]['attempt_id']!=command.attempt_id or attempts[0]['phase']!='stopped'
                    or attempts[0]['reservation_active'] or attempts[0]['lease_expires_at'] is not None
                    or attempts[0]['result_sha256'] is not None or attempts[0]['published_at'] is not None):
                raise Rejected(409,'reconcile_required')
            cursor.execute('SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE task_id=%s AND receipt_id=%s',
                (task['id'],command.reconciliation_receipt_id));rec=cursor.fetchone()
            if (not rec or rec['attempt_id']!=command.attempt_id or rec['task_revision']!=task['revision']
                    or rec['stop_proof'].get('scope_stopped') is not True
                    or rec['stop_proof'].get('source')!='systemd' or rec['stop_proof'].get('unit_name')!=attempts[0]['unit_name']):
                raise Rejected(409,'reconcile_required')
            cursor.execute("SELECT payload FROM forge_agent.coding_events WHERE task_id=%s AND event_kind='checkpoint' AND payload->>'stage'='unstarted_plan_workspace_materialized' AND payload->>'attempt_id'=%s",
                (task['id'],str(command.attempt_id)));markers=cursor.fetchall()
            if len(markers)!=1:raise Rejected(409,'materialization_not_attested')
            marker=markers[0]['payload']
            if (marker.get('materialized_after_failure') is not True or marker.get('workspace_digest')!=rec['workspace_digest']
                    or marker.get('reconciliation_receipt_id')!=str(rec['receipt_id'])
                    or marker.get('source_action_id')!=str(source['action_id'])
                    or marker.get('actor')!=operator_actor(store.owner)
                    or marker.get('recipe_digest')!=task['recipe_digest'] or marker.get('base_revision')!=task['base_revision']):
                raise Rejected(409,'materialization_not_attested')
            cursor.execute("""SELECT EXISTS(SELECT 1 FROM forge_agent.coding_result_chunks WHERE attempt_id=%s)
              OR EXISTS(SELECT 1 FROM forge_agent.coding_events WHERE task_id=%s AND event_kind='checkpoint'
                AND payload->>'stage' IS DISTINCT FROM 'unstarted_plan_workspace_materialized')
              OR EXISTS(SELECT 1 FROM forge_agent.coding_tasks WHERE workflow_id=%s AND status IN('queued','running','cancel_requested'))
              OR EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND status IN('pending','leased'))
              OR EXISTS(SELECT 1 FROM forge_agent.coding_control_receipts c JOIN forge_agent.coding_tasks t
                ON(t.owner_id,t.project_alias,t.id)=(c.owner_id,c.project_alias,c.task_id) WHERE t.workflow_id=%s AND c.action='cancel')
              OR EXISTS(SELECT 1 FROM forge_agent.coding_events e JOIN forge_agent.coding_tasks t
                ON(t.owner_id,t.project_alias,t.id)=(e.owner_id,e.project_alias,e.task_id) WHERE t.workflow_id=%s
                AND e.event_kind IN('cancelled','cancel_requested') AND(e.payload->>'cause'='operator_request' OR e.payload->'actor'->>'kind'='operator')) AS busy""",
                (command.attempt_id,task['id'],w['workflow_id'],w['workflow_id'],w['workflow_id'],w['workflow_id']))
            if cursor.fetchone()['busy']:raise Rejected(409,'reconcile_required')
            p=flow._snapshot(w);recipe=recipe_from_snapshot(p,'author_plan');flow._current_policy(w)
            from work_agent_branch import OWNED_FEATURE_BRANCHES
            if (p['author_profile'] not in OWNED_FEATURE_BRANCHES
                    or flow.config.profiles[p['author_profile']].branch!=OWNED_FEATURE_BRANCHES[p['author_profile']]):
                raise Rejected(409,'branch_not_qualified')
            if task['agent_profile']!=p['author_profile'] or task['pinned_recipe']!=recipe.to_dict() or task['recipe_digest']!=recipe.digest:
                raise Rejected(409,'source_changed')
            if any(e['status'] in ('open','exhausted') or e['used']>=e['attempt_limit']
                   for e in self.retry_evidence.read(store.owner,w['project_alias'],w['workflow_id'])):
                raise Rejected(409,'retry_exhausted')
            cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase='plan',escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=%s",(w['workflow_id'],))
            for profile,mode in ((p['author_profile'],'plan'),(p['reviewer_profile'],'review')):flow._grant(cursor,w,profile,mode)
            cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND policy_digest=ANY(%s) AND observed_at>=clock_timestamp()-interval '90 seconds'",(store.owner,[v[0] for v in NATIVE_POLICIES.values()]))
            ready=cursor.fetchall()
            for profile,mode,entry in ((p['author_profile'],'plan','author_plan'),(p['reviewer_profile'],'review','reviewer_review')):
                if not _ready_for_recipe(ready,w['project_alias'],profile,mode,recipe_from_snapshot(p,entry)):raise Rejected(503,'unavailable')
            authority=BoundUnstartedPlanAuthority(store,cursor,w['workflow_id'],task['id'],command.attempt_id,task['revision'])
            receipt=store.resume(project_alias=w['project_alias'],task_id=task['id'],expected_revision=task['revision'],
                idempotency_key='unstarted-plan-'+str(command.idempotency_key),agent_profile=p['author_profile'],
                reconciliation_receipt_id=rec['receipt_id'],note='The prior Plan claim was rejected before native launch. The pristine pinned workspace was materialized and reconciled after that failure. Produce the fresh Plan; independent review remains required.',
                allowed_modes={'plan'},resolve_recipe=lambda _:recipe,_authority=authority,actor=operator_actor(store.owner))
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt) VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",
                (w['workflow_id'],store.owner,w['project_alias'],key,Json(intent),Json({'receipt':receipt})))
            insert_event(cursor,store.owner,w['project_alias'],root['id'],'checkpoint',
                {'workflow_id':str(w['workflow_id']),'phase':'plan','related_task_id':str(task['id']),
                 'summary':'Native Plan startup was reconciled after a lease-contract failure before launch. The same fresh Plan is queued with its verified pristine workspace.'},actor=COORDINATOR_ACTOR)
            return receipt

    def execute(self, command):
        if not isinstance(command, PolicyIncidentContinuation):
            raise Rejected(400, 'invalid_request')
        from work_workflow import recipe_from_snapshot, _ready_for_recipe
        from work_policy import NATIVE_POLICIES
        flow, store = self.flow, self.flow.store
        intent = {'policy': POLICY, **{k: str(v) for k, v in vars(command).items()}}
        key = PREFIX + str(command.idempotency_key)
        with flow._transaction() as cursor:
            root, w = self._lock(cursor, command.project_alias, command.workflow_id)
            cursor.execute('''SELECT intent,finish_receipt FROM forge_agent.coding_workflow_actions
              WHERE workflow_id=%s AND kind='decide' AND source_key=%s''', (w['workflow_id'], key))
            previous = cursor.fetchone()
            if previous:
                if previous['intent'] != intent:
                    raise Rejected(409, 'idempotency_conflict')
                return previous['finish_receipt']['receipt']
            if w['enabled'] or w['phase'] != 'escalated' or w['escalation_reason'] != INCIDENT:
                raise Rejected(409, 'not_policy_incident')
            cursor.execute('''SELECT 1 FROM forge_agent.coding_workflow_actions
              WHERE workflow_id=%s AND source_key LIKE %s LIMIT 1''', (w['workflow_id'], PREFIX + '%'))
            if cursor.fetchone():
                raise Rejected(409, 'continuation_limit')
            source = self._source(cursor, w)
            if sha256_canonical(source) != command.source_sha256:
                raise Rejected(409, 'source_changed')
            if any(c['action'] == 'cancel' for c in source['controls']) or any(
                    e['event_kind'] in ('cancelled', 'cancel_requested') and
                    (e['payload'].get('cause') == 'operator_request'
                     or e['payload'].get('actor', {}).get('kind') == 'operator')
                    for e in source['events']):
                raise Rejected(409, 'operator_cancelled')
            if any(t['status'] in ('queued', 'running', 'cancel_requested', 'needs_reconcile')
                   for t in source['tasks']):
                raise Rejected(409, 'reconcile_required')
            for attempt in source['attempts']:
                if (attempt['phase'] != 'stopped' or attempt['reservation_active']
                        or attempt['lease_expires_at'] is not None
                        or not any(r['attempt_id'] == attempt['attempt_id'] for r in source['stops'])):
                    raise Rejected(409, 'reconcile_required')
            cursor.execute('''SELECT EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions
                WHERE workflow_id=%s AND status IN('pending','leased'))
              OR EXISTS(SELECT 1 FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t
                ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id)
                WHERE t.workflow_id=%s AND q.status IN('pending','checking','retry_infrastructure'))
              OR EXISTS(SELECT 1 FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t
                ON(t.owner_id,t.project_alias,t.id)=(p.owner_id,p.project_alias,p.task_id)
                WHERE t.workflow_id=%s) AS busy''', (w['workflow_id'],) * 3)
            if cursor.fetchone()['busy']:
                raise Rejected(409, 'reconcile_required')
            # Existing publications need their own remote/rollback reconciliation.
            # Recovery does not reset, resolve or manufacture retry observations.
            if any(e['status'] in ('open','exhausted') or e['used'] >= e['attempt_limit']
                   for e in source['retry_events']):
                raise Rejected(409, 'retry_exhausted')
            if w['plan_revisions'] >= flow._retry_limit(w, 'max_plan_revisions'):
                raise Rejected(409, 'retry_exhausted')
            flow._current_policy(w)
            p = flow._snapshot(w)
            from work_agent_branch import OWNED_FEATURE_BRANCHES
            # Review runs use pinned detached read-only snapshots. Only the
            # author can develop/publish, and must have the owned feature ref.
            for alias in (p['author_profile'],):
                if (alias not in OWNED_FEATURE_BRANCHES
                        or flow.config.profiles[alias].branch != OWNED_FEATURE_BRANCHES[alias]):
                    raise Rejected(409, 'branch_not_qualified')
            cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase='plan',escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=%s", (w['workflow_id'],))
            for profile, mode in ((p['author_profile'], 'plan'), (p['author_profile'], 'implement'),
                                  (p['reviewer_profile'], 'review')):
                flow._grant(cursor, w, profile, mode)
            cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND policy_digest=ANY(%s) AND observed_at>=clock_timestamp()-interval '90 seconds'", (store.owner, [v[0] for v in NATIVE_POLICIES.values()]))
            ready = cursor.fetchall()
            for role, mode, entry in (('author_profile', 'plan', 'author_plan'),
                    ('author_profile', 'implement', 'author_implement'),
                    ('reviewer_profile', 'review', 'reviewer_review')):
                if not _ready_for_recipe(ready, w['project_alias'], p[role], mode, recipe_from_snapshot(p, entry)):
                    raise Rejected(503, 'unavailable')
            cursor.execute("SELECT count(*) AS n FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND status IN('queued','running','cancel_requested')", (store.owner, w['project_alias']))
            if cursor.fetchone()['n'] >= store.max_pending:
                raise Rejected(503, 'queue_full')
            task_id = uuid4()
            recipe = recipe_from_snapshot(p, 'author_plan')
            base = store._delivery_head(cursor, w['project_alias']) or root['base_revision']
            prompt = (root['prompt'] + '\n\nCoordinator incident continuation: inspect current main and retained evidence. '
                      'Write a fresh Plan for the remaining acceptance criteria. Earlier approvals do not approve this new Plan. '
                      'Develop only on your assigned feature branch. Preserve previous workspaces and receipts. '
                      'Operator context: ' + command.note)
            cursor.execute('''INSERT INTO forge_agent.coding_tasks(owner_id,project_alias,id,idempotency_key,intent_digest,
              base_revision,mode,agent_profile,title,prompt,acceptance_criteria,pinned_recipe,recipe_digest,status,revision,
              auto_repair,review_required,workflow_required,workflow_id,parent_task_id,repair_root_task_id)
              VALUES(%s,%s,%s,%s,%s,%s,'plan',%s,%s,%s,%s,%s,%s,'queued',1,%s,%s,true,%s,%s,%s)''',
              (store.owner,w['project_alias'],task_id,'continuation-'+str(task_id),sha256_canonical(intent),base,
               p['author_profile'],prepare_title('Continue '+root['title']),prompt[:65536],root['acceptance_criteria'],
               Json(recipe.to_dict()),recipe.digest,root['auto_repair'],root['review_required'],w['workflow_id'],root['id'],root['id']))
            receipt = {'status':'plan_queued','policy':POLICY,'workflow_id':str(w['workflow_id']),
                       'task_id':str(task_id),'base_revision':base,'source_sha256':command.source_sha256,
                       'prior_state':source['workflow'],
                       'preserved_retry_events':source['retry_events'],
                       'repair_budget_sha256':sha256_canonical(source['repair_budget']),
                       'retained_sources':[task_binding(t) for t in source['tasks']]}
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt) VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",
              (w['workflow_id'],store.owner,w['project_alias'],key,Json(intent),Json({'receipt':receipt})))
            actor = operator_actor(store.owner)
            insert_event(cursor,store.owner,w['project_alias'],root['id'],'checkpoint',
                         {'stage':'policy_incident_continued',**receipt},actor=actor)
            insert_event(cursor,store.owner,w['project_alias'],root['id'],'checkpoint',
                {'workflow_id':str(w['workflow_id']),'phase':'plan','related_task_id':str(task_id),
                 'summary':'Policy incident recovered. A fresh Plan against the recorded base is queued; independent review is required.'},actor=COORDINATOR_ACTOR)
            insert_event(cursor,store.owner,w['project_alias'],task_id,'queued',
                         {'stage':'fresh_plan_required','revision':1,'status':'queued','project_alias':w['project_alias'],
                          'task_id':str(task_id),'recipe_digest':recipe.digest,'source_workflow_id':str(w['workflow_id'])},actor=actor,first=True)
            cursor.execute("SELECT pg_notify('forge_coding_jobs','')")
            return receipt
