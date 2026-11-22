"""Private recovery entry-point and authority acquisition; no production I/O."""
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock,patch
import contextlib,io,runpy,unittest
from uuid import uuid4
import work_external_recovery as recovery
from test_external_flow import cfg

class RecoveryCliTest(unittest.TestCase):
 def test_direct_script_entry_delegates_to_canonical_module_not_duplicate_dto(self):
  with patch.object(recovery,'main') as main:
   runpy.run_path(recovery.__file__,run_name='__main__')
  main.assert_called_once_with()
 def test_cli_passes_canonical_dto_to_selected_factory_without_model_or_http(self):
  import work_api,work_config
  args=['operator','--config','/owned/config.json','--project','homelab','--workflow',str(uuid4()),'--root-task',str(uuid4()),'--expected-workflow-revision','5','--expected-root-revision','4','--idempotency-key',str(uuid4()),'--reason','Correct the deployed immutable context setup failure.','--deployed-fix-revision','f'*40,'--execute']
  service=Mock();service.recover_external_review_infrastructure.return_value={'trusted_source_pending':True}
  factory=Mock();factory.return_value.create.return_value=service
  with patch.object(recovery.os,'geteuid',return_value=0),patch.object(recovery,'verified_deployed_fix') as verify,patch.object(work_config,'load_work_config',return_value=cfg()),patch.object(work_api,'WorkServiceFactory',factory),patch('sys.argv',args),contextlib.redirect_stdout(io.StringIO()):
   recovery.main()
  command=service.recover_external_review_infrastructure.call_args.args[0]
  self.assertIs(type(command),recovery.ExternalInfrastructureRetry)
  self.assertEqual(command.expected_workflow_revision,5);verify.assert_called_once_with(recovery.__file__,'f'*40)
  self.assertTrue(callable(factory.call_args.kwargs['external_factory']))
 def test_nonroot_operator_and_uninstalled_or_mutable_runtime_are_rejected(self):
  with self.assertRaises(ValueError):recovery.verified_deployed_fix('/tmp/work_external_recovery.py','f'*40)
  with patch.object(recovery.os,'geteuid',return_value=1000),self.assertRaises(RuntimeError):recovery.peer_connect('forge_agent')
 def test_peer_acquisition_restores_operator_credentials_even_when_connect_fails(self):
  import psycopg2,threading
  with patch.object(recovery.os,'geteuid',return_value=0),patch.object(threading,'active_count',return_value=1),patch.object(recovery.os,'getegid',return_value=0),patch.object(recovery.pwd,'getpwnam',return_value=SimpleNamespace(pw_uid=123,pw_gid=456)),patch.object(recovery.os,'seteuid') as uid,patch.object(recovery.os,'setegid') as gid,patch.object(psycopg2,'connect',side_effect=OSError('owned fixture failure')):
   with self.assertRaises(OSError):recovery.peer_connect('forge_agent')
  self.assertEqual([call.args[0] for call in uid.call_args_list],[123,0]);self.assertEqual([call.args[0] for call in gid.call_args_list],[456,0])

if __name__=='__main__':unittest.main()
