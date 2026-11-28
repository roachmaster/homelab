"""Bounded release scheduling; fake clocks and owned database ports only."""
from copy import deepcopy
from dataclasses import replace
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import threading
import unittest
from unittest.mock import Mock

from work_delivery_batch import ReleaseBatch, RELEASE_TURN_LIMIT
from work_delivery_worker import ReleaseWorker
from work_publication import PublicationWorker, PublicationWorkerFactory
from work_publication_clients import PublicationError
import test_delivery_worker as release_fixture
import test_work_delivery_automation as db_fixture
from work_pg_fixture import PG16_AVAILABLE


class Clock:
    def __init__(self): self.now = 0
    def advance(self, seconds): self.now += seconds


class ScheduledReleases:
    """Scheduling model of existing pending+5s and unstarted finish ports."""
    def __init__(self, clock):
        self.clock, self.claims, self.active = clock, [], None
        self.rows = [{'id':name,'status':'queued','due':0,'generation':0}
                     for name in ('poison','stale1','stale2','stale3','fresh')]
        self.journal = {'phase':'rolling_back','cleanup_confirmed':False}
        self.budgets = {'repair_rounds':7,'infrastructure_retries':11}
        self.actor_calls = []
    def run_once(self):
        assert self.active is None
        row = next((r for r in self.rows if r['status']=='queued' and r['due']<=self.clock.now),None)
        if row is None: return False
        self.active = row['id'];self.claims.append(row['id']);row['generation'] += 1
        if row['id']=='poison':
            row['due'] = self.clock.now+5
        elif row['id'].startswith('stale'):
            # Actual SQL-port coverage below proves this no-actor transition.
            row.update(status='superseded',receipt={'process_started':False})
            self.clock.advance(6)
        else:
            row['status']='current_verified_claimed'
        self.active = None
        return True


