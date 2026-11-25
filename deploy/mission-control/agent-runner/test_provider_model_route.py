"""Real PostgreSQL source/Plan-preserving provider backup and normal lifecycle."""
from contextlib import closing
from dataclasses import replace
from datetime import UTC,datetime
from datetime import timedelta
import hashlib
from types import SimpleNamespace
import unittest
from uuid import UUID,uuid4
from unittest.mock import patch
from concurrent.futures import ThreadPoolExecutor
from threading import Barrier
from tempfile import TemporaryDirectory

from queue_store import Rejected
from work_api import WorkServiceFactory
from work_pg_fixture import DisposablePostgresCluster,PG16_AVAILABLE
from work_provider_model_route import ModelRoute,ModelRouteEvidence,ProviderModelRouteFactory
from work_quota_diagnostics import quota_summary
from work_provider_cooldown import ProviderCooldownFactory
from work_worker_config import parse_config as parse_worker_config,verify_recipe
from test_work_executor import raw_config
from test_work_execution_and_usage import make_stop_proof
import test_work_native_retry as legacy
import work_policy

BACKUP='claude-opus-5-5-high'


@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class ProviderModelRouteTest(unittest.TestCase):
    submit=legacy.NativeTimeoutRetryTest.submit
    claim=legacy.NativeTimeoutRetryTest.claim
    publish=legacy.NativeTimeoutRetryTest.publish
    get=legacy.NativeTimeoutRetryTest.get
    verdict=legacy.NativeTimeoutRetryTest.verdict
    finish_plan_round=legacy.NativeTimeoutRetryTest.finish_plan_round
    implementation=legacy.NativeTimeoutRetryTest.implementation
    rows=legacy.NativeTimeoutRetryTest.rows
    @classmethod
    def setUpClass(cls):cls.cluster=DisposablePostgresCluster().start();cls.addClassCleanup(cls.cluster.stop)
    def setUp(self):
        legacy.NativeTimeoutRetryTest.setUp(self)
        profiles=dict(self.config.profiles)
        profiles['agy-v2']=replace(profiles['agy-v2'],backup_models=(BACKUP,))
        self.config=replace(self.config,profiles=profiles)
        self.service=WorkServiceFactory(self.config,lambda:self.cluster.connect('forge_agent'),
            retry_event_factory=lambda _:SimpleNamespace(create=lambda:None)).create()
        self.coordinator=self.service.workflow_worker
        self.advertise()
    def advertise(self,*,backup=True):
        profiles=[{'alias':p.alias,'backend':p.backend,'model':model,'modes':['plan','review','implement']}
            for p in self.config.profiles.values()if p.alias in {'agy-v2','codex-v2'}
            for model in (p.model,*p.backup_models)if backup or model!=BACKUP]
        self.db.advertise('workflow-worker',self.incarnation,[{'alias':'homelab','base_revision':'a'*40}],profiles,work_policy.POLICY_DIGEST)
    def stop_failed_attempt(self,lease):
        proof=make_stop_proof(lease.unit_name)
        self.db.publish(lease,'failed',None,None,'Agent invocation did not complete successfully.',proof)
        rec=self.db.reconcile(lease,'c'*64,proof)
        self.coordinator.run_once()
        command=ModelRoute('homelab',UUID(lease.task_id),UUID(lease.attempt_id),self.get(lease.task_id)['revision'],
            UUID(rec),BACKUP,'quota-route-'+lease.attempt_id,'Explicit user-approved AGY backup after genuine quota.')
        return command
    def route(self,lease,command):
        evidence=ModelRouteEvidence(command.task_id,command.attempt_id,command.expected_revision,
            hashlib.sha256(b'').hexdigest(),command.model,'a'*64,'b'*64,'d'*64)
        recipe=self.rows('SELECT recipe_digest FROM forge_agent.coding_attempts WHERE attempt_id=%s',(lease.attempt_id,))[0][0]
        evidence=replace(evidence,recipe_digest=recipe)
        port=ProviderModelRouteFactory(self.coordinator.store,SimpleNamespace(acquire=lambda *args:evidence)).create()
        return port.recover(command),port
    def frozen(self,lease):
        return self.rows('SELECT w.snapshot,w.policy_digest,a.pinned_recipe,a.recipe_digest FROM forge_agent.coding_workflows w JOIN forge_agent.coding_attempts a ON a.workflow_id=w.workflow_id WHERE a.attempt_id=%s',(lease.attempt_id,))[0]
    def stop_quota_attempt(self,lease,*,reset=1256):
        proof=make_stop_proof(lease.unit_name)
        diagnostic=dict(version=1,code='provider_quota_exhausted',error_sha256='f'*64,retry_after_seconds=reset)
        self.db.publish(lease,'quota_blocked',None,None,quota_summary(diagnostic),proof)
        return self.db.reconcile(lease,'c'*64,proof)
    def test_real_published_quota_automatically_routes_only_once_then_backup_waits_for_reset(self):
        self.submit();primary=self.claim('agy-v2');frozen=self.frozen(primary)
        self.stop_quota_attempt(primary);self.coordinator.run_once()
        backup=self.claim('agy-v2');self.assertIsNotNone(backup);self.assertEqual(backup.recipe['model'],BACKUP)
        self.stop_quota_attempt(backup,reset=1);self.coordinator.run_once()
        self.assertIsNone(self.claim('agy-v2'))
        detail=self.get(primary.task_id)
        self.assertEqual(detail['status'],'quota_blocked');self.assertNotEqual(detail['workflow']['phase'],'escalated')
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE source_key LIKE 'provider-model-route:%%'"),[(1,)])
        clock=SimpleNamespace(now=lambda tz:datetime.now(UTC)+timedelta(seconds=3),fromisoformat=datetime.fromisoformat)
        with patch('work_provider_model_route.datetime',clock):self.coordinator.run_once()
        resumed=self.claim('agy-v2');self.assertIsNotNone(resumed);self.assertEqual(resumed.recipe['model'],BACKUP)
        self.assertEqual(resumed.resume_attempt_id,backup.attempt_id)
        self.assertEqual(self.frozen(primary),frozen)
        self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_retry_event_admissions'),[(0,)])
    def test_unconfigured_or_unattested_quota_preserves_existing_escalation(self):
        for case in ('unattested','unconfigured'):
            with self.subTest(case=case):
                self.setUp();self.submit();primary=self.claim('agy-v2');proof=make_stop_proof(primary.unit_name)
                if case=='unconfigured':
                    profile=self.config.profiles['agy-v2'];profiles=dict(self.config.profiles);profiles['agy-v2']=replace(profile,backup_models=())
                    self.coordinator.store.config=replace(self.config,profiles=profiles)
                    self.coordinator.store.catalog=self.coordinator.store.config.catalog()
                    summary=quota_summary(dict(version=1,code='provider_quota_exhausted',error_sha256='a'*64,retry_after_seconds=10))
                else:summary='A response claims provider quota but is not a trusted normalized diagnostic.'
                self.db.publish(primary,'quota_blocked',None,None,summary,proof);self.db.reconcile(primary,'c'*64,proof)
                self.coordinator.run_once()
                self.assertEqual(self.get(primary.task_id)['workflow']['phase'],'escalated')
                self.assertIsNone(self.claim('agy-v2'))
    def test_twenty_backup_reset_resumes_are_bounded_in_one_availability_episode(self):
        self.submit();primary=self.claim('agy-v2');self.stop_quota_attempt(primary);self.coordinator.run_once()
        current=self.claim('agy-v2')
        clock=SimpleNamespace(now=lambda tz:datetime.now(UTC)+timedelta(seconds=3),fromisoformat=datetime.fromisoformat)
        for ordinal in range(20):
            self.stop_quota_attempt(current,reset=1)
            with patch('work_provider_model_route.datetime',clock):self.coordinator.run_once()
            current=self.claim('agy-v2');self.assertIsNotNone(current,ordinal)
            self.assertEqual(current.recipe['model'],BACKUP)
        self.stop_quota_attempt(current,reset=1)
        with patch('work_provider_model_route.datetime',clock):
            self.coordinator.run_once();self.coordinator.run_once()
        self.assertIsNone(self.claim('agy-v2'))
        self.assertEqual(self.get(primary.task_id)['status'],'quota_blocked')
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE source_key LIKE 'provider-quota-resume:%%'"),[(20,)])
        self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_retry_event_admissions'),[(0,)])
    def test_concurrent_identical_private_recovery_is_once_and_changed_nonce_rejected(self):
        self.submit();original=self.claim('agy-v2');command=self.stop_failed_attempt(original)
        barrier=Barrier(2)
        def run():barrier.wait(timeout=5);return self.route(original,command)[0]
        with ThreadPoolExecutor(max_workers=2)as executor:
            left=executor.submit(run);right=executor.submit(run)
            self.assertEqual(left.result(timeout=10),right.result(timeout=10))
        with self.assertRaises(Rejected):self.route(original,replace(command,idempotency_key='different-source-nonce'))
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE source_key LIKE 'provider-model-route:%%'"),[(1,)])
    def test_unattended_pg_admission_withholds_exhausted_primary_but_claims_pinned_backup(self):
        with TemporaryDirectory()as root:
            profiles=dict(self.config.profiles);profiles['agy-v2']=replace(profiles['agy-v2'],resource_profile=work_policy.AGY_POLICY_ID)
            self.config=replace(self.config,profiles=profiles)
            self.service=WorkServiceFactory(self.config,lambda:self.cluster.connect('forge_agent'),
                retry_event_factory=lambda _:SimpleNamespace(create=lambda:None)).create()
            self.coordinator=self.service.workflow_worker
            raw=raw_config(root);raw['profiles']={'agy-v2':dict(backend='antigravity',model=profiles['agy-v2'].model,
                modes=['plan','review','implement'],backup_models=[BACKUP])}
            raw['binaries']={'antigravity':raw['binaries']['antigravity']};raw['resource_profile']=work_policy.AGY_POLICY_ID
            raw['allow_implement']=True
            worker=parse_worker_config(raw);worker.state_root.mkdir(parents=True,mode=0o700)
            cooldown=ProviderCooldownFactory(worker,clock=lambda:1000).create()
            def advertise():self.db.advertise(worker.worker_id,self.incarnation,[dict(alias='homelab',base_revision='a'*40)],cooldown.selection()[1],worker.policy_digest)
            def claim():return self.db.claim(worker.worker_id,self.incarnation,['agy-v2'])
            advertise();self.submit('quota-first');primary=claim();self.assertIsNotNone(primary)
            self.submit('queued-primary-remains-queued')
            diagnostic=dict(version=1,code='provider_quota_exhausted',error_sha256='a'*64,retry_after_seconds=20)
            self.stop_quota_attempt(primary);cooldown.record(primary.to_dict(),diagnostic);advertise()
            self.assertIsNone(claim(),'Queued Gemini task cannot consume another invocation while quota-blocked')
            self.coordinator.run_once();backup=claim();self.assertIsNotNone(backup)
            self.assertEqual(backup.task_id,primary.task_id);self.assertEqual(backup.recipe['model'],BACKUP)
            self.assertTrue(verify_recipe(worker,backup.to_dict()))
            self.assertFalse(verify_recipe(worker,{**backup.to_dict(),'recipe':{**dict(backup.recipe),'model':'unconfigured-sonnet'}}))
            self.stop_quota_attempt(backup);cooldown.record(backup.to_dict(),diagnostic);advertise()
            self.assertIsNone(claim(),'Both exhausted models keep queued work untouched')
    def test_backup_deadline_recovery_keeps_effective_model_and_original_plan(self):
        _,plan,primary=self.implementation();command=self.stop_failed_attempt(primary);self.route(primary,command)
        backup=self.claim('agy-v2');approved=self.db.read_plan(backup)
        legacy.NativeTimeoutRetryTest.stop_at_deadline(self,backup)
        self.coordinator.store._recover_native_timeouts()
        resumed=self.claim('agy-v2');self.assertIsNotNone(resumed)
        self.assertEqual(resumed.recipe['model'],BACKUP);self.assertEqual(self.db.read_plan(resumed),approved)
    def test_failed_plan_routes_backup_then_normal_peer_review_creates_backup_implementation(self):
        _,admitted=self.submit();original=self.claim('agy-v2');frozen=self.frozen(original)
        command=self.stop_failed_attempt(original)
        receipt,port=self.route(original,command)
        self.assertEqual(port.recover(command),receipt)
        next_plan=self.claim('agy-v2');self.assertEqual(next_plan.recipe['model'],BACKUP)
        self.assertEqual(next_plan.task_id,original.task_id);self.assertEqual(next_plan.resume_attempt_id,original.attempt_id)
        self.publish(next_plan,'Independent complete Plan after quota recovery.')
        self.coordinator.run_once();critic=self.claim('codex-v2');self.assertIsNotNone(critic)
        self.publish(critic,self.verdict(critic,'approve'));self.coordinator.run_once()
        implementation=self.claim('agy-v2');self.assertIsNotNone(implementation)
        self.assertEqual(implementation.recipe['model'],BACKUP)
        self.assertIsNotNone(implementation.plan_decision_id)
        self.assertEqual(self.frozen(original),frozen)
        self.assertEqual(self.get(original.task_id)['workflow']['plan_revisions'],0)
        # A new workflow still uses the configured Gemini primary.
        self.submit('primary-stays-default');fresh=self.claim('agy-v2')
        self.assertEqual(fresh.recipe['model'],original.recipe['model'])
    def test_approved_primary_plan_retained_backup_implementation_enters_normal_checks(self):
        _,plan,original=self.implementation();approved=self.db.read_plan(original);frozen=self.frozen(original)
        command=self.stop_failed_attempt(original);self.route(original,command)
        resumed=self.claim('agy-v2')
        self.assertEqual(resumed.recipe['model'],BACKUP)
        self.assertEqual(resumed.plan_decision_id,original.plan_decision_id)
        self.assertEqual(self.db.read_plan(resumed),approved)
        rec=self.publish(resumed,'Completed same approved Plan using explicit backup.')
        self.db.queue_review(resumed,rec)
        check=self.db.review_claim('fixed-checks');self.assertIsNotNone(check)
        self.assertEqual(str(check['attempt_id']),resumed.attempt_id)
        self.db.review_finish(check,dict(disposition='awaiting_review',candidate_commit='d'*40,gate_manifest_sha256='e'*64,
            findings=[],verified_finding_ids=[],fingerprint=None,summary='All fixed checks passed.',cleanup_confirmed=True,
            verification=dict(checks_passed=True,candidate_workspace_digest='f'*64,repair_requires_new_workspace=False,
                cleanup_confirmed=True,integration_base='a'*40,toolchain='fixture',checks=[dict(check_id='make-ci',status='passed',head_sha='d'*40)])))
        self.coordinator.run_once();critic=self.claim('codex-v2');self.assertIsNotNone(critic)
        self.assertIsNone(self.db.publication_claim('publisher'))
        self.publish(critic,self.verdict(critic,'approve'));self.coordinator.run_once()
        pub=self.db.publication_claim('publisher');self.assertIsNotNone(pub)
        self.db.publication_finish(pub,'publishing','Reviewed branch',branch='forge/backup-fixture',commit_sha='d'*40)
        self.db.publication_finish(pub,'published','Reviewed PR',branch='forge/backup-fixture',commit_sha='d'*40,
            pr_url='https://github.com/fixture/project/pull/1')
        self.coordinator.run_once();self.assertIsNone(self.db.merge_claim('merger'))
        self.db.record_hosted_checks(pub['publication_id'],'d'*40,[dict(name=name,status='success',head_sha='d'*40)
            for name in('check','pr-fast-gate','real-browser')])
        self.coordinator.run_once();merge=self.db.merge_claim('merger');self.assertIsNotNone(merge)
        self.assertEqual(merge['policy_digest'],frozen[1])
        intent=dict(version=1,merge_id=merge['merge_id'],workflow_id=merge['workflow_id'],task_id=merge['task_id'],
            repository='fixture/project',pr_url=merge['pr_url'],candidate_commit='d'*40,candidate_tree='1'*40,
            expected_main='a'*40,merge_commit='2'*40,review_id=merge['review_id'],gate_manifest_sha256='e'*64,
            commit_payload=dict(message='Merge reviewed backup implementation',tree='1'*40,parents=['a'*40,'d'*40],
                author=dict(name='Forge',email='forge@example.test',date='2026-11-20T00:00:00Z'),
                committer=dict(name='Forge',email='forge@example.test',date='2026-11-20T00:00:00Z')))
        self.db.merge_intent(merge,intent)
        self.db.merge_finish(merge,'merged',dict(candidate_commit='d'*40,merge_commit='2'*40,pr_url=merge['pr_url'],summary='Exact reviewed source merged.'))
        self.assertEqual(self.get(resumed.task_id)['workflow']['phase'],'completed')
        self.assertEqual(self.frozen(original),frozen)
        self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND source_key LIKE %s',
            (resumed.workflow_id,'native-deadline-retry:%'))[0][0],0)
    def test_missing_backup_readiness_or_stale_source_rejects_without_overlay(self):
        for case in ('readiness','revision','attempt','cancel','policy'):
            with self.subTest(case=case):
                self.setUp();self.submit();original=self.claim('agy-v2');command=self.stop_failed_attempt(original)
                if case=='readiness':self.advertise(backup=False)
                elif case=='revision':command=replace(command,expected_revision=command.expected_revision+1)
                elif case=='attempt':command=replace(command,attempt_id=uuid4())
                elif case=='cancel':self.cluster.admin_exec("UPDATE forge_agent.coding_tasks SET status='cancelled',cancelled_at=clock_timestamp() WHERE id='"+original.task_id+"'",owner=False)
                elif case=='policy':self.coordinator.store.config=replace(self.config,project_workflows={})
                with self.assertRaises(Rejected):self.route(original,command)
                self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE source_key LIKE 'provider-model-route:%%'"),[(0,)])
                self.assertEqual(self.get(original.task_id)['status'],'cancelled' if case=='cancel' else 'failed')


if __name__=='__main__':unittest.main()
