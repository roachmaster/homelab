"""Private operator recovery DTO/CLI. No HTTP route and no model execution."""
from dataclasses import dataclass
from pathlib import Path
from uuid import UUID
import argparse,hashlib,json,os,pwd,re,stat,unicodedata


@dataclass(frozen=True)
class ExternalInfrastructureRetry:
    project_alias:str
    workflow_id:UUID
    root_task_id:UUID
    expected_workflow_revision:int
    expected_root_revision:int
    idempotency_key:UUID
    reason:str
    deployed_fix_revision:str

    def __post_init__(self):
        if not isinstance(self.project_alias,str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,64}',self.project_alias):raise ValueError('Invalid recovery project')
        if any(not isinstance(value,UUID) for value in (self.workflow_id,self.root_task_id,self.idempotency_key)):raise ValueError('Invalid recovery identity')
        if any(type(value) is not int or not 1<=value<9007199254740991 for value in (self.expected_workflow_revision,self.expected_root_revision)):raise ValueError('Invalid recovery revision')
        if not isinstance(self.reason,str) or not self.reason.strip() or len(self.reason.encode())>1024 or any(unicodedata.category(c)=='Cc' for c in self.reason):raise ValueError('Invalid recovery reason')
        if not isinstance(self.deployed_fix_revision,str) or not re.fullmatch('[0-9a-f]{40}',self.deployed_fix_revision):raise ValueError('Invalid deployed fix revision')

    def intent(self,owner):
        return {'version':1,'owner':owner,**{key:str(value) if isinstance(value,UUID) else value for key,value in vars(self).items()}}


def verified_deployed_fix(module_path,revision):
    """The operator runs the actual immutable release, not an arbitrary SHA label."""
    module=Path(module_path).absolute()
    if '..' in module.parts or len(module.parents)<5:raise ValueError('Expected immutable release module')
    release=module.parents[4]
    expected=release/'source/deploy/mission-control/agent-runner/work_external_recovery.py'
    if module!=expected or release.parent!=Path('/opt/homelab-forge/releases'):raise ValueError('Recovery must run from an installed immutable release')
    for directory in module.parents:
        info=directory.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid!=0 or info.st_mode&0o022:raise ValueError('Mutable recovery runtime parent')
    for path in (module,release/'release.json',module.with_name('work_executor.py')):
        info=path.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_uid!=0 or info.st_mode&0o022 or info.st_nlink!=1:raise ValueError('Mutable recovery runtime')
    metadata=json.loads((release/'release.json').read_text())
    if metadata.get('source_revision')!=revision:raise ValueError('Deployed fix revision does not match this runtime')
    import work_executor
    if Path(work_executor.__file__).absolute()!=module.with_name('work_executor.py'):raise ValueError('Supervisor was imported outside the deployed release')
    return {'deployed_fix_revision':revision,'supervisor_sha256':hashlib.sha256(module.with_name('work_executor.py').read_bytes()).hexdigest()}


def peer_connect(role):
    """Operator-only acquisition; DB sessions retain the real authenticated role."""
    import psycopg2,threading
    if os.geteuid()!=0 or threading.active_count()!=1:raise RuntimeError('Operator peer acquisition requires a single-threaded root process')
    if role not in {'postgres','forge_agent'}:raise ValueError('Unexpected operator database role')
    account=pwd.getpwnam(role);previous_gid=os.getegid()
    try:
        os.setegid(account.pw_gid);os.seteuid(account.pw_uid)
        return psycopg2.connect(host='/var/run/postgresql',port=5432,dbname='forge_agent',user=role,connect_timeout=4,options='-c statement_timeout=10000 -c lock_timeout=4000')
    finally:os.seteuid(0);os.setegid(previous_gid)


