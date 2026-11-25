"""Private, once-only verification exception after the proven M38 helper fix.

Automatic infrastructure allowance remains exhausted. This capability appends
one exact stopped-source review; it grants no author turn or code approval.
"""
from dataclasses import dataclass
from contextlib import ExitStack
import argparse,json,re
from pathlib import Path
from uuid import UUID
from psycopg2.extras import Json
from queue_store import Rejected
from work_config import sha256_canonical
from work_events import operator_actor
from work_owner_reverification import BoundOwnerVerificationAuthority,SOURCE
from work_workflow import BoundPreparationRetryAuthority,recipe_from_snapshot,_ready_for_recipe,NATIVE_POLICIES

POLICY='forge-native-ci-postfix-m38-v1'
REASON='infrastructure_retries_exhausted'
_ADMISSION=object()


@dataclass(frozen=True)
class NativeCiPostfix:
    project_alias:str
    task_id:UUID
    expected_revision:int
    review_id:UUID
    reconciliation_receipt_id:UUID
    event_id:UUID
    deployed_fix_revision:str
    new_helper_sha256:str
    idempotency_key:str
    note:str
    def __post_init__(self):
        if (not isinstance(self.project_alias,str)or not re.fullmatch('[A-Za-z0-9_.-]{1,64}',self.project_alias)
                or any(not isinstance(v,UUID)for v in (self.task_id,self.review_id,self.reconciliation_receipt_id,self.event_id))
                or type(self.expected_revision)is not int or not 1<=self.expected_revision<2**53
                or not isinstance(self.deployed_fix_revision,str)or not re.fullmatch('[0-9a-f]{40}',self.deployed_fix_revision)
                or not isinstance(self.new_helper_sha256,str)or not re.fullmatch('[0-9a-f]{64}',self.new_helper_sha256)
                or not isinstance(self.idempotency_key,str)or not re.fullmatch('[A-Za-z0-9_.-]{1,128}',self.idempotency_key)
                or not isinstance(self.note,str)or not self.note.strip()or len(self.note.encode())>1536):raise ValueError('Invalid native post-fix command')


@dataclass(frozen=True)
class NativePostfixEvidence:
    review_id:UUID
    candidate_commit:str
    event_digest:str
    deployed_fix_revision:str
    new_helper_sha256:str
    proof:dict
    def __post_init__(self):
        if (not isinstance(self.review_id,UUID)or not re.fullmatch('[0-9a-f]{40}',self.candidate_commit)
                or not re.fullmatch('[0-9a-f]{64}',self.event_digest)
                or not re.fullmatch('[0-9a-f]{40}',self.deployed_fix_revision)
                or not re.fullmatch('[0-9a-f]{64}',self.new_helper_sha256)
                or not isinstance(self.proof,dict)or self.proof.get('cleanup_confirmed')is not True
                or type(self.proof.get('checks_executed'))is not int or self.proof.get('checks_executed')!=0 or self.proof.get('matched_m38_policy')is not True):
            raise ValueError('Invalid trusted post-fix evidence')


@dataclass(frozen=True)
class BoundPostfixVerificationAuthority(BoundOwnerVerificationAuthority):
    """Private subclass uses the existing exact manual append, never a reset."""
    def __post_init__(self):
        if self._admission is not _ADMISSION:raise TypeError('Post-fix authority requires locked explicit admission')
    def validate(self,cursor,task,kind):
        BoundPreparationRetryAuthority.validate(self,cursor,task,kind)
        if (self._admission is not _ADMISSION or self.store.owner!=self.owner_id
                or task['owner_id']!=self.owner_id or task['project_alias']!=self.project_alias):raise Rejected(403,'denied')
    def validate_append(self,cursor,task,review):
        self.validate(cursor,task,'code');self.validate_review_decision('retry_infrastructure',review['review_id'])
        if (review['phase']!='local'or review['status']!='handoff'or review['reason']!=REASON
                or tuple(review[k]for k in SOURCE)!=self.source_identity
                or review['infrastructure_retries']!=20 or self.infrastructure_retries!=20
                or review['candidate_commit']!=self.candidate_commit or review['gate_manifest_sha256']!=self.gate_manifest_sha256
                or sha256_canonical(review['verification'])!=self.verification_sha256):raise Rejected(409,'not_reviewable')
        cursor.execute('SELECT max(review_no) AS latest FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s',
            (self.owner_id,self.project_alias,self.task_id))
        if cursor.fetchone()['latest']!=review['review_no']:raise Rejected(409,'not_reviewable')


