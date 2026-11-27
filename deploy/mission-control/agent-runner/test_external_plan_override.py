"""Private operator external plan approve-override tests (pure + disposable PG16)."""
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from dataclasses import fields, replace
import hashlib
import threading
from types import SimpleNamespace
import unittest
from uuid import UUID, uuid4

import psycopg2
from psycopg2.extras import Json

from queue_store import Rejected
from test_work_execution_and_usage import make_stop_proof
import test_external_flow as fixture
from work_api import (
    Destination,
    WorkAccept,
    WorkEvents,
    WorkGet,
    WorkPlanDecide,
    WorkRepair,
    WorkResume,
    WorkReviewDecide,
    WorkServiceFactory,
)
from work_events import COORDINATOR_ACTOR
from work_external import ExternalWorkProviderFactory
from work_external_plan_override import (
    ExternalPlanApproveOverride,
    PlanApproveOverrideGrantScope,
)
from work_external_recovery import NativeReviewerGrantScope
from work_external_review_context import canonical
from work_pg_fixture import PG16_AVAILABLE, SQL_DIR
from work_retry_policy import AGENT_RETRY_LIMIT


def _valid_command(**overrides):
    base = {
        'project_alias': 'homelab',
        'workflow_id': uuid4(),
        'root_task_id': uuid4(),
        'source_review_id': uuid4(),
        'plan_assignment_id': uuid4(),
        'plan_sha256': 'a' * 64,
        'expected_workflow_revision': 42,
        'expected_root_revision': 3,
        'idempotency_key': uuid4(),
        'reason': 'Human owner reviewed and accepted the final plan artifact as-is.',
        'acceptance_ref': 'owner-decision:2026-11-24-001',
        'confirm_accept_as_is': True,
    }
    return ExternalPlanApproveOverride(**{**base, **overrides})


