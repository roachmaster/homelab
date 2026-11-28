"""Exercise the actual stage-log authorization SQL with an in-memory database."""
from contextlib import contextmanager
import json
import sqlite3
import unittest
from uuid import uuid4

from queue_store import Rejected
from work_store import WorkStore


class SqlCursor:
    def __init__(self, connection):
        self.connection = connection
        self.rows = []
        self.description = None

    def execute(self, query, parameters=()):
        if "to_regclass('forge_agent.jenkins_stage_results')" in query:
            self.rows = [{'has_table': True}]
            self.description = [('has_table',)]
            return
        # SQLite executes the production predicates; only PostgreSQL's array
        # parameter spelling and namespace differ in this owned fixture.
        query = query.replace('forge_agent.', '')
        if isinstance(parameters, dict):
            # The proxy registry spelling: named parameters, bound by
            # name; array parameters keep the ANY -> json_each form.
            import re
            query = re.sub(r'= ANY\(\%\((\w+)\)s::\w+\[\]\)',
                r'IN (SELECT value FROM json_each(:\1))', query)
            query = re.sub(r'\%\((\w+)\)s', r':\1', query)
            values = {key: (json.dumps([str(v) for v in value])
                            if isinstance(value, list)
                            else str(value) if hasattr(value, 'hex') else value)
                      for key, value in parameters.items()}
            result = self.connection.execute(query, values)
        else:
            query = query.replace('= ANY(%s)',
                'IN (SELECT value FROM json_each(%s))').replace('%s', '?')
            values = [json.dumps([str(v) for v in value]) if isinstance(value, list)
                      else str(value) if hasattr(value, 'hex') else value for value in parameters]
            result = self.connection.execute(query, values)
        self.description = result.description
        self.rows = [dict(row) for row in result]

    def fetchone(self):
        return self.rows.pop(0) if self.rows else None

    def fetchall(self):
        rows, self.rows = self.rows, []
        return rows


class MainStageLogBindingTest(unittest.TestCase):
    def setUp(self):
        self.db = sqlite3.connect(':memory:')
        self.addCleanup(self.db.close)
        self.db.row_factory = sqlite3.Row
        self.db.executescript('''
          CREATE TABLE coding_tasks(id,correlation_id,workflow_id,repair_root_task_id,owner_id,project_alias);
          CREATE TABLE jenkins_stage_results(owner_id,project_alias,action_id,correlation_id,commit_sha,
            branch_scope,pipeline_scope,stage_index,phase_id,logical_stage_id,label,status,returncode,
            tests_passed,tests_failed,tests_skipped,test_summary,error_excerpt,log_sha256,log_byte_count,log_chunk_count);
        ''')
        self.task, self.correlation, self.action, self.generated = (uuid4() for _ in range(4))
        self.commit = 'a' * 40
        self.db.execute('INSERT INTO coding_tasks VALUES(?,?,NULL,NULL,?,?)',
            (str(self.task), str(self.correlation), 'owner', 'homelab'))
        self.store = WorkStore('owner', ('homelab',), lambda: None)

        @contextmanager
        def transaction(**_options):
            yield SqlCursor(self.db)

        self.store._transaction = transaction
        self.delivery = {'verification_id': str(self.action), 'commit_sha': self.commit,
                         'verification': {'status': 'running'}}
        self.store._delivery_projection = lambda _cursor, _project, task: self.delivery if task == self.task else None

    def insert_stage(self, **changes):
        row = dict(owner_id='owner', project_alias='homelab', action_id=str(self.action),
            correlation_id=str(self.generated), commit_sha=self.commit, branch_scope='main',
            pipeline_scope='main', stage_index=0, phase_id='all-features-test', logical_stage_id='check',
            label='tests', status='failed', returncode=1, error_excerpt='Bound main diagnostic')
        row.update(changes)
        self.db.execute('INSERT INTO jenkins_stage_results ('+','.join(row)+') VALUES ('+
                        ','.join('?' for _ in row)+')', list(row.values()))

    def read(self, **changes):
        request = dict(project_alias='homelab', task_id=self.task, correlation_id=self.correlation,
                       action_id=self.action, stage_id='check', view='errors')
        request.update(changes)
        return self.store.cicd_stage_log(**request)

    def test_exact_main_binding_reads_generated_correlation(self):
        self.insert_stage()
        self.assertEqual(self.read()['content'], 'Bound main diagnostic')

    def test_other_action_commit_owner_project_or_scope_does_not_grant_access(self):
        for change in ({'action_id': str(uuid4())}, {'commit_sha': 'b'*40},
                       {'owner_id': 'other'}, {'project_alias': 'other'},
                       {'branch_scope': 'branch/other'}, {'pipeline_scope': 'branch'}):
            with self.subTest(change=change):
                self.db.execute('DELETE FROM jenkins_stage_results')
                self.insert_stage(**change)
                with self.assertRaises(Rejected) as failure:
                    self.read()
                self.assertEqual(failure.exception.status, 404)

    def test_unrelated_task_or_wrong_request_correlation_is_denied(self):
        self.insert_stage()
        other, correlation = uuid4(), uuid4()
        self.db.execute('INSERT INTO coding_tasks VALUES(?,?,NULL,NULL,?,?)',
                        (str(other), str(correlation), 'owner', 'homelab'))
        with self.assertRaises(Rejected) as failure:
            self.read(task_id=other, correlation_id=correlation)
        self.assertEqual(failure.exception.status, 404)
        with self.assertRaises(Rejected) as failure:
            self.read(correlation_id=self.generated)
        self.assertEqual(failure.exception.status, 403)

    def test_missing_or_malformed_delivery_cannot_widen_access(self):
        self.insert_stage()
        for value in (None, {}, {**self.delivery, 'verification_id': 'invalid'},
                      {**self.delivery, 'commit_sha': 'A'*40}, {**self.delivery, 'verification': None}):
            with self.subTest(delivery=value):
                self.delivery = value
                with self.assertRaises(Rejected):
                    self.read()

    def test_existing_family_correlation_still_authorizes_branch_log(self):
        self.delivery = None
        self.insert_stage(correlation_id=str(self.correlation), branch_scope='branch/task', pipeline_scope='branch')
        self.assertEqual(self.read()['content'], 'Bound main diagnostic')


if __name__ == '__main__':
    unittest.main()
