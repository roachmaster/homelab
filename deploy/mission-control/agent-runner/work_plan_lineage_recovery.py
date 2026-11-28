"""Private, source-bound recovery of the M51 continuation Plan-read defect.

No public route or automatic polling. The operator injects qualified deployment,
real stopped-scope and unchanged workspace evidence before normal Resume.
"""
from dataclasses import dataclass
from uuid import UUID
import re
from psycopg2.extras import Json
from queue_store import Rejected
from work_events import insert_event, operator_actor

SUMMARY = "The saved plan could not be verified; review is required before execution."
POLICY = 'forge-plan-lineage-fix-v1'
PREFIX = 'plan-lineage-fix:'
M52_SOURCE_SHA256 = '5a6de019b50d66dfd9aab6b282eed47a6e5f7de94e3ae857ac3c3617b54a9236'


@dataclass(frozen=True)
class BoundPlanLineageAuthority:
    store: object
    cursor: object
    workflow_id: object
    task_id: object
    attempt_id: object
    revision: int

    def validate(self,cursor,task,kind):
        if (cursor is not self.cursor or kind!='plan_lineage_fix' or task['id']!=self.task_id
                or task.get('workflow_id')!=self.workflow_id or task['latest_attempt_id']!=self.attempt_id
                or task['revision']!=self.revision or task['mode']!='implement' or task['status']!='needs_reconcile'):
            raise Rejected(403,'denied')


