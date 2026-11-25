"""Private real-CI completion of the one-off M38 verification exception.

This resolves only the retained infrastructure cause. It never changes task,
workflow, source, review approval, author admission or merge state.
"""
from contextlib import closing
from dataclasses import dataclass
import argparse,json,re,select,time
from pathlib import Path
from uuid import UUID
from psycopg2.extras import Json,RealDictCursor
import psycopg2
from queue_store import Rejected
from work_config import sha256_canonical
from work_feedback import parse_feedback_json
from work_native_ci_postfix import POLICY,_immutable_operator_runtime
from work_owner_reverification import SOURCE
from work_retry_events import RetryScope
from work_workflow import recipe_from_snapshot

COMPLETION_KEY=POLICY+':verified'
MAX_LINEAGE=128


@dataclass(frozen=True)
class PostfixCompletionScope:
    owner_id:str
    project_alias:str
    workflow_id:UUID
    event_id:UUID
    def __post_init__(self):
        if (any(not isinstance(s,str)or not re.fullmatch('[A-Za-z0-9_.-]{1,64}',s)for s in (self.owner_id,self.project_alias))
                or not isinstance(self.workflow_id,UUID)or not isinstance(self.event_id,UUID)):raise ValueError('Invalid private completion scope')


def _node(cursor,review_id):
    cursor.execute('''SELECT q.*,t.workflow_id AS task_workflow,t.parent_task_id,t.repair_root_task_id,t.repair_source_review_id,
      t.repair_candidate_commit,t.base_revision AS task_base,t.agent_profile AS task_author,t.mode AS task_mode,t.workflow_required,
      t.correlation_id,t.cancelled_at,t.plan_decision_id AS task_plan,t.scope_plan_decision_id AS task_scope_plan,
      a.feedback_id AS attempt_feedback,a.feedback_sha256 AS attempt_feedback_sha,a.agent_profile AS attempt_author,
      a.workflow_id AS attempt_workflow,a.plan_decision_id AS attempt_plan,a.scope_plan_decision_id AS attempt_scope_plan,
      a.result_sha256 AS attempt_result,a.phase AS attempt_phase,a.reservation_active,a.lease_expires_at AS attempt_lease,
      a.published_at,a.recipe_digest AS attempt_recipe,a.pinned_recipe AS attempt_pinned_recipe
      FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id)
      JOIN forge_agent.coding_attempts a ON(a.owner_id,a.project_alias,a.task_id,a.attempt_id)=(q.owner_id,q.project_alias,q.task_id,q.attempt_id)
      WHERE q.review_id=%s FOR SHARE OF q,t,a''',(review_id,))
    return cursor.fetchone()


