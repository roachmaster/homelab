"""Bounded recorded attribution, negotiated before effects."""
from email.message import Message
import unittest
from unittest.mock import Mock
from queue_store import Rejected
from work_event_attribution import public_attribution
from work_api import event_attribution_version, event_attribution_response

class AttributionTest(unittest.TestCase):
    def test_recorded_actor_cause_independent_and_payload_private(self):
        value = {'actor': {'kind': 'worker', 'id': 'grok-primary'}, 'cause': 'workflow_cancelled',
                 'cause_detail': 'Stopped after cancellation.', 'secret': 'private'}
        self.assertEqual(public_attribution('cancelled', value),
            {'actor': value['actor'], 'cause': value['cause'], 'detail': value['cause_detail']})
        self.assertIsNone(public_attribution('cancelled', {}))
        self.assertIsNone(public_attribution('checkpoint', value))
        self.assertEqual(value['secret'], 'private')

    def test_invalid_and_unbounded_fields_do_not_escape(self):
        for detail in ('x' * 513, 'é' * 257, 'bad\x00text', 'bad\ntext', 'bad\u0085text', '\ud800'):
            self.assertEqual(public_attribution('cancel_requested', {'cause': 'operator_request', 'cause_detail': detail}),
                {'cause': 'operator_request'})
        self.assertIsNone(public_attribution('cancelled', {'actor': {'kind': 'operator', 'id': '<script>'}, 'cause': 'invented'}))
        self.assertIsNone(public_attribution('blocked_permission', {'cause': 'operator_request'}))
        self.assertEqual(public_attribution('cancelled', {'cause': 'superseded', 'cause_detail': '<safe text>'}),
            {'cause': 'superseded', 'detail': '<safe text>'})

    def test_exact_negotiation_and_legacy_projection(self):
        self.assertIsNone(event_attribution_version(Message()))
        h = Message(); h['Forge-Event-Attribution-Version'] = '1'
        self.assertEqual(event_attribution_version(h), 1)
        h['Forge-Event-Attribution-Version'] = '1'
        with self.assertRaises(Rejected): event_attribution_version(h)
        response = (200, {'event': {'kind': 'work_events', 'events': [
            {'kind': 'cancelled', 'attribution': {'cause': 'superseded'}, 'sequence': 1}]}})
        self.assertIs(event_attribution_response(response, 1), response)
        self.assertEqual(event_attribution_response(response, None)[1]['event']['events'],
            [{'kind': 'cancelled', 'sequence': 1}])
        self.assertIn('attribution', response[1]['event']['events'][0])

    def test_gateway_rejects_bad_negotiation_before_service_effects(self):
        from work_api import Destination
        from test_work_api import runner_server, work_post, REQUEST_ID
        service = Mock()
        with runner_server(work_service=service) as port:
            for value in ('0', '2', '1,1', 'true'):
                status, _, _ = work_post(port, Destination.CATALOG,
                    {'kind': 'work_catalog', 'request_id': REQUEST_ID},
                    extra_headers={'Forge-Event-Attribution-Version': value})
                self.assertEqual(status, 400)
        self.assertFalse(service.method_calls)

    def test_malformed_actor_kinds_are_omitted(self):
        for kind in ([], {}, None, 1):
            self.assertIsNone(public_attribution('cancelled', {'actor': {'kind': kind, 'id': 'worker'}}))

    def test_gateway_negotiates_real_event_reply_without_changing_legacy_shape(self):
        from work_api import Destination
        from test_work_api import runner_server, work_post, REQUEST_ID
        task = '00000000-0000-4000-8000-000000000001'
        event = {'event_id': task, 'correlation_id': task, 'sequence': 1,
                 'kind': 'cancelled', 'created_at': '2026-11-27T00:00:00Z',
                 'attribution': {'cause': 'superseded'}}
        service = Mock()
        service.events.return_value = {'project_alias': 'homelab', 'task_id': task,
            'revision': 2, 'status': 'cancelled', 'events': [event], 'next_sequence': 1}
        command = {'kind': 'work_events', 'request_id': REQUEST_ID, 'project_alias': 'homelab',
            'task_id': task, 'after_sequence': 0, 'limit': 50}
        with runner_server(work_service=service) as port:
            for version in (None, '1'):
                headers = {} if version is None else {'Forge-Event-Attribution-Version': version}
                status, body, _ = work_post(port, Destination.EVENTS, command, extra_headers=headers)
                self.assertEqual(status, 200)
                self.assertEqual('attribution' in body['event']['events'][0], version == '1')
        self.assertEqual(service.events.call_count, 2)
