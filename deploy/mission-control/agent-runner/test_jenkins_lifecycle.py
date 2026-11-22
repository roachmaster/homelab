"""Selected host lifecycle and local-only origin coverage without model or CI calls."""
from contextlib import contextmanager
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import threading
import unittest
from unittest.mock import Mock, patch
from uuid import uuid4

import jenkins_api
from jenkins_config import parse_jenkins_config
import jenkins_consumer
from jenkins_consumer import JenkinsConsumerFactory, HeartbeatCleanupError
from jenkins_events import ConsumptionReceipt, JenkinsEventEmit, validate_origin, validate_build_url
import runner


def configuration(**changes):
    raw = {'owner': 'mission-control', 'allowed_projects': ['homelab'],
        'allowed_instances': {'ci-primary': {'origin': 'http://127.0.0.1:18084'}},
        'allowed_jobs': {'homelab': ['forge-native-candidate']},
        'consumers': [{'name': 'observation', 'handler': 'observation'}]}
    return parse_jenkins_config({**raw, **changes})


class IdleStore:
    def __init__(self):
        self.ready = threading.Event()
        self.closed = threading.Event()

    @contextmanager
    def listen(self):
        self.ready.set()
        try:
            yield self
        finally:
            self.closed.set()

    def claim_next(self, *_args, **_kwargs):
        return None

    def wait_for_signal(self, _listener, _timeout, stop):
        stop.wait(.01)


