"""Private verification-only recovery for a stopped native CI context change."""
from dataclasses import dataclass
import argparse,hashlib,json,os,re,stat
from pathlib import Path
from uuid import UUID,uuid5
from psycopg2.extras import Json
from queue_store import Rejected
from work_config import sha256_canonical
from work_events import operator_actor
from work_external_delivery import read_private, native_action_ids
from work_external_review_context import canonical
from work_workflow import BoundPreparationRetryAuthority,recipe_from_snapshot,_ready_for_recipe,NATIVE_POLICIES

POLICY='forge-native-ci-reprepare-v1'
REASON='native_ci_review_changed'


@dataclass(frozen=True)
class NativeCiReprepare:
    project_alias:str
    task_id:UUID
    expected_revision:int
    review_id:UUID
    reconciliation_receipt_id:UUID
    idempotency_key:str
    note:str
    def __post_init__(self):
        if (not isinstance(self.project_alias,str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,64}',self.project_alias)
                or any(not isinstance(x,UUID) for x in (self.task_id,self.review_id,self.reconciliation_receipt_id))
                or type(self.expected_revision) is not int or not 1<=self.expected_revision<2**53
                or not isinstance(self.idempotency_key,str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,128}',self.idempotency_key)
                or not isinstance(self.note,str) or not self.note.strip() or len(self.note.encode())>1536):
            raise ValueError('Invalid native CI recovery command')


@dataclass(frozen=True)
class NativeCiProofConfig:
    state_root:Path
    owner_uid:int
    def __post_init__(self):
        if (not isinstance(self.state_root,Path) or not self.state_root.is_absolute() or '..' in self.state_root.parts
                or type(self.owner_uid) is not int or self.owner_uid<0):raise ValueError('Invalid trusted CI proof scope')


class NativeCiStoppedProof:
    """Read only the selected publication scope; never stop or submit a process."""
    def __init__(self,config):self.config=config
    def acquire(self,task,review,workflow):
        root=self.config.state_root;owner=self.config.owner_uid
        def read(path):
            raw=read_private(path,owner=owner,maximum=262144)
            return json.loads(raw),hashlib.sha256(raw).hexdigest()
        index,index_sha=read(root/'reviews'/(str(review['review_id'])+'.json'))
        if set(index)!={'version','action_id'} or index['version'] not in {'forge-native-jenkins-v2','forge-native-jenkins-v3'}:
            raise Rejected(409,'reconcile_required')
        action=str(UUID(index['action_id']));directory=root/'actions'/action
        binding,binding_sha=read(directory/'native-binding.json')
        cancel,cancel_sha=read(directory/'cancel-observation.json')
        expected={'owner_id':task['owner_id'],'project_alias':task['project_alias'],'task_id':str(task['id']),
            'attempt_id':str(task['latest_attempt_id']),'review_id':str(review['review_id']),
            'workflow_id':str(workflow['workflow_id']),'correlation_id':str(task['correlation_id'])}
        request=binding.get('request',{});receipt=cancel.get('receipt',{})
        identity={**expected,'candidate_commit':request.get('candidate_commit'),'candidate_tree':request.get('candidate_tree'),
            'native_gate_manifest_sha256':binding.get('native_gate_manifest_sha256')}
        if (binding.get('version')!=index['version'] or binding.get('scope')!=expected
                or any(request.get(k)!=v for k,v in expected.items()) or request.get('action_id')!=action
                or action not in native_action_ids(index['version'],identity) or request.get('prepare_action_id')!=str(uuid5(UUID(action),'candidate-bundle'))
                or request.get('candidate_commit')!=review['candidate_commit']
                or request.get('integration_base')!=review['verification'].get('integration_base')
                or cancel.get('status')!='cancelled' or receipt.get('provider')!='jenkins'
                or receipt.get('target_action_id')!=action or receipt.get('process_started') is not False
                or (directory/'jenkins.json').exists()):raise Rejected(409,'reconcile_required')
        return {'action_id':action,'index_sha256':index_sha,'binding_sha256':binding_sha,
                'cancel_sha256':cancel_sha,'process_started':False}


