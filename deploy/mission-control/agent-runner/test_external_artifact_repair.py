"""Permanent source guard failures return to the same author through real events."""
from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
import py_compile
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock, patch
from uuid import uuid4

import test_external_flow as flow_fixture
import test_work_external_delivery as git_fixture
import work_external_delivery as delivery
from work_api import WorkServiceFactory
from work_external_review_context import canonical
from work_external_delivery_store import ExternalDeliveryLease
from work_pg_fixture import PG16_AVAILABLE
from work_publication_clients import PublicationError


CACHE_PATHS = tuple('deploy/mission-control/agent-runner/__pycache__/' + name + '.cpython-312.pyc'
                    for name in ('forge_pg_proxy', 'proxy_statements'))


def rejected(request):
    return delivery.SourceArtifactRejected(request['candidate_commit'], request['candidate_tree'],
        request['candidate_tree'], [(path, 'private_or_generated_artifact') for path in CACHE_PATHS], 2)


def committed_cache_fixture(root):
    fixture = git_fixture.LocalGitSanitizerTest()
    config, author, remote, repo, base, _, port = fixture._setup_repo_fixture(root)
    for path in CACHE_PATHS:
        target = author / path
        target.parent.mkdir(parents=True, exist_ok=True)
        source = author / (Path(path).name.split('.')[0] + '.py')
        source.write_text('def example(): return 1\n')
        py_compile.compile(str(source), cfile=str(target), dfile=source.name, doraise=True)
    fixture.git(author, 'add', '-f', '.')
    fixture.git(author, 'commit', '--quiet', '-m', 'Owned reproduction with two tracked caches')
    commit = fixture.git(author, 'rev-parse', 'HEAD').decode().strip()
    tree = fixture.git(author, 'rev-parse', 'HEAD^{tree}').decode().strip()
    fixture.git(author, 'push', '--quiet', str(remote), 'HEAD:refs/heads/feature/muse')
    return fixture, config, author, base, commit, tree, port


