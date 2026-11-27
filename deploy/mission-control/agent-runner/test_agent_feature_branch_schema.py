"""M51 accepts owned feature refs while retaining actual lease/scope/audit checks."""
from contextlib import closing
from uuid import UUID, uuid4
import unittest
import psycopg2

from work_api import WorkReviewDecide
from work_agent_branch import branch_scope_for_branch
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
from work_worker_store import WorkerStoreError
import test_native_jenkins_sql_contract as native
import test_work_feedback as feedback


@unittest.skipUnless(PG16_AVAILABLE, 'Requires disposable PostgreSQL16')
class AgentFeatureBranchSchemaTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def test_feature_publication_finish_and_constraint_keep_other_refs_rejected(self):
        f = feedback.FeedbackStoreTest(); f.cluster = self.cluster; f.setUp()
        lease, receipt = f.task(); review = f.db.queue_review(lease, receipt)
        job = f.db.review_claim('reviewer'); f.db.review_finish(job, f.outcome())
        detail = f.get(lease)
        f.service.review_decide(WorkReviewDecide('homelab', UUID(lease.task_id), detail['revision'],
            UUID(review), 'approve', 'codex-v2', 'Controlled source review.', 'approve-feature'))
        publication = f.db.publication_claim('publisher')
        for bad in ('main', 'feature/unknown', 'feature/grok..x', 'feature/grok/', 'x' * 129):
            with self.subTest(branch=bad), self.assertRaises((WorkerStoreError, ValueError, psycopg2.Error)):
                f.db.publication_finish(publication, 'publishing', 'Controlled fixture.', branch=bad, commit_sha='d'*40)
        f.db.publication_finish(publication, 'publishing', 'Controlled feature fixture.',
                                branch='feature/grok', commit_sha='d'*40)
        result = f.db.publication_finish(publication, 'published', 'Controlled feature fixture.',
            branch='feature/grok', commit_sha='d'*40, pr_url='https://github.com/fixture/project/pull/1')
        self.assertEqual(f.db.publication_finish(publication, 'published', 'Controlled feature fixture.',
            branch='feature/grok', commit_sha='d'*40, pr_url='https://github.com/fixture/project/pull/1'), result)

    def test_native_feature_receipt_preserves_profile_scope_epoch_token_and_replay(self):
        fixture = native.NativeJenkinsDatabaseContractTest(); fixture.cluster = self.cluster; fixture.setUp()
        admitted, author, job = fixture.local_review(); db = fixture.fixture.db
        context = db.native_ci_acquire_context(job)
        branch = 'feature/agy'
        reservation = db.agent_branch_acquire(project_alias='homelab', agent_profile='agy-v2',
            branch_name=branch, branch_scope=branch_scope_for_branch(branch), scope_kind='native_workflow',
            scope_id=context['workflow_id'], root_task_id=admitted['task_id'], task_id=author.task_id)
        scope = {k: context[k] for k in ('owner_id','project_alias','task_id','attempt_id',
                                        'review_id','workflow_id','correlation_id')}
        receipt = dict(version='forge-native-branch-v2', scope=scope, candidate_commit='d'*40,
            branch_publication_id=job['review_id'], branch=branch, agent_profile='agy-v2',
            reservation_epoch=reservation['reservation_epoch'])
        for changed in ({'branch':'feature/grok'}, {'branch':'main'}, {'agent_profile':'grok-primary'},
                        {'reservation_epoch':999}, {'scope':{**scope,'workflow_id':str(uuid4())}}):
            with self.subTest(changed=changed), self.assertRaises((WorkerStoreError, ValueError, psycopg2.Error)):
                db.native_agent_branch_published(job, {**receipt, **changed})
        with self.assertRaises((WorkerStoreError, ValueError, psycopg2.Error)):
            db.native_agent_branch_published({**job,'lease_token':str(uuid4())}, receipt)
        event = db.native_agent_branch_published(job, receipt)
        self.assertEqual(db.native_agent_branch_published(job, receipt), event)
        with self.assertRaises((WorkerStoreError, ValueError, psycopg2.Error)):
            db.native_agent_branch_published(job, {**receipt,'candidate_commit':'e'*40})

    def test_strict_verifier_rejects_function_source_or_acl_and_constraint_drift(self):
        verifier = (SQL_DIR/'management/verify-agent-feature-branches.sql').read_text()
        for signature in ('forge_coding_reconciler_api.publication_finish(uuid,uuid,text,text,text,text,text)',
                          'forge_coding_reconciler_api.native_agent_branch_published(uuid,uuid,jsonb)'):
            with self.subTest(signature=signature), closing(self.cluster.connect('postgres')) as c:
                try:
                    with c.cursor() as q:
                        q.execute('SELECT pg_get_functiondef(%s::regprocedure)', (signature,))
                        definition = q.fetchone()[0]
                        q.execute(definition.replace('BEGIN', 'BEGIN\n -- Unreviewed source drift', 1))
                        with self.assertRaises(psycopg2.Error): q.execute(verifier)
                finally: c.rollback()
        for sql in ("GRANT EXECUTE ON FUNCTION forge_coding_reconciler_api.native_agent_branch_published(uuid,uuid,jsonb) TO forge_agent",
                    "ALTER TABLE forge_agent.coding_publications DROP CONSTRAINT coding_publications_branch_check"):
            with self.subTest(sql=sql), closing(self.cluster.connect('postgres')) as c:
                try:
                    with c.cursor() as q:
                        q.execute(sql)
                        with self.assertRaises(psycopg2.Error): q.execute(verifier)
                finally: c.rollback()


if __name__ == '__main__': unittest.main()
