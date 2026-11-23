"""Real external assignment admission uses independent persisted event budgets."""
from contextlib import closing
import hashlib
import unittest
from uuid import UUID

from psycopg2.extras import RealDictCursor
from work_pg_fixture import PG16_AVAILABLE, SQL_DIR
from work_external_review_context import canonical
from work_api import WorkServiceFactory
import test_external_flow as fixture


def apply_retry_event_migration(pg):
    with closing(pg.connect('postgres')) as connection,connection.cursor() as cursor:
        cursor.execute("SELECT to_regclass('forge_agent.coding_retry_events') IS NOT NULL")
        if cursor.fetchone()[0]:return
    wrapper=pg.root/'test-apply-retry-events.sql'
    wrapper.write_text("BEGIN; SET LOCAL ROLE forge_agent_owner;\n\\ir "+str(SQL_DIR/'migrations/0031_retry_events.sql')+"\nINSERT INTO forge_agent.schema_migrations(version,name) VALUES(31,'retry_events'); COMMIT;\n")
    result=pg.run_sql_script(wrapper)
    if result.returncode:raise AssertionError(result.stderr)


@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class ExternalRetryEventPGTest(unittest.TestCase):
    call=fixture.ExternalFlowPGTest.call
    rows=fixture.ExternalFlowPGTest.rows
    review=fixture.ExternalFlowPGTest.review
    author=fixture.ExternalFlowPGTest.author
    submit_plan=fixture.ExternalFlowPGTest.submit_plan

    @classmethod
    def setUpClass(cls):
        fixture.ExternalFlowPGTest.setUpClass.__func__(cls)
        apply_retry_event_migration(cls.pg)

    @classmethod
    def connect(cls):return cls.pg.connect('forge_agent')

    def setUp(self):
        fixture.ExternalFlowPGTest.setUp(self)
        self.pg.admin_exec('TRUNCATE forge_agent.coding_retry_events CASCADE')

    def service(self):return WorkServiceFactory(fixture.cfg(),self.connect).create()

    def finish_author(self,number):
        job=self.call('claim','homelab',fixture.ALIAS)
        self.assertIsNotNone(job)
        self.call('finish',job['assignment_id'],job['generation'],job['token'],
            'Complete immutable candidate '+str(number),'Candidate returned')
        return job

    def admit_repair(self,flow,kind,number,*,passed_kinds=(),phase_id=None,passed_phase_ids=()):
        with flow._transaction() as cursor:
            cursor.execute('SELECT * FROM forge_external_workflow.workflows FOR UPDATE')
            w=cursor.fetchone();flow._scope(cursor,'homelab',fixture.ALIAS)
            artifact=flow._artifact(cursor,w,flow._task(cursor,w))
            flow._revise(cursor,w,'code',{'summary':'Independently observed '+kind+' failure.','findings':[]},artifact,
                retry_kind=kind,source_id='protected-check:'+str(number),
                evidence_digest=hashlib.sha256(('protected check '+str(number)).encode()).hexdigest(),
                passed_kinds=passed_kinds,phase_id=phase_id,passed_phase_ids=passed_phase_ids)
            event=flow._project_retry_event(cursor,w)
            return dict(w),event

    def test_twenty_compilation_then_twenty_test_repairs_in_the_same_workflow(self):
        _,root,flow,_=self.submit_plan()
        self.review(flow)
        self.finish_author(0)
        compilation=None
        for number in range(1,21):
            w,event=self.admit_repair(flow,'compilation',number,phase_id='rust_compile')
            compilation=compilation or event['event_id']
            self.assertEqual((event['event_id'],event['used']), (compilation,number))
            self.finish_author(number)
        tests=None
        for number in range(1,21):
            w,event=self.admit_repair(flow,'tests',20+number,passed_kinds=('compilation',),
                phase_id='rust_tests',passed_phase_ids=('rust_compile',))
            tests=tests or event['event_id']
            self.assertNotEqual(tests,compilation)
            self.assertEqual((event['event_id'],event['used']), (tests,number))
            self.assertEqual(w['code_revisions'],20+number)
            self.finish_author(20+number)
        w,event=self.admit_repair(flow,'tests',41,phase_id='rust_tests')
        self.assertEqual((event['event_id'],event['used'],event['status']), (tests,20,'exhausted'))
        self.assertEqual((w['phase'],w['code_revisions']),('escalated',40))
        self.assertIsNone(self.call('claim','homelab',fixture.ALIAS))
        events=self.rows('SELECT kind,phase_id,used,status FROM forge_agent.coding_retry_events ORDER BY kind')
        self.assertEqual([(r['kind'],r['used'],r['status']) for r in events],
            [('compilation',20,'resolved'),('tests',20,'exhausted')])
        self.assertEqual(self.rows('SELECT count(*) AS n FROM forge_agent.coding_retry_event_admissions')[0]['n'],40)
        self.assertEqual(self.rows('SELECT count(*) AS n FROM forge_external_agent.artifacts')[0]['n'],42)

    def test_two_distinct_test_phases_do_not_share_one_tests_counter(self):
        _,_,flow,_=self.submit_plan();self.review(flow);self.finish_author(0)
        for number in range(1,21):
            _,rust=self.admit_repair(flow,'tests',number,phase_id='rust_tests')
            self.finish_author(number)
        w,backend=self.admit_repair(flow,'tests',21,phase_id='backend_tests',passed_phase_ids=('rust_tests',))
        self.assertNotEqual(rust['event_id'],backend['event_id'])
        self.assertEqual((backend['used'],w['code_revisions']), (1,21))
        self.assertEqual(self.rows("SELECT status FROM forge_agent.coding_retry_events WHERE phase_id='rust_tests'")[0]['status'],'resolved')


if __name__=='__main__':unittest.main()