class BatchTest(unittest.TestCase):
    def publisher(self, release):
        store = Mock();store.publication_claim.return_value = None
        return PublicationWorker(SimpleNamespace(worker=SimpleNamespace(worker_id='test')),
            store,None,None,None,None,candidates=None,release=release)
    def test_five_second_publisher_poll_repeats_poison_but_batch_reaches_fresh_within_eight(self):
        clock=Clock();old=ScheduledReleases(clock);publisher=self.publisher(old)
        for _ in range(4):
            self.assertTrue(publisher.run_once())
            self.assertFalse(publisher.run_once())
            clock.advance(5)
        self.assertEqual(old.claims,['poison']*4)
        clock=Clock();ready=ScheduledReleases(clock)
        before_journal,before_budgets=deepcopy(ready.journal),deepcopy(ready.budgets)
        publisher=self.publisher(ReleaseBatch(ready,stop=threading.Event()))
        self.assertTrue(publisher.run_once())
        self.assertEqual(ready.claims,['poison','stale1','poison','stale2','poison','stale3','poison','fresh'])
        self.assertEqual(ready.rows[-1]['status'],'current_verified_claimed')
        self.assertEqual(ready.rows[0]['status'],'queued')
        self.assertEqual((ready.journal,ready.budgets),(before_journal,before_budgets))
        self.assertEqual(ready.actor_calls,[])
        self.assertFalse(publisher.run_once())  # ordinary yield and idle backoff remain
    def test_idle_limit_cancellation_and_exceptions_remain_bounded(self):
        stop=threading.Event();ready=Mock();ready.run_once.return_value=False
        self.assertFalse(ReleaseBatch(ready,stop=stop).run_once());ready.run_once.assert_called_once_with()
        ready.reset_mock();ready.run_once.return_value=True
        self.assertTrue(ReleaseBatch(ready,stop=stop).run_once());self.assertEqual(ready.run_once.call_count,8)
        ready.reset_mock();ready.run_once.side_effect=lambda:stop.set() or True
        self.assertTrue(ReleaseBatch(ready,stop=stop).run_once());ready.run_once.assert_called_once_with()
        ready.reset_mock();self.assertFalse(ReleaseBatch(ready,stop=stop).run_once());ready.run_once.assert_not_called()
        stop.clear();ready.run_once.side_effect=[True,RuntimeError('store unavailable')]
        with self.assertRaisesRegex(RuntimeError,'store unavailable'):ReleaseBatch(ready,stop=stop).run_once()
        self.assertEqual(ready.run_once.call_count,2)
        for limit in (False,0,9):
            with self.assertRaises(ValueError):ReleaseBatch(ready,stop=stop,limit=limit)
    def test_unchanged_release_worker_processes_pending_then_verified_fresh_with_one_lease(self):
        poison=release_fixture.job_for();poison.update(actor_started=True,receipt={'reconciliation_required':True,'cleanup_confirmed':False})
        fresh=release_fixture.job_for()
        class Store:
            def __init__(self): self.jobs=[poison,fresh];self.active=None;self.records=[];self.job={}
            def delivery_release_claim(self,worker):
                assert self.active is None
                if not self.jobs:return None
                self.active=self.jobs.pop(0);self.job=deepcopy(self.active);return deepcopy(self.active)
            def delivery_release_renew(self,job):return self.active is not None and self.active['release_id']==job['release_id']
            def delivery_release_start_allowed(self,job):return job['release_id']==fresh['release_id']
            def delivery_release_checkpoint(self,job,plan):self.job['request']=deepcopy(plan);return True
            def delivery_release_finish(self,job,status,receipt,summary):
                assert self.active['release_id']==job['release_id']
                self.records.append((job['release_id'],status,deepcopy(receipt)));self.active=None;return True
        store=Store()
        class Provider(release_fixture.Provider):
            def reconcile(self,job,plan,require_lease):
                require_lease();raise PublicationError('release_actor_failed')
        provider=Provider(fresh,store)
        config=SimpleNamespace(release_enabled=True,repositories={'homelab':'fixture/project'},worker=SimpleNamespace(owner='fixture',worker_id='test'))
        self.assertTrue(ReleaseBatch(ReleaseWorker(config,store,provider),stop=threading.Event()).run_once())
        self.assertEqual([r[1]for r in store.records],['pending','deployed'])
        self.assertTrue(store.records[0][2]['reconciliation_required'])
        self.assertNotIn('activation_receipt_sha256',store.records[0][2])
        self.assertEqual(poison['receipt']['cleanup_confirmed'],False)
        self.assertIsNone(store.active)
    def test_normal_factory_wraps_only_selected_ready_release_and_shares_cancellation(self):
        from test_work_post_merge import configuration
        from work_ci_contract import SCOPED_PROFILE
        with TemporaryDirectory()as temp:
            cfg,_=configuration(Path(temp))
            cfg=replace(cfg,delivery_enabled=True,release_enabled=True,release_actor_sha256='a'*64,
                        jenkins_verification_profile=SCOPED_PROFILE)
            stop=threading.Event();ready=Mock();ready.run_once.return_value=False
            store=Mock();store.delivery_available.return_value=True
            selected=Mock(return_value=ready);release_provider=Mock()
            factory=PublicationWorkerFactory(cfg,Mock(return_value=store),stop=stop,
                workspace_factory=Mock(),snapshot_factory=Mock(),git_factory=Mock(),supervisor_factory=Mock(),
                pull_request_factory=Mock(),candidate_factory=Mock(),reviewer_factory=Mock(),
                merge_worker_factory=Mock(),merge_provider_factory=Mock(),source_sync_factory=Mock(),
                native_jenkins_factory=Mock(),post_merge_provider_factory=Mock(),post_merge_worker_factory=Mock(),
                release_provider_factory=release_provider,release_worker_factory=selected)
            selected.assert_not_called();worker=factory.create()
            batch=worker.automation[-1]
            self.assertIsInstance(batch,ReleaseBatch);self.assertIs(batch.worker,ready)
            self.assertIs(batch.stop,stop);self.assertIs(worker.stop,stop)
            ready.run_once.assert_not_called();selected.assert_called_once()
            self.assertEqual(selected.call_args.kwargs,{'stop':stop})
            selected.reset_mock();release_provider.reset_mock()
            factory.config=replace(cfg,release_enabled=False,release_actor_sha256=None)
            held=factory.create();selected.assert_not_called();release_provider.assert_not_called()
            self.assertFalse(any(isinstance(port,ReleaseBatch)for port in held.automation))