class PlanLineageRecovery:
    def __init__(self,flow,evidence):
        self.flow=flow;self.store=flow.store;self.evidence=evidence

    def resume(self,*,project_alias,task_id,attempt_id,expected_revision,reconciliation_receipt_id,idempotency_key):
        from work_workflow import recipe_from_snapshot,_ready_for_recipe,NATIVE_POLICIES
        from work_agent_branch import OWNED_FEATURE_BRANCHES
        if (any(not isinstance(v,UUID) for v in (task_id,attempt_id,reconciliation_receipt_id,idempotency_key))
                or type(expected_revision) is not int or expected_revision<1):raise Rejected(400,'invalid_request')
        self.store._require_project(project_alias)
        request=dict(project_alias=project_alias,task_id=str(task_id),attempt_id=str(attempt_id),
            expected_revision=expected_revision,reconciliation_receipt_id=str(reconciliation_receipt_id),idempotency_key=str(idempotency_key))
        key=PREFIX+str(attempt_id)
        with self.flow._transaction() as cursor:
            self.flow._project_lock(cursor,project_alias)
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE',(self.store.owner,project_alias,task_id));task=cursor.fetchone()
            if not task:raise Rejected(404,'not_found')
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key=%s",(task['workflow_id'],key));prior=cursor.fetchone()
            if prior:
                if prior['intent'].get('request')!=request:raise Rejected(409,'idempotency_conflict')
                if prior['status']!='completed' or prior['intent'].get('policy')!=POLICY:raise Rejected(409,'reconcile_required')
                return dict(prior['finish_receipt']['receipt'])
            cursor.execute('SELECT * FROM forge_agent.coding_workflows WHERE owner_id=%s AND project_alias=%s AND workflow_id=%s FOR UPDATE',(self.store.owner,project_alias,task['workflow_id']));w=cursor.fetchone()
            if (not w or w['enabled'] or w['phase']!='escalated' or w['escalation_reason']!='Agent stopped: needs_reconcile'
                    or task['revision']!=expected_revision or task['status']!='needs_reconcile' or task['mode']!='implement'
                    or not task['workflow_required'] or task['workflow_review_id'] is not None or task['cancelled_at'] is not None
                    or task['latest_attempt_id']!=attempt_id or not task['repair_source_review_id']
                    or not task['scope_plan_decision_id'] or task['plan_decision_id'] is not None):raise Rejected(409,'not_reviewable')
            self.flow._current_policy(w);p=self.flow._snapshot(w);recipe=recipe_from_snapshot(p,'author_implement')
            if task['agent_profile']!=p['author_profile'] or task['pinned_recipe']!=recipe.to_dict() or task['recipe_digest']!=recipe.digest:raise Rejected(409,'result_mismatch')
            if OWNED_FEATURE_BRANCHES.get(task['agent_profile'])!=self.flow.config.profiles[task['agent_profile']].branch:raise Rejected(409,'not_reviewable')
            cursor.execute("SELECT encode(sha256(convert_to(prosrc,'UTF8')),'hex')=%s AS present FROM pg_proc WHERE pronamespace=(SELECT oid FROM pg_namespace WHERE nspname='forge_coding_worker_api') AND proname='read_plan' AND proargtypes='2950 2950'::oidvector",(M52_SOURCE_SHA256,))
            if not (cursor.fetchone() or {}).get('present'):raise Rejected(409,'reconcile_required')
            cursor.execute('SELECT * FROM forge_agent.coding_attempts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s',(self.store.owner,project_alias,task_id,attempt_id));a=cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND receipt_id=%s',(self.store.owner,project_alias,task_id,reconciliation_receipt_id));rec=cursor.fetchone()
            if (not a or not rec or rec['attempt_id']!=attempt_id or rec['task_revision']!=expected_revision
                    or a['phase']!='stopped' or a['reservation_active'] or a['lease_expires_at'] is not None
                    or a['summary']!=SUMMARY or a['outcome']!='needs_reconcile' or not a['published_at']
                    or any(a[k] is not None for k in ('result_sha256','result_byte_count','result_chunk_count'))
                    or any(a[k]!=task[k] for k in ('pinned_recipe','recipe_digest','plan_decision_id','scope_plan_decision_id','feedback_id','feedback_sha256','repair_source_review_id','repair_candidate_commit','workflow_feedback_review_id','workflow_feedback_sha256'))
):raise Rejected(409,'reconcile_required')
            if a['publish_receipt']!=dict(result=None,status='needs_reconcile',outcome='needs_reconcile',task_id=str(task_id),revision=expected_revision,attempt_id=str(attempt_id),project_alias=project_alias,outcome_digest=a['outcome_digest']):raise Rejected(409,'result_mismatch')
            proof=rec['stop_proof']
            if (proof.get('source')!='systemd' or proof.get('scope_stopped') is not True or proof.get('unit_name')!=a['unit_name']
                    or rec['created_at']<a['finished_at']):raise Rejected(409,'reconcile_required')
            cursor.execute('SELECT * FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND review_id=%s FOR UPDATE',(self.store.owner,project_alias,task['repair_source_review_id']));q=cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s',(self.store.owner,project_alias,task['parent_task_id']));parent=cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_plan_decisions WHERE owner_id=%s AND project_alias=%s AND decision_id=%s',(self.store.owner,project_alias,task['scope_plan_decision_id']));plan=cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_feedback WHERE owner_id=%s AND project_alias=%s AND feedback_id=%s',(self.store.owner,project_alias,task['feedback_id']));feedback=cursor.fetchone()
            if (not q or not parent or not plan or not feedback or plan['decision']!='resume' or plan['task_id']!=parent['id']
                    or parent['plan_decision_id']!=plan['decision_id'] or parent['workflow_id']!=w['workflow_id']
                    or q['task_id']!=parent['id'] or q['repair_task_id']!=task_id or q['status']!='repair'
                    or q['candidate_commit']!=task['base_revision'] or q['candidate_commit']!=task['repair_candidate_commit']
                    or (q['plan_decision_id'] or q['scope_plan_decision_id'])!=plan['decision_id']
                    or q['verification'].get('checks_passed') is not True or q['verification'].get('cleanup_confirmed') is not True
                    or feedback['task_id']!=task_id or feedback['review_id']!=q['review_id']
                    or feedback['parent_attempt_id']!=q['attempt_id'] or feedback['feedback_sha256']!=task['feedback_sha256']):raise Rejected(409,'result_mismatch')
            artifact=self.flow._artifact(cursor,plan['task_id'],plan['plan_attempt_id'])
            if artifact['result_sha256']!=plan['plan_sha256']:raise Rejected(409,'result_mismatch')
            cursor.execute("SELECT EXISTS(SELECT FROM forge_agent.coding_result_chunks WHERE attempt_id=%s) OR EXISTS(SELECT FROM forge_agent.coding_reviews WHERE task_id=%s AND attempt_id=%s) OR EXISTS(SELECT FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND status IN('pending','leased')) OR EXISTS(SELECT FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON t.id=a.task_id WHERE t.workflow_id=%s AND(a.reservation_active OR a.lease_expires_at IS NOT NULL)) OR EXISTS(SELECT FROM forge_agent.coding_tasks WHERE workflow_id=%s AND status IN('queued','running','cancel_requested')) OR EXISTS(SELECT FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t ON t.id=p.task_id WHERE t.workflow_id=%s AND p.status IN('pending','checking','publishing')) OR EXISTS(SELECT FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND source_key LIKE %s) AS busy",(attempt_id,task_id,attempt_id,w['workflow_id'],w['workflow_id'],w['workflow_id'],w['workflow_id'],w['workflow_id'],PREFIX+'%'))
            if cursor.fetchone()['busy']:raise Rejected(409,'reconcile_required')
            from work_feedback import parse_feedback_json
            packet=parse_feedback_json(feedback['feedback_json'],feedback['feedback_sha256'])
            if packet.get('workspace_digest_kind')!='candidate_tree':raise Rejected(409,'result_mismatch')
            from work_workflow_continuation import retained_sources,is_retained_source
            retained=retained_sources(cursor,w['workflow_id'])
            cursor.execute("SELECT * FROM forge_agent.coding_tasks WHERE workflow_id=%s AND id<>%s AND status IN('failed','needs_reconcile','quota_blocked','blocked_auth','blocked_permission','cancelled')",(w['workflow_id'],task_id))
            if any(not is_retained_source(sibling,retained) for sibling in cursor.fetchall()):raise Rejected(409,'not_reviewable')
            evidence=self.evidence(task,a,rec)
            if (not isinstance(evidence,dict) or set(evidence)!={'qualified_commit','workspace_digest','source_digest','candidate_commit','scope_stopped'}
                    or not re.fullmatch('[0-9a-f]{40}',evidence['qualified_commit']) or evidence['workspace_digest']!=rec['workspace_digest']
                    or evidence['source_digest']!=packet['workspace_digest'] or evidence['candidate_commit']!=task['base_revision'] or evidence['scope_stopped'] is not True):raise Rejected(409,'reconcile_required')
            cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase='repair',escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=%s",(w['workflow_id'],))
            for role,mode,entry in ((p['author_profile'],'implement','author_implement'),(p['reviewer_profile'],'review','reviewer_review')):
                self.flow._grant(cursor,w,role,mode)
                cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND observed_at>=clock_timestamp()-interval '90 seconds' AND policy_digest=ANY(%s)",(self.store.owner,[x[0] for x in NATIVE_POLICIES.values()]))
                if not _ready_for_recipe(cursor.fetchall(),project_alias,role,mode,recipe_from_snapshot(p,entry)):raise Rejected(503,'unavailable')
            events=self.flow.retry_events
            if events is None:raise Rejected(503,'unavailable')
            event=events.observe_failure(cursor,self.flow._retry_scope(w),'infrastructure',str(attempt_id),a['outcome_digest'],phase_id='plan_lineage')
            event=events.reserve(cursor,event['event_id'],key)
            if event['status']=='exhausted':raise Rejected(409,'retry_exhausted')
            authority=BoundPlanLineageAuthority(self.store,cursor,w['workflow_id'],task_id,attempt_id,expected_revision)
            receipt=self.store.resume(project_alias=project_alias,task_id=task_id,expected_revision=expected_revision,
                idempotency_key=str(idempotency_key),agent_profile=p['author_profile'],reconciliation_receipt_id=reconciliation_receipt_id,
                note='The verified Plan-lineage lookup fix is deployed. Continue the same approved Plan and independently requested code repair. Preserve source, review findings and budgets.',
                allowed_modes={'implement'},resolve_recipe=lambda _:recipe,_authority=authority,actor=operator_actor(self.store.owner))
            intent=dict(policy=POLICY,request=request,evidence=evidence,retry_event=event)
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt) VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",(w['workflow_id'],self.store.owner,project_alias,key,Json(intent),Json(dict(status='plan_lineage_resumed',receipt=receipt))))
            insert_event(cursor,self.store.owner,project_alias,task_id,'checkpoint',dict(stage='plan_lineage_resumed',policy=POLICY,source_attempt_id=str(attempt_id),reconciliation_receipt_id=str(reconciliation_receipt_id),summary='Verified Plan lookup repair resumed the same source-bound code repair.',receipt=receipt),actor=operator_actor(self.store.owner))
            return receipt
