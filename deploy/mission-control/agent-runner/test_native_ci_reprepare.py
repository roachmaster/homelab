"""Private native verification recovery preserves old evidence and writer scope."""
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from dataclasses import replace
import hashlib,json,os
from pathlib import Path
from tempfile import TemporaryDirectory
from uuid import UUID,uuid5,uuid4
import unittest
from psycopg2.extras import RealDictCursor,Json
from work_api import WorkServiceFactory
from work_external_review_context import canonical
from work_native_ci_reprepare import NativeCiReprepare,NativeCiProofConfig,NativeCiReprepareFactory,REASON
from work_pg_fixture import DisposablePostgresCluster,PG16_AVAILABLE
from queue_store import Rejected
import test_work_preparation_retry as preparation
import test_work_workflow as fixtures


@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class NativeCiReprepareTest(unittest.TestCase):
    submit=fixtures.AutonomousWorkflowTest.submit
    claim=fixtures.AutonomousWorkflowTest.claim
    publish=fixtures.AutonomousWorkflowTest.publish
    get=fixtures.AutonomousWorkflowTest.get
    verdict=fixtures.AutonomousWorkflowTest.verdict
    finish_plan_round=fixtures.AutonomousWorkflowTest.finish_plan_round
    checked_implementation=fixtures.AutonomousWorkflowTest.checked_implementation
    preparation_failure=preparation.PreparationRetryTest.preparation_failure
    rows=preparation.PreparationRetryTest.rows
    @classmethod
    def setUpClass(cls):
        cls.cluster=DisposablePostgresCluster().start();cls.addClassCleanup(cls.cluster.stop)
    def setUp(self):
        fixtures.AutonomousWorkflowTest.setUp(self)
        self.service=WorkServiceFactory(self.config,lambda:self.cluster.connect('forge_agent')).create()
        self.coordinator=self.service.workflow_worker
        self.temp=TemporaryDirectory(prefix='forge-native-ci-reprepare-');self.addCleanup(self.temp.cleanup)
        self.path=Path(self.temp.name)
    @staticmethod
    def failed_outcome(job):
        return {'disposition':'handoff','reason':REASON,'candidate_commit':'d'*40,'gate_manifest_sha256':'e'*64,
            'findings':[],'verified_finding_ids':[],'fingerprint':None,'summary':'Independent review stopped before approval.',
            'verification':{'checks_passed':False,'checks':[],'cleanup_confirmed':True,
                'candidate_workspace_digest':job['workspace_digest'],'repair_requires_new_workspace':False,
                'integration_base':job['base_revision'],'failure_stage':'checks','exception_type':'PublicationError'}}
    def row(self,sql,args=()):
        with closing(self.cluster.connect('postgres')) as c,c.cursor(cursor_factory=RealDictCursor) as q:
            q.execute(sql,args);return q.fetchone()
    def write(self,sql,args=()):
        with closing(self.cluster.connect('postgres')) as c:
            with c,c.cursor() as q:q.execute(sql,args)
    def failed(self):
        author,child,plan,source,job,raw,request=self.preparation_failure(reason=REASON)
        task=self.row('SELECT * FROM forge_agent.coding_tasks WHERE id=%s',(child.task_id,))
        review=self.row('SELECT * FROM forge_agent.coding_reviews WHERE review_id=%s',(job['review_id'],))
        workflow=self.row('SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s',(task['workflow_id'],))
        scope={'owner_id':task['owner_id'],'project_alias':task['project_alias'],'task_id':str(task['id']),
            'attempt_id':str(task['latest_attempt_id']),'review_id':str(review['review_id']),
            'workflow_id':str(workflow['workflow_id']),'correlation_id':str(task['correlation_id'])}
        identity={**scope,'candidate_commit':review['candidate_commit'],'candidate_tree':'f'*40,'native_gate_manifest_sha256':'a'*64}
        action=str(uuid5(workflow['workflow_id'],'forge-native-jenkins-v3:'+hashlib.sha256(canonical(identity)).hexdigest()))
        self.directory=self.path/'actions'/action;self.directory.mkdir(parents=True)
        (self.path/'reviews').mkdir()
        self.index=self.path/'reviews'/(job['review_id']+'.json')
        binding={'version':'forge-native-jenkins-v3','scope':scope,'native_gate_manifest_sha256':'a'*64,
            'request':{**scope,'action_id':action,'prepare_action_id':str(uuid5(UUID(action),'candidate-bundle')),
                'candidate_commit':review['candidate_commit'],'candidate_tree':'f'*40,'integration_base':review['verification']['integration_base']}}
        for path,value in ((self.index,{'version':'forge-native-jenkins-v3','action_id':action}),
                (self.directory/'native-binding.json',binding),
                (self.directory/'cancel-observation.json',{'status':'cancelled','receipt':{'provider':'jenkins','target_action_id':action,'process_started':False}})):
            path.write_bytes(canonical(value));path.chmod(0o600)
        command=NativeCiReprepare('homelab',UUID(child.task_id),task['revision'],UUID(job['review_id']),UUID(str(request['reconciliation_receipt_id'])),'native-ci-reprepare','Retry only verification against the current fixed runtime policy.')
        provider=NativeCiReprepareFactory(self.coordinator.store,NativeCiProofConfig(self.path,os.geteuid())).create()
        return provider,command,task,review,workflow
    def test_append_reprepares_same_stopped_artifact_without_changing_old_evidence_or_creating_writer(self):
        provider,command,task,old,workflow=self.failed()
        index=self.index.read_bytes();binding=(self.directory/'native-binding.json').read_bytes()
        attempt=self.row('SELECT to_jsonb(a) AS value FROM forge_agent.coding_attempts a WHERE attempt_id=%s',(task['latest_attempt_id'],))
        chunks=self.rows('SELECT content FROM forge_agent.coding_result_chunks WHERE attempt_id=%s ORDER BY chunk_index',(task['latest_attempt_id'],))
        receipt=provider.recover(command);self.assertEqual(receipt,provider.recover(command))
        self.assertEqual(receipt['infrastructure_used'],1)
        self.assertEqual(receipt['infrastructure_status'],'resolved');self.assertIs(receipt['verification_pending'],True);self.assertIs(receipt['approval_granted'],False)
        with self.assertRaises(Rejected):provider.recover(replace(command,note='Different intent'))
        new=self.row('SELECT * FROM forge_agent.coding_reviews WHERE review_id=%s',(receipt['review_id'],))
        self.assertNotEqual(new['review_id'],old['review_id']);self.assertEqual(new['previous_review_id'],old['review_id'])
        self.assertEqual(new['infrastructure_retries'],1);self.assertEqual(new['status'],'pending')
        for key in ('attempt_id','workspace_digest','parent_result_sha256','plan_decision_id','scope_plan_decision_id','reconciliation_receipt_id','base_revision'):
            self.assertEqual(new[key],old[key],key)
        self.assertEqual(new['source_revision'],old['source_revision']+1)
        self.assertIsNone(new['candidate_commit']);self.assertIsNone(new['gate_manifest_sha256']);self.assertEqual(new['verification'],{});self.assertIsNone(new['owner_decision'])
        self.assertEqual(self.row('SELECT * FROM forge_agent.coding_reviews WHERE review_id=%s',(old['review_id'],)),old)
        self.assertEqual(self.index.read_bytes(),index);self.assertEqual((self.directory/'native-binding.json').read_bytes(),binding)
        self.assertEqual(self.row('SELECT to_jsonb(a) AS value FROM forge_agent.coding_attempts a WHERE attempt_id=%s',(task['latest_attempt_id'],)),attempt)
        self.assertEqual(self.rows('SELECT content FROM forge_agent.coding_result_chunks WHERE attempt_id=%s ORDER BY chunk_index',(task['latest_attempt_id'],)),chunks)
        now=self.row('SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s',(workflow['workflow_id'],))
        self.assertEqual((now['phase'],now['enabled']),('checks',True));self.assertEqual(now['snapshot'],workflow['snapshot']);self.assertEqual(now['plan_revisions'],workflow['plan_revisions'])
        self.assertIsNone(self.claim('agy-v2'))
        job=self.db.review_claim('checks');self.assertIsNotNone(job);self.assertEqual(job['attempt_id'],str(task['latest_attempt_id']));self.assertIsNone(job['candidate_commit'])
    def test_concurrent_replay_consumes_one_event_admission_and_one_successor(self):
        provider,command,task,old,w=self.failed()
        with ThreadPoolExecutor(max_workers=2) as pool:
            receipts=list(pool.map(lambda _:provider.recover(command),range(2)))
        self.assertEqual(receipts[0],receipts[1])
        self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_reviews WHERE previous_review_id=%s',(old['review_id'],))[0][0],1)
        self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s',(receipts[0]['infrastructure_event_id'],))[0][0],1)
    def test_no_process_proof_scope_user_cancel_active_work_and_budget_fence_recovery(self):
        for case in ('cancel','root_cancel','post','process','index','scope','artifact','budget','active','reason'):
            with self.subTest(case=case):
                self.setUp();provider,command,task,old,w=self.failed()
                if case in {'cancel','root_cancel'}:self.write("INSERT INTO forge_agent.coding_control_receipts(owner_id,project_alias,task_id,action,idempotency_key,intent_digest,base_revision,result_revision,receipt_json) VALUES(%s,%s,%s,'cancel',%s,%s,1,2,%s)",(task['owner_id'],task['project_alias'],task['id'] if case=='cancel' else w['root_task_id'],str(uuid4()),'0'*64,Json({'status':'cancelled'})))
                elif case=='process':
                    p=self.directory/'cancel-observation.json';x=json.loads(p.read_text());x['receipt']['process_started']=True;p.write_bytes(canonical(x));p.chmod(0o600)
                elif case=='index':
                    x=json.loads(self.index.read_text());x['action_id']=str(uuid4());self.index.write_bytes(canonical(x));self.index.chmod(0o600)
                elif case=='post':(self.directory/'jenkins.json').write_bytes(b'{}')
                elif case=='scope':
                    p=self.directory/'native-binding.json';x=json.loads(p.read_text());x['scope']['task_id']=str(uuid4());p.write_bytes(canonical(x));p.chmod(0o600)
                elif case=='artifact':self.write('UPDATE forge_agent.coding_reviews SET parent_result_sha256=%s WHERE review_id=%s',('0'*64,old['review_id']))
                elif case=='budget':self.write('UPDATE forge_agent.coding_reviews SET infrastructure_retries=20 WHERE review_id=%s',(old['review_id'],))
                elif case=='active':self.write("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key) VALUES(%s,%s,%s,'decide','fixture-active')",(w['workflow_id'],task['owner_id'],task['project_alias']))
                else:self.write("UPDATE forge_agent.coding_reviews SET reason='unknown_failure' WHERE review_id=%s",(old['review_id'],))
                before=self.row('SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s',(w['workflow_id'],))
                with self.assertRaises((Rejected,ValueError,OSError)):provider.recover(command)
                self.assertEqual(self.row('SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s',(w['workflow_id'],)),before)
                self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_reviews WHERE previous_review_id=%s',(old['review_id'],))[0][0],0)

if __name__=='__main__':unittest.main()
