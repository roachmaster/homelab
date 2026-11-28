"""Repair-only role admission: owned SQL and fake artifacts, no model calls."""
from dataclasses import fields, replace
import unittest
from uuid import uuid4
import psycopg2
from queue_store import Rejected
from work_api import WorkServiceFactory, WorkSubmit
from work_config import WorkProfile, WorkConfig, WorkflowPolicy, parse_work_config
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE
from work_policy import POLICY_ID, POLICY_DIGEST
from work_workflow_policy import POLICY as FROZEN_ROLES
from test_work_execution_and_usage import make_v2_config, make_submit
import test_work_delivery_automation as delivery_fixtures
import test_work_workflow as workflow_fixtures


def config(**changes):
    profiles = {
        'grok-primary': WorkProfile('grok-primary', 'grok', 'fixture-model', POLICY_ID,
                                    execution_enabled=True, branch='feature/grok'),
        'codex-author': WorkProfile('codex-author', 'codex', 'gpt-5.4', POLICY_ID, branch='feature/codex'),
        'codex-primary': WorkProfile('codex-primary', 'codex', 'gpt-5.4', POLICY_ID),
    }
    base = replace(make_v2_config(projects=('homelab',)), profiles=profiles,
                   project_workflows={'homelab': WorkflowPolicy('grok-primary', 'codex-primary')},
                   workflow_policy_compatibility=FROZEN_ROLES, identity_protocol='content-v2', delivery_enabled=True,
                   delivery_repair_projects=('homelab',), delivery_repair_authors={'homelab':'codex-author'})
    return replace(base, **changes) if changes else base


class RepairAuthorConfigTest(unittest.TestCase):
    def test_derived_policy_preserves_default_reviewer_budgets_and_owned_branch(self):
        cfg=config();normal=cfg.project_workflows['homelab'].snapshot(cfg.catalog(),cfg.repair_policy)
        derived=cfg.delivery_repair_workflow('homelab').snapshot(cfg.catalog(),cfg.repair_policy)
        self.assertEqual(normal['author_profile'],'grok-primary')
        self.assertEqual(derived['author_profile'],'codex-author')
        for key in ('reviewer_profile','max_plan_revisions','max_code_revisions','repair_policy','auto_merge','merge_method'):
            self.assertEqual(derived[key],normal[key])
        self.assertEqual(derived['max_plan_revisions'],20)
        self.assertNotEqual(derived['policy_digest'],normal['policy_digest'])
        with self.assertRaises(TypeError):cfg.delivery_repair_authors['homelab']='grok-primary'

    def test_json_selector_is_optional_closed_and_copied_before_use(self):
        cfg=config()
        legacy_args=[getattr(cfg,f.name) for f in fields(cfg) if f.name != 'delivery_repair_authors']
        legacy=WorkConfig(*legacy_args)
        self.assertEqual(dict(legacy.delivery_repair_authors),{})
        self.assertEqual(legacy.identity_protocol,cfg.identity_protocol)
        raw={'owner':cfg.owner,'allowed_projects':list(cfg.allowed_projects),
             'allowed_modes':list(cfg.allowed_modes),'profiles':{name:{'backend':p.backend,'model':p.model,
             'resource_profile':p.resource_profile,'execution_enabled':p.execution_enabled,'branch':p.branch}
             for name,p in cfg.profiles.items()},'project_workflows':{'homelab':{
             'author_profile':'grok-primary','reviewer_profile':'codex-primary'}},
             'workflow_policy_compatibility':FROZEN_ROLES,'delivery_enabled':True,
             'delivery_repair_projects':['homelab']}
        self.assertEqual(dict(parse_work_config(raw).delivery_repair_authors),{})
        raw['delivery_repair_authors']={'homelab':'codex-author'}
        parsed=parse_work_config(raw)
        raw['delivery_repair_authors']['homelab']='grok-primary'
        self.assertEqual(parsed.delivery_repair_authors['homelab'],'codex-author')
        for invalid in (None,[],{'homelab':True}):
            with self.subTest(invalid=invalid),self.assertRaises(ValueError):
                parse_work_config({**raw,'delivery_repair_authors':invalid})

    def test_disabled_foreign_branch_reviewer_and_missing_compatibility_reject(self):
        cfg=config()
        for patch in (
            {'workflow_policy_compatibility':'strict-defaults-v1'},
            {'delivery_repair_authors':{'other':'codex-author'}},
            {'delivery_repair_authors':{'homelab':'codex-primary'}},
            {'profiles':{**cfg.profiles,'codex-author':replace(cfg.profiles['codex-author'],execution_enabled=False)}},
            {'profiles':{**cfg.profiles,'codex-author':replace(cfg.profiles['codex-author'],branch='forge/agent/codex-author')}},
        ):
            with self.subTest(keys=list(patch)),self.assertRaises(ValueError):replace(cfg,**patch)
        with self.assertRaises(ValueError):replace(cfg.profiles['codex-author'],branch='feature/grok')