def _lineage(cursor,w,review_id,anchor,plan_id,artifact):
    """Trace same-source retries or immutable feedback, never mere chronology."""
    author=w['snapshot']['author_profile'];recipe=recipe_from_snapshot(w['snapshot'],'author_implement');seen=set();proof=[]
    node=_node(cursor,review_id);first=node
    for _ in range(MAX_LINEAGE):
        if not node or node['review_id']in seen:raise Rejected(409,'result_mismatch')
        seen.add(node['review_id'])
        if (node['owner_id']!=w['owner_id']or node['project_alias']!=w['project_alias']or node['task_workflow']!=w['workflow_id']
                or node['attempt_workflow']!=w['workflow_id']or node['task_mode']!='implement'or not node['workflow_required']
                or (node['repair_root_task_id']or node['task_id'])!=w['root_task_id']
                or node['task_author']!=author or node['attempt_author']!=author or node['cancelled_at']is not None
                or (node['plan_decision_id']or node['scope_plan_decision_id'])!=plan_id
                or (node['attempt_plan']or node['attempt_scope_plan'])!=plan_id
                or (node['task_plan']or node['task_scope_plan'])!=plan_id
                or node['attempt_phase']!='stopped'or node['reservation_active']or node['attempt_lease']is not None or not node['published_at']
                or node['attempt_result']!=node['parent_result_sha256']or node['attempt_recipe']!=recipe.digest or node['attempt_pinned_recipe']!=recipe.to_dict()):raise Rejected(409,'result_mismatch')
        if artifact(cursor,node['task_id'],node['attempt_id'])['result_sha256']!=node['parent_result_sha256']:raise Rejected(409,'result_mismatch')
        proof.append({'review_id':str(node['review_id']),'task_id':str(node['task_id']),'attempt_id':str(node['attempt_id']),
            'source_sha256':node['parent_result_sha256']})
        if node['review_id']==anchor:return first,proof
        previous=_node(cursor,node['previous_review_id'])if node['previous_review_id']else None
        if previous is not None and previous['attempt_id']==node['attempt_id']:
            if tuple(previous[k]for k in SOURCE)!=tuple(node[k]for k in SOURCE):raise Rejected(409,'result_mismatch')
            node=previous;continue
        if not node['attempt_feedback']or not node['attempt_feedback_sha']:raise Rejected(409,'result_mismatch')
        cursor.execute('SELECT * FROM forge_agent.coding_feedback WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND feedback_id=%s FOR SHARE',
            (w['owner_id'],w['project_alias'],node['task_id'],node['attempt_feedback']));f=cursor.fetchone()
        if not f or f['feedback_sha256']!=node['attempt_feedback_sha']:raise Rejected(409,'result_mismatch')
        payload=parse_feedback_json(f['feedback_json'],f['feedback_sha256']);parent=_node(cursor,f['review_id'])
        if (not parent or parent['attempt_id']==node['attempt_id']or f['parent_attempt_id']!=parent['attempt_id']
                or payload['feedback_id']!=str(f['feedback_id'])or payload['task_id']!=str(node['task_id'])
                or payload['parent_attempt_id']!=str(parent['attempt_id'])or payload['parent_result_sha256']!=parent['parent_result_sha256']
                or payload['reconciliation_receipt_id']!=str(parent['reconciliation_receipt_id'])or payload['source_revision']!=parent['source_revision']
                or payload['candidate_commit']!=parent['candidate_commit']or payload['gate_manifest_sha256']!=parent['gate_manifest_sha256']
                or payload['plan_decision_id']!=str(plan_id)or payload['agent_profile']!=author or payload['disposition']!='repair'):
            raise Rejected(409,'result_mismatch')
        if parent['task_id']!=node['task_id']:
            if (node['parent_task_id']!=parent['task_id']or node['repair_source_review_id']!=parent['review_id']
                    or node['repair_candidate_commit']!=parent['candidate_commit']or node['task_base']!=parent['candidate_commit']
                    or parent['repair_task_id']!=node['task_id']or payload.get('workspace_digest_kind')!='candidate_tree'
                    or payload['workspace_digest']!=parent['verification'].get('candidate_workspace_digest')):raise Rejected(409,'result_mismatch')
        elif payload.get('workspace_digest_kind','native_receipt')!='native_receipt'or payload['workspace_digest']!=parent['workspace_digest']:
            raise Rejected(409,'result_mismatch')
        node=parent
    raise Rejected(409,'result_mismatch')