class NativeCiPostfixRecovery:
    def __init__(self,flow,proof,ledger):self.flow,self.proof,self.ledger=flow,proof,ledger
    def recover(self,command):
        if not isinstance(command,NativeCiPostfix):raise TypeError('Expected typed post-fix command')
        flow=self.flow;store=flow.store;store._require_project(command.project_alias)
        request={k:str(v)if isinstance(v,UUID)else v for k,v in vars(command).items()}
        key=POLICY;intent_digest=sha256_canonical(request)
        with ExitStack()as evidence_locks,flow._transaction()as cursor:
            flow._project_lock(cursor,command.project_alias)
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE',
                (store.owner,command.project_alias,command.task_id));task=cursor.fetchone()
            if not task:raise Rejected(404,'not_found')
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key=%s",(task['workflow_id'],key));prior=cursor.fetchone()
            if prior:
                if prior['intent'].get('request')!=request or prior['intent'].get('intent_digest')!=intent_digest:raise Rejected(409,'idempotency_conflict')
                if prior['status']!='completed'or prior['finish_receipt'].get('status')!='postfix_verification_queued':raise Rejected(409,'reconcile_required')
                return dict(prior['finish_receipt']['receipt'])
            cursor.execute('SELECT * FROM forge_agent.coding_workflows WHERE owner_id=%s AND project_alias=%s AND workflow_id=%s FOR UPDATE',
                (store.owner,command.project_alias,task['workflow_id']));w=cursor.fetchone()
            if (not w or w['enabled']or w['phase']!='escalated'or w['escalation_reason']!=REASON
                    or task['revision']!=command.expected_revision or task['status']!='awaiting_review'or task['mode']!='implement'
                    or not task['workflow_required']or task['workflow_review_id']is not None or task['cancelled_at']is not None):raise Rejected(409,'not_reviewable')
            flow._current_policy(w);recipe=recipe_from_snapshot(w['snapshot'],'author_implement')
            if (task['agent_profile']!=w['snapshot']['author_profile']or task['recipe_digest']!=recipe.digest
                    or task['pinned_recipe']!=recipe.to_dict()):raise Rejected(409,'result_mismatch')
            cursor.execute('SELECT * FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s ORDER BY review_no DESC LIMIT 1 FOR UPDATE',
                (store.owner,command.project_alias,task['id']));q=cursor.fetchone()
            if (not q or q['review_id']!=command.review_id or q['status']!='handoff'or q['phase']!='local'or q['reason']!=REASON
                    or q['source_revision']!=task['revision']or q['attempt_id']!=task['latest_attempt_id']or q['infrastructure_retries']!=20
                    or q['base_revision']!=task['base_revision']or q['plan_decision_id']!=task['plan_decision_id']or q['scope_plan_decision_id']!=task['scope_plan_decision_id']
                    or q['reconciliation_receipt_id']!=command.reconciliation_receipt_id or q['finish_token']is None
                    or not q['candidate_commit']or not q['gate_manifest_sha256']or q['repair_task_id']is not None):raise Rejected(409,'not_reviewable')
            v=q['verification'];finish=q['finish_receipt'];out=finish.get('outcome',{})if isinstance(finish,dict)else {}
            if (not isinstance(v,dict)or not isinstance(finish,dict)or v.get('checks_passed')is not False or v.get('cleanup_confirmed')is not True
                    or not isinstance(v.get('checks'),list)or len(v['checks'])!=45
                    or any(not isinstance(c,dict)or c.get('status')!='not_run'or c.get('head_sha')!=q['candidate_commit']for c in v['checks'])
                    or finish.get('status')!='handoff'or finish.get('review_id')!=str(q['review_id'])
                    or out.get('disposition')!='handoff'or out.get('reason')!=REASON
                    or out.get('verification')!=v or out.get('candidate_commit')!=q['candidate_commit']or out.get('gate_manifest_sha256')!=q['gate_manifest_sha256']):raise Rejected(409,'not_reviewable')
            if flow.retry_events is None:raise Rejected(503,'unavailable')
            locked_event=evidence_locks.enter_context(self.ledger.acquire(store.owner,command.project_alias,w['workflow_id'],q['review_id'],command.event_id))
            event=flow.retry_events.get(cursor,command.event_id)
            if (not event or event.get('kind')!='infrastructure'or event.get('status')!='exhausted'or event.get('used')!=20 or event.get('limit')!=20):raise Rejected(409,'retry_exhausted')
            # The private locked ledger binds all twenty admissions to this exact
            # workflow, latest source review, failure digest and observation.
            cursor.execute('SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND receipt_id=%s',
                (store.owner,command.project_alias,task['id'],command.reconciliation_receipt_id));rec=cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_attempts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s',
                (store.owner,command.project_alias,task['id'],task['latest_attempt_id']));attempt=cursor.fetchone()
            if (not rec or not attempt or rec['attempt_id']!=task['latest_attempt_id']or rec['task_revision']!=task['revision']
                    or rec['workspace_digest']!=q['workspace_digest']or attempt['workflow_id']!=w['workflow_id']
                    or attempt['plan_decision_id']!=task['plan_decision_id']or attempt['scope_plan_decision_id']!=task['scope_plan_decision_id']
                    or attempt['phase']!='stopped'or attempt['reservation_active']or attempt['lease_expires_at']is not None
                    or not attempt['published_at']or attempt['result_sha256']!=q['parent_result_sha256']or attempt['recipe_digest']!=recipe.digest
                    or attempt['pinned_recipe']!=recipe.to_dict()or rec['stop_proof'].get('scope_stopped')is not True
                    or rec['stop_proof'].get('source')!='systemd'or rec['stop_proof'].get('unit_name')!=attempt['unit_name']):raise Rejected(409,'reconcile_required')
            cursor.execute("SELECT decision_id,plan_attempt_id,plan_sha256,task_id FROM forge_agent.coding_plan_decisions WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND decision_id=%s AND decision='resume'",
                (store.owner,command.project_alias,task['id']if task['plan_decision_id']else w['root_task_id'],task['plan_decision_id']or task['scope_plan_decision_id']));plan=cursor.fetchone()
            if not plan or flow._artifact(cursor,plan['task_id'],plan['plan_attempt_id'])['result_sha256']!=plan['plan_sha256']:raise Rejected(409,'result_mismatch')
            if flow._artifact(cursor,task['id'],attempt['attempt_id'])['result_sha256']!=q['parent_result_sha256']:raise Rejected(409,'result_mismatch')
            cursor.execute("SELECT EXISTS(SELECT FROM forge_agent.coding_control_receipts c JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(c.owner_id,c.project_alias,c.task_id)WHERE t.workflow_id=%s AND c.action='cancel')AS cancelled",(w['workflow_id'],))
            if cursor.fetchone()['cancelled']:raise Rejected(409,'not_reviewable')
            cursor.execute("""SELECT EXISTS(SELECT FROM forge_agent.coding_tasks WHERE workflow_id=%s AND status IN('queued','running','cancel_requested','failed','needs_reconcile','quota_blocked','blocked_auth','blocked_permission','cancelled'))
              OR EXISTS(SELECT FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id)WHERE t.workflow_id=%s AND(a.phase<>'stopped'OR a.reservation_active OR a.lease_expires_at IS NOT NULL))
              OR EXISTS(SELECT FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND status IN('pending','leased'))
              OR EXISTS(SELECT FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id)WHERE t.workflow_id=%s AND q.status IN('pending','checking'))
              OR EXISTS(SELECT FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(p.owner_id,p.project_alias,p.task_id)WHERE t.workflow_id=%s AND p.status IN('pending','checking','publishing'))AS busy""",(w['workflow_id'],)*5)
            if cursor.fetchone()['busy']:raise Rejected(409,'reconcile_required')
            proof=self.proof.acquire(command,task,q,w)
            if (not isinstance(proof,NativePostfixEvidence)or proof.review_id!=q['review_id']or proof.candidate_commit!=q['candidate_commit']
                    or proof.deployed_fix_revision!=command.deployed_fix_revision or proof.new_helper_sha256!=command.new_helper_sha256
                    or locked_event.event_id!=command.event_id or proof.event_digest!=locked_event.source_digest):raise Rejected(409,'result_mismatch')
            cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase='checks',escalation_reason='',updated_at=clock_timestamp()WHERE workflow_id=%s",(w['workflow_id'],))
            flow._grant(cursor,w,w['snapshot']['reviewer_profile'],'review')
            cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND policy_digest=ANY(%s)AND observed_at>=clock_timestamp()-interval '90 seconds'",(store.owner,[x[0]for x in NATIVE_POLICIES.values()]))
            if not _ready_for_recipe(cursor.fetchall(),command.project_alias,w['snapshot']['reviewer_profile'],'review',recipe_from_snapshot(w['snapshot'],'reviewer_review')):raise Rejected(503,'unavailable')
            authority=BoundPostfixVerificationAuthority(store,cursor,w['workflow_id'],task['id'],attempt['attempt_id'],task['revision'],q['review_id'],
                store.owner,command.project_alias,20,q['candidate_commit'],q['gate_manifest_sha256'],sha256_canonical(v),tuple(q[k]for k in SOURCE),_ADMISSION)
            receipt=store.review_decide(project_alias=command.project_alias,task_id=task['id'],expected_revision=task['revision'],review_id=q['review_id'],decision='retry_infrastructure',
                agent_profile=None,note=command.note,idempotency_key=command.idempotency_key,_authority=authority,actor=operator_actor(store.owner))
            cursor.execute("UPDATE forge_agent.coding_reviews SET candidate_commit=NULL,gate_manifest_sha256=NULL,verification='{}'::jsonb,owner_decision=NULL WHERE review_id=%s AND previous_review_id=%s AND status='pending' RETURNING *",(receipt['review_id'],q['review_id']));new=cursor.fetchone()
            if (not new or new['infrastructure_retries']!=21 or new['source_revision']!=command.expected_revision+1
                    or tuple(new[k]for k in SOURCE)!=tuple(q[k]for k in SOURCE)
                    or new['candidate_commit']is not None or new['gate_manifest_sha256']is not None or new['verification']!={}or new['owner_decision']is not None):raise Rejected(409,'result_mismatch')
            receipt.update(infrastructure_event_id=str(command.event_id),automatic_attempts_used=20,manual_verification_exception=True,
                automatic_allowance_renewed=False,verification_pending=True,approval_granted=False)
            intent={'policy':POLICY,'request':request,'intent_digest':intent_digest,'proof':proof.proof,'event_digest':proof.event_digest,
                'source_attempt_id':str(attempt['attempt_id']),'source_result_sha256':q['parent_result_sha256'],'approved_plan_id':str(plan['decision_id'])}
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt)VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",
                (w['workflow_id'],store.owner,command.project_alias,key,Json(intent),Json({'status':'postfix_verification_queued','receipt':receipt})))
            cursor.execute("SELECT pg_notify('forge_coding_jobs','')")
            return receipt