@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class RepairAuthorPostgresTest(unittest.TestCase):
    publish=workflow_fixtures.AutonomousWorkflowTest.publish
    verdict=workflow_fixtures.AutonomousWorkflowTest.verdict
    @classmethod
    def setUpClass(cls):
        cls.cluster=DisposablePostgresCluster().start();cls.addClassCleanup(cls.cluster.stop)
    def setUp(self):
        self.fixture=delivery_fixtures.DeliveryAutomationPGTest();self.fixture.pg=self.cluster;self.fixture.setUp()
        self.db=self.fixture.db;self.config=config()
        self.service=WorkServiceFactory(self.config,lambda:self.cluster.connect('forge_agent')).create()
        self.coordinator=self.service.workflow_worker;self.incarnation=str(uuid4())
        self.cluster.admin_exec('TRUNCATE forge_agent.coding_worker_grants,forge_agent.coding_worker_readiness CASCADE',owner=False)
        for profile in self.config.profiles:self.cluster.grant_worker('mission-control','homelab',profile)
        self.db.advertise('workflow-worker',self.incarnation,[{'alias':'homelab','base_revision':'a'*40}],
                          [{'alias':p.alias,'backend':p.backend,'model':p.model,'modes':['plan','review','implement']}
                           for p in self.config.profiles.values()],POLICY_DIGEST)
    def rows(self,sql,args=()):return self.fixture.rows(sql,args)
    def claim(self,profile):return self.db.claim('workflow-worker',self.incarnation,[profile])

    def test_real_plan_review_implementation_and_publication_keep_repair_author(self):
        normal=self.config.project_workflows['homelab'].snapshot(self.config.catalog(),self.config.repair_policy)
        old=self.service.submit(replace(make_submit(agent_profile='grok-primary',idempotency_key='ordinary-grok'),
                                        workflow_policy_digest=normal['policy_digest']))
        original=self.rows('SELECT snapshot,policy_digest,plan_revisions FROM forge_agent.coding_workflows WHERE root_task_id=%s',(old['task_id'],))[0]
        self.fixture.verification('failed');consumer=self.coordinator._delivery_repair
        self.assertTrue(consumer.run_once())
        task=self.rows("SELECT * FROM forge_agent.coding_tasks WHERE agent_profile='codex-author'")[0]
        frozen=self.rows('SELECT snapshot,policy_digest,plan_revisions FROM forge_agent.coding_workflows WHERE root_task_id=%s',(task['id'],))[0]
        self.assertEqual(frozen['snapshot']['author_profile'],'codex-author')
        self.assertEqual(frozen['snapshot']['reviewer_profile'],'codex-primary')
        self.assertEqual(self.claim('grok-primary').task_id,old['task_id'])
        plan=self.claim('codex-author');self.assertEqual(plan.task_id,str(task['id']));self.assertEqual(plan.mode,'plan')
        self.publish(plan,'A fresh source-bound repair Plan.');self.assertTrue(self.coordinator.run_once())
        reviewer=self.claim('codex-primary');self.assertIsNotNone(reviewer)
        self.publish(reviewer,self.verdict(reviewer));self.assertTrue(self.coordinator.run_once())
        implementation=self.claim('codex-author');self.assertEqual(implementation.task_id,plan.task_id)
        self.assertEqual(implementation.mode,'implement');self.assertIsNotNone(self.db.read_plan(implementation))
        receipt=self.publish(implementation,'Implemented approved scoped repair.');self.db.queue_review(implementation,receipt)
        self.coordinator.run_once();checks=self.db.review_claim('checks');self.assertIsNotNone(checks)
        self.db.review_finish(checks,dict(disposition='awaiting_review',candidate_commit='d'*40,gate_manifest_sha256='e'*64,
            findings=[],verified_finding_ids=[],fingerprint=None,summary='Owned fake checks passed.',cleanup_confirmed=True,
            verification=dict(checks_passed=True,candidate_workspace_digest='f'*64,repair_requires_new_workspace=False,
                cleanup_confirmed=True,integration_base='a'*40,toolchain='fixture',
                checks=[dict(check_id='make-ci',status='passed',head_sha='d'*40)])))
        self.coordinator.run_once();critic=self.claim('codex-primary');self.assertIsNotNone(critic)
        self.publish(critic,self.verdict(critic));self.coordinator.run_once()
        publication=self.db.publication_claim('publisher');self.assertIsNotNone(publication)
        self.assertEqual(publication['task_id'],plan.task_id);self.assertEqual(publication['attempt_id'],implementation.attempt_id)
        for task_id,expected in ((old['task_id'],original),(task['id'],frozen)):
            self.assertEqual(self.rows('SELECT snapshot,policy_digest,plan_revisions FROM forge_agent.coding_workflows WHERE root_task_id=%s',(task_id,))[0],expected)
        self.assertEqual(self.rows('SELECT rounds FROM forge_agent.delivery_lineages')[0]['rounds'],1)

    def test_saved_exact_default_request_survives_override_and_replays_once(self):
        self.fixture.verification('failed')
        old_service=WorkServiceFactory(replace(self.config,delivery_repair_authors={}),lambda:self.cluster.connect('forge_agent')).create()
        old=old_service.workflow_worker._delivery_repair;job=old.store.claim('old-router');intent=old._intent(job)
        self.assertTrue(old.store.checkpoint(job,intent))
        before=self.rows("SELECT request FROM forge_agent.delivery_operations WHERE kind='repair'")[0]
        self.cluster.admin_exec("UPDATE forge_agent.delivery_operations SET lease_until=clock_timestamp()-interval '1 second' WHERE kind='repair'",owner=False)
        consumer=self.coordinator._delivery_repair;self.assertTrue(consumer.run_once());self.assertFalse(consumer.run_once())
        self.assertEqual(self.rows('SELECT agent_profile FROM forge_agent.coding_tasks'),[{'agent_profile':'grok-primary'}])
        self.assertEqual(self.rows("SELECT request FROM forge_agent.delivery_operations WHERE kind='repair'")[0],before)
        self.assertEqual(self.rows('SELECT rounds FROM forge_agent.delivery_lineages')[0]['rounds'],1)

    def test_public_submit_cannot_select_repair_policy(self):
        derived=self.config.delivery_repair_workflow('homelab').snapshot(self.config.catalog(),self.config.repair_policy)
        with self.assertRaises(Rejected):self.service.submit(replace(make_submit(agent_profile='codex-author'),workflow_policy_digest=derived['policy_digest']))
        self.assertEqual(self.rows('SELECT count(*) n FROM forge_agent.coding_tasks')[0]['n'],0)

    def test_changed_role_or_fake_private_lease_cannot_pass_atomic_intent(self):
        self.fixture.verification('failed');consumer=self.coordinator._delivery_repair
        job=consumer.store.claim('router');intent=consumer._intent(job);self.assertTrue(consumer.store.checkpoint(job,intent))
        authority={key:job[key] for key in ('operation_id','generation','lease_token')}
        altered={**intent['command'],'agent_profile':'grok-primary'}
        for command,proof in ((altered,authority),(intent['command'],{**authority,'lease_token':str(uuid4())})):
            with self.subTest(altered=command is altered),self.assertRaises((psycopg2.Error,Rejected)):
                self.service.submit(WorkSubmit(**command,delivery_admission=proof))
        self.assertEqual(self.rows('SELECT count(*) n FROM forge_agent.coding_tasks')[0]['n'],0)
        self.assertEqual(self.rows('SELECT count(*) n FROM forge_agent.coding_workflows')[0]['n'],0)
