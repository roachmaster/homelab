"""Normal host composition of independent reviewer and Jenkins event ports."""

from contextlib import contextmanager
from dataclasses import replace
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import threading
import unittest
from unittest.mock import Mock, patch

import jenkins_api
from jenkins_config import parse_jenkins_config
import runner
from test_work_execution_and_usage import make_v2_config
from work_config import WorkflowPolicy
from work_reviewer_correction import ReviewerCorrectionProvider
from work_workflow import WorkflowCoordinator


class IdleJenkinsStore:
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


class JenkinsReviewerCompositionTest(unittest.TestCase):
    def test_full_host_selects_both_real_factories_and_owns_their_lifetimes(self):
        work_config = replace(make_v2_config(), project_workflows={
            "homelab": WorkflowPolicy("agy-v2", "codex-v2"),
        })
        jenkins_config = parse_jenkins_config({
            "owner": "mission-control", "allowed_projects": ["homelab"],
            "allowed_instances": {"local": {"origin": "http://127.0.0.1:18084"}},
            "allowed_jobs": {"homelab": ["forge-native-candidate"]},
            "consumers": [{"name": "observation", "handler": "observation"}],
        })
        store = IdleJenkinsStore()
        selected, events, legacy_targets = {}, [], []
        database = Mock(side_effect=AssertionError("Bootstrap must not query PostgreSQL"))
        real_thread = threading.Thread

        def assert_correction_ports(service):
            workflow_flow = service.workflow_worker.store
            control_flow = service._native_control.workflows
            for flow in (workflow_flow, control_flow):
                self.assertIs(flow.store, service._store)
                self.assertIsInstance(flow.reviewer_correction, ReviewerCorrectionProvider)
                self.assertIs(flow.reviewer_correction.flow, flow)
            self.assertIsNot(workflow_flow.reviewer_correction, control_flow.reviewer_correction)

        def start_workflow(workflow):
            self.assertIs(workflow, selected["work"].workflow_worker)
            assert_correction_ports(selected["work"])
            events.append("workflow-start")

        def close_workflow(workflow):
            self.assertIs(workflow, selected["work"].workflow_worker)
            self.assertTrue(store.closed.is_set())
            self.assertIsNone(selected["jenkins"].consumer_worker._thread)
            events.append("workflow-close")

        @contextmanager
        def server(_token, _ready, inference, **kwargs):
            self.assertIsNone(inference)
            self.assertTrue(kwargs["distributed"])
            selected.update(work=kwargs["work_service"], jenkins=kwargs["jenkins_service"])
            self.assertIsInstance(selected["jenkins"], jenkins_api.JenkinsService)
            self.assertIsInstance(selected["work"].workflow_worker, WorkflowCoordinator)
            assert_correction_ports(selected["work"])
            events.append("server-open")

            def serve():
                self.assertTrue(store.ready.wait(2))
                self.assertTrue(selected["jenkins"].consumer_worker._thread.is_alive())
                self.assertEqual(events, ["server-open", "workflow-start"])
                events.append("serve")
            try:
                yield SimpleNamespace(serve_forever=serve)
            finally:
                self.assertTrue(store.closed.is_set())
                events.append("server-close")

        with TemporaryDirectory(prefix="forge-jenkins-reviewer-composition-") as temporary:
            (Path(temporary) / "runner.token").write_text("a" * 64)
            with patch.dict(runner.os.environ, {
                "FORGE_RUNNER_API_ONLY": "0", "FORGE_RUNNER_MODE": "dispatch",
                "CREDENTIALS_DIRECTORY": temporary,
            }, clear=True), patch.object(runner, "Server", server), \
                    patch.object(runner.work_config, "load_work_config", side_effect=[work_config, None]) as work_loader, \
                    patch.object(runner.jenkins_config, "load_jenkins_config", return_value=jenkins_config) as jenkins_loader, \
                    patch.object(jenkins_api, "JenkinsStore", return_value=store) as selected_store, \
                    patch.object(runner.store, "connect", database), \
                    patch.object(WorkflowCoordinator, "start", autospec=True, side_effect=start_workflow), \
                    patch.object(WorkflowCoordinator, "close", autospec=True, side_effect=close_workflow), \
                    patch.object(runner, "Inference") as inference, patch.object(runner, "worker") as model_worker, \
                    patch.object(runner, "complete_dispatched_runs") as replies, \
                    patch.object(runner, "expire_chat_history") as expiry:
                def selected_thread(*args, **kwargs):
                    target = kwargs.get("target")
                    if target in (replies, expiry):
                        legacy_targets.append(target)
                        return SimpleNamespace(start=lambda: None)
                    self.assertEqual(target, selected["jenkins"].consumer_worker.run)
                    return real_thread(*args, **kwargs)
                with patch.object(runner.threading, "Thread", side_effect=selected_thread):
                    runner.main()
                database.assert_not_called()
                self.assertEqual(work_loader.call_count, 2)
                jenkins_loader.assert_called_once()
                selected_store.assert_called_once_with(jenkins_config, database)
                self.assertEqual(legacy_targets, [replies, expiry])
                inference.assert_not_called()
                model_worker.assert_not_called()
                replies.assert_not_called()
                expiry.assert_not_called()
        self.assertEqual(events, ["server-open", "workflow-start", "serve", "workflow-close", "server-close"])
        self.assertTrue(store.closed.is_set())
        self.assertIsNone(selected["jenkins"].consumer_worker._thread)


if __name__ == "__main__":
    unittest.main()