class NativeCiPostfixFactory:
    def __init__(self,flow,proof,ledger):
        if (not callable(getattr(flow,'_transaction',None))or not callable(getattr(proof,'acquire',None))
                or not callable(getattr(ledger,'acquire',None))):raise TypeError('Expected ready workflow, proof and private event-lock ports')
        self.flow,self.proof,self.ledger=flow,proof,ledger
    def create(self):return NativeCiPostfixRecovery(self.flow,self.proof,self.ledger)


def _immutable_operator_runtime(module_path,revision):
    import os,stat
    module=Path(module_path).absolute()
    if len(module.parents)<5:raise ValueError('Expected an installed immutable operator runtime')
    release=module.parents[4]
    if (module!=release/'source/deploy/mission-control/agent-runner/work_native_ci_postfix.py'
            or release.parent!=Path('/opt/homelab-forge/releases')):raise ValueError('Run post-fix recovery from the installed immutable release')
    for parent in module.parents:
        info=parent.lstat()
        if not stat.S_ISDIR(info.st_mode)or info.st_uid!=0 or info.st_mode&0o022:raise ValueError('Mutable operator runtime parent')
    for path in (module,module.with_name('work_native_ci_postfix_proof.py'),release/'release.json'):
        info=path.lstat()
        if not stat.S_ISREG(info.st_mode)or info.st_uid!=0 or info.st_mode&0o022 or info.st_nlink!=1:raise ValueError('Mutable post-fix operator runtime')
    if json.loads((release/'release.json').read_text()).get('source_revision')!=revision:raise ValueError('Post-fix source revision does not match the installed runtime')