class JenkinsHostLifecycleTest(unittest.TestCase):
    def test_api_only_main_runs_enabled_consumer_and_never_starts_chat_or_inference(self):
        store = IdleStore()
        service = jenkins_api.JenkinsServiceFactory(configuration(), store_factory=lambda _: store).create()
        consumer = service.consumer_worker
        with TemporaryDirectory(prefix='forge-jenkins-host-') as temporary:
            (Path(temporary) / 'runner.token').write_text('a' * 64)
            @contextmanager
            def server(*_args, **kwargs):
                self.assertIs(kwargs['jenkins_service'], service)
                def serve():
                    self.assertTrue(store.ready.wait(2))
                    self.assertTrue(consumer._thread.is_alive())
                yield SimpleNamespace(serve_forever=serve)
            with patch.dict(runner.os.environ, {'FORGE_RUNNER_API_ONLY': '1',
                    'FORGE_RUNNER_MODE': 'dispatch', 'CREDENTIALS_DIRECTORY': temporary}, clear=True), \
                    patch.object(runner, 'Server', server), \
                    patch.object(runner, 'build_work_and_usage_services', return_value=(None, None)), \
                    patch.object(runner, 'build_jenkins_service', return_value=service), \
                    patch.object(runner, 'Inference') as inference, \
                    patch.object(runner, 'worker') as chat, \
                    patch.object(runner, 'complete_dispatched_runs') as replies, \
                    patch.object(runner, 'expire_chat_history') as expiry:
                runner.main()
                inference.assert_not_called()
                chat.assert_not_called()
                replies.assert_not_called()
                expiry.assert_not_called()
        self.assertTrue(store.closed.is_set())
        self.assertIsNone(consumer._thread)

    def test_full_dispatch_main_owns_enabled_consumer_before_and_after_serving(self):
        store = IdleStore()
        service = jenkins_api.JenkinsServiceFactory(configuration(), store_factory=lambda _: store).create()
        consumer = service.consumer_worker
        events, legacy_targets = [], []
        original_start, original_close, real_thread = consumer.start, consumer.close, threading.Thread
        def start_consumer():
            events.append('jenkins-start')
            original_start()
        def close_consumer():
            original_close()
            self.assertTrue(store.closed.is_set())
            self.assertIsNone(consumer._thread)
            events.append('jenkins-close')
        def close_workflow():
            self.assertTrue(store.closed.is_set())
            self.assertIsNone(consumer._thread)
            events.append('workflow-close')
        workflow = SimpleNamespace(start=lambda: events.append('workflow-start'), close=close_workflow)
        with TemporaryDirectory(prefix='forge-jenkins-full-host-') as temporary:
            (Path(temporary) / 'runner.token').write_text('a' * 64)
            @contextmanager
            def server(_token, _ready, inference, **kwargs):
                self.assertIsNone(inference)
                self.assertTrue(kwargs['distributed'])
                self.assertIs(kwargs['jenkins_service'], service)
                events.append('server-open')
                def serve():
                    self.assertTrue(store.ready.wait(2))
                    self.assertTrue(consumer._thread.is_alive())
                    events.append('serve')
                try:
                    yield SimpleNamespace(serve_forever=serve)
                finally:
                    events.append('server-close')
            with patch.dict(runner.os.environ, {'FORGE_RUNNER_API_ONLY': '0',
                    'FORGE_RUNNER_MODE': 'dispatch', 'CREDENTIALS_DIRECTORY': temporary}, clear=True), \
                    patch.object(runner, 'Server', server), \
                    patch.object(runner, 'build_work_and_usage_services', return_value=(SimpleNamespace(workflow_worker=workflow), None)), \
                    patch.object(runner, 'build_jenkins_service', return_value=service), \
                    patch.object(runner, 'Inference') as inference, patch.object(runner, 'worker') as chat, \
                    patch.object(runner, 'complete_dispatched_runs') as replies, \
                    patch.object(runner, 'expire_chat_history') as expiry, \
                    patch.object(consumer, 'start', side_effect=start_consumer), \
                    patch.object(consumer, 'close', side_effect=close_consumer):
                def selected_thread(*args, **kwargs):
                    target = kwargs.get('target')
                    if target in (replies, expiry):
                        # The existing full-host support loops are selected but
                        # mocked: no inference, model process or network starts.
                        legacy_targets.append(target)
                        return SimpleNamespace(start=lambda: None)
                    self.assertEqual(target, consumer.run)
                    return real_thread(*args, **kwargs)
                with patch.object(runner.threading, 'Thread', side_effect=selected_thread):
                    runner.main()
                self.assertEqual(legacy_targets, [replies, expiry])
                inference.assert_not_called()
                chat.assert_not_called()
        self.assertEqual(events, ['server-open', 'workflow-start', 'jenkins-start',
            'serve', 'jenkins-close', 'workflow-close', 'server-close'])
        self.assertTrue(store.closed.is_set())
        self.assertIsNone(consumer._thread)

    def test_partial_worker_start_and_failed_close_still_release_other_owned_workers(self):
        events = []
        class Worker:
            def __init__(self, name, fail_start=False, fail_close=False):
                self.name,self.fail_start,self.fail_close = name,fail_start,fail_close
            def start(self):
                events.append(self.name + '-start')
                if self.fail_start:raise RuntimeError('selected worker startup failed')
            def close(self):
                events.append(self.name + '-close')
                if self.fail_close:raise RuntimeError('selected worker stop failed')
        for fail_start in (False, True):
            events.clear()
            with self.assertRaises(RuntimeError):
                with runner.background_services(SimpleNamespace(workflow_worker=Worker('workflow')),
                        SimpleNamespace(consumer_worker=Worker('jenkins',fail_start,True))):
                    if not fail_start:raise RuntimeError('host stopped')
            self.assertEqual(events,['workflow-start','jenkins-start','jenkins-close','workflow-close'])

    def test_disabled_selection_has_no_database_config_or_thread_effects(self):
        connect = Mock()
        with patch.object(Path,'read_bytes',side_effect=AssertionError('disabled config read')), \
                patch.object(threading.Thread,'start',side_effect=AssertionError('disabled thread start')):
            self.assertIsNone(runner.build_jenkins_service({},connect))
        connect.assert_not_called()
        selected = jenkins_api.JenkinsServiceFactory(configuration(consumers=[
            {'name':'observation','handler':'observation','enabled':False}]),store_factory=Mock()).create()
        self.assertIsNone(selected.consumer_worker)

    def test_selected_handler_partial_acquisition_and_stop_are_owned(self):
        events = []
        class Handler:
            def handle(self, *_args):raise AssertionError('no event was admitted')
            def close(self):events.append('closed')
        def fail(_store):raise RuntimeError('selected handler unavailable')
        config = configuration(consumers=[{'name':'first','handler':'first'}, {'name':'second','handler':'second'}])
        with self.assertRaises(RuntimeError):
            JenkinsConsumerFactory(config,Mock(),handler_factories={'first':lambda _:Handler(),'second':fail}).create()
        self.assertEqual(events,['closed'])
        store = IdleStore()
        worker = JenkinsConsumerFactory(configuration(consumers=[{'name':'first','handler':'first'}]),store,
            handler_factories={'first':lambda _:Handler()}).create()
        worker.start()
        self.assertTrue(store.ready.wait(2))
        worker.close(timeout=2)
        worker.close(timeout=2)
        self.assertEqual(events,['closed','closed'])
        with self.assertRaises(RuntimeError):worker.start()

    def test_blocked_heartbeat_prevents_owned_handler_release_until_it_stops(self):
        entered,release = threading.Event(),threading.Event()
        event_id = str(uuid4())
        event = SimpleNamespace(event_id=event_id)
        delivery = SimpleNamespace(delivery_row_id=str(uuid4()),lease_token=str(uuid4()),consumer_name='observation')
        store = IdleStore()
        store.claim_next = Mock(side_effect=[(event,delivery),None])
        def renew(*_args,**_kwargs):
            entered.set();release.wait(5);return True
        store.renew_lease = renew
        store.complete_delivery,store.fail_delivery = Mock(),Mock()
        closed = Mock()
        class Handler:
            def handle(self,*_args):
                if not entered.wait(3):raise AssertionError('heartbeat never started')
                return ConsumptionReceipt('observation',event_id,'a'*64,'build:1')
            def close(self):closed()
        config = configuration(lease_seconds=5,renew_seconds=1)
        worker = JenkinsConsumerFactory(config,store,handler_factories={'observation':lambda _:Handler()}).create()
        try:
            with patch.object(jenkins_consumer,'HEARTBEAT_STOP_SECONDS',.02):
                worker.start()
                self.assertTrue(entered.wait(3))
                self.assertTrue(worker.stop.wait(2))
                worker._thread.join(timeout=2)
                self.assertFalse(worker._thread.is_alive())
                with self.assertRaises(HeartbeatCleanupError):worker.close(timeout=.02)
                closed.assert_not_called()
                store.complete_delivery.assert_not_called()
                store.fail_delivery.assert_not_called()
                release.set()
                worker.close(timeout=2)
                closed.assert_called_once()
        finally:
            release.set()
            worker.close(timeout=2)

    def test_lost_heartbeat_never_completes_or_deadletters_the_delivery(self):
        entered = threading.Event()
        event_id = str(uuid4())
        event = SimpleNamespace(event_id=event_id)
        delivery = SimpleNamespace(delivery_row_id=str(uuid4()),lease_token=str(uuid4()),consumer_name='observation')
        store = IdleStore()
        store.claim_next = Mock(return_value=(event,delivery))
        def renew(*_args,**_kwargs):entered.set();return False
        store.renew_lease = renew
        store.complete_delivery,store.fail_delivery = Mock(),Mock()
        class Handler:
            def handle(self,*_args):
                if not entered.wait(3):raise AssertionError('heartbeat never started')
                return ConsumptionReceipt('observation',event_id,'a'*64,'build:1')
        worker = JenkinsConsumerFactory(configuration(lease_seconds=5,renew_seconds=1),store,
            handler_factories={'observation':lambda _:Handler()}).create()
        self.assertTrue(worker.run_once())
        store.complete_delivery.assert_not_called()
        store.fail_delivery.assert_not_called()
        worker.close(timeout=2)

    def test_direct_run_once_is_quiesced_before_its_handler_can_be_released(self):
        entered,release = threading.Event(),threading.Event()
        event_id = str(uuid4())
        event = SimpleNamespace(event_id=event_id)
        delivery = SimpleNamespace(delivery_row_id=str(uuid4()),lease_token=str(uuid4()),consumer_name='observation')
        store = IdleStore()
        store.claim_next = Mock(return_value=(event,delivery))
        store.complete_delivery = Mock(return_value=True)
        closed = Mock()
        class Handler:
            def handle(self,*_args):
                entered.set();release.wait(5)
                return ConsumptionReceipt('observation',event_id,'a'*64,'build:1')
            def close(self):closed()
        worker = JenkinsConsumerFactory(configuration(),store,
            handler_factories={'observation':lambda _:Handler()}).create()
        caller = threading.Thread(target=worker.run_once)
        try:
            caller.start()
            self.assertTrue(entered.wait(2))
            with self.assertRaises(RuntimeError):worker.close(timeout=.01)
            closed.assert_not_called()
        finally:
            release.set();caller.join(2);worker.close(timeout=2)
        self.assertFalse(caller.is_alive())
        closed.assert_called_once()
        self.assertFalse(worker.run_once())