class NativeCiReprepareRecovery:
    def __init__(self,flow,proof):self.flow,self.proof=flow,proof
    def recover(self,command):
        if not isinstance(command,NativeCiReprepare):raise TypeError('Expected typed native CI recovery')
        flow=self.flow;store=flow.store;store._require_project(command.project_alias)
        request={k:str(v) if isinstance(v,UUID) else v for k,v in vars(command).items()}
        key=POLICY+':'+str(command.review_id);intent_sha=sha256_canonical(request)
        with flow._transaction() as cursor:
            flow._project_lock(cursor,command.project_alias)
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE',
                (store.owner,command.project_alias,command.task_id));task=cursor.fetchone()
            if not task:raise Rejected(404,'not_found')
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key=%s",(task['workflow_id'],key));prior=cursor.fetchone()
            if prior:
                if prior['intent'].get('request')!=request or prior['intent'].get('intent_sha256')!=intent_sha:raise Rejected(409,'idempotency_conflict')
                if prior['status']!='completed' or prior['finish_receipt'].get('status')!='native_ci_reprepare_queued':raise Rejected(409,'reconcile_required')
                return dict(prior['finish_receipt']['receipt'])
            cursor.execute('SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s AND owner_id=%s AND project_alias=%s FOR UPDATE',
                (task['workflow_id'],store.owner,command.project_alias));w=cursor.fetchone()
            if (not w or w['enabled'] or w['phase']!='escalated' or w['escalation_reason']!=REASON
                    or task['revision']!=command.expected_revision or task['status']!='awaiting_review'
                    or not task['workflow_required'] or task['workflow_review_id'] is not None
                    or task['mode']!='implement' or task['cancelled_at'] is not None):raise Rejected(409,'not_reviewable')
            flow._current_policy(w);recipe=recipe_from_snapshot(w['snapshot'],'author_implement')
            if (task['agent_profile']!=w['snapshot']['author_profile'] or task['recipe_digest']!=recipe.digest
                    or task['pinned_recipe']!=recipe.to_dict() or not(task['plan_decision_id'] or task['scope_plan_decision_id'])):raise Rejected(409,'result_mismatch')
            cursor.execute('SELECT * FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s ORDER BY review_no DESC LIMIT 1 FOR UPDATE',
                (store.owner,command.project_alias,task['id']));q=cursor.fetchone()
            if (not q or q['review_id']!=command.review_id or q['status']!='handoff' or q['phase']!='local' or q['reason']!=REASON
                    or q['source_revision']!=task['revision'] or q['attempt_id']!=task['latest_attempt_id']
                    or q['base_revision']!=task['base_revision'] or q['plan_decision_id']!=task['plan_decision_id']
                    or q['scope_plan_decision_id']!=task['scope_plan_decision_id'] or q['reconciliation_receipt_id']!=command.reconciliation_receipt_id
                    or not q['candidate_commit'] or not q['gate_manifest_sha256'] or q['finish_token'] is None
                    or q['infrastructure_retries']>=flow._repair_limit(w,'max_infrastructure_retries')):raise Rejected(409,'not_reviewable')
            v=q['verification'];out=(q['finish_receipt'] or {}).get('outcome',{})
            if (not isinstance(v,dict) or v.get('checks_passed') is not False or v.get('checks')!=[]
                    or v.get('cleanup_confirmed') is not True or v.get('failure_stage')!='checks'
                    or v.get('exception_type')!='PublicationError' or q['finish_receipt'].get('status')!='handoff'
                    or q['finish_receipt'].get('review_id')!=str(q['review_id']) or out.get('disposition')!='handoff'
                    or out.get('reason')!=REASON or out.get('verification')!=v
                    or out.get('candidate_commit')!=q['candidate_commit'] or out.get('gate_manifest_sha256')!=q['gate_manifest_sha256']):raise Rejected(409,'not_reviewable')
            cursor.execute('SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND receipt_id=%s',
                (store.owner,command.project_alias,task['id'],command.reconciliation_receipt_id));rec=cursor.fetchone()
            cursor.execute('SELECT owner_id,project_alias,task_id,attempt_id,workflow_id,plan_decision_id,scope_plan_decision_id,phase,reservation_active,lease_expires_at,published_at,result_sha256,unit_name,recipe_digest FROM forge_agent.coding_attempts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s',
                (store.owner,command.project_alias,task['id'],task['latest_attempt_id']));attempt=cursor.fetchone()
            if (not rec or not attempt or rec['attempt_id']!=task['latest_attempt_id'] or rec['task_revision']!=task['revision']
                    or (attempt['owner_id'],attempt['project_alias'],attempt['task_id'],attempt['attempt_id'],attempt['workflow_id'])!=(store.owner,command.project_alias,task['id'],task['latest_attempt_id'],w['workflow_id'])
                    or attempt['plan_decision_id']!=task['plan_decision_id'] or attempt['scope_plan_decision_id']!=task['scope_plan_decision_id']
                    or rec['workspace_digest']!=q['workspace_digest'] or attempt['phase']!='stopped' or attempt['reservation_active']
                    or attempt['lease_expires_at'] is not None or not attempt['published_at'] or attempt['result_sha256']!=q['parent_result_sha256']
                    or attempt['recipe_digest']!=recipe.digest or rec['stop_proof'].get('scope_stopped') is not True
                    or rec['stop_proof'].get('source')!='systemd' or rec['stop_proof'].get('unit_name')!=attempt['unit_name']):raise Rejected(409,'reconcile_required')
            cursor.execute('SELECT decision_id FROM forge_agent.coding_plan_decisions WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND decision_id=%s AND decision=\'resume\'',
                (store.owner,command.project_alias,task['id'] if task['plan_decision_id'] else w['root_task_id'],task['plan_decision_id'] or task['scope_plan_decision_id']))
            if not cursor.fetchone():raise Rejected(409,'result_mismatch')
            cursor.execute("SELECT EXISTS(SELECT FROM forge_agent.coding_control_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=ANY(%s::uuid[]) AND action='cancel') AS cancelled",
                (store.owner,command.project_alias,[str(task['id']),str(w['root_task_id'])]))
            if cursor.fetchone()['cancelled']:raise Rejected(409,'not_reviewable')
            cursor.execute("""SELECT EXISTS(SELECT FROM forge_agent.coding_tasks WHERE workflow_id=%s AND status IN('queued','running','cancel_requested','failed','needs_reconcile','quota_blocked','blocked_auth','blocked_permission','cancelled'))
              OR EXISTS(SELECT FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id) WHERE t.workflow_id=%s AND(a.phase<>'stopped' OR a.reservation_active OR a.lease_expires_at IS NOT NULL))
              OR EXISTS(SELECT FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND status IN('pending','leased'))
              OR EXISTS(SELECT FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id) WHERE t.workflow_id=%s AND q.status IN('pending','checking'))
              OR EXISTS(SELECT FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(p.owner_id,p.project_alias,p.task_id) WHERE t.workflow_id=%s AND p.status IN('pending','checking','publishing')) AS busy""",(w['workflow_id'],)*5)
            if cursor.fetchone()['busy']:raise Rejected(409,'reconcile_required')
            flow._artifact(cursor,task['id'],task['latest_attempt_id'])
            proof=self.proof.acquire(task,q,w)
            if proof.get('process_started') is not False:raise Rejected(409,'reconcile_required')
            if flow.retry_events is None:raise Rejected(503,'unavailable')
            phase='infrastructure.native-ci-context.'+str(task['latest_attempt_id'])
            event=flow.retry_events.observe_failure(cursor,flow._retry_scope(w),'infrastructure','review:'+str(q['review_id']),
                sha256_canonical({'finish_receipt':q['finish_receipt'],'proof':proof}),phase_id=phase)
            event=flow.retry_events.reserve(cursor,event['event_id'],key)
            if event['status']=='exhausted':raise Rejected(409,'retry_exhausted')
            cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase='checks',escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=%s",(w['workflow_id'],))
            flow._grant(cursor,w,w['snapshot']['author_profile'],'implement');flow._grant(cursor,w,w['snapshot']['reviewer_profile'],'review')
            cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND policy_digest=ANY(%s) AND observed_at>=clock_timestamp()-interval '90 seconds'",(store.owner,[x[0] for x in NATIVE_POLICIES.values()]));ready=cursor.fetchall()
            for profile,mode,entry in ((w['snapshot']['author_profile'],'implement','author_implement'),(w['snapshot']['reviewer_profile'],'review','reviewer_review')):
                if not _ready_for_recipe(ready,command.project_alias,profile,mode,recipe_from_snapshot(w['snapshot'],entry)):raise Rejected(503,'unavailable')
            authority=BoundPreparationRetryAuthority(store,cursor,w['workflow_id'],task['id'],task['latest_attempt_id'],task['revision'],q['review_id'])
            result=store.review_decide(project_alias=command.project_alias,task_id=task['id'],expected_revision=task['revision'],review_id=q['review_id'],decision='retry_infrastructure',agent_profile=None,note=command.note,idempotency_key=command.idempotency_key,_authority=authority,actor=operator_actor(store.owner))
            cursor.execute("UPDATE forge_agent.coding_reviews SET candidate_commit=NULL,gate_manifest_sha256=NULL,verification='{}'::jsonb,owner_decision=NULL WHERE review_id=%s AND previous_review_id=%s AND status='pending' RETURNING *",(result['review_id'],q['review_id']))
            successor=cursor.fetchone()
            if (not successor or successor['review_id']==q['review_id'] or successor['source_revision']!=command.expected_revision+1
                    or any(successor[k]!=q[k] for k in ('owner_id','project_alias','task_id','attempt_id','reconciliation_receipt_id','workspace_digest','parent_result_sha256','plan_decision_id','scope_plan_decision_id'))
                    or successor['candidate_commit'] is not None or successor['gate_manifest_sha256'] is not None
                    or successor['verification']!={} or successor['owner_decision'] is not None):raise Rejected(409,'result_mismatch')
            # The repaired infrastructure is context registration, not CI success.
            # A fresh bound UUID cannot retarget the old index; actual checks and
            # independent approval remain pending under their existing budgets.
            registration={'source_review_id':str(q['review_id']),'review_id':result['review_id'],
                'source_attempt_id':str(task['latest_attempt_id']),'source_sha256':q['parent_result_sha256'],
                'workspace_digest':q['workspace_digest'],'proof':proof,'verification_pending':True,'approval_granted':False}
            resolved=flow.retry_events.resolve_verified(cursor,flow._retry_scope(w),'infrastructure',
                'ci-registration:'+result['review_id'],sha256_canonical(registration),phase_id=phase)
            if not resolved or resolved.get('event_id')!=event['event_id'] or resolved.get('status')!='resolved':raise Rejected(409,'result_mismatch')
            receipt={**result,'infrastructure_event_id':event['event_id'],'infrastructure_used':event['used'],
                'infrastructure_status':'resolved','source_attempt_id':str(task['latest_attempt_id']),
                'verification_pending':True,'approval_granted':False}
            intent={'policy':POLICY,'request':request,'intent_sha256':intent_sha,'proof':proof}
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt) VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",
                (w['workflow_id'],store.owner,command.project_alias,key,Json(intent),Json({'status':'native_ci_reprepare_queued','receipt':receipt})))
            cursor.execute("SELECT pg_notify('forge_coding_jobs','')")
            return receipt