@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class ReleaseBatchPortTest(unittest.TestCase):
    setUpClass=classmethod(db_fixture.DeliveryAutomationPGTest.setUpClass.__func__)
    setUp=db_fixture.DeliveryAutomationPGTest.setUp
    rows=db_fixture.DeliveryAutomationPGTest.rows
    observe=db_fixture.DeliveryAutomationPGTest.observe
    root_authorize=db_fixture.DeliveryAutomationPGTest.root_authorize
    verification=db_fixture.DeliveryAutomationPGTest.verification
    def test_existing_sql_ports_preserve_started_pending_and_supersede_only_unstarted_stale(self):
        self.verification();poison=self.db.delivery_release_claim('seed')
        self.assertTrue(self.root_authorize(poison)['allowed'])
        self.pg.admin_exec("UPDATE forge_agent.delivery_lineages SET rounds=7 WHERE lineage_id='"+poison['lineage_id']+"'")
        retained={'reconciliation_required':True,'operator_required':True,'cleanup_confirmed':False}
        self.db.delivery_release_finish(poison,'pending',retained,'Original unresolved journal')
        for index,char in enumerate('bcde'):
            self.observe(char,char,'abcde'[index]*40)
            verify=self.db.delivery_claim('verify');value=db_fixture.request(verify)
            self.db.delivery_checkpoint(verify,value)
            self.db.delivery_finish(verify,'passed',db_fixture.passed(value),'Actual fixture main proof')
        original=self.rows('SELECT receipt,actor_started,request FROM forge_agent.delivery_operations WHERE operation_id=%s',(poison['release_id'],))[0]
        def make_poison_due():
            self.pg.admin_exec("UPDATE forge_agent.delivery_operations SET next_poll_at=clock_timestamp()-interval '1 second' WHERE operation_id='"+poison['release_id']+"'")
        make_poison_due();db=self.db;claims=[];active=[]
        class NormalPortTurn:
            def run_once(inner):
                assert not active
                job=db.delivery_release_claim('batch-test')
                if job is None:return False
                active.append(job['release_id']);claims.append(job)
                if job['release_id']==poison['release_id']:
                    db.delivery_release_finish(job,'pending',retained,'Original unresolved journal')
                elif not db.delivery_release_start_allowed(job):
                    self.assertFalse(job['actor_started']);self.assertIsNone(job['request'])
                    db.delivery_release_finish(job,'superseded',{'process_started':False},'Main advanced before Root effects')
                    make_poison_due()  # model a >5s turn without sleeping or live clocks
                else:
                    db.delivery_release_finish(job,'pending',{'fresh_verified_claim_observed':True},'No test actor execution')
                active.pop();return True
        self.assertTrue(ReleaseBatch(NormalPortTurn(),stop=threading.Event()).run_once())
        self.assertEqual(len(claims),8)
        self.assertEqual([job['commit_sha'][0]for job in claims],list('abacadae'))
        self.assertEqual(len(self.rows("SELECT * FROM forge_agent.delivery_operations WHERE kind='release' AND status='superseded'")),3)
        self.assertEqual(self.rows('SELECT receipt,actor_started,request FROM forge_agent.delivery_operations WHERE operation_id=%s',(poison['release_id'],))[0],original)
        self.assertEqual(self.rows('SELECT sum(rounds) AS rounds FROM forge_agent.delivery_lineages')[0]['rounds'],7)
        self.assertEqual(active,[])

if __name__=='__main__':unittest.main()
