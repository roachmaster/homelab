"""Private stopped-pin recovery uses real DB authority, never quota/timeout proof."""
from concurrent.futures import ThreadPoolExecutor
from dataclasses import replace
from datetime import UTC,datetime,timedelta
import hashlib
import json
from pathlib import Path
from threading import Barrier
from types import SimpleNamespace
import unittest
from unittest.mock import Mock,patch
from uuid import UUID,uuid4

from queue_store import Rejected
from work_api import WorkResume
from work_config import sha256_canonical
from work_pg_fixture import DisposablePostgresCluster,PG16_AVAILABLE
from work_provider_model_route import (BINARY_PIN_FAILURE,BinaryPinRecovery,BinaryPinEvidence,
    StoppedBinaryPinRecoveryFactory,PinnedBinaryProofFactory)
from work_retry_events import RetryScope,RetryEventProviderFactory
from test_work_execution_and_usage import make_stop_proof,make_v2_config
from test_work_executor import raw_config
import test_work_workflow as fixtures
import work_policy

BINARY='/opt/homelab-forge/tools/antigravity/1.5.17/agy'


@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class StoppedBinaryPinPGTest(unittest.TestCase):
    setUp=fixtures.AutonomousWorkflowTest.setUp
    submit=fixtures.AutonomousWorkflowTest.submit
    claim=fixtures.AutonomousWorkflowTest.claim
    publish=fixtures.AutonomousWorkflowTest.publish
    get=fixtures.AutonomousWorkflowTest.get
    verdict=fixtures.AutonomousWorkflowTest.verdict
    @classmethod
    def setUpClass(cls):cls.cluster=DisposablePostgresCluster().start();cls.addClassCleanup(cls.cluster.stop)
    def rows(self,statement,args=()):
        from contextlib import closing
        with closing(self.cluster.connect('postgres'))as connection,connection.cursor()as cursor:
            cursor.execute(statement,args);return cursor.fetchall()
    def stopped(self,summary=BINARY_PIN_FAILURE):
        self.submit();lease=self.claim('agy-v2');proof=make_stop_proof(lease.unit_name)
        self.db.publish(lease,'failed',None,None,summary,proof)
        rec=self.db.reconcile(lease,'c'*64,proof);self.coordinator.run_once()
        command=BinaryPinRecovery('homelab',UUID(lease.task_id),UUID(lease.attempt_id),self.get(lease.task_id)['revision'],
            UUID(rec),'binary-pin-'+lease.attempt_id,'Pinned immutable AGY installation verified by the operator.')
        activated=datetime.now(UTC)
        evidence=BinaryPinEvidence(command.task_id,command.attempt_id,command.expected_revision,command.reconciliation_receipt_id,
            sha256_canonical(dict(lease.recipe)),BINARY,'a'*64,'1.5.17','b'*64,'d'*64,'workflow-worker',UUID(self.incarnation),activated)
        self.db.advertise('workflow-worker',self.incarnation,[{'alias':'homelab','base_revision':'a'*40}],
            [{'alias':p.alias,'backend':p.backend,'model':p.model,'modes':['plan','review','implement']}
             for p in self.config.profiles.values()if p.alias in {'agy-v2','codex-v2'}],work_policy.POLICY_DIGEST)
        return lease,command,evidence
    def port(self,evidence):
        proof=SimpleNamespace(acquire=Mock(return_value=evidence))
        return StoppedBinaryPinRecoveryFactory(self.coordinator.store,proof).create(),proof
    def frozen(self,lease):
        return self.rows('SELECT w.snapshot,w.policy_digest,a.pinned_recipe,a.recipe_digest FROM forge_agent.coding_workflows w JOIN forge_agent.coding_attempts a ON a.workflow_id=w.workflow_id WHERE a.attempt_id=%s',(lease.attempt_id,))[0]
    def test_recovery_preserves_original_plan_recipe_history_and_nonzero_retry_budget(self):
        original,command,evidence=self.stopped();frozen=self.frozen(original)
        provider=RetryEventProviderFactory('mission-control').create()
        with self.coordinator.store.store._transaction()as cursor:
            scope=RetryScope('mission-control','homelab','native_workflow',UUID(original.workflow_id))
            event=provider.observe_failure(cursor,scope,'tests','existing-test-event','e'*64,phase_id='rust-tests')
            for index in range(3):provider.reserve(cursor,event['event_id'],'old-admission-'+str(index))
        budgets=self.rows('SELECT event_id,used,attempt_limit,status,source_id,source_digest FROM forge_agent.coding_retry_events')
        with self.assertRaises(Rejected):
            self.service.resume(WorkResume('homelab',command.task_id,command.expected_revision,'public-still-denied','agy-v2',command.reconciliation_receipt_id))
        port,proof=self.port(evidence);receipt=port.recover(command)
        self.assertEqual(port.recover(command),receipt);proof.acquire.assert_called_once()
        next_plan=self.claim('agy-v2');self.assertIsNotNone(next_plan)
        self.assertEqual((next_plan.task_id,next_plan.mode,dict(next_plan.recipe)),(original.task_id,'plan',dict(original.recipe)))
        self.assertEqual((next_plan.resume_attempt_id,next_plan.resume_workspace_digest),(original.attempt_id,'c'*64))
        self.assertIsNone(next_plan.plan_decision_id)
        self.assertEqual(self.frozen(original),frozen)
        self.assertEqual(self.rows('SELECT event_id,used,attempt_limit,status,source_id,source_digest FROM forge_agent.coding_retry_events'),budgets)
        self.publish(next_plan,'New full Plan after the verified binary pin correction.');self.coordinator.run_once()
        critic=self.claim('codex-v2');self.assertIsNotNone(critic)
        self.publish(critic,self.verdict(critic,'approve'));self.coordinator.run_once()
        implementation=self.claim('agy-v2');self.assertIsNotNone(implementation)
        self.assertEqual(implementation.mode,'implement');self.assertIsNotNone(implementation.plan_decision_id)
        self.assertEqual(implementation.recipe['model'],original.recipe['model'])
        self.assertEqual(self.frozen(original),frozen)
        self.assertEqual(self.rows("SELECT count(*)FROM forge_agent.coding_workflow_actions WHERE source_key LIKE 'stopped-binary-pin:%%'"),[(1,)])
    def test_wrong_source_stop_readiness_or_cancellation_cannot_resume(self):
        for case in('summary','revision','receipt','incarnation','activation','cancel','stage','grant','pending_review'):
            with self.subTest(case=case):
                self.setUp();original,command,evidence=self.stopped('Unrelated failure'if case=='summary'else BINARY_PIN_FAILURE)
                if case=='revision':command=replace(command,expected_revision=command.expected_revision+1)
                elif case=='receipt':command=replace(command,reconciliation_receipt_id=uuid4())
                elif case=='incarnation':evidence=replace(evidence,incarnation=uuid4())
                elif case=='activation':evidence=replace(evidence,activated_at=datetime.now(UTC)-timedelta(hours=1))
                elif case=='cancel':self.cluster.admin_exec("UPDATE forge_agent.coding_tasks SET status='cancelled',cancelled_at=clock_timestamp()WHERE id='"+original.task_id+"'",owner=False)
                elif case=='stage':self.cluster.admin_exec("UPDATE forge_agent.coding_workflows SET phase='code_review',enabled=true WHERE workflow_id='"+original.workflow_id+"'",owner=False)
                elif case=='grant':self.cluster.admin_exec("DELETE FROM forge_agent.coding_worker_grants WHERE agent_profile='agy-v2'",owner=False)
                elif case=='pending_review':
                    self.cluster.admin_exec("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status) "
                        "VALUES('"+original.workflow_id+"','mission-control','homelab','review','pending-peer-review-fixture','pending')",owner=False)
                before=self.get(original.task_id);port,_=self.port(evidence)
                with self.assertRaises(Rejected):port.recover(command)
                self.assertEqual(self.get(original.task_id),before)
                self.assertEqual(self.rows("SELECT count(*)FROM forge_agent.coding_workflow_actions WHERE source_key LIKE 'stopped-binary-pin:%%'"),[(0,)])
    def test_concurrent_recovery_is_once_and_changed_operator_request_is_rejected(self):
        original,command,evidence=self.stopped();port,proof=self.port(evidence);barrier=Barrier(2)
        def run():barrier.wait(timeout=5);return port.recover(command)
        with ThreadPoolExecutor(max_workers=2)as executor:
            first=executor.submit(run);second=executor.submit(run)
            self.assertEqual(first.result(timeout=10),second.result(timeout=10))
        self.assertEqual(proof.acquire.call_count,1)
        with self.assertRaises(Rejected):port.recover(replace(command,idempotency_key='changed-request'))
        self.assertEqual(self.rows("SELECT count(*)FROM forge_agent.coding_workflow_actions WHERE source_key LIKE 'stopped-binary-pin:%%'"),[(1,)])
        resumed=self.claim('agy-v2');self.assertEqual(resumed.task_id,original.task_id)