class NativeCiReprepareFactory:
    def __init__(self,flow,config):
        if not isinstance(config,NativeCiProofConfig) or not callable(getattr(flow,'_transaction',None)):raise TypeError('Expected ready workflow and typed proof scope')
        self.flow,self.config=flow,config
    def create(self):return NativeCiReprepareRecovery(self.flow,NativeCiStoppedProof(self.config))


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('work-config','publication-config','project','task','review','reconciliation-receipt','idempotency-key','note'):
        parser.add_argument('--'+name,required=True)
    parser.add_argument('--expected-revision',type=int,required=True);parser.add_argument('--execute',action='store_true',required=True)
    args=parser.parse_args()
    if os.geteuid()!=0:raise RuntimeError('Private native CI recovery requires the operator')
    from work_external_recovery import peer_connect
    from work_config import load_work_config
    from work_api import WorkServiceFactory
    from work_publication_config import load_config
    config=load_config(args.publication_config)
    service=WorkServiceFactory(load_work_config(args.work_config),lambda:peer_connect('forge_agent')).create()
    if service is None or service._config.owner!=config.worker.owner:raise ValueError('Native recovery scopes disagree')
    command=NativeCiReprepare(args.project,UUID(args.task),args.expected_revision,UUID(args.review),UUID(args.reconciliation_receipt),args.idempotency_key,args.note)
    provider=NativeCiReprepareFactory(service.workflow_worker.store,NativeCiProofConfig(config.state_root/'native-ci',config.worker.account.uid)).create()
    print(json.dumps(provider.recover(command),sort_keys=True))

if __name__=='__main__':main()
