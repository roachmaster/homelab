"""Negotiated retry progress is display data; factory composition owns its port."""
from email.message import Message
from types import SimpleNamespace
from unittest.mock import Mock
import unittest

from queue_store import Rejected
from work_api import WorkServiceFactory, retry_events_version, retry_events_response
from work_retry_events import RetryEventProviderFactory
from test_external_flow import cfg


class RetryEventTransportTest(unittest.TestCase):
    def test_version_is_exact_and_optional(self):
        self.assertIsNone(retry_events_version(Message()))
        headers=Message();headers['Forge-Retry-Events-Version']='1'
        self.assertEqual(retry_events_version(headers),1)
        headers['Forge-Retry-Events-Version']='1'
        with self.assertRaises(Rejected):retry_events_version(headers)
        for value in ('0','2','1,1','true',' 1'):
            headers=Message();headers['Forge-Retry-Events-Version']=value
            with self.subTest(value=value),self.assertRaises(Rejected):retry_events_version(headers)

    def test_legacy_shape_strips_only_event_fields_and_preserves_actual_totals(self):
        event={'event_id':'11111111-1111-4111-8111-111111111111','kind':'tests','used':4,'limit':20,'status':'open'}
        response=(200,{'protocol_version':1,'event':{'kind':'work_opened','task':{
            'retry_event':event,'workflow':{'retry_event':event,'code_revisions':40,'max_code_revisions':3},
            'review':{'round':40,'max_rounds':3,'retry_event':event},
            'delivery':{'repair':{'round':40,'max_rounds':20,'retry_event':event}}}}})
        self.assertIs(retry_events_response(response,1),response)
        _,legacy=retry_events_response(response,None)
        task=legacy['event']['task']
        self.assertNotIn('retry_event',task)
        self.assertNotIn('retry_event',task['workflow'])
        self.assertNotIn('retry_event',task['review'])
        self.assertNotIn('retry_event',task['delivery']['repair'])
        self.assertEqual(task['workflow']['code_revisions'],40)
        self.assertEqual(task['workflow']['max_code_revisions'],3)
        self.assertEqual(task['review']['round'],40)
        self.assertIn('retry_event',response[1]['event']['task'])

    def test_one_ready_port_is_injected_and_explicit_legacy_none_is_preserved(self):
        config=cfg();port=RetryEventProviderFactory(config.owner).create()
        selected=Mock(return_value=SimpleNamespace(create=lambda:port))
        service=WorkServiceFactory(config,Mock(),retry_event_factory=selected).create()
        selected.assert_called_once_with(config)
        self.assertIs(service._store.retry_events,port)
        self.assertIs(service._external_workflow._retry_events,port)
        legacy=WorkServiceFactory(config,Mock(),retry_event_factory=lambda _:SimpleNamespace(create=lambda:None)).create()
        self.assertIsNone(legacy._store.retry_events)
        self.assertIsNone(legacy._external_workflow._retry_events)
        unused=Mock(side_effect=AssertionError('Unconfigured means no factory resolution'))
        self.assertIsNone(WorkServiceFactory(None,retry_event_factory=unused).create())
        unused.assert_not_called()

    def test_actual_gateway_rejects_malformed_capability_before_service_effects(self):
        from work_api import Destination
        from test_work_api import runner_server,work_post,REQUEST_ID,reply
        service=Mock()
        with runner_server(work_service=service) as port:
            for value in ('0','2','1,1','true'):
                actual=work_post(port,Destination.CATALOG,{'kind':'work_catalog','request_id':REQUEST_ID},
                    extra_headers={'Forge-Retry-Events-Version':value})
                self.assertEqual(actual,(400,reply('work_failed',code='invalid_request'),'no-store'))
        self.assertFalse(service.method_calls)

    def test_source_bound_ci_observation_cannot_be_relabelled_from_diagnostics(self):
        from work_ci_contract import CiGateContractFactory,EVENT_PROFILE
        from work_ci_phases import event_stage_evidence
        from work_verification import protected_retry_observation
        from work_external_workflow import ExternalWorkflowStore
        from copy import deepcopy
        plan=CiGateContractFactory(EVENT_PROFILE).create().plan('main')
        failed=next(index for index,phase in enumerate(plan.phases) if phase.kind=='tests')
        checks=[{'index':index,'argv':list(phase.argv),'returncode':1 if index==failed else 0}
            for index,phase in enumerate(plan.phases[:failed+1])]
        action={'action_id':'11111111-1111-4111-8111-111111111111'}
        candidate={'candidate_commit':'a'*40,'candidate_tree':'b'*40,'integration_base':'c'*40,
            'gate_manifest_sha256':'d'*64,'branch_scope':'main','verification_profile':EVENT_PROFILE}
        w={'workflow_id':'22222222-2222-4222-8222-222222222222','project_alias':'homelab','state':{'candidate':candidate}}
        receipt={'provider':'jenkins','action_id':action['action_id'],'workflow_id':w['workflow_id'],'project_alias':'homelab',
            **{key:candidate[key] for key in ('candidate_commit','candidate_tree','gate_manifest_sha256','branch_scope','verification_profile')},
            'expected_main':candidate['integration_base'],'version':3,'result_version':3,'cleanup_confirmed':True,
            'check_receipt_sha256':'e'*64,'checks':checks,'failure':'checks_failed',
            **event_stage_evidence('main',checks,'checks_failed')}
        receipt['retry_observation']=protected_retry_observation(plan,receipt,'e'*64)
        observed=ExternalWorkflowStore._verified_stage_observation(w,action,receipt)
        self.assertEqual(observed['failure_kind'],'tests')
        self.assertEqual(observed['phase_id'],plan.phases[failed].phase_id)
        for change in ('candidate','cleanup','kind','phase','wire_version'):
            altered=deepcopy(receipt)
            if change=='candidate':altered['candidate_commit']='f'*40
            if change=='cleanup':altered['cleanup_confirmed']=False
            if change=='kind':altered['retry_observation']['failure_kind']='compilation'
            if change=='phase':altered['phase_results'][-1]['phase_id']='invented'
            if change=='wire_version':altered['version']=2
            with self.subTest(change=change),self.assertRaises(ValueError):
                ExternalWorkflowStore._verified_stage_observation(w,action,altered)
        # A failed outer Jenkins result cannot turn completed fixed checks into
        # a source failure or put "verification" in both passed/failed sets.
        from uuid import UUID
        w['workflow_id']=UUID(w['workflow_id'])
        receipt['checks']=[{'index':index,'argv':list(phase.argv),'returncode':0}
            for index,phase in enumerate(plan.phases)]
        receipt['failure']=None
        receipt.update(event_stage_evidence('main',receipt['checks'],None))
        receipt['retry_observation']=protected_retry_observation(plan,receipt,'e'*64)
        flow=ExternalWorkflowStore(cfg(),cfg().catalog())
        flow._retry_events=Mock()
        flow._action=Mock(return_value={**action,'status':'failed','result':{'receipt':receipt,'summary':'Jenkins terminal result was not SUCCESS.'}})
        flow._delivery_request=Mock(return_value={})
        flow._escalate=Mock();flow._queue_author=Mock()
        self.assertTrue(flow._consume_verification(Mock(),w))
        self.assertIn('no source repair was admitted',flow._escalate.call_args.args[2])
        flow._queue_author.assert_not_called()
        flow._retry_events.reserve.assert_not_called()
        self.assertGreater(flow._retry_events.resolve_verified.call_count,0)


if __name__=='__main__':unittest.main()