class ArtifactRejectionTests(unittest.TestCase):
    def test_real_two_tracked_pyc_files_fail_once_with_both_paths_and_no_prepared_candidate(self):
        with TemporaryDirectory(prefix='forge-rejected-source-') as temporary:
            fixture, config, author, base, commit, tree, port = committed_cache_fixture(temporary)
            request = {**git_fixture.identity_request(), 'base_revision': base,
                'candidate_commit': commit, 'candidate_tree': tree, 'source_branch': 'feature/muse'}
            lease = ExternalDeliveryLease(request['action_id'], request['workflow_id'], 'homelab', 'prepare',
                1, str(uuid4()), '2099-01-01T00:00:00Z', request)
            dao = Mock(); dao.claim.return_value = lease; dao.renew.return_value = True
            self.assertTrue(delivery.ExternalDeliveryWorker(dao, port, 'owned-worker').run_once())
            result = dao.checkpoint.call_args.args[1]
            self.assertEqual(result['status'], 'failed')
            self.assertEqual(result['receipt']['reason'], 'source_artifact_rejected')
            self.assertEqual(result['receipt']['candidate_commit'], commit)
            self.assertEqual(result['receipt']['candidate_tree'], tree)
            self.assertEqual(result['receipt']['rejected_count'], 2)
            self.assertEqual({row['path'] for row in result['receipt']['rejections']}, set(CACHE_PATHS))
            self.assertTrue(all(row['code'] == 'private_or_generated_artifact' for row in result['receipt']['rejections']))
            for path in CACHE_PATHS:self.assertIn(path, result['summary'])
            directory = config.state_root / 'actions' / lease.action_id
            self.assertFalse((directory / 'prepared.json').exists())
            self.assertFalse((directory / 'candidate.bundle').exists())
            self.assertEqual(fixture.git(author, 'rev-parse', 'HEAD').decode().strip(), commit)
            self.assertEqual(fixture.git(author, 'status', '--porcelain'), b'')
            port.jenkins.submit.assert_not_called()

    def test_transient_or_unrelated_publication_errors_stay_pending_and_wrong_scope_is_rejected(self):
        request = git_fixture.identity_request()
        lease = ExternalDeliveryLease(request['action_id'], request['workflow_id'], 'homelab', 'prepare',
            1, str(uuid4()), '2099-01-01T00:00:00Z', request)
        for error in (delivery.ObservationUnavailable('offline'), PublicationError('publication_command_timeout'),
                      PublicationError('publication_command_failed')):
            with self.subTest(error=type(error).__name__):
                dao = Mock(); dao.claim.return_value = lease; dao.renew.return_value = True
                port = Mock(); port.prepare_candidate.side_effect = error
                self.assertTrue(delivery.ExternalDeliveryWorker(dao, port, 'owned-worker').run_once())
                self.assertEqual(dao.checkpoint.call_args.args[1]['status'], 'pending')
        for field in ('candidate_commit', 'candidate_tree'):
            dao = Mock(); dao.claim.return_value = lease; dao.renew.return_value = True
            port = Mock(); port.prepare_candidate.side_effect = rejected({**request, field: '0' * 40})
            with self.subTest(field=field), self.assertRaises(ValueError):
                delivery.ExternalDeliveryWorker(dao, port, 'owned-worker').run_once()
            dao.checkpoint.assert_not_called()

    def test_structured_diagnostics_are_bounded_and_cannot_contain_raw_artifact_bytes(self):
        request = git_fixture.identity_request()
        paths = [('界' * 250 + str(i), 'private_key_artifact') for i in range(32)]
        result = delivery.SourceArtifactRejected(request['candidate_commit'], request['candidate_tree'],
            request['candidate_tree'], paths, 100).result()
        self.assertLess(len(result['summary'].encode()), 4096)
        self.assertLess(len(json.dumps(result).encode()), 65536)
        self.assertEqual(result['receipt']['rejected_count'], 100)
        self.assertEqual(len(result['receipt']['rejections']), 32)
        self.assertEqual(set(result['receipt']['rejections'][0]), {'path', 'code'})
        for code in delivery.ARTIFACT_REJECTION_CODES:
            self.assertEqual(rejected(request).code, 'source_artifact_rejected')
            delivery.SourceArtifactRejected(request['candidate_commit'], request['candidate_tree'],
                request['candidate_tree'], [('owned.txt', code)], 1)
        with self.assertRaises(ValueError):
            delivery.SourceArtifactRejected(request['candidate_commit'], request['candidate_tree'],
                request['candidate_tree'], [('../outside', 'private_key_artifact')], 1)


