"""An owner-wide allowance resumes one genuine external revision without resetting it."""
from copy import deepcopy
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from dataclasses import replace
from threading import Barrier
from uuid import UUID, uuid4
from types import SimpleNamespace
import json
import unittest

from psycopg2.extras import RealDictCursor
from queue_store import Rejected
from work_api import WorkServiceFactory
from work_external import ExternalWorkProviderFactory
from work_external_recovery import NativeReviewerGrantScope
from work_pg_fixture import PG16_AVAILABLE, SQL_DIR
from work_retry_continuation import ExternalRetryPolicyContinuation
from work_retry_policy import AGENT_RETRY_LIMIT
from test_work_execution_and_usage import make_submit
import test_external_flow as fixture


class ExternalSharedRetryPolicyTest(unittest.TestCase):
    def test_every_external_author_uses_the_same_shared_defaults_and_bounds(self):
        from work_external_flow_config import ExternalWorkflowPolicy
        for author in ('muse-db-primary', 'future-external-author'):
            policy = ExternalWorkflowPolicy(author, 'codex-v2')
            self.assertEqual((policy.max_plan_revisions, policy.max_code_revisions), (20, 20))
            for changes in ({'max_plan_revisions': 21}, {'max_code_revisions': 21}, {'max_plan_revisions': True}):
                with self.subTest(author=author, changes=changes), self.assertRaises(ValueError):
                    replace(policy, **changes)

    def test_historical_projection_retains_frozen_limit_when_current_policy_drifted(self):
        from work_config import sha256_canonical
        from work_external_workflow import ExternalWorkflowStore
        config = fixture.cfg()
        flow = ExternalWorkflowStore(config, config.catalog())
        frozen = config.external_workflows['homelab'].snapshot(config.catalog())
        frozen['max_plan_revisions'] = frozen['max_code_revisions'] = 3
        frozen['merge_method'] = 'squash'  # A distinct historical, immutable policy.
        frozen['policy_digest'] = sha256_canonical({k:v for k,v in frozen.items() if k!='policy_digest'})
        w = {'project_alias':'homelab', 'snapshot':frozen}
        with self.assertRaises(ValueError):flow._retry_limit(w, 'max_plan_revisions')
        self.assertEqual(flow._project_retry_limit(w, 'max_plan_revisions'), 3)

    def recorded_projection(self):
        from work_config import sha256_canonical
        from work_external_workflow import ExternalWorkflowStore
        config=fixture.cfg()
        frozen=config.external_workflows['homelab'].snapshot(config.catalog())
        frozen['max_plan_revisions']=frozen['max_code_revisions']=3
        frozen['policy_digest']=sha256_canonical({k:v for k,v in frozen.items() if k!='policy_digest'})
        wid,root,source,assignment,token,task=(uuid4() for _ in range(6))
        command=ExternalRetryPolicyContinuation('homelab',wid,root,source,10,5,token,'Apply the shared allowance.')
        intent=command.intent(config.owner)
        from work_external_review_context import canonical
        import hashlib
        receipt={'status':'plan_revision_queued','project_alias':'homelab','workflow_id':str(wid),'root_task_id':str(root),
            'task_id':str(task),'workflow_revision':11,'root_revision':6,'plan_revisions':4,'max_plan_revisions':20,'max_code_revisions':20}
        record={'policy':'forge-agent-retry-v1','frozen_policy_digest':frozen['policy_digest'],'intent':intent,
            'intent_digest':hashlib.sha256(canonical(intent)).hexdigest(),'idempotency_key':str(token),
            'source_review_id':str(source),'source_assignment_id':str(assignment),'source_sha256':'a'*64,'reviewer_result_sha256':'b'*64,
            'prior_workflow_revision':10,'prior_root_revision':5,'plan_revisions_before':3,'code_revisions_before':0,
            'max_plan_revisions':20,'max_code_revisions':20,'receipt':receipt}
        w={'project_alias':'homelab','workflow_id':wid,'root_task_id':root,'revision':11,'plan_revisions':4,'code_revisions':0,
            'snapshot':frozen,'policy_digest':frozen['policy_digest'],'state':{'retry_policy_updates':[record]}}
        reviewer=config.external_workflows['homelab'].reviewer_profile
        drifted=replace(config,profiles={**config.profiles,reviewer:replace(config.profiles[reviewer],model='replacement-model')})
        return ExternalWorkflowStore(drifted,drifted.catalog()),w

    def test_recorded_twenty_is_historical_display_evidence_after_config_drift(self):
        flow,w=self.recorded_projection();before=deepcopy(w)
        for key in ('max_plan_revisions','max_code_revisions'):
            with self.assertRaises(ValueError):flow._retry_limit(w,key)
            self.assertEqual(flow._project_retry_limit(w,key),20)
        self.assertEqual(w,before)
        flow.policies={}
        self.assertEqual(flow._project_retry_limit(w,'max_plan_revisions'),20)

    def test_missing_or_altered_allowance_record_never_authorizes_display_increase(self):
        flow,original=self.recorded_projection()
        transforms=[lambda w:w.update(state={}),lambda w:w['state'].update(retry_policy_updates={}),
            lambda w:w['state']['retry_policy_updates'][0].update(policy='other'),
            lambda w:w['state']['retry_policy_updates'][0].update(frozen_policy_digest='0'*64),
            lambda w:w['state']['retry_policy_updates'][0].update(intent_digest='0'*64),
            lambda w:w['state']['retry_policy_updates'][0].update(max_plan_revisions=21),
            lambda w:w['state']['retry_policy_updates'][0].update(source_assignment_id='bad'),
            lambda w:w['state']['retry_policy_updates'][0].update(source_sha256='bad'),
            lambda w:w['state']['retry_policy_updates'][0]['receipt'].update(status='approved'),
            lambda w:w['state']['retry_policy_updates'][0]['receipt'].update(workflow_id=str(uuid4())),
            lambda w:w['state']['retry_policy_updates'][0]['receipt'].update(root_task_id=str(uuid4())),
            lambda w:w['state']['retry_policy_updates'][0]['receipt'].update(max_code_revisions=True),
            lambda w:w['state']['retry_policy_updates'][0]['receipt'].update(plan_revisions=5),
            lambda w:w.update(policy_digest='0'*64)]
        for index,change in enumerate(transforms):
            with self.subTest(index=index):
                w=deepcopy(original);change(w)
                self.assertEqual(flow._project_retry_limit(w,'max_plan_revisions'),3)
                with self.assertRaises(ValueError):flow._retry_limit(w,'max_plan_revisions')


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class ExternalRetryContinuationTest(unittest.TestCase):
    setUp = fixture.ExternalFlowPGTest.setUp
    call = fixture.ExternalFlowPGTest.call
    rows = fixture.ExternalFlowPGTest.rows
    review = fixture.ExternalFlowPGTest.review
    author = fixture.ExternalFlowPGTest.author

    @classmethod
    def setUpClass(cls):
        fixture.ExternalFlowPGTest.setUpClass.__func__(cls)
        # Full source applies30 at startup. The component test also runs against
        # a legacy29 fixture while the additive migration is integrated.
        with closing(cls.pg.connect('postgres')) as connection, connection.cursor() as cursor:
            cursor.execute('SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid=\'forge_external_workflow.workflows\'::regclass AND conname=\'workflows_plan_revisions_check\'')
            definition = cursor.fetchone()[0]
        if '20' not in definition:
            cls.pg.admin_exec((SQL_DIR / 'migrations/0030_agent_wide_retry_policy.sql').read_text())

    @classmethod
    def connect(cls):
        return cls.pg.connect('forge_agent')

    def legacy_exhaustion(self):
        old_config = replace(fixture.cfg(), external_workflows={'homelab': {
            'author_profile': fixture.ALIAS, 'reviewer_profile': 'codex-v2',
            'max_plan_revisions': 3, 'max_code_revisions': 3}})
        service = WorkServiceFactory(old_config, self.connect).create()
        command = replace(make_submit(agent_profile=fixture.ALIAS, mode='plan', idempotency_key=str(uuid4())),
            workflow_policy_digest=old_config.external_workflows['homelab'].summary(old_config.catalog())['policy_digest'])
        root = service.submit(command)
        self.call('advertise', 'homelab', fixture.ALIAS, 'owned-muse')
        job = self.call('claim', 'homelab', fixture.ALIAS)
        self.call('finish', job['assignment_id'], job['generation'], job['token'], 'Original complete plan.', 'Plan ready')
        flow = service._external_workflow
        flow.run_once()
        action = self.delivery.claim('owned-delivery')
        self.delivery.checkpoint(action, {'status': 'passed', 'receipt': {'base_revision': 'a' * 40}, 'summary': 'Exact source'})
        flow.run_once()
        for index in range(3):
            self.review(flow, 'revise')
            self.author(flow, 'Full plan revision ' + str(index + 1))
        self.review(flow, 'revise')
        w = self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual((w['phase'], w['plan_revisions'], w['code_revisions']), ('escalated', 3, 0))
        return root, w

    def private_flow(self, config=None):
        grants = NativeReviewerGrantScope(lambda: self.pg.connect('postgres'))
        service = WorkServiceFactory(config or fixture.cfg(), self.connect,
            external_factory=lambda cfg: ExternalWorkProviderFactory(cfg, recovery_grant_provider=grants).create()).create()
        return service._external_workflow

    def command(self, root, w, **changes):
        root_row = self.rows('SELECT revision FROM forge_agent.coding_tasks WHERE id=%s', (root['task_id'],))[0]
        values = dict(project_alias='homelab', workflow_id=w['workflow_id'], root_task_id=UUID(root['task_id']),
            source_review_id=UUID(w['state']['active_review_id']), expected_workflow_revision=w['revision'],
            expected_root_revision=root_row['revision'], idempotency_key=uuid4(),
            reason='Apply the owner-wide twenty-attempt policy and continue the actual independent plan revision.')
        return ExternalRetryPolicyContinuation(**{**values, **changes})

    def test_same_workflow_continues_at_four_of_twenty_with_exact_saved_artifacts(self):
        root, before = self.legacy_exhaustion()
        old_attempts = self.rows('SELECT * FROM forge_agent.coding_attempts ORDER BY attempt_id')
        old_reviews = self.rows('SELECT * FROM forge_external_workflow.reviews ORDER BY review_id')
        old_artifacts = self.rows('SELECT * FROM forge_external_agent.artifacts ORDER BY assignment_id')
        old_children = self.rows('SELECT t.id,t.status,t.revision FROM forge_agent.coding_tasks t JOIN forge_external_workflow.tasks x ON(x.owner_id,x.project_alias,x.task_id)=(t.owner_id,t.project_alias,t.id) WHERE t.id<>%s ORDER BY t.id', (root['task_id'],))
        flow = self.private_flow()
        command = self.command(root, before)
        receipt = flow.continue_after_retry_policy_update(command)
        self.assertEqual(receipt['plan_revisions'], 4)
        self.assertEqual(receipt['max_plan_revisions'], AGENT_RETRY_LIMIT)
        self.assertEqual(flow.continue_after_retry_policy_update(command), receipt)
        with self.assertRaises(Rejected):
            flow.continue_after_retry_policy_update(replace(command, reason='A conflicting replay'))
        after = self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual((after['workflow_id'], after['snapshot'], after['policy_digest']),
                         (before['workflow_id'], before['snapshot'], before['policy_digest']))
        self.assertEqual((after['phase'], after['enabled'], after['plan_revisions'], after['code_revisions']),
                         ('plan_revision', True, 4, 0))
        self.assertEqual(old_attempts, self.rows('SELECT * FROM forge_agent.coding_attempts ORDER BY attempt_id'))
        self.assertEqual(old_reviews, self.rows('SELECT * FROM forge_external_workflow.reviews ORDER BY review_id'))
        self.assertEqual(old_artifacts, self.rows('SELECT * FROM forge_external_agent.artifacts ORDER BY assignment_id'))
        self.assertEqual(old_children, self.rows('SELECT id,status,revision FROM forge_agent.coding_tasks WHERE id=ANY(%s) ORDER BY id', ([r['id'] for r in old_children],)))
        events = self.rows("SELECT payload FROM forge_agent.coding_events WHERE task_id=%s AND payload->>'stage'='retry_policy_updated'", (root['task_id'],))
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0]['payload']['plan_revisions_before'], 3)
        self.assertEqual(len(after['state']['retry_policy_updates']), 1)
        next_job = self.call('claim', 'homelab', fixture.ALIAS)
        context = next_job['external_context']
        self.assertEqual(next_job['task_id'], receipt['task_id'])
        self.assertEqual(context['previous_artifact']['content'], 'Full plan revision 3')
        last_review = next(r for r in old_reviews if str(r['review_id']) == before['state']['active_review_id'])
        self.assertEqual(context['feedback'], last_review['verdict'])
        self.assertEqual(context['previous_artifact']['sha256'], last_review['source_sha256'])
        self.assertIsNone(context['approved_plan'])
        with flow._transaction() as cursor:
            projected = flow.project(cursor, 'homelab', UUID(root['task_id']))
        self.assertEqual((projected['plan_revisions'], projected['max_plan_revisions']), (4, 20))
        current=fixture.cfg();reviewer=current.external_workflows['homelab'].reviewer_profile
        drifted=replace(current,profiles={**current.profiles,reviewer:replace(current.profiles[reviewer],model='replacement-model')})
        historical=self.private_flow(drifted)
        with self.assertRaises(ValueError):historical._retry_limit(after,'max_plan_revisions')
        with historical._transaction() as cursor:
            preserved=historical.project(cursor,'homelab',UUID(root['task_id']))
        self.assertEqual((preserved['plan_revisions'],preserved['max_plan_revisions'],preserved['max_code_revisions']),(4,20,20))


    def test_concurrent_delivery_is_one_continuation_and_one_author_assignment(self):
        root, w = self.legacy_exhaustion()
        flow = self.private_flow()
        command = self.command(root, w)
        barrier = Barrier(2)
        def run(_):
            barrier.wait()
            return flow.continue_after_retry_policy_update(command)
        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(run, range(2)))
        self.assertEqual(results[0], results[1])
        self.assertEqual(self.rows("SELECT count(*) AS n FROM forge_agent.coding_events WHERE payload->>'stage'='retry_policy_updated'")[0]['n'], 1)
        self.assertEqual(self.rows("SELECT count(*) AS n FROM forge_external_agent.assignments WHERE status='queued'")[0]['n'], 1)

    def test_wrong_revision_scope_grants_review_or_manual_cancel_cannot_resume(self):
        for case in ('public_factory', 'revision', 'review', 'grant', 'manual_cancel', 'verdict', 'source', 'disabled_reviewer', 'wrong_reason'):
            with self.subTest(case=case):
                self.setUp()
                root, w = self.legacy_exhaustion()
                flow = self.private_flow()
                if case == 'disabled_reviewer':
                    base = fixture.cfg()
                    flow.config = SimpleNamespace(profiles={**base.profiles,
                        'codex-v2': replace(base.profiles['codex-v2'], execution_enabled=False)})
                command = self.command(root, w)
                if case == 'public_factory':
                    flow = WorkServiceFactory(fixture.cfg(), self.connect).create()._external_workflow
                elif case == 'revision': command = replace(command, expected_root_revision=command.expected_root_revision + 1)
                elif case == 'review': command = replace(command, source_review_id=uuid4())
                elif case == 'grant': self.pg.admin_exec('UPDATE forge_external_agent.scope_grants SET enabled=false')
                elif case == 'manual_cancel':
                    self.pg.admin_exec("INSERT INTO forge_agent.coding_control_receipts(owner_id,project_alias,task_id,action,idempotency_key,intent_digest,base_revision,result_revision,receipt_json) VALUES('test-owner','homelab','%s','cancel','explicit-user-cancel','%s',3,4,'{}')" % (root['task_id'], 'f' * 64))
                elif case == 'verdict':
                    self.pg.admin_exec("UPDATE forge_external_workflow.reviews SET verdict=verdict||'{\"decision\":\"approve\"}'::jsonb WHERE review_id='%s'" % command.source_review_id)
                elif case == 'source':
                    self.pg.admin_exec("UPDATE forge_agent.coding_tasks SET revision=revision+1 WHERE id='%s'" % w['active_task_id'])
                elif case == 'wrong_reason':
                    self.pg.admin_exec("UPDATE forge_external_workflow.workflows SET state=state||'{\"escalation_reason\":\"Operator stopped this workflow.\"}'::jsonb")
                before = self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
                with self.assertRaises(Rejected):
                    flow.continue_after_retry_policy_update(command)
                self.assertEqual(before, self.rows('SELECT * FROM forge_external_workflow.workflows')[0])
                self.assertEqual(self.rows("SELECT count(*) AS n FROM forge_external_agent.assignments WHERE status='queued'")[0]['n'], 0)

    def test_twenty_first_revision_is_refused_by_shared_runtime_and_database_bounds(self):
        _, w = self.legacy_exhaustion()
        flow = self.private_flow()
        self.pg.admin_exec("UPDATE forge_external_workflow.workflows SET plan_revisions=20 WHERE workflow_id='%s'" % w['workflow_id'])
        root = {'task_id': str(w['root_task_id'])}
        current = self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        with self.assertRaises(Rejected):
            flow.continue_after_retry_policy_update(self.command(root, current))
        with self.assertRaises(Exception):
            self.pg.admin_exec("UPDATE forge_external_workflow.workflows SET plan_revisions=21 WHERE workflow_id='%s'" % w['workflow_id'])

    def test_genuine_twentieth_revision_is_allowed_and_next_revision_escalates(self):
        root, w = self.legacy_exhaustion()
        flow = self.private_flow()
        flow.continue_after_retry_policy_update(self.command(root, w))
        self.author(flow, 'Full revised plan 4')
        for number in range(5, AGENT_RETRY_LIMIT + 1):
            self.review(flow, 'revise')
            current = self.rows('SELECT plan_revisions FROM forge_external_workflow.workflows')[0]
            self.assertEqual(current['plan_revisions'], number)
            self.author(flow, 'Full revised plan ' + str(number))
        self.review(flow, 'revise')
        terminal = self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual((terminal['phase'], terminal['plan_revisions'], terminal['code_revisions']), ('escalated', 20, 0))
        self.assertIsNone(self.call('claim', 'homelab', fixture.ALIAS))
        self.assertEqual(terminal['snapshot'], w['snapshot'])

    def test_native_review_delivery_corrections_share_the_twenty_attempt_allowance(self):
        root, w = self.legacy_exhaustion()
        flow = self.private_flow()
        flow.continue_after_retry_policy_update(self.command(root, w))
        self.author(flow, 'Full revised plan 4')
        for _ in range(AGENT_RETRY_LIMIT + 1):
            fixture.ExternalFlowPGTest.failed_setup_review(self, flow)
        terminal = self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual((terminal['phase'], terminal['state']['review_retries']), ('escalated', 20))
        self.assertEqual(terminal['plan_revisions'], 4)


if __name__ == '__main__':
    unittest.main()