class NativeReviewerGrantScope:
    """Private operator-owned read/lock port; never injected into the HTTP host."""
    def __init__(self,connect_factory):
        if not callable(connect_factory):raise TypeError('Expected grant-lock connection factory')
        self._connect=connect_factory

    def acquire(self,owner,project,profile,workflow_id):
        from contextlib import contextmanager,closing
        from queue_store import Rejected
        @contextmanager
        def scope():
            with closing(self._connect()) as connection:
                with connection,connection.cursor() as cursor:
                    cursor.execute("SET LOCAL lock_timeout='4s'");cursor.execute("SET LOCAL statement_timeout='10s'")
                    cursor.execute("SELECT 1 FROM forge_agent.coding_worker_grants WHERE worker_role='forge_coding_worker' AND(owner_id,project_alias,agent_profile,mode)=(%s,%s,%s,'review') FOR SHARE",(owner,project,profile))
                    if cursor.fetchone() is None:raise Rejected(403,'denied')
                    class EvidenceLock:
                        def lock_review_evidence(self,review_id,attempt_id):
                            cursor.execute('''SELECT a.attempt_id FROM forge_external_workflow.reviews v
                              JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(v.owner_id,v.project_alias,v.reviewer_task_id)
                              JOIN forge_agent.coding_attempts a ON(a.owner_id,a.project_alias,a.task_id,a.attempt_id)=(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)
                              WHERE(v.workflow_id,v.owner_id,v.project_alias,v.review_id,a.attempt_id,t.agent_profile)=(%s,%s,%s,%s,%s,%s) FOR SHARE OF a''',
                              (workflow_id,owner,project,review_id,attempt_id,profile))
                            if cursor.fetchone() is None:raise Rejected(409,'not_reviewable')
                            cursor.execute('SELECT receipt_id FROM forge_agent.coding_reconciliation_receipts WHERE attempt_id=%s FOR SHARE',(attempt_id,))
                            if cursor.fetchone() is None:raise Rejected(409,'reconcile_required')
                        def exhausted_reviewer_event(self,review_id,task_id,attempt_id,target_sha256,*,failure_status='failed'):
                            from work_external_review_context import canonical
                            if failure_status not in {'failed','needs_reconcile'}:raise ValueError('Unexpected reviewer failure evidence')
                            digest=hashlib.sha256(canonical({'target':target_sha256,'task_id':str(task_id),
                                'attempt_id':str(attempt_id),'summary':'Native reviewer could not finish: '+failure_status})).hexdigest()
                            cursor.execute('''SELECT jsonb_build_object('event_id',e.event_id,'kind',e.kind,
                                'used',e.used,'limit',e.attempt_limit,'status',e.status) FROM forge_agent.coding_retry_events e
                                JOIN forge_agent.coding_retry_event_observations o ON o.event_id=e.event_id
                                WHERE(e.owner_id,e.project_alias,e.scope_kind,e.scope_id,e.kind,e.phase_id)=
                                  (%s,%s,'external_workflow',%s,'reviewer_correction','reviewer_correction')
                                AND(e.used,e.attempt_limit,e.status)=(20,20,'exhausted')
                                AND(o.owner_id,o.project_alias,o.scope_kind,o.scope_id,o.kind,o.phase_id,o.source_id,o.source_digest,o.observation_kind)=
                                  (e.owner_id,e.project_alias,e.scope_kind,e.scope_id,e.kind,e.phase_id,%s,%s,'failure')
                                AND(SELECT count(*) FROM forge_agent.coding_retry_event_admissions a WHERE a.event_id=e.event_id)=20
                                FOR SHARE OF e,o''',(owner,project,workflow_id,'reviewer:'+str(review_id),digest))
                            row=cursor.fetchone()
                            return row[0] if row else None
                    yield EvidenceLock()
        return scope()


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config',required=True)
    parser.add_argument('--project',required=True)
    parser.add_argument('--workflow',type=UUID,required=True)
    parser.add_argument('--root-task',type=UUID,required=True)
    parser.add_argument('--expected-workflow-revision',type=int,required=True)
    parser.add_argument('--expected-root-revision',type=int,required=True)
    parser.add_argument('--idempotency-key',type=UUID,required=True)
    parser.add_argument('--reason',required=True)
    parser.add_argument('--deployed-fix-revision',required=True)
    parser.add_argument('--execute',action='store_true',required=True)
    args=parser.parse_args()
    if os.geteuid()!=0:raise RuntimeError('The private operator command requires root; it acquires only scoped real forge_agent mutations')
    verified_deployed_fix(__file__,args.deployed_fix_revision)
    import work_external_recovery as contract
    command=contract.ExternalInfrastructureRetry(args.project,args.workflow,args.root_task,args.expected_workflow_revision,args.expected_root_revision,args.idempotency_key,args.reason,args.deployed_fix_revision)
    from work_config import load_work_config
    from work_api import WorkServiceFactory
    from work_external import ExternalWorkProviderFactory
    grants=NativeReviewerGrantScope(lambda:peer_connect('postgres'))
    service=WorkServiceFactory(load_work_config(args.config),lambda:peer_connect('forge_agent'),external_factory=lambda config:ExternalWorkProviderFactory(config,recovery_grant_provider=grants).create()).create()
    if service is None:raise RuntimeError('Configured external service is unavailable')
    print(json.dumps(service.recover_external_review_infrastructure(command),sort_keys=True))

if __name__=='__main__':
    import work_external_recovery as _canonical
    _canonical.main()