class PinnedBinaryProofTest(unittest.TestCase):
    def fixture(self):
        config=make_v2_config();recipe=config.catalog().resolve('agy-v2','plan')
        command=BinaryPinRecovery('homelab',uuid4(),uuid4(),3,uuid4(),'source-pin','Verified installation')
        task=dict(owner_id='mission-control',agent_profile='agy-v2',pinned_recipe=recipe.to_dict(),recipe_digest=recipe.digest)
        workflow=dict(workflow_id=uuid4())
        selected=raw_config('/tmp/pin-proof-fixture');selected['owner']='mission-control'
        selected['profiles']={'agy-v2':dict(backend='antigravity',model=recipe.model,modes=['plan','review','implement'])}
        selected['binaries']={'antigravity':BINARY};data=json.dumps(selected).encode()
        manifest=dict(version=1,owner='mission-control',binary=dict(path=BINARY,sha256='a'*64,version='1.5.17'),
            worker=dict(config_path='/etc/homelab-forge/coding-worker/pinned.json',config_sha256=hashlib.sha256(data).hexdigest(),
                worker_id=selected['worker_id'],uid=selected['account']['uid'],incarnation=str(uuid4()),activated_at=datetime.now(UTC).isoformat()),
            sources=[dict(workflow_id=str(workflow['workflow_id']),task_id=str(command.task_id),attempt_id=str(command.attempt_id),
                source_revision=3,recipe_digest=recipe.digest,reconciliation_receipt_id=str(command.reconciliation_receipt_id))])
        return command,task,workflow,data,manifest
    def test_selected_root_manifest_and_worker_owned_config_bind_actual_binary_without_spawning(self):
        command,task,workflow,data,manifest=self.fixture()
        path=Path('/root/private-binary-pin.json')
        def read(selected,*,owner,maximum):
            if selected==path:self.assertEqual(owner,0);return json.dumps(manifest).encode()
            self.assertEqual(owner,1000);return data
        with patch('os.open',side_effect=AssertionError('No acquisition in constructors')):
            proof=PinnedBinaryProofFactory(path).create()
        with patch('work_external_delivery.read_private',side_effect=read),patch('work_provider_model_route._immutable_binary_sha256',return_value='a'*64)as digest,patch('subprocess.Popen',side_effect=AssertionError('No native/model command')):
            evidence=proof.acquire(command,task,{},workflow)
        digest.assert_called_once_with(Path(BINARY));self.assertEqual(evidence.binary_version,'1.5.17')
        self.assertEqual(evidence.recipe_digest,task['recipe_digest'])
    def test_changed_binary_config_manifest_or_source_is_rejected_before_recovery(self):
        for case in('binary','config','version','source','duplicate','uid'):
            with self.subTest(case=case):
                command,task,workflow,data,manifest=self.fixture()
                if case=='config':manifest['worker']['config_sha256']='f'*64
                elif case=='version':manifest['version']=True
                elif case=='source':manifest['sources'][0]['source_revision']=4
                elif case=='duplicate':manifest['sources']*=2
                elif case=='uid':manifest['worker']['uid']=True
                def read(selected,**options):return json.dumps(manifest).encode()if str(selected).startswith('/root/')else data
                with patch('work_external_delivery.read_private',side_effect=read),patch('work_provider_model_route._immutable_binary_sha256',return_value='b'*64 if case=='binary'else'a'*64):
                    with self.assertRaises(ValueError):PinnedBinaryProofFactory('/root/private.json').create().acquire(command,task,{},workflow)


if __name__=='__main__':unittest.main()
