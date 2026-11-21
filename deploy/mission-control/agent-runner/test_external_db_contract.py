from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
import hashlib,json,unittest
from uuid import uuid4
import psycopg2
from psycopg2.extras import Json
from work_pg_fixture import DisposablePostgresCluster,PG16_AVAILABLE,SQL_DIR
POLICY=hashlib.sha256(b'forge-external-db-v1').hexdigest()
@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class ExternalDBContractTest(unittest.TestCase):
 @classmethod
 def setUpClass(cls):
  cls.pg=DisposablePostgresCluster().start();cls.addClassCleanup(cls.pg.stop)
  cls.pg.admin_exec('CREATE ROLE muse_events LOGIN INHERIT','CREATE ROLE external_outsider LOGIN',owner=False)
  for n in ('grant-muse-read-access.sql','grant-external-agent.sql'):
   r=cls.pg.run_sql_script(SQL_DIR/'management'/n);assert r.returncode==0,r.stdout+r.stderr
  cls.pg.apply_and_verify()
 def setUp(self):
  self.pg.admin_exec('TRUNCATE forge_agent.coding_tasks CASCADE','DELETE FROM forge_external_agent.readiness','DELETE FROM forge_external_agent.scope_grants',"INSERT INTO forge_external_agent.scope_grants(external_role,owner_id,project_alias,profile_alias,enabled) VALUES('muse_events','test-owner','homelab','muse-db-primary',true)")
 def call(self,n,*a,user='muse_events',conn=None):
  if conn:
   with conn.cursor() as c:c.execute('SELECT forge_external_agent_api.'+n+'('+','.join(['%s']*len(a))+')',a);return c.fetchone()[0]
  with closing(self.pg.connect(user)) as db:
   with db:return self.call(n,*a,conn=db)
 def rows(self,s,a=()):
  with closing(self.pg.connect('postgres')) as db,db.cursor() as c:c.execute(s,a);return c.fetchall()
 def task(self,patch=None,mode='plan'):
  t=str(uuid4());r=dict(version=2,backend='muse',model='external-runner',resource_profile='forge-external-db-v1',mode=mode,generation_token_limit=None,policy_digest=POLICY);r.update(patch or {})
  if r['version']==1:r.pop('policy_digest',None)
  with closing(self.pg.connect('forge_agent')) as db:
   with db,db.cursor() as c:
    c.execute("INSERT INTO forge_agent.coding_tasks(owner_id,project_alias,id,idempotency_key,intent_digest,base_revision,mode,agent_profile,title,prompt,acceptance_criteria,pinned_recipe,recipe_digest,status,revision,review_required) VALUES('test-owner','homelab',%s,%s,%s,%s,%s,'muse-db-primary','External plan','Write a plan','Review independently',%s,%s,'queued',1,false)",(t,t,'a'*64,'b'*40,mode,Json(r),hashlib.sha256(json.dumps(r,sort_keys=True).encode()).hexdigest()))
    c.execute("INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload) VALUES('test-owner','homelab',%s,1,'queued','{}')",(t,))
  return t
 def setup_job(self):
  t=self.task();a=str(self.call('enqueue','test-owner','homelab',t,user='forge_agent'));self.call('advertise','homelab','muse-db-primary','fixture');l=self.call('claim','homelab','muse-db-primary');return t,a,l
 def test_finish_replay_artifact_and_no_native_proof(self):
  t=self.task();a=str(self.call('enqueue','test-owner','homelab',t,user='forge_agent'));self.assertEqual(a,str(self.call('enqueue','test-owner','homelab',t,user='forge_agent')));self.assertEqual(self.call('catalogue','test-owner',user='forge_agent'),[])
  self.call('advertise','homelab','muse-db-primary','fixture');l=self.call('claim','homelab','muse-db-primary');self.call('renew',a,1,l['token']);self.call('progress',a,1,l['token'],'Working')
  r=self.call('finish',a,1,l['token'],'Plan','Done');self.assertEqual(r,self.call('finish',a,1,l['token'],'Plan','Done'));self.assertEqual(r['status'],'awaiting_review')
  x=self.call('artifact',a);self.assertEqual(x['sha256'],hashlib.sha256(b'Plan').hexdigest());self.assertEqual(x,self.call('artifact_by_task','test-owner','homelab',t,user='forge_agent'))
  self.assertEqual(self.rows('SELECT status,latest_attempt_id,accepted_at FROM forge_agent.coding_tasks WHERE id=%s',(t,)),[('awaiting_review',None,None)])
  self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_attempts'),[(0,)]);self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_reconciliation_receipts'),[(0,)])
  for args in ((a,1,l['token'],'Changed','Done'),(a,1,l['token'],'Plan','Other'),(a,2,l['token'],'Plan','Done'),(a,1,str(uuid4()),'Plan','Done')):
   with self.assertRaises(psycopg2.Error):self.call('finish',*args)
  with self.assertRaises(psycopg2.Error):self.pg.admin_exec("UPDATE forge_external_agent.artifacts SET content='forged'")
 def test_expiry_reconcile_generation_and_cancellation(self):
  t,a,l=self.setup_job();self.pg.admin_exec("UPDATE forge_external_agent.assignments SET lease_until=clock_timestamp()-interval '1 second'")
  with self.assertRaises(psycopg2.Error):self.call('progress',a,1,l['token'],'Stale')
  self.call('reconcile','test-owner',user='forge_agent');self.assertEqual(self.rows('SELECT status FROM forge_agent.coding_tasks WHERE id=%s',(t,)),[('queued',)])
  second=self.call('claim','homelab','muse-db-primary');self.assertEqual(second['generation'],2)
  with self.assertRaises(psycopg2.Error):self.call('finish',a,1,l['token'],'Old','Old')
  self.pg.admin_exec("UPDATE forge_agent.coding_tasks SET status='cancel_requested',revision=revision+1")
  with self.assertRaises(psycopg2.Error):self.call('finish',a,2,second['token'],'Cancelled','Cancelled')
  self.call('reconcile','test-owner',user='forge_agent');self.assertEqual(self.rows('SELECT status FROM forge_agent.coding_tasks WHERE id=%s',(t,)),[('cancelled',)])
 def test_recipe_and_scope_denials(self):
  for mode,p in [('implement',{}),('plan',{'version':1}),('plan',{'model':'native'}),('plan',{'policy_digest':'a'*64})]:
   t=self.task(p,mode)
   with self.assertRaises(psycopg2.Error):self.call('enqueue','test-owner','homelab',t,user='forge_agent')
  with self.assertRaises(psycopg2.Error):self.call('advertise','other-project','muse-db-primary','fixture')
  with closing(self.pg.connect('muse_events')) as db:
   with self.assertRaises(psycopg2.Error),db,db.cursor() as c:c.execute('SELECT * FROM forge_external_agent.scope_grants')
 def test_concurrent_claim_and_event_sequences(self):
  t,a,l=self.setup_job()
  with ThreadPoolExecutor(max_workers=4) as pool:list(pool.map(lambda n:self.call('progress',a,1,l['token'],'Progress '+str(n)),range(8)))
  self.assertEqual([r[0] for r in self.rows('SELECT sequence FROM forge_agent.coding_events WHERE task_id=%s ORDER BY sequence',(t,))],list(range(1,11)))
  self.assertIsNone(self.call('claim','homelab','muse-db-primary'))
 def test_claim_cap_and_scope_revoked_before_replay(self):
  t,a,l=self.setup_job()
  for gen in (1,2,3):
   if gen>1:l=self.call('claim','homelab','muse-db-primary')
   self.assertEqual(l['generation'],gen);self.call('release',a,gen,l['token'])
  self.assertIsNone(self.call('claim','homelab','muse-db-primary'));self.assertEqual(self.rows('SELECT status FROM forge_agent.coding_tasks WHERE id=%s',(t,)),[('failed',)])
  self.setUp();t,a,l=self.setup_job();self.call('finish',a,1,l['token'],'Plan','Done');self.pg.admin_exec('UPDATE forge_external_agent.scope_grants SET enabled=false')
  with self.assertRaises(psycopg2.Error):self.call('finish',a,1,l['token'],'Plan','Done')
 def test_repeatable_read_revocation_fences_finish_and_receipt_replay(self):
  for replay in (False,True):
   self.setUp();t,a,l=self.setup_job()
   if replay:self.call('finish',a,1,l['token'],'Plan','Done')
   with closing(self.pg.connect('muse_events')) as db:
    db.set_session(isolation_level='REPEATABLE READ')
    self.call('capabilities',conn=db)
    self.pg.admin_exec('UPDATE forge_external_agent.scope_grants SET enabled=false')
    with self.assertRaises(psycopg2.errors.SerializationFailure):self.call('finish',a,1,l['token'],'Plan','Done',conn=db)
    db.rollback()
 def test_revoked_external_scope_is_reconciled_by_trusted_service(self):
  t,a,l=self.setup_job();self.pg.admin_exec('UPDATE forge_external_agent.scope_grants SET enabled=false')
  with self.assertRaises(psycopg2.Error):self.call('renew',a,1,l['token'])
  self.call('reconcile','test-owner',user='forge_agent')
  self.assertEqual(self.rows('SELECT status FROM forge_agent.coding_tasks WHERE id=%s',(t,)),[('blocked_permission',)])
  self.assertEqual(self.rows('SELECT status,lease_token,lease_until FROM forge_external_agent.assignments WHERE assignment_id=%s',(a,)),[('failed',None,None)])
 def test_native_claim_allowlist_excludes_external_resource(self):
  from work_worker_store import WorkerDB
  from work_policy import POLICY_DIGEST
  t=self.task();self.call('enqueue','test-owner','homelab',t,user='forge_agent')
  self.pg.grant_worker('test-owner','homelab','muse-db-primary',('plan',))
  worker=WorkerDB(lambda:self.pg.connect('forge_coding_worker'),'test-owner');incarnation=uuid4()
  worker.advertise('native-fixture',incarnation,[dict(alias='homelab',base_revision='b'*40)],[dict(alias='muse-db-primary',backend='muse',model='external-runner',modes=['plan'])],POLICY_DIGEST)
  self.assertIsNone(worker.claim('native-fixture',incarnation,['muse-db-primary']))
  self.assertEqual(self.rows('SELECT count(*) FROM forge_agent.coding_attempts'),[(0,)])
 def test_schema_grant_option_and_function_body_drift_rejected(self):
  self.pg.apply_and_verify()
  self.pg.admin_exec('GRANT USAGE ON SCHEMA forge_external_agent_api TO muse_events WITH GRANT OPTION')
  self.assertNotEqual(self.pg.run_sql_script(SQL_DIR/'management/verify.sql').returncode,0)
  self.pg.admin_exec('REVOKE GRANT OPTION FOR USAGE ON SCHEMA forge_external_agent_api FROM muse_events')
  original=self.rows("SELECT pg_get_functiondef('forge_external_agent_api.capabilities()'::regprocedure)")[0][0]
  try:
   self.pg.admin_exec("CREATE OR REPLACE FUNCTION forge_external_agent_api.capabilities() RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $x$ BEGIN RETURN '[]'::jsonb;END;$x$")
   self.assertNotEqual(self.pg.run_sql_script(SQL_DIR/'management/verify.sql').returncode,0)
  finally:self.pg.admin_exec(original)
  self.pg.apply_and_verify()
if __name__=='__main__':unittest.main()