class JenkinsLocalOriginTest(unittest.TestCase):
    def test_http_requires_explicit_local_origin_and_matching_port(self):
        for origin in ('http://127.0.0.1:18084','http://localhost:18084','http://[::1]:18084'):
            with self.subTest(origin=origin):
                self.assertEqual(validate_origin(origin),origin)
                self.assertEqual(validate_build_url(origin+'/job/build/1/',expected_origin=origin),origin+'/job/build/1/')
        for origin in ('http://example.com','http://192.168.1.2:8080','http://127.0.0.1.evil.test',
                       'http://user@127.0.0.1:8080','ftp://127.0.0.1','http://127.0.0.1:99999'):
            with self.subTest(origin=origin),self.assertRaises(ValueError):validate_origin(origin)
        with self.assertRaises(ValueError):
            validate_build_url('http://127.0.0.1:8080/job/build/1/',expected_origin='http://127.0.0.1:18084')

    def test_local_event_service_forwards_only_observation_to_selected_store(self):
        store = Mock(spec=['emit'])
        service = jenkins_api.JenkinsService(configuration(),store)
        event = JenkinsEventEmit(project_alias='homelab',jenkins_instance='ci-primary',
            job_name='forge-native-candidate',delivery_id='local-build-1',event_kind='build_completed',
            occurred_at='2026-11-21T21:00:00Z',summary='Observed completion only',build_number=1,
            build_result='success',build_url='http://127.0.0.1:18084/job/forge-native-candidate/1/')
        service.emit(event)
        store.emit.assert_called_once_with(event)


if __name__ == '__main__':unittest.main()
