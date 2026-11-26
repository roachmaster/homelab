"""GB-3 item 4: the private operator `revive` command reuses the guarded revival, with no bypass."""

import contextlib
import io
import json
import unittest
from types import SimpleNamespace
from unittest.mock import Mock, patch
from uuid import UUID, uuid4

import work_api
import work_config
import work_external_recovery as recovery
from test_workflow_revival import WorkflowRevivalTest


def argv(workflow, key="cli-1", note="Coordinator policy failure; the approved Plan is still valid.", *extra):
    return ["--config", "/owned/config.json", "--project", "homelab", "--workflow", str(workflow),
            "--idempotency-key", key, "--note", note, *extra]


class ReviveCliUnitTest(unittest.TestCase):
    def run_cli(self, args, result=None, euid=0):
        coordinator = Mock()
        coordinator.revive_workflow.return_value = result or {"status": "revived", "task_id": "t"}
        service = SimpleNamespace(workflow_worker=coordinator)
        factory = Mock()
        factory.return_value.create.return_value = service
        out = io.StringIO()
        with patch.object(recovery.os, "geteuid", return_value=euid), \
                patch.object(work_config, "load_work_config", return_value=object()), \
                patch.object(work_api, "WorkServiceFactory", factory), \
                contextlib.redirect_stdout(out):
            code = recovery.revive_main(args)
        return code, json.loads(out.getvalue()), coordinator

    def test_passes_only_typed_arguments_to_the_existing_revival(self):
        workflow = uuid4()
        code, printed, coordinator = self.run_cli(argv(workflow, "k-1", "Because.", "--execute"))
        self.assertEqual(code, 0)
        self.assertEqual(printed["status"], "revived")
        coordinator.revive_workflow.assert_called_once_with(
            project_alias="homelab", workflow_id=workflow, idempotency_key="k-1", note="Because.")

    def test_refusal_exits_nonzero_and_prints_reason(self):
        code, printed, _ = self.run_cli(
            argv(uuid4(), "k", "n", "--execute"), {"status": "refused", "reason": "plan_not_approved"})
        self.assertEqual((code, printed["reason"]), (3, "plan_not_approved"))

    def test_requires_execute_flag_uuid_and_root(self):
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            recovery.revive_main(argv(uuid4()))  # no --execute
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            recovery.revive_main(argv("not-a-uuid", "k", "n", "--execute"))
        with self.assertRaises(RuntimeError):
            self.run_cli(argv(uuid4(), "k", "n", "--execute"), euid=1000)

    def test_main_dispatches_the_revive_subcommand_only(self):
        with patch.object(recovery, "revive_main", return_value=3) as revive, \
                patch("sys.argv", ["x", "revive", "--note", "n"]):
            with self.assertRaises(SystemExit) as raised:
                recovery.main()
        self.assertEqual(raised.exception.code, 3)
        revive.assert_called_once_with(["--note", "n"])


class ReviveCliPostgresTest(WorkflowRevivalTest):
    def cli(self, workflow_id, key="cli-1", note="Coordinator policy failure; the approved Plan is still valid."):
        service = SimpleNamespace(workflow_worker=self.coordinator)
        out = io.StringIO()
        with patch.object(recovery.os, "geteuid", return_value=0), \
                patch.object(work_config, "load_work_config", return_value=object()), \
                patch.object(work_api, "WorkServiceFactory", Mock(return_value=Mock(create=Mock(return_value=service)))), \
                contextlib.redirect_stdout(out):
            code = recovery.revive_main(argv(workflow_id, key, note, "--execute"))
        return code, json.loads(out.getvalue())

    def test_cli_revives_through_the_guarded_function_and_replays(self):
        root, _plan, workflow_id = self.approved_and_cancelled()
        code, receipt = self.cli(workflow_id)
        self.assertEqual((code, receipt["status"]), (0, "revived"))
        self.assertEqual(self.rows("SELECT phase,enabled FROM forge_agent.coding_workflows WHERE workflow_id=%s", (workflow_id,))[0], ("implement", True))
        audit = self.audit(root, "workflow_revived")
        self.assertEqual(audit[0][0]["actor"], {"kind": "operator", "id": "mission-control"})
        self.assertEqual(self.cli(workflow_id), (0, receipt))

    def test_cli_cannot_bypass_the_guards(self):
        root, _plan, workflow_id = self.approved_and_cancelled()
        self.cluster.admin_exec(
            "INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload) "
            "SELECT owner_id,project_alias,task_id,max(sequence)+1,'cancelled','{\"cause\":\"operator_request\",\"actor\":{\"kind\":\"operator\",\"id\":\"mission-control\"}}'::jsonb "
            "FROM forge_agent.coding_events WHERE task_id='" + root + "' GROUP BY owner_id,project_alias,task_id", owner=False)
        code, result = self.cli(workflow_id)
        self.assertEqual((code, result["status"], result["reason"]), (3, "refused", "operator_cancelled"))
        self.assertEqual(self.rows("SELECT phase,enabled FROM forge_agent.coding_workflows WHERE workflow_id=%s", (workflow_id,))[0], ("escalated", False))
        self.assertEqual(self.audit(root, "workflow_revival_refused")[-1][0]["reason"], "operator_cancelled")

    def test_unknown_workflow_is_not_found(self):
        from queue_store import Rejected
        with self.assertRaises(Rejected) as raised:
            self.cli(uuid4())
        self.assertEqual(raised.exception.code, "not_found")


def load_tests(loader, tests, pattern):
    suite = unittest.TestSuite()
    suite.addTests(loader.loadTestsFromTestCase(ReviveCliUnitTest))
    suite.addTests(loader.loadTestsFromNames([
        f"{__name__}.ReviveCliPostgresTest.test_cli_revives_through_the_guarded_function_and_replays",
        f"{__name__}.ReviveCliPostgresTest.test_cli_cannot_bypass_the_guards",
        f"{__name__}.ReviveCliPostgresTest.test_unknown_workflow_is_not_found",
    ]))
    return suite


if __name__ == "__main__":
    unittest.main()
