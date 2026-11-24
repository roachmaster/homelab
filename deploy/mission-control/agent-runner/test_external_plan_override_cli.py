"""Private external plan approve-override CLI entry-point and release-runtime checks."""
import contextlib
import io
import json
import runpy
import unittest
from unittest.mock import Mock, patch
from uuid import uuid4

from test_external_flow import cfg
import work_external_plan_override as override_mod


class PlanOverrideCliTest(unittest.TestCase):
    def _cli_args(self, *, include_confirm=True, include_execute=True):
        args = [
            'operator',
            '--config', '/owned/config.json',
            '--project', 'homelab',
            '--workflow', str(uuid4()),
            '--root-task', str(uuid4()),
            '--source-review', str(uuid4()),
            '--plan-assignment', str(uuid4()),
            '--plan-sha256', 'a' * 64,
            '--expected-workflow-revision', '44',
            '--expected-root-revision', '3',
            '--idempotency-key', str(uuid4()),
            '--reason', 'Human owner explicitly accepts the final plan as-is.',
            '--acceptance-ref', 'owner-decision:2026-11-24-cli',
        ]
        if include_confirm:
            args.append('--confirm-accept-as-is')
        if include_execute:
            args.append('--execute')
        return args

    def test_direct_script_entry_delegates_to_canonical_module_not_duplicate_dto(self):
        with patch.object(override_mod, 'main') as main:
            runpy.run_path(override_mod.__file__, run_name='__main__')
        main.assert_called_once_with()

    def test_cli_passes_canonical_dto_and_override_provider_only(self):
        import work_api
        import work_config
        import work_external

        args = self._cli_args()
        service = Mock()
        service.approve_external_plan_override.return_value = {'status': 'implement_queued'}
        factory = Mock()
        factory.return_value.create.return_value = service
        external_factory_spy = Mock(wraps=work_external.ExternalWorkProviderFactory)
        stdout = io.StringIO()

        with (
            patch.object(override_mod.os, 'geteuid', return_value=0),
            patch.object(override_mod, 'verified_release_runtime') as verify,
            patch.object(work_config, 'load_work_config', return_value=cfg()),
            patch.object(work_api, 'WorkServiceFactory', factory),
            patch.object(work_external, 'ExternalWorkProviderFactory', external_factory_spy),
            patch('sys.argv', args),
            contextlib.redirect_stdout(stdout),
        ):
            override_mod.main()

        verify.assert_called_once_with(override_mod.__file__)
        command = service.approve_external_plan_override.call_args.args[0]
        self.assertIs(type(command), override_mod.ExternalPlanApproveOverride)
        self.assertEqual(command.expected_workflow_revision, 44)
        self.assertEqual(command.expected_root_revision, 3)
        self.assertIs(command.confirm_accept_as_is, True)
        self.assertEqual(json.loads(stdout.getvalue()), {'status': 'implement_queued'})

        # Verify external_factory wires ONLY plan_override_grant_provider (not recovery_grant_provider).
        ext_builder = factory.call_args.kwargs['external_factory']
        self.assertTrue(callable(ext_builder))
        dao = ext_builder(cfg())
        self.assertIsInstance(
            dao.workflow._plan_override_grants, override_mod.PlanApproveOverrideGrantScope
        )
        self.assertIsNone(dao.workflow._recovery_grants)

    def test_cli_requires_root_confirm_execute_and_immutable_release(self):
        with self.assertRaises(ValueError):
            override_mod.verified_release_runtime('/tmp/work_external_plan_override.py')
        with self.assertRaises(ValueError):
            override_mod.verified_release_runtime(
                '/opt/homelab-forge/releases/../tmp/source/deploy/mission-control/agent-runner/work_external_plan_override.py'
            )

        # Missing --confirm-accept-as-is or --execute exits via argparse error.
        for missing in (
            self._cli_args(include_confirm=False),
            self._cli_args(include_execute=False),
        ):
            with (
                patch('sys.argv', missing),
                contextlib.redirect_stderr(io.StringIO()),
                self.assertRaises(SystemExit) as exited,
            ):
                override_mod.main()
            self.assertNotEqual(exited.exception.code, 0)

        # Non-root invocation rejected before DB access.
        with (
            patch('sys.argv', self._cli_args()),
            patch.object(override_mod.os, 'geteuid', return_value=1000),
            self.assertRaises(RuntimeError),
        ):
            override_mod.main()


if __name__ == '__main__':
    unittest.main()