class NativePostfixCompletion:
    def __init__(self,scope,connect,flow,retries,proof):self.scope,self.connect,self.flow,self.retries,self.proof=scope,connect,flow,retries,proof
    def resolve_verified(self):
        from work_native_ci_postfix_pass import NativePostfixPassEvidence
        s=self.scope
        with closing(self.connect())as connection,connection,connection.cursor(cursor_factory=RealDictCursor)as cursor:
            cursor.execute("SET LOCAL lock_timeout='4s'");cursor.execute("SET LOCAL statement_timeout='15s'")
            cursor.execute("SELECT set_config('forge.coding_owner',%s,true)",(s.owner_id,));self.flow._project_lock(cursor,s.project_alias)
            cursor.execute('SELECT * FROM forge_agent.coding_workflows WHERE owner_id=%s AND project_alias=%s AND workflow_id=%s FOR SHARE',
                (s.owner_id,s.project_alias,s.workflow_id));w=cursor.fetchone()
            if not w:raise Rejected(404,'not_found')
            self.flow._current_policy(w)
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND owner_id=%s AND project_alias=%s AND kind='decide'AND source_key=%s",
                (s.workflow_id,s.owner_id,s.project_alias,COMPLETION_KEY));completed=cursor.fetchone()
            if completed:
                receipt=completed['finish_receipt']
                if (completed['status']!='completed'or completed['intent'].get('event_id')!=str(s.event_id)
                        or receipt.get('status')!='postfix_infrastructure_resolved'):raise Rejected(409,'reconcile_required')
                return dict(receipt)
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND owner_id=%s AND project_alias=%s AND kind='decide'AND source_key=%s",
                (s.workflow_id,s.owner_id,s.project_alias,POLICY));admission=cursor.fetchone()
            if (not admission or admission['status']!='completed'or admission['intent'].get('policy')!=POLICY
                    or admission['intent']['request'].get('event_id')!=str(s.event_id)
                    or admission['finish_receipt'].get('status')!='postfix_verification_queued'):raise Rejected(409,'not_reviewable')
            admitted=admission['finish_receipt']['receipt'];anchor=UUID(admitted['review_id']);plan_id=UUID(admission['intent']['approved_plan_id'])
            if (admitted.get('automatic_attempts_used')!=20 or admitted.get('manual_verification_exception')is not True
                    or admitted.get('automatic_allowance_renewed')is not False or admitted.get('approval_granted')is not False):raise Rejected(409,'result_mismatch')
            registered=_node(cursor,anchor);original=admission['intent']['request']
            if (not registered or registered['previous_review_id']!=UUID(original['review_id'])
                    or registered['source_revision']!=original['expected_revision']+1 or registered['infrastructure_retries']!=21
                    or str(registered['task_id'])!=original['task_id']or str(registered['attempt_id'])!=admission['intent']['source_attempt_id']
                    or registered['parent_result_sha256']!=admission['intent']['source_result_sha256']):raise Rejected(409,'result_mismatch')
            cursor.execute("SELECT event_id,status,used,attempt_limit FROM forge_agent.coding_retry_events WHERE owner_id=%s AND project_alias=%s AND scope_kind='native_workflow'AND scope_id=%s AND kind='infrastructure'AND phase_id='infrastructure'AND event_id=%s FOR UPDATE",
                (s.owner_id,s.project_alias,s.workflow_id,s.event_id));event=cursor.fetchone()
            cursor.execute('SELECT count(*)AS n FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s',(s.event_id,));count=cursor.fetchone()['n']
            if not event or event['used']!=20 or event['attempt_limit']!=20 or event['status']not in {'exhausted','resolved'}or count!=20:raise Rejected(409,'result_mismatch')
            cursor.execute("SELECT EXISTS(SELECT FROM forge_agent.coding_control_receipts c JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(c.owner_id,c.project_alias,c.task_id)WHERE t.workflow_id=%s AND c.action='cancel')AS cancelled",(s.workflow_id,))
            if cursor.fetchone()['cancelled']:raise Rejected(409,'not_reviewable')
            cursor.execute("SELECT task_id,plan_attempt_id,plan_sha256 FROM forge_agent.coding_plan_decisions WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND decision_id=%s AND decision='resume'",
                (s.owner_id,s.project_alias,w['root_task_id'],plan_id));plan=cursor.fetchone()
            if not plan or self.flow._artifact(cursor,plan['task_id'],plan['plan_attempt_id'])['result_sha256']!=plan['plan_sha256']:raise Rejected(409,'result_mismatch')
            cursor.execute("""SELECT q.review_id FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id)
              WHERE t.workflow_id=%s AND q.owner_id=%s AND q.project_alias=%s AND q.phase='local'
              AND q.verification->'checks_passed'='true'::jsonb AND q.verification->'cleanup_confirmed'='true'::jsonb
              ORDER BY q.created_at DESC LIMIT 64""",(s.workflow_id,s.owner_id,s.project_alias));candidates=cursor.fetchall()
            for candidate in candidates:
                try:node,lineage=_lineage(cursor,w,candidate['review_id'],anchor,plan_id,self.flow._artifact)
                except (Rejected,ValueError):continue
                task={'id':node['task_id'],'owner_id':node['owner_id'],'project_alias':node['project_alias'],
                    'workflow_id':node['task_workflow'],'correlation_id':node['correlation_id'],'latest_attempt_id':node['attempt_id']}
                evidence=self.proof.acquire(node,task,w,admission['intent']['request']['new_helper_sha256'])
                if (not isinstance(evidence,NativePostfixPassEvidence)or evidence.review_id!=node['review_id']or evidence.task_id!=node['task_id']
                        or evidence.attempt_id!=node['attempt_id']or evidence.candidate_commit!=node['candidate_commit']
                        or evidence.helper_sha256!=admission['intent']['request']['new_helper_sha256']):raise Rejected(409,'result_mismatch')
                if event['status']=='exhausted':
                    resolved=self.retries.resolve_verified(cursor,RetryScope(s.owner_id,s.project_alias,'native_workflow',s.workflow_id),'infrastructure',
                        'postfix-pass:'+str(node['review_id']),evidence.root_receipt_sha256,phase_id='infrastructure')
                    if resolved.get('event_id')!=str(s.event_id)or resolved.get('used')!=20 or resolved.get('status')!='resolved':raise Rejected(409,'result_mismatch')
                receipt={'status':'postfix_infrastructure_resolved','workflow_id':str(s.workflow_id),'event_id':str(s.event_id),
                    'review_id':str(node['review_id']),'task_id':str(node['task_id']),'retained_attempts_used':20,
                    'automatic_allowance_renewed':False,'approval_granted':False,'root_receipt_sha256':evidence.root_receipt_sha256}
                intent={'policy':COMPLETION_KEY,'event_id':str(s.event_id),'admission_action_id':str(admission['action_id']),
                    'anchor_review_id':str(anchor),'approved_plan_id':str(plan_id),'lineage':lineage,'proof':evidence.proof}
                cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt)VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",
                    (s.workflow_id,s.owner_id,s.project_alias,COMPLETION_KEY,Json(intent),Json(receipt)))
                cursor.execute("SELECT pg_notify('forge_coding_jobs','')")
                return receipt
            return {'status':'waiting_for_verified_ci','workflow_id':str(s.workflow_id),'event_id':str(s.event_id)}