def main():
    import os
    from work_external_recovery import peer_connect
    from work_api import WorkServiceFactory
    from work_config import load_work_config
    from work_publication_config import load_config
    from work_external_delivery import DeliveryConfig,read_private
    from work_native_ci_postfix_proof import NativePostfixProofConfig,NativePostfixProofFactory,NativePostfixLedger
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('work-config','publication-config','project','task','review','reconciliation-receipt','event','deployed-fix-revision','new-helper-sha256','idempotency-key','note'):
        parser.add_argument('--'+name,required=True)
    parser.add_argument('--expected-revision',required=True,type=int);parser.add_argument('--execute',action='store_true',required=True)
    args=parser.parse_args()
    if os.geteuid()!=0:raise RuntimeError('Post-fix verification exception requires the operator')
    _immutable_operator_runtime(__file__,args.deployed_fix_revision)
    publication=load_config(args.publication_config)
    delivery=DeliveryConfig.parse(json.loads(read_private(publication.jenkins_delivery_config,owner=publication.worker.account.uid)))
    service=WorkServiceFactory(load_work_config(args.work_config),lambda:peer_connect('forge_agent')).create()
    if service is None or service._config.owner!=publication.worker.owner:raise ValueError('Post-fix owner scopes disagree')
    request=NativeCiPostfix(args.project,UUID(args.task),args.expected_revision,UUID(args.review),UUID(args.reconciliation_receipt),UUID(args.event),
        args.deployed_fix_revision,args.new_helper_sha256,args.idempotency_key,args.note)
    proof=NativePostfixProofFactory(NativePostfixProofConfig(Path(args.publication_config),publication.state_root,publication.worker.account.uid,delivery.results_root)).create()
    ledger=NativePostfixLedger(lambda:peer_connect('postgres'))
    provider=NativeCiPostfixFactory(service.workflow_worker.store,proof,ledger).create()
    print(json.dumps(provider.recover(request),sort_keys=True))

if __name__=='__main__':
    # The proof sibling imports these typed contracts by their canonical module
    # name. Compose there too, rather than minting a second __main__ type graph.
    from work_native_ci_postfix import main as entry
    entry()