@unittest.skipUnless(PG16_AVAILABLE, 'Requires owned PostgreSQL16')
class ArtifactRepairEventPGTests(unittest.TestCase):
    call = flow_fixture.ExternalFlowPGTest.call
    rows = flow_fixture.ExternalFlowPGTest.rows
    review = flow_fixture.ExternalFlowPGTest.review
    author = flow_fixture.ExternalFlowPGTest.author
    submit_plan = flow_fixture.ExternalFlowPGTest.submit_plan

    @classmethod
    def setUpClass(cls):flow_fixture.ExternalFlowPGTest.setUpClass.__func__(cls)
    @classmethod
    def connect(cls):return cls.pg.connect('forge_agent')
    def setUp(self):
        flow_fixture.ExternalFlowPGTest.setUp(self)
        self.pg.admin_exec('TRUNCATE forge_agent.coding_retry_events CASCADE')
    def service(self):return WorkServiceFactory(flow_fixture.cfg(), self.connect).create()

    def plan_at(self, base):
        service = self.service()
        command = replace(flow_fixture.make_submit(agent_profile=flow_fixture.ALIAS, mode='plan',
            idempotency_key=str(uuid4())), base_revision=base,
            workflow_policy_digest=flow_fixture.cfg().external_workflows['homelab'].summary(flow_fixture.cfg().catalog())['policy_digest'])
        root = service.submit(command)
        self.call('advertise', 'homelab', flow_fixture.ALIAS, 'owned-muse')
        job = self.call('claim', 'homelab', flow_fixture.ALIAS)
        self.call('finish', job['assignment_id'], job['generation'], job['token'], 'Approved retained Plan.', 'Plan returned')
        flow = service._store._external.workflow; flow.run_once()
        source = self.delivery.claim('owned-delivery')
        self.delivery.checkpoint(source, {'status': 'passed', 'receipt': {'base_revision': base}, 'summary': 'Trusted local source'})
        flow.run_once(); self.review(flow)
        return root, flow

    def test_real_git_rejection_checkpoint_emits_one_same_owner_repair_with_plan_and_paths(self):
        with TemporaryDirectory(prefix='forge-rejected-event-') as temporary:
            _, config, _, base, commit, tree, port = committed_cache_fixture(temporary)
            root, flow = self.plan_at(base)
            manifest = canonical({'version': 1, 'candidate_commit': commit, 'candidate_tree': tree,
                'base_revision': base, 'source_branch': 'feature/muse', 'summary': 'Retained feature implementation'}).decode()
            source = self.author(flow, manifest)
            worker = delivery.ExternalDeliveryWorker(self.delivery, port, 'owned-delivery')
            self.assertTrue(worker.run_once())
            action = self.rows("SELECT * FROM forge_external_workflow.actions WHERE kind='prepare'")[0]
            self.assertEqual((action['status'], action['result']['status']), ('failed', 'failed'))
            self.assertEqual(action['result']['receipt']['rejected_count'], 2)
            self.assertTrue(flow.run_once())
            repair = self.call('claim', 'homelab', flow_fixture.ALIAS)
            self.assertIsNotNone(repair)
            context = repair['external_context']
            self.assertEqual(repair['agent_profile'], flow_fixture.ALIAS)
            self.assertEqual(context['stage'], 'implement')
            self.assertEqual(context['previous_artifact']['sha256'], hashlib.sha256(manifest.encode()).hexdigest())
            self.assertIn('Approved retained Plan.', str(context['approved_plan']))
            for path in CACHE_PATHS:self.assertIn(path, context['feedback']['summary'])
            events = self.rows('SELECT kind,used,attempt_limit,status FROM forge_agent.coding_retry_events')
            self.assertEqual([(row['kind'], row['used'], row['attempt_limit'], row['status']) for row in events],
                             [('verification', 1, 20, 'open')])
            self.assertFalse(flow.run_once())
            self.assertEqual(self.rows('SELECT count(*) AS n FROM forge_agent.coding_retry_event_admissions')[0]['n'], 1)
            self.assertEqual(self.rows("SELECT count(*) AS n FROM forge_external_workflow.reviews WHERE target_kind='code'")[0]['n'], 0)
            self.assertEqual(self.rows('SELECT phase FROM forge_external_workflow.workflows')[0]['phase'], 'repair')
            self.assertFalse((config.state_root / 'actions' / str(action['action_id']) / 'prepared.json').exists())
            port.jenkins.submit.assert_not_called()

    def test_repeated_permanent_rejections_use_one_bounded_twenty_attempt_event(self):
        _, _, flow, _ = self.submit_plan(); self.review(flow)
        port = Mock()
        port.prepare_candidate.side_effect = lambda request, lease: (_ for _ in ()).throw(rejected(request))
        worker = delivery.ExternalDeliveryWorker(self.delivery, port, 'owned-delivery')
        event_id = None
        for number in range(21):
            manifest = canonical({'version': 1, 'candidate_commit': 'b' * 40, 'candidate_tree': 'd' * 40,
                'base_revision': 'a' * 40, 'summary': 'Preserved candidate revision ' + str(number)}).decode()
            self.author(flow, manifest)
            self.assertTrue(worker.run_once())
            self.assertTrue(flow.run_once())
            event = self.rows('SELECT event_id,used,status FROM forge_agent.coding_retry_events')[0]
            event_id = event_id or event['event_id']
            self.assertEqual(event['event_id'], event_id)
            self.assertEqual(event['used'], min(number + 1, 20))
        self.assertEqual((event['used'], event['status']), (20, 'exhausted'))
        self.assertEqual(self.rows('SELECT phase,code_revisions FROM forge_external_workflow.workflows')[0],
                         {'phase': 'escalated', 'code_revisions': 20})
        self.assertIsNone(self.call('claim', 'homelab', flow_fixture.ALIAS))
        self.assertEqual(self.rows('SELECT count(*) AS n FROM forge_agent.coding_retry_event_admissions')[0]['n'], 20)


if __name__ == '__main__':unittest.main()
