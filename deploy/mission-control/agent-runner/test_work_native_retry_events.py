"""A real native DAO workflow gets independent compilation and test budgets."""
from contextlib import closing
import hashlib,json,subprocess,sys
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from psycopg2.extras import RealDictCursor
from work_feedback import parse_feedback_json
from work_pg_fixture import DisposablePostgresCluster,PG16_AVAILABLE
from work_retry_events import RetryEventProviderFactory
import test_work_workflow as fixtures

@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class NativeRetryEventWorkflowTest(unittest.TestCase):
 @classmethod
 def setUpClass(cls):
  cls.cluster=DisposablePostgresCluster().start();cls.addClassCleanup(cls.cluster.stop)
 def setUp(self):
  self.f=fixtures.AutonomousWorkflowTest();self.f.cluster=self.cluster;self.f.setUp()
  provider=RetryEventProviderFactory(self.f.config.owner).create()
  self.f.service._store.retry_events=provider;self.f.coordinator.store.retry_events=provider
  self.scratch=TemporaryDirectory(prefix='forge-native-event-proof-');self.addCleanup(self.scratch.cleanup)
  self.program=Path(self.scratch.name)/'program.py'
 def rows(self,sql,args=()):
  with closing(self.cluster.connect('postgres')) as c,c.cursor(cursor_factory=RealDictCursor) as q:q.execute(sql,args);return q.fetchall()
 def finish_check(self,lease,index,kind):
  f=self.f;receipt=f.publish(lease,'Agent claims completion '+str(index));f.db.queue_review(lease,receipt);f.coordinator.run_once()
  job=f.db.review_claim('trusted-fixed-phase');self.assertIsNotNone(job)
  compile_argv=[sys.executable,'-B','-m','py_compile',str(self.program)]
  compile_result=subprocess.run(compile_argv,capture_output=True,timeout=5)
  if kind=='compilation':self.assertNotEqual(compile_result.returncode,0);result=compile_result;argv=compile_argv;phase='fixture-compile';passed=[]
  else:
   self.assertEqual(compile_result.returncode,0)
   argv=[sys.executable,'-B','-c',"import runpy,sys;m=runpy.run_path(sys.argv[1]);assert m['answer']()==2",str(self.program)]
   result=subprocess.run(argv,capture_output=True,timeout=5);self.assertNotEqual(result.returncode,0);phase='fixture-tests';passed=['fixture-compile']
  digest=hashlib.sha256(json.dumps({'phase':phase,'argv':argv,'returncode':result.returncode,'stderr_sha256':hashlib.sha256(result.stderr).hexdigest()},sort_keys=True).encode()).hexdigest()
  candidate=hashlib.sha1(self.program.read_bytes()+str(index).encode()).hexdigest()
  outcome={'disposition':'repair','reason':'fixed_phase_failed','candidate_commit':candidate,'gate_manifest_sha256':'e'*64,
   'findings':[{'finding_id':'fixed-'+kind,'category':'source','check_id':phase,'summary':'The fixed trusted phase failed.'}],
   'verified_finding_ids':[],'fingerprint':hashlib.sha256(result.stderr).hexdigest(),'summary':'Fixed phase failed.','cleanup_confirmed':True,
   'verification':{'checks_passed':False,'candidate_workspace_digest':hashlib.sha256(self.program.read_bytes()).hexdigest(),
    'repair_requires_new_workspace':False,'cleanup_confirmed':True,'integration_base':'a'*40,
    'checks':[{'check_id':phase,'status':'failed','head_sha':candidate}]},
   'retry_observation':{'failure_kind':kind,'phase_id':phase,'passed_kinds':['compilation'] if passed else [],'passed_phase_ids':passed,'stage_evidence_digest':digest}}
  answer=f.db.review_finish(job,outcome)
  self.assertEqual(f.db.review_finish(job,outcome),answer)
  return answer
 def test_same_native_workflow_compilation_twenty_then_tests_twenty_and_twenty_first_stops(self):
  f=self.f;_command,admitted=f.submit();f.finish_plan_round('approve');lease=f.claim('agy-v2')
  frozen=self.rows('SELECT snapshot,policy_digest FROM forge_agent.coding_workflows')[0]
  for index in range(40):
   kind='compilation' if index<20 else 'tests'
   self.program.write_text(('def answer(:\n return 1\n' if kind=='compilation' else 'def answer():\n return 1\n')+'# revision '+str(index)+'\n')
   result=self.finish_check(lease,index,kind);self.assertEqual(result['status'],'repair')
   lease=f.claim('agy-v2');self.assertIsNotNone(lease)
   packet=f.db.read_feedback(lease);feedback=parse_feedback_json(packet['feedback_json'],lease.feedback_sha256)
   self.assertEqual(feedback['round'],index+1)
  budget=self.rows('SELECT repair_rounds FROM forge_agent.coding_review_budget')[0]
  self.assertEqual(budget['repair_rounds'],40)
  events=self.rows('SELECT kind,phase_id,used,status FROM forge_agent.coding_retry_events ORDER BY created_at')
  self.assertEqual([(e['kind'],e['used'],e['status']) for e in events],[('compilation',20,'resolved'),('tests',20,'open')])
  self.assertEqual(self.rows('SELECT snapshot,policy_digest FROM forge_agent.coding_workflows')[0],frozen)
  result=self.finish_check(lease,40,'tests');self.assertEqual(result['status'],'handoff');self.assertIsNone(f.claim('agy-v2'))
  f.coordinator.run_once();detail=f.get(admitted['task_id'])
  self.assertEqual((detail['workflow']['code_revisions'],detail['retry_event']['kind'],detail['retry_event']['used'],detail['retry_event']['limit']),(40,'tests',20,20))
  self.assertEqual(detail['retry_event']['status'],'exhausted')
  self.assertEqual(self.rows('SELECT count(*) AS n FROM forge_agent.coding_retry_event_admissions')[0]['n'],40)