class ExternalPlanOverridePureTest(unittest.TestCase):
    def test_dto_narrowness_and_validation(self):
        names = {f.name for f in fields(ExternalPlanApproveOverride)}
        self.assertFalse(names & {'target_phase', 'target_status', 'state', 'phase', 'status'})

        # confirm_accept_as_is is mandatory (no default) and must be literal True.
        kwargs = {
            'project_alias': 'homelab',
            'workflow_id': uuid4(),
            'root_task_id': uuid4(),
            'source_review_id': uuid4(),
            'plan_assignment_id': uuid4(),
            'plan_sha256': 'b' * 64,
            'expected_workflow_revision': 2,
            'expected_root_revision': 2,
            'idempotency_key': 'override-key-1',
            'reason': 'Accepted as-is.',
            'acceptance_ref': 'ref-1',
        }
        with self.assertRaises(TypeError):
            ExternalPlanApproveOverride(**kwargs)
        for bad_confirm in (False, 1, 'true', None):
            with self.subTest(confirm=bad_confirm), self.assertRaises(ValueError):
                ExternalPlanApproveOverride(**kwargs, confirm_accept_as_is=bad_confirm)

        cmd = _valid_command()
        self.assertIsInstance(cmd.idempotency_key, str)
        self.assertEqual(cmd.intent('owner-1')['action'], 'plan_approve_override')
        self.assertEqual(cmd.intent('owner-1')['owner'], 'owner-1')

        bad_cases = [
            {'project_alias': ''},
            {'project_alias': '../bad'},
            {'workflow_id': 'not-a-uuid'},
            {'root_task_id': 'not-a-uuid'},
            {'source_review_id': 'not-a-uuid'},
            {'plan_assignment_id': 'not-a-uuid'},
            {'plan_sha256': 'A' * 64},
            {'plan_sha256': 'a' * 63},
            {'plan_sha256': 'g' * 64},
            {'expected_workflow_revision': 0},
            {'expected_workflow_revision': True},
            {'expected_workflow_revision': 2**53},
            {'expected_root_revision': 0},
            {'expected_root_revision': False},
            {'idempotency_key': ''},
            {'idempotency_key': 'bad key with spaces'},
            {'idempotency_key': 'x' * 129},
            {'reason': ''},
            {'reason': '   '},
            {'reason': 'line1\nline2'},
            {'reason': 'has\x00nul'},
            {'reason': 'x' * 2049},
            {'acceptance_ref': ''},
            {'acceptance_ref': '  '},
            {'acceptance_ref': 'ref\t1'},
            {'acceptance_ref': 'r' * 257},
        ]
        for bad in bad_cases:
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                _valid_command(**bad)

    def test_wiring_defaults_and_no_http_route(self):
        self.assertNotIn('ide.work.external.plan.approve_override', {d.value for d in Destination})
        with self.assertRaises(TypeError):
            PlanApproveOverrideGrantScope('not-callable')
        with self.assertRaises(TypeError):
            ExternalWorkProviderFactory(fixture.cfg(), plan_override_grant_provider=object())
        default_service = WorkServiceFactory(fixture.cfg(), lambda: None).create()
        self.assertIsNone(default_service._external_workflow._plan_override_grants)
        self.assertIsNone(default_service._external_workflow._recovery_grants)


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class ExternalPlanOverridePGTest(unittest.TestCase):
    setUpClass = classmethod(fixture.ExternalFlowPGTest.setUpClass.__func__)
    connect = classmethod(fixture.ExternalFlowPGTest.connect.__func__)
    call = fixture.ExternalFlowPGTest.call
    rows = fixture.ExternalFlowPGTest.rows
    review = fixture.ExternalFlowPGTest.review
    author = fixture.ExternalFlowPGTest.author
    prepared = fixture.ExternalFlowPGTest.prepared

    def setUp(self):
        fixture.ExternalFlowPGTest.setUp(self)
        self.pg.reset_retry_events()

    def write(self, sql, args=()):
        with closing(self.pg.connect('postgres')) as connection:
            with connection, connection.cursor() as cursor:
                cursor.execute(sql, args)

    def override_service(self, *, config=None, retry_events=True, grants=True, recovery_grants=False):
        cfg = config or fixture.cfg()
        override_scope = (
            PlanApproveOverrideGrantScope(lambda: self.pg.connect('postgres')) if grants else None
        )
        recovery_scope = (
            NativeReviewerGrantScope(lambda: self.pg.connect('postgres'))
            if recovery_grants
            else None
        )
        extra = {} if retry_events else {'retry_event_factory': lambda _: SimpleNamespace(create=lambda: None)}
        return WorkServiceFactory(
            cfg,
            self.connect,
            external_factory=lambda c: ExternalWorkProviderFactory(
                c,
                recovery_grant_provider=recovery_scope,
                plan_override_grant_provider=override_scope,
            ).create(),
            **extra,
        ).create()

    def submit_plan_with_service(self, service, content='# Initial external plan\nStep 1: design.'):
        command = fixture.make_submit(
            agent_profile=fixture.ALIAS, mode='plan', idempotency_key=str(uuid4())
        )
        command = replace(
            command,
            workflow_policy_digest=fixture.cfg().external_workflows['homelab'].summary(
                fixture.cfg().catalog()
            )['policy_digest'],
        )
        receipt = service.submit(command)
        self.call('advertise', 'homelab', fixture.ALIAS, 'owned-muse')
        job = self.call('claim', 'homelab', fixture.ALIAS)
        self.call('finish', job['assignment_id'], job['generation'], job['token'], content, 'Plan ready')
        flow = service._store._external.workflow
        flow.run_once()
        action = self.delivery.claim('owned-delivery')
        self.assertEqual(action.kind, 'source')
        self.delivery.checkpoint(
            action,
            {'status': 'passed', 'receipt': {'base_revision': 'a' * 40}, 'summary': 'Trusted main'},
        )
        flow.run_once()
        return receipt, flow, job

    def exhaust_m31_plan(self, *, service=None):
        service = service or self.override_service()
        root, flow, job = self.submit_plan_with_service(service)
        last_job = job
        for n in range(AGENT_RETRY_LIMIT):
            self.review(flow, 'revise')
            last_job = self.author(
                flow, f'# Revised external plan round {n + 1}\nAccepted architecture detail.'
            )
        # 21st revise verdict triggers exhaustion (used == 20, status == 'exhausted').
        self.review(flow, 'revise')
        w = self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual((w['phase'], w['enabled'], w['plan_revisions'], w['code_revisions']),
                         ('escalated', False, AGENT_RETRY_LIMIT, 0))
        event = self.rows(
            "SELECT * FROM forge_agent.coding_retry_events WHERE scope_id=%s AND kind='planning'",
            (w['workflow_id'],),
        )[0]
        self.assertEqual((event['used'], event['attempt_limit'], event['status']),
                         (AGENT_RETRY_LIMIT, AGENT_RETRY_LIMIT, 'exhausted'))
        return service, root, flow, last_job, w, event

    def make_override_command(self, w, root, **changes):
        root_row = self.rows('SELECT revision FROM forge_agent.coding_tasks WHERE id=%s', (root['task_id'],))[0]
        review = self.rows(
            'SELECT * FROM forge_external_workflow.reviews WHERE review_id=%s',
            (UUID(w['state']['active_review_id']),),
        )[0]
        base = {
            'project_alias': 'homelab',
            'workflow_id': w['workflow_id'],
            'root_task_id': UUID(root['task_id']),
            'source_review_id': review['review_id'],
            'plan_assignment_id': review['source_assignment_id'],
            'plan_sha256': review['source_sha256'],
            'expected_workflow_revision': w['revision'],
            'expected_root_revision': root_row['revision'],
            'idempotency_key': uuid4(),
            'reason': 'Reviewed all 20 plan rounds; accepting final plan artifact as-is.',
            'acceptance_ref': 'owner-acceptance:plan-override-001',
            'confirm_accept_as_is': True,
        }
        return ExternalPlanApproveOverride(**{**base, **changes})

    def test_m31_exhaustion_override_audit_history_and_full_delivery_lifecycle(self):
        service, root, flow, last_job, w, event = self.exhaust_m31_plan()
        cmd = self.make_override_command(w, root)

        # Default HTTP service (no plan_override_grant_provider) and recovery-only service cannot invoke.
        with self.assertRaises(Rejected) as default_denied:
            WorkServiceFactory(fixture.cfg(), self.connect).create().approve_external_plan_override(cmd)
        self.assertEqual((default_denied.exception.status, default_denied.exception.code), (403, 'denied'))

        with self.assertRaises(Rejected) as recovery_only_denied:
            self.override_service(grants=False, recovery_grants=True).approve_external_plan_override(cmd)
        self.assertEqual((recovery_only_denied.exception.status, recovery_only_denied.exception.code), (403, 'denied'))

        # Override-only service cannot cross-invoke infrastructure recovery.
        with self.assertRaises(Rejected) as cross_denied:
            service.recover_external_review_infrastructure(
                fixture.ExternalFlowPGTest.recovery_command(self, w, root)
            )
        self.assertEqual((cross_denied.exception.status, cross_denied.exception.code), (403, 'denied'))

        # Manual portal controls remain forbidden on external workflows.
        root_rev = self.rows('SELECT revision FROM forge_agent.coding_tasks WHERE id=%s', (root['task_id'],))[0]['revision']
        for fn in (
            lambda: service.accept(WorkAccept('homelab', UUID(root['task_id']), root_rev, 'm-accept')),
            lambda: service.resume(WorkResume('homelab', UUID(root['task_id']), root_rev, 'm-resume', 'codex-v2', uuid4())),
            lambda: service.plan_decide(WorkPlanDecide('homelab', UUID(root['task_id']), root_rev, uuid4(), 'a' * 64, 'resume', 'codex-v2', 'm-plan')),
            lambda: service.review_decide(WorkReviewDecide('homelab', UUID(root['task_id']), root_rev, uuid4(), 'approve', 'codex-v2', '', 'm-rev')),
            lambda: service.repair(WorkRepair('homelab', UUID(root['task_id']), root_rev, 'a' * 40, 'codex-v2', '', ({'finding_id': 'f1', 'category': 'source', 'summary': 's'},), 'm-rep')),
        ):
            with self.assertRaises(Rejected) as manual:
                fn()
            self.assertEqual((manual.exception.status, manual.exception.code), (409, 'not_reviewable'))

        # Snapshot immutable history BEFORE override.
        reviews_before = self.rows('SELECT * FROM forge_external_workflow.reviews ORDER BY review_id')
        retry_events_before = self.rows('SELECT * FROM forge_agent.coding_retry_events ORDER BY event_id')
        admissions_before = self.rows('SELECT * FROM forge_agent.coding_retry_event_admissions ORDER BY event_id, ordinal')
        attempts_before = self.rows('SELECT * FROM forge_agent.coding_attempts ORDER BY attempt_id')
        artifacts_before = self.rows('SELECT * FROM forge_external_agent.artifacts ORDER BY assignment_id')
        events_before = self.rows(
            'SELECT event_id, sequence, event_kind, payload FROM forge_agent.coding_events WHERE task_id=%s ORDER BY sequence',
            (root['task_id'],),
        )

        receipt = service.approve_external_plan_override(cmd)
        self.assertEqual(receipt['status'], 'implement_queued')
        self.assertEqual(receipt['prior_phase'], 'escalated')
        self.assertEqual(receipt['phase'], 'implement')
        self.assertEqual(receipt['actor'], {'kind': 'operator', 'id': 'test-owner'})
        self.assertEqual(receipt['reason'], cmd.reason)
        self.assertEqual(receipt['acceptance_ref'], cmd.acceptance_ref)
        self.assertEqual(receipt['plan_assignment_id'], str(cmd.plan_assignment_id))
        self.assertEqual(receipt['plan_sha256'], cmd.plan_sha256)
        self.assertEqual(receipt['prior_review_id'], str(cmd.source_review_id))
        self.assertEqual(receipt['retry_event_id'], str(event['event_id']))
        self.assertEqual(receipt['plan_revisions'], AGENT_RETRY_LIMIT)
        self.assertEqual(receipt['workflow_revision'], w['revision'] + 1)
        self.assertEqual(receipt['root_revision'], root_rev + 1)

        # Immutable history preserved byte-for-byte.
        self.assertEqual(self.rows('SELECT * FROM forge_external_workflow.reviews ORDER BY review_id'), reviews_before)
        self.assertEqual(self.rows('SELECT * FROM forge_agent.coding_retry_events ORDER BY event_id'), retry_events_before)
        self.assertEqual(self.rows('SELECT * FROM forge_agent.coding_retry_event_admissions ORDER BY event_id, ordinal'), admissions_before)
        self.assertEqual(self.rows('SELECT * FROM forge_agent.coding_attempts ORDER BY attempt_id'), attempts_before)
        self.assertEqual(self.rows('SELECT * FROM forge_external_agent.artifacts ORDER BY assignment_id'), artifacts_before)

        # Audit table row exists and matches receipt.
        audit_rows = self.rows('SELECT * FROM forge_external_workflow.plan_approve_overrides')
        self.assertEqual(len(audit_rows), 1)
        audit = audit_rows[0]
        self.assertEqual(str(audit['override_id']), receipt['override_id'])
        self.assertEqual((audit['actor_kind'], audit['actor_id']), ('operator', 'test-owner'))
        self.assertEqual(audit['reason'], cmd.reason)
        self.assertEqual(audit['acceptance_ref'], cmd.acceptance_ref)
        self.assertEqual(audit['plan_sha256'], cmd.plan_sha256)
        self.assertEqual(audit['plan_assignment_id'], cmd.plan_assignment_id)
        self.assertEqual(audit['prior_phase'], 'escalated')
        self.assertEqual(audit['prior_plan_revisions'], AGENT_RETRY_LIMIT)
        self.assertEqual(audit['prior_review_id'], cmd.source_review_id)
        self.assertEqual(audit['retry_event_id'], event['event_id'])
        self.assertEqual(str(audit['implement_task_id']), receipt['implement_task_id'])
        self.assertEqual(str(audit['implement_assignment_id']), receipt['implement_assignment_id'])
        self.assertEqual(audit['receipt'], receipt)

        # Workflow transitioned cleanly.
        after_w = self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual((after_w['phase'], after_w['enabled'], after_w['plan_revisions'], after_w['code_revisions']),
                         ('implement', True, AGENT_RETRY_LIMIT, 0))
        self.assertEqual(str(after_w['active_task_id']), receipt['implement_task_id'])
        self.assertNotIn('escalation_reason', after_w['state'])
        self.assertNotIn('active_review_id', after_w['state'])
        self.assertEqual(set(after_w['state']['approved_plan']), {'assignment_id', 'sha256', 'content'})
        self.assertEqual(after_w['state']['approved_plan']['sha256'], cmd.plan_sha256)
        self.assertEqual(after_w['state']['plan_review']['decision'], 'user_override_approve')
        self.assertEqual(after_w['state']['plan_review']['override_id'], receipt['override_id'])
        self.assertEqual(after_w['state']['plan_review']['prior_review_id'], str(cmd.source_review_id))
        self.assertEqual(after_w['state']['plan_review']['prior_decision'], 'revise')
        self.assertEqual(after_w['state']['plan_approve_override']['implement_assignment_id'], receipt['implement_assignment_id'])

        # Additive events: all prior events untouched + Event N+1 (operator) + Event N+2 (coordinator).
        events_after = self.rows(
            'SELECT event_id, sequence, event_kind, payload FROM forge_agent.coding_events WHERE task_id=%s ORDER BY sequence',
            (root['task_id'],),
        )
        self.assertEqual(events_after[:len(events_before)], events_before)
        added = events_after[len(events_before):]
        self.assertEqual(len(added), 2)
        op_ev, coord_ev = added
        self.assertEqual(op_ev['event_kind'], 'checkpoint')
        self.assertEqual(op_ev['payload']['actor'], {'kind': 'operator', 'id': 'test-owner'})
        self.assertEqual(op_ev['payload']['stage'], 'plan_approve_override')
        self.assertEqual(op_ev['payload']['override_id'], receipt['override_id'])
        self.assertEqual(op_ev['payload']['plan_sha256'], cmd.plan_sha256)
        self.assertEqual(op_ev['payload']['acceptance_ref'], cmd.acceptance_ref)
        self.assertEqual(coord_ev['event_kind'], 'checkpoint')
        self.assertEqual(coord_ev['payload']['actor'], COORDINATOR_ACTOR)
        self.assertEqual(coord_ev['payload']['phase'], 'implement')
        self.assertEqual(coord_ev['payload']['related_task_id'], receipt['implement_task_id'])
        self.assertIn(receipt['override_id'], coord_ev['payload']['summary'])

        # Public events API returns Event N+2 as valid workflow_progress with zero ABI drift.
        public_events = service.events(WorkEvents('homelab', UUID(root['task_id']), 0, 100))
        self.assertEqual(public_events['events'][-1]['workflow_progress'], {
            'workflow_id': str(w['workflow_id']),
            'phase': 'implement',
            'summary': coord_ev['payload']['summary'],
            'related_task_id': receipt['implement_task_id'],
        })
        self.assertNotIn('workflow_progress', public_events['events'][-2])

        # Exact replay returns identical receipt.
        self.assertEqual(service.approve_external_plan_override(cmd), receipt)

        # Muse claim receives canonical external_context with exact approved_plan & base_revision.
        impl_job = self.call('claim', 'homelab', fixture.ALIAS)
        self.assertIsNotNone(impl_job)
        self.assertEqual(impl_job['assignment_id'], receipt['implement_assignment_id'])
        self.assertEqual(impl_job['task_id'], receipt['implement_task_id'])
        self.assertEqual(impl_job['external_context']['stage'], 'implement')
        self.assertEqual(impl_job['external_context']['approved_plan'], after_w['state']['approved_plan'])
        self.assertEqual(impl_job['external_context']['base_revision'], 'a' * 40)
        self.assertIsNone(impl_job['external_context']['feedback'])
        self.assertEqual(
            hashlib.sha256(impl_job['external_context_json'].encode()).hexdigest(),
            impl_job['external_context_sha256'],
        )

        # Replay STILL works after Muse has claimed the implement task.
        self.assertEqual(service.approve_external_plan_override(cmd), receipt)

        # Complete full downstream lifecycle: finish -> prepare -> Codex code_review -> verify -> merge -> completed.
        manifest = canonical({
            'version': 1,
            'candidate_commit': 'b' * 40,
            'candidate_tree': 'd' * 40,
            'base_revision': 'a' * 40,
            'summary': 'Implementation of operator-approved plan.',
        }).decode()
        self.call('finish', impl_job['assignment_id'], impl_job['generation'], impl_job['token'], manifest, 'Candidate ready')
        self.assertTrue(flow.run_once())
        candidate = self.prepared(flow)
        code_target = self.review(flow, 'approve')
        self.assertEqual(code_target['target_kind'], 'code')
        self.assertEqual(code_target['approved_plan'], after_w['state']['approved_plan'])

        self.assertTrue(flow.run_once())
        verify_action = self.delivery.claim('owned-delivery')
        self.assertEqual(verify_action.kind, 'verify')
        self.assertEqual(verify_action.request['plan_review']['decision'], 'user_override_approve')
        self.assertEqual(verify_action.request['plan_review']['override_id'], receipt['override_id'])

        # Use V2-compatible verify receipt shape (with owned-tests profile matching candidate).
        verified_receipt = {
            'version': 1,
            'provider': 'jenkins',
            'action_id': verify_action.action_id,
            'workflow_id': str(w['workflow_id']),
            'project_alias': 'homelab',
            'candidate_commit': candidate['candidate_commit'],
            'candidate_tree': candidate['candidate_tree'],
            'expected_main': candidate['integration_base'],
            'gate_manifest_sha256': candidate['gate_manifest_sha256'],
            'branch_scope': candidate['branch_scope'],
            'verification_profile': candidate['verification_profile'],
            'cleanup_confirmed': True,
            'check_receipt_sha256': '2' * 64,
            'result': 'SUCCESS',
            'build_id': 77,
        }
        # Re-check exact version expected by CiGateContractFactory for 'owned-tests':
        # use LEGACY_PROFILE in candidate so _verified_stage_observation accepts version 1.
        self.write(
            "UPDATE forge_external_workflow.workflows SET state=jsonb_set(state,'{candidate,verification_profile}','\"forge-sdlc-jenkins-v1\"'::jsonb) WHERE workflow_id=%s",
            (w['workflow_id'],),
        )
        verified_receipt['verification_profile'] = 'forge-sdlc-jenkins-v1'
        self.delivery.checkpoint(
            verify_action,
            {'status': 'passed', 'receipt': verified_receipt, 'summary': 'Exact source checks passed'},
        )
        self.assertTrue(flow.run_once())
        self.assertTrue(flow.run_once())
        merge_action = self.delivery.claim('owned-delivery')
        self.assertEqual(merge_action.kind, 'merge')
        self.assertEqual(merge_action.request['plan_review']['decision'], 'user_override_approve')
        self.delivery.checkpoint(
            merge_action,
            {
                'status': 'merged',
                'receipt': {
                    'merge_commit': candidate['candidate_commit'],
                    'candidate_tree': candidate['candidate_tree'],
                    'expected_main': candidate['integration_base'],
                },
                'summary': 'Merged onto main',
            },
        )
        self.assertTrue(flow.run_once())
        final_w = self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual(final_w['phase'], 'completed')
        self.assertFalse(final_w['enabled'])
        self.assertEqual(service.get(WorkGet('homelab', UUID(root['task_id'])))['status'], 'completed')
        # Planning retry event STILL remains exhausted (never reset or resolved).
        self.assertEqual(
            self.rows('SELECT used, status FROM forge_agent.coding_retry_events WHERE event_id=%s', (event['event_id'],))[0],
            {'used': AGENT_RETRY_LIMIT, 'status': 'exhausted'},
        )

    def test_adopted_legacy_exhaustion_succeeds_and_preserves_history(self):
        # Simulate pre-M31 3 rounds adopted via retry_event_adopt_legacy, then 17 more rounds to reach 20.
        legacy_service = self.override_service(retry_events=False)
        root, legacy_flow, _ = self.submit_plan_with_service(legacy_service)
        for n in range(3):
            self.review(legacy_flow, 'revise')
            self.author(legacy_flow, f'# Legacy plan round {n + 1}')
        # Switch to M31 service and adopt legacy_used = 3.
        service = self.override_service(retry_events=True)
        flow = service._store._external.workflow
        w = self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        with flow._transaction() as cursor:
            ev = flow._retry_events.observe_failure(
                cursor, flow._retry_scope(w), 'planning', 'legacy:seed', 'e' * 64, phase_id='planning'
            )
            flow._retry_events.adopt_legacy(cursor, ev['event_id'], 3, 'legacy:seed', 'e' * 64)
        # Drive 17 more revisions (3 + 17 = 20 admissions) then 1 exhausting revise verdict.
        for n in range(17):
            self.review(flow, 'revise')
            self.author(flow, f'# Adopted M31 plan round {n + 4}')
        self.review(flow, 'revise')
        w = self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        event = self.rows(
            "SELECT * FROM forge_agent.coding_retry_events WHERE scope_id=%s AND kind='planning'",
            (w['workflow_id'],),
        )[0]
        self.assertEqual((event['legacy_used'], event['used'], event['status']), (3, 20, 'exhausted'))
        self.assertEqual(
            self.rows('SELECT count(*) AS n FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s', (event['event_id'],))[0]['n'],
            17,
        )
        cmd = self.make_override_command(w, root)
        receipt = service.approve_external_plan_override(cmd)
        self.assertEqual(receipt['status'], 'implement_queued')
        self.assertEqual(receipt['retry_event_id'], str(event['event_id']))
        self.assertEqual(
            self.rows('SELECT legacy_used, used, status FROM forge_agent.coding_retry_events WHERE event_id=%s', (event['event_id'],))[0],
            {'legacy_used': 3, 'used': 20, 'status': 'exhausted'},
        )

    def test_pre_m31_legacy_budget_exhaustion_succeeds_with_null_retry_event_id(self):
        service = self.override_service(retry_events=False)
        root, flow, _ = self.submit_plan_with_service(service)
        for n in range(AGENT_RETRY_LIMIT):
            self.review(flow, 'revise')
            self.author(flow, f'# Legacy-only plan round {n + 1}')
        self.review(flow, 'revise')
        w = self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual(w['phase'], 'escalated')
        self.assertTrue(w['state']['escalation_reason'].startswith('The plan revision budget is exhausted. '))
        self.assertEqual(self.rows('SELECT count(*) AS n FROM forge_agent.coding_retry_events')[0]['n'], 0)

        cmd = self.make_override_command(w, root)
        receipt = service.approve_external_plan_override(cmd)
        self.assertEqual(receipt['status'], 'implement_queued')
        self.assertIsNone(receipt['retry_event_id'])
        audit = self.rows('SELECT * FROM forge_external_workflow.plan_approve_overrides')[0]
        self.assertIsNone(audit['retry_event_id'])
        self.assertIsNotNone(self.call('claim', 'homelab', fixture.ALIAS))

    def test_idempotency_conflict_one_shot_and_concurrency_races(self):
        # 1. Same-key concurrent race returns identical receipt and queues exactly one implement task.
        service, root, flow, _, w, _ = self.exhaust_m31_plan()
        cmd = self.make_override_command(w, root)
        barrier = threading.Barrier(2)

        def invoke(command):
            barrier.wait()
            return service.approve_external_plan_override(command)

        with ThreadPoolExecutor(max_workers=2) as pool:
            receipts = list(pool.map(invoke, [cmd, cmd]))
        self.assertEqual(receipts[0], receipts[1])
        self.assertEqual(self.rows('SELECT count(*) AS n FROM forge_external_workflow.plan_approve_overrides')[0]['n'], 1)
        self.assertEqual(
            self.rows("SELECT count(*) AS n FROM forge_agent.coding_tasks WHERE mode='implement'")[0]['n'], 1
        )

        # 2. Same idempotency_key with different intent raises 409 idempotency_conflict.
        with self.assertRaises(Rejected) as conflict:
            service.approve_external_plan_override(replace(cmd, reason='Different reason with same key.'))
        self.assertEqual((conflict.exception.status, conflict.exception.code), (409, 'idempotency_conflict'))

        # 3. Different idempotency_key on already-overridden workflow raises 409 not_reviewable.
        with self.assertRaises(Rejected) as second:
            service.approve_external_plan_override(replace(cmd, idempotency_key=uuid4()))
        self.assertEqual((second.exception.status, second.exception.code), (409, 'not_reviewable'))

        # 4. DB UNIQUE(workflow_id) rejects a second audit row directly.
        audit = self.rows('SELECT * FROM forge_external_workflow.plan_approve_overrides')[0]
        with closing(self.pg.connect('forge_agent')) as conn:
            with conn.cursor() as cur:
                with self.assertRaises(psycopg2.errors.UniqueViolation):
                    cur.execute(
                        '''INSERT INTO forge_external_workflow.plan_approve_overrides(
                               owner_id, project_alias, workflow_id, root_task_id,
                               actor_kind, actor_id, reason, acceptance_ref,
                               plan_task_id, plan_assignment_id, plan_sha256,
                               prior_workflow_revision, prior_root_revision, prior_phase,
                               prior_plan_revisions, prior_review_id, prior_escalation_reason,
                               retry_event_id, idempotency_key, intent_digest,
                               implement_task_id, implement_assignment_id, receipt
                           ) VALUES (
                               %s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s
                           )''',
                        (
                            audit['owner_id'], audit['project_alias'], audit['workflow_id'], audit['root_task_id'],
                            'operator', 'test-owner', 'Second row', 'ref-2',
                            audit['plan_task_id'], audit['plan_assignment_id'], audit['plan_sha256'],
                            audit['prior_workflow_revision'], audit['prior_root_revision'], 'escalated',
                            audit['prior_plan_revisions'], audit['prior_review_id'], audit['prior_escalation_reason'],
                            audit['retry_event_id'], 'second-key', '1' * 64,
                            audit['implement_task_id'], audit['implement_assignment_id'], Json({}),
                        ),
                    )
            conn.rollback()

        # 5. Distinct-key concurrent race executes exactly one override and rejects the loser.
        self.setUp()
        service, root, flow, _, w, _ = self.exhaust_m31_plan()
        cmd1 = self.make_override_command(w, root, idempotency_key=uuid4())
        cmd2 = self.make_override_command(w, root, idempotency_key=uuid4())
        barrier2 = threading.Barrier(2)

        def invoke_catch(command):
            barrier2.wait()
            try:
                return ('ok', service.approve_external_plan_override(command))
            except Rejected as err:
                return ('rejected', (err.status, err.code))

        with ThreadPoolExecutor(max_workers=2) as pool:
            outcomes = list(pool.map(invoke_catch, [cmd1, cmd2]))
        oks = [item for item in outcomes if item[0] == 'ok']
        rejs = [item for item in outcomes if item[0] == 'rejected']
        self.assertEqual(len(oks), 1)
        self.assertEqual(len(rejs), 1)
        self.assertEqual(rejs[0][1], (409, 'not_reviewable'))

    def test_narrowness_forbidden_state_matrix(self):
        # A. Reviewer 'escalate' verdict is rejected.
        service = self.override_service()
        root, flow, _ = self.submit_plan_with_service(service)
        self.review(flow, 'escalate', summary='Scope violation requires escalation.')
        w = self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual(w['phase'], 'escalated')
        cmd = self.make_override_command(w, root)
        with self.assertRaises(Rejected) as exc:
            service.approve_external_plan_override(cmd)
        self.assertEqual((exc.exception.status, exc.exception.code), (409, 'not_reviewable'))

        # B. Reviewer-correction exhaustion is rejected.
        self.setUp()
        service = self.override_service()
        root, flow, _ = self.submit_plan_with_service(service)
        for _ in range(AGENT_RETRY_LIMIT + 1):
            lease = self.db.claim('review-worker', self.inc, ['codex-v2'])
            proof = make_stop_proof(lease.unit_name)
            self.db.publish(lease, 'failed', None, None, 'The coding agent returned malformed machine output.', proof)
            self.db.reconcile(lease, 'b' * 64, proof)
            self.assertTrue(flow.run_once())
            self.pg.admin_exec("UPDATE forge_external_workflow.workflows SET state=jsonb_set(state,'{review_retry_wait,not_before}',to_jsonb((clock_timestamp()-interval '1 second')::text))")
            self.assertTrue(flow.run_once())
        w = self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual(w['phase'], 'escalated')
        cmd = self.make_override_command(w, root)
        with self.assertRaises(Rejected) as exc:
            service.approve_external_plan_override(cmd)
        self.assertEqual((exc.exception.status, exc.exception.code), (409, 'not_reviewable'))

        # C. Completed workflow (via normal approval + merge, no override) is rejected.
        self.setUp()
        service = self.override_service(retry_events=False)
        root, flow, job = self.submit_plan_with_service(service)
        plan_review = self.review(flow, 'approve')
        manifest = canonical({
            'version': 1,
            'candidate_commit': 'b' * 40,
            'candidate_tree': 'd' * 40,
            'base_revision': 'a' * 40,
            'summary': 'Normal implementation.',
        }).decode()
        self.author(flow, manifest)
        candidate = self.prepared(flow)
        self.review(flow, 'approve')
        flow.run_once()
        verify_action = self.delivery.claim('owned-delivery')
        verified = {
            **{k: candidate[k] for k in ('candidate_commit', 'candidate_tree', 'gate_manifest_sha256', 'branch_scope')},
            'result': 'SUCCESS',
        }
        self.delivery.checkpoint(verify_action, {'status': 'passed', 'receipt': verified, 'summary': 'Passed'})
        flow.run_once()
        flow.run_once()
        merge_action = self.delivery.claim('owned-delivery')
        self.delivery.checkpoint(
            merge_action,
            {
                'status': 'merged',
                'receipt': {
                    'merge_commit': candidate['candidate_commit'],
                    'candidate_tree': candidate['candidate_tree'],
                    'expected_main': candidate['integration_base'],
                },
                'summary': 'Merged',
            },
        )
        flow.run_once()
        completed_w = self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual(completed_w['phase'], 'completed')
        completed_root_rev = self.rows(
            'SELECT revision FROM forge_agent.coding_tasks WHERE id=%s', (root['task_id'],)
        )[0]['revision']
        completed_cmd = ExternalPlanApproveOverride(
            project_alias='homelab',
            workflow_id=completed_w['workflow_id'],
            root_task_id=UUID(root['task_id']),
            source_review_id=UUID(plan_review['review_id']),
            plan_assignment_id=UUID(plan_review['source_assignment_id']),
            plan_sha256=plan_review['source_sha256'],
            expected_workflow_revision=completed_w['revision'],
            expected_root_revision=completed_root_rev,
            idempotency_key=uuid4(),
            reason='Should fail on completed workflow.',
            acceptance_ref='ref-completed',
            confirm_accept_as_is=True,
        )
        with self.assertRaises(Rejected) as exc:
            service.approve_external_plan_override(completed_cmd)
        self.assertEqual((exc.exception.status, exc.exception.code), (409, 'not_reviewable'))

        # D. Mutations on a single exhausted M31 workflow must all be rejected cleanly.
        self.setUp()
        service, root, flow, _, w, event = self.exhaust_m31_plan()
        base_cmd = self.make_override_command(w, root)
        original_state = dict(w['state'])
        obs_row = self.rows(
            'SELECT source_digest FROM forge_agent.coding_retry_event_observations '
            'WHERE event_id=%s AND source_id=%s',
            (event['event_id'], 'review:' + str(base_cmd.source_review_id)),
        )[0]
        original_obs_digest = obs_row['source_digest']
        latest_attempt_id = self.rows(
            'SELECT max(attempt_id::text)::uuid AS id FROM forge_agent.coding_attempts'
        )[0]['id']

        cases = (
            'wrong_phase_plan',
            'wrong_phase_plan_review',
            'wrong_phase_plan_revision',
            'wrong_phase_implement',
            'wrong_phase_code_review',
            'wrong_phase_checks',
            'wrong_phase_cancelled',
            'user_cancelled_receipt',
            'code_stage_escalation',
            'approved_plan_present',
            'mismatched_plan_sha256',
            'mismatched_plan_assignment_id',
            'mismatched_source_review_id',
            'stale_workflow_revision',
            'stale_root_revision',
            'revoked_scope_grant',
            'disabled_author_profile',
            'disabled_reviewer_profile',
            'missing_worker_review_grant',
            'active_workflow_lease',
            'active_child_task',
            'active_attempt',
            'active_delivery_action',
            'unexhausted_retry_event',
            'mismatched_observation_digest',
            'missing_admission_row',
        )
        for case in cases:
            with self.subTest(case=case):
                cmd = base_cmd
                target_service = service
                cleanup = None
                try:
                    if case.startswith('wrong_phase_'):
                        phase = case.removeprefix('wrong_phase_')
                        self.write(
                            'UPDATE forge_external_workflow.workflows SET phase=%s WHERE workflow_id=%s',
                            (phase, w['workflow_id']),
                        )
                        cleanup = lambda: self.write(
                            "UPDATE forge_external_workflow.workflows SET phase='escalated' WHERE workflow_id=%s",
                            (w['workflow_id'],),
                        )
                    elif case == 'user_cancelled_receipt':
                        self.write(
                            "INSERT INTO forge_agent.coding_control_receipts(owner_id,project_alias,task_id,action,idempotency_key,intent_digest,base_revision,result_revision,receipt_json) "
                            "VALUES('test-owner','homelab',%s,'cancel','user-cancel',%s,1,2,'{}'::jsonb)",
                            (root['task_id'], '1' * 64),
                        )
                        cleanup = lambda: self.write(
                            "DELETE FROM forge_agent.coding_control_receipts WHERE task_id=%s AND idempotency_key='user-cancel'",
                            (root['task_id'],),
                        )
                    elif case == 'code_stage_escalation':
                        self.write(
                            'UPDATE forge_external_workflow.workflows SET code_revisions=1 WHERE workflow_id=%s',
                            (w['workflow_id'],),
                        )
                        cleanup = lambda: self.write(
                            'UPDATE forge_external_workflow.workflows SET code_revisions=0 WHERE workflow_id=%s',
                            (w['workflow_id'],),
                        )
                    elif case == 'approved_plan_present':
                        self.write(
                            "UPDATE forge_external_workflow.workflows SET state=jsonb_set(state,'{approved_plan}','{\"x\":1}'::jsonb) WHERE workflow_id=%s",
                            (w['workflow_id'],),
                        )
                        cleanup = lambda: self.write(
                            'UPDATE forge_external_workflow.workflows SET state=%s WHERE workflow_id=%s',
                            (Json(original_state), w['workflow_id']),
                        )
                    elif case == 'mismatched_plan_sha256':
                        cmd = replace(base_cmd, plan_sha256='0' * 64)
                    elif case == 'mismatched_plan_assignment_id':
                        cmd = replace(base_cmd, plan_assignment_id=uuid4())
                    elif case == 'mismatched_source_review_id':
                        cmd = replace(base_cmd, source_review_id=uuid4())
                    elif case == 'stale_workflow_revision':
                        cmd = replace(base_cmd, expected_workflow_revision=w['revision'] + 1)
                    elif case == 'stale_root_revision':
                        cmd = replace(base_cmd, expected_root_revision=1)
                    elif case == 'revoked_scope_grant':
                        self.write('UPDATE forge_external_agent.scope_grants SET enabled=false')
                        cleanup = lambda: self.write('UPDATE forge_external_agent.scope_grants SET enabled=true')
                    elif case in ('disabled_author_profile', 'disabled_reviewer_profile'):
                        profile_name = fixture.ALIAS if case == 'disabled_author_profile' else 'codex-v2'
                        orig_cfg = flow.config
                        disabled_profiles = {
                            **orig_cfg.profiles,
                            profile_name: replace(orig_cfg.profiles[profile_name], execution_enabled=False),
                        }
                        flow.config = SimpleNamespace(profiles=disabled_profiles)
                        cleanup = lambda orig=orig_cfg: setattr(flow, 'config', orig)
                    elif case == 'missing_worker_review_grant':
                        self.write('DELETE FROM forge_agent.coding_worker_grants')
                        cleanup = lambda: self.pg.grant_worker('test-owner', 'homelab', 'codex-v2')
                    elif case == 'active_workflow_lease':
                        self.write(
                            "UPDATE forge_external_workflow.workflows SET lease_token=gen_random_uuid(), lease_until=clock_timestamp()+interval '1 hour' WHERE workflow_id=%s",
                            (w['workflow_id'],),
                        )
                        cleanup = lambda: self.write(
                            'UPDATE forge_external_workflow.workflows SET lease_token=NULL, lease_until=NULL WHERE workflow_id=%s',
                            (w['workflow_id'],),
                        )
                    elif case == 'active_child_task':
                        extra_task_id = uuid4()
                        self.write(
                            "INSERT INTO forge_agent.coding_tasks(owner_id,project_alias,id,idempotency_key,intent_digest,base_revision,mode,agent_profile,title,prompt,acceptance_criteria,pinned_recipe,recipe_digest,status,revision,auto_repair,review_required) "
                            "SELECT owner_id,project_alias,%s,%s,%s,base_revision,mode,agent_profile,title,prompt,acceptance_criteria,pinned_recipe,recipe_digest,'queued',1,false,false "
                            "FROM forge_agent.coding_tasks WHERE id=%s",
                            (extra_task_id, extra_task_id, '3' * 64, w['active_task_id']),
                        )
                        self.write(
                            "INSERT INTO forge_external_workflow.tasks(owner_id,project_alias,task_id,workflow_id,role) VALUES('test-owner','homelab',%s,%s,'author')",
                            (extra_task_id, w['workflow_id']),
                        )
                        cleanup = lambda tid=extra_task_id: self.write(
                            "ALTER TABLE forge_external_workflow.tasks DISABLE TRIGGER external_task_binding; "
                            "DELETE FROM forge_external_workflow.tasks WHERE task_id=%s; "
                            "ALTER TABLE forge_external_workflow.tasks ENABLE TRIGGER external_task_binding; "
                            "DELETE FROM forge_agent.coding_tasks WHERE id=%s",
                            (tid, tid),
                        )
                    elif case == 'active_attempt':
                        self.write(
                            "UPDATE forge_agent.coding_attempts SET lease_expires_at=clock_timestamp()+interval '1 hour' WHERE attempt_id=%s",
                            (latest_attempt_id,),
                        )
                        cleanup = lambda: self.write(
                            'UPDATE forge_agent.coding_attempts SET lease_expires_at=NULL WHERE attempt_id=%s',
                            (latest_attempt_id,),
                        )
                    elif case == 'active_delivery_action':
                        self.write(
                            "INSERT INTO forge_external_workflow.actions(workflow_id,owner_id,project_alias,workflow_revision,kind,source_key,intent,status) "
                            "VALUES(%s,'test-owner','homelab',%s,'source','extra-active','{}'::jsonb,'queued')",
                            (w['workflow_id'], w['revision']),
                        )
                        cleanup = lambda: self.write(
                            "ALTER TABLE forge_external_workflow.actions DISABLE TRIGGER external_action_binding; "
                            "DELETE FROM forge_external_workflow.actions WHERE workflow_id=%s AND source_key='extra-active'; "
                            "ALTER TABLE forge_external_workflow.actions ENABLE TRIGGER external_action_binding",
                            (w['workflow_id'],),
                        )
                    elif case == 'unexhausted_retry_event':
                        self.write(
                            "UPDATE forge_agent.coding_retry_events SET used=19, status='open' WHERE event_id=%s",
                            (event['event_id'],),
                        )
                        cleanup = lambda: self.write(
                            "UPDATE forge_agent.coding_retry_events SET used=20, status='exhausted' WHERE event_id=%s",
                            (event['event_id'],),
                        )
                    elif case == 'mismatched_observation_digest':
                        self.write(
                            'UPDATE forge_agent.coding_retry_event_observations SET source_digest=%s WHERE event_id=%s AND source_id=%s',
                            ('0' * 64, event['event_id'], 'review:' + str(base_cmd.source_review_id)),
                        )
                        cleanup = lambda: self.write(
                            'UPDATE forge_agent.coding_retry_event_observations SET source_digest=%s WHERE event_id=%s AND source_id=%s',
                            (original_obs_digest, event['event_id'], 'review:' + str(base_cmd.source_review_id)),
                        )
                    elif case == 'missing_admission_row':
                        self.write(
                            'DELETE FROM forge_agent.coding_retry_event_admissions WHERE event_id=%s AND ordinal=20',
                            (event['event_id'],),
                        )

                    with self.assertRaises(Rejected):
                        target_service.approve_external_plan_override(cmd)
                    self.assertEqual(
                        self.rows('SELECT count(*) AS n FROM forge_external_workflow.plan_approve_overrides')[0]['n'],
                        0,
                    )
                finally:
                    if cleanup is not None:
                        cleanup()

    def test_immutability_trigger_role_isolation_and_sql_verifiers(self):
        service, root, _, _, w, _ = self.exhaust_m31_plan()
        cmd = self.make_override_command(w, root)
        receipt = service.approve_external_plan_override(cmd)

        # UPDATE and DELETE on plan_approve_overrides raise P0009 even as postgres superuser.
        with closing(self.pg.connect('postgres')) as conn:
            with conn.cursor() as cur:
                with self.assertRaises(psycopg2.Error) as upd:
                    cur.execute(
                        "UPDATE forge_external_workflow.plan_approve_overrides SET reason='tampered' WHERE override_id=%s",
                        (UUID(receipt['override_id']),),
                    )
                self.assertEqual(upd.exception.pgcode, 'P0009')
            conn.rollback()
            with conn.cursor() as cur:
                with self.assertRaises(psycopg2.Error) as dele:
                    cur.execute(
                        'DELETE FROM forge_external_workflow.plan_approve_overrides WHERE override_id=%s',
                        (UUID(receipt['override_id']),),
                    )
                self.assertEqual(dele.exception.pgcode, 'P0009')
            conn.rollback()

        # forge_agent has SELECT + INSERT only (no UPDATE, no DELETE).
        with closing(self.pg.connect('forge_agent')) as conn:
            with conn.cursor() as cur:
                with self.assertRaises(psycopg2.errors.InsufficientPrivilege):
                    cur.execute("UPDATE forge_external_workflow.plan_approve_overrides SET reason='x'")
            conn.rollback()
            with conn.cursor() as cur:
                with self.assertRaises(psycopg2.errors.InsufficientPrivilege):
                    cur.execute('DELETE FROM forge_external_workflow.plan_approve_overrides')
            conn.rollback()

        # muse_events, forge_coding_worker, and forge_coding_reconciler have zero privileges.
        for role in ('muse_events', 'forge_coding_worker', 'forge_coding_reconciler'):
            for priv in ('SELECT', 'INSERT', 'UPDATE', 'DELETE', 'TRUNCATE', 'REFERENCES', 'TRIGGER'):
                with self.subTest(role=role, priv=priv):
                    allowed = self.rows(
                        "SELECT has_table_privilege(%s, 'forge_external_workflow.plan_approve_overrides', %s) AS ok",
                        (role, priv),
                    )[0]['ok']
                    self.assertFalse(allowed)

        # All management verifiers pass cleanly.
        for script, user in (
            ('management/verify-external-workflows.sql', 'postgres'),
            ('management/verify.sql', 'postgres'),
            ('management/verify-runner-privileges.sql', 'forge_agent'),
        ):
            res = self.pg.run_sql_script(SQL_DIR / script, user=user)
            self.assertEqual(res.returncode, 0, f'{script} failed: {res.stdout} {res.stderr}')


if __name__ == '__main__':
    unittest.main()
