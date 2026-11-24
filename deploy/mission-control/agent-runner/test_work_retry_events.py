"""Real per-cause budgets, independent of source IDs and lifetime totals."""
from contextlib import closing
from concurrent.futures import ThreadPoolExecutor
import hashlib
from pathlib import Path
import unittest
from uuid import UUID,uuid4
import psycopg2
from psycopg2.extras import RealDictCursor
from work_api import WorkServiceFactory
from work_pg_fixture import DisposablePostgresCluster,PG16_AVAILABLE,SQL_DIR
from work_retry_events import RetryScope,RetryEventProviderFactory
from test_work_execution_and_usage import make_submit,make_v2_config

@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class RetryEventPostgresTest(unittest.TestCase):
 @classmethod
 def setUpClass(cls):
  cls.pg=DisposablePostgresCluster().start();cls.addClassCleanup(cls.pg.stop)
  with closing(cls.pg.connect('postgres')) as c,c.cursor() as q:q.execute("SELECT to_regclass('forge_agent.coding_retry_events') IS NOT NULL");ready=q.fetchone()[0]
  if not ready:
   cls.pg._run('psql','-h',cls.pg.root,'-U','postgres','-X','-v','ON_ERROR_STOP=1','-d','forge_agent','-c','SET ROLE forge_agent_owner','-f',SQL_DIR/'migrations/0031_retry_events.sql')
 def setUp(self):
  self.pg.reset_coding_tables()
  self.service=WorkServiceFactory(make_v2_config(),lambda:self.pg.connect('forge_agent')).create()
  self.root=self.service.submit(make_submit(idempotency_key=str(uuid4())))
  self.scope=RetryScope('mission-control','homelab','manual_task',UUID(self.root['task_id']))
  self.provider=RetryEventProviderFactory('mission-control').create()
 def call(self,method,*args,**kwargs):
  with closing(self.pg.connect('forge_agent')) as c,c,c.cursor(cursor_factory=RealDictCursor) as q:
   q.execute("SELECT set_config('forge.coding_owner','mission-control',true)")
   return getattr(self.provider,method)(q,*args,**kwargs)
 def observe(self,kind='compilation',source=None,**kw):
  source=source or str(uuid4());return self.call('observe_failure',self.scope,kind,source,hashlib.sha256(source.encode()).hexdigest(),**kw)
 def test_twenty_compile_then_twenty_tests_and_same_event_twenty_first_refused(self):
  ids=set()
  for n in range(20):
   e=self.observe('compilation',phase_id='rust-build');ids.add(e['event_id'])
   r=self.call('reserve',e['event_id'],'fix-'+str(n));self.assertEqual(r['used'],n+1);self.assertEqual(r['status'],'open')
  self.assertEqual(len(ids),1)
  e=self.observe('compilation',phase_id='rust-build');self.assertEqual(e['status'],'exhausted')
  self.assertEqual(self.call('reserve',e['event_id'],'fix-21')['status'],'exhausted')
  for n in range(20):
   t=self.observe('tests',phase_id='rust-tests',passed_phase_ids=('rust-build',))
   r=self.call('reserve',t['event_id'],'test-fix-'+str(n));self.assertEqual(r['used'],n+1)
  self.assertNotEqual(e['event_id'],t['event_id']);self.assertEqual(self.call('get',e['event_id'])['status'],'resolved')
  self.assertEqual(self.call('reserve',t['event_id'],'test-fix-21')['status'],'exhausted')
 def test_duplicate_admission_is_exact_once_even_after_new_source(self):
  e=self.observe();a=self.call('reserve',e['event_id'],'one')
  newer=self.observe();self.assertEqual(newer['event_id'],e['event_id'])
  self.call('reserve',e['event_id'],'two')
  self.assertEqual(self.call('reserve',e['event_id'],'one'),a);self.assertEqual(self.call('get',e['event_id'])['used'],2)
 def test_distinct_test_phases_are_distinct_events_and_old_proofs_never_close_new_events(self):
  original=self.observe('tests',source='original',phase_id='rust-tests')
  self.call('reserve',original['event_id'],'rust-try')
  backend=self.observe('tests',source='next',phase_id='backend-tests',passed_phase_ids=('rust-tests',))
  self.assertNotEqual(original['event_id'],backend['event_id']);self.assertEqual(backend['used'],0)
  fresh=self.observe('tests',source='regression',phase_id='rust-tests')
  self.observe('tests',source='next',phase_id='backend-tests',passed_phase_ids=('rust-tests',))
  self.assertEqual(self.call('get',fresh['event_id'])['status'],'open')
  self.call('resolve_verified',self.scope,'tests','pass','a'*64,phase_id='rust-tests')
  later=self.observe('tests',phase_id='rust-tests')
  self.call('resolve_verified',self.scope,'tests','pass','a'*64,phase_id='rust-tests')
  self.assertEqual(self.call('get',later['event_id'])['status'],'open')
 def test_raced_delivery_reserves_once_and_worker_cannot_mint(self):
  e=self.observe()
  with ThreadPoolExecutor(max_workers=2) as pool:
   a=pool.submit(self.call,'reserve',e['event_id'],'same');b=pool.submit(self.call,'reserve',e['event_id'],'same')
   self.assertEqual(a.result(),b.result())
  self.assertEqual(self.call('get',e['event_id'])['used'],1)
  with closing(self.pg.connect('forge_coding_worker')) as c,c,c.cursor() as q:
   with self.assertRaises(psycopg2.errors.InsufficientPrivilege):q.execute('SELECT forge_agent.retry_event_reserve(%s,%s)',(e['event_id'],'unsafe'))
 def test_cross_owner_scope_and_unproved_reset_are_rejected(self):
  e=self.observe()
  with self.assertRaises(ValueError):self.call('observe_failure',RetryScope('other','homelab','manual_task',self.scope.scope_id),'tests','id','a'*64)
  with self.assertRaises(psycopg2.Error):self.call('adopt_legacy',e['event_id'],0,'old','b'*64)