class NativePostfixCompletionFactory:
    def __init__(self,scope,connect,flow,retries,proof):
        if (not isinstance(scope,PostfixCompletionScope)or not callable(connect)or not callable(getattr(flow,'_artifact',None))
                or not callable(getattr(retries,'resolve_verified',None))or not callable(getattr(proof,'acquire',None))):raise TypeError('Expected selected private completion ports')
        flow.store._require_project(scope.project_alias)
        if flow.store.owner!=scope.owner_id:raise ValueError('Completion owner scope changed')
        self.arguments=scope,connect,flow,retries,proof
    def create(self):return NativePostfixCompletion(*self.arguments)


def watch_verified(provider,connect,*,timeout_seconds=5400,clock=time.monotonic,wait=select.select):
    if type(timeout_seconds)is not int or not 1<=timeout_seconds<=5400:raise ValueError('Bounded completion watch required')
    deadline=clock()+timeout_seconds
    with closing(connect())as connection:
        connection.autocommit=True
        with connection.cursor()as cursor:cursor.execute('LISTEN forge_coding_jobs')
        while True:
            try:result=provider.resolve_verified()
            except psycopg2.Error as error:
                # resolve_verified has already rolled its scoped transaction
                # back. Only ordinary lock/serialization contention may wait;
                # schema, source, permission and proof errors remain failures.
                if error.pgcode not in {'55P03','40001','40P01'}:raise
                result={'status':'waiting_for_verified_ci','transient_database_retry':error.pgcode}
            if result['status']!='waiting_for_verified_ci':return result
            remaining=deadline-clock()
            if remaining<=0:return {**result,'watch_deadline_reached':True}
            wait([connection],[],[],min(30,remaining));connection.poll();connection.notifies.clear()


def main():
    import os,stat
    from work_external_recovery import peer_connect
    from work_api import WorkServiceFactory
    from work_config import load_work_config
    from work_publication_config import load_config
    from work_external_delivery import DeliveryConfig,read_private
    from work_retry_events import RetryEventProviderFactory
    from work_native_ci_postfix_pass import NativePostfixPassConfig,NativePostfixPassProofFactory
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('work-config','publication-config','project','workflow','event','deployed-fix-revision'):parser.add_argument('--'+name,required=True)
    parser.add_argument('--watch',action='store_true');parser.add_argument('--timeout-seconds',type=int,default=5400)
    parser.add_argument('--execute',action='store_true',required=True);args=parser.parse_args()
    if os.geteuid()!=0:raise RuntimeError('Completion requires the private operator')
    module=Path(__file__).absolute();_immutable_operator_runtime(module.with_name('work_native_ci_postfix.py'),args.deployed_fix_revision)
    info=module.lstat()
    if not stat.S_ISREG(info.st_mode)or info.st_uid!=0 or info.st_mode&0o022 or info.st_nlink!=1:raise ValueError('Mutable completion runtime')
    publication=load_config(args.publication_config);delivery=DeliveryConfig.parse(json.loads(read_private(publication.jenkins_delivery_config,owner=publication.worker.account.uid)))
    service=WorkServiceFactory(load_work_config(args.work_config),lambda:peer_connect('forge_agent')).create()
    if service is None or service._config.owner!=publication.worker.owner:raise ValueError('Completion scopes disagree')
    scope=PostfixCompletionScope(service._config.owner,args.project,UUID(args.workflow),UUID(args.event))
    proof=NativePostfixPassProofFactory(NativePostfixPassConfig(publication.state_root,delivery.spool_root,delivery.results_root,publication.worker.account.uid)).create()
    provider=NativePostfixCompletionFactory(scope,lambda:peer_connect('postgres'),service.workflow_worker.store,
        RetryEventProviderFactory(scope.owner_id).create(),proof).create()
    result=watch_verified(provider,lambda:peer_connect('postgres'),timeout_seconds=args.timeout_seconds)if args.watch else provider.resolve_verified()
    print(json.dumps(result,sort_keys=True))

if __name__=='__main__':
    from work_native_ci_postfix_completion import main as entry
    entry()
