"""Disposable PostgreSQL 16 integration tests for JenkinsStore and migration 0027 invariants."""

from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
import threading
import unittest
from uuid import UUID, uuid4

import psycopg2

from jenkins_config import parse_jenkins_config
from jenkins_events import JenkinsEventEmit, JenkinsEventKind
from jenkins_store import JenkinsStore
from queue_store import Rejected
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE

ORIGIN = "https://jenkins.internal:8443"
OCCURRED_AT = "2026-11-21T05:00:00Z"
SHA1 = "1" * 40
RECEIPT_SHA = "a" * 64


def make_config(**overrides):
    raw = {
        "owner": "mission-control",
        "allowed_projects": ["homelab", "docs"],
        "allowed_instances": {
            "ci-primary": {"origin": ORIGIN},
        },
        "allowed_jobs": {
            "homelab": ["forge/ci-main", "forge/nightly"],
            "docs": ["docs/build"],
        },
        "consumers": [
            {"name": "observation", "handler": "observation", "max_attempts": 3},
            {"name": "audit", "handler": "audit", "projects": ["homelab"], "max_attempts": 2},
        ],
        "max_pending_deliveries": 64,
        "lease_seconds": 30,
        "renew_seconds": 10,
        "poll_interval_seconds": 2,
        "backoff_seconds": 1,
        "max_backoff_seconds": 10,
    }
    raw.update(overrides)
    return parse_jenkins_config(raw)


def sample_event(**overrides):
    data = dict(
        project_alias="homelab",
        jenkins_instance="ci-primary",
        job_name="forge/ci-main",
        delivery_id="del-1",
        event_kind=JenkinsEventKind.BUILD_STARTED,
        occurred_at=OCCURRED_AT,
        summary="Build #1 started",
        build_number=1,
        commit_sha=SHA1,
        branch="main",
        build_url=f"{ORIGIN}/job/forge/job/ci-main/1/",
    )
    data.update(overrides)
    return JenkinsEventEmit(**data)


@unittest.skipUnless(PG16_AVAILABLE, "Disposable PostgreSQL 16 binary not available")
class JenkinsStorePostgresTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()

    @classmethod
    def tearDownClass(cls):
        cls.cluster.stop()

    def setUp(self):
        self.cluster.reset_jenkins_tables()

    def make_store(self, **cfg_overrides):
        return JenkinsStore(make_config(**cfg_overrides), self.cluster.connect)

    def _assert_narrowed_delivery_is_preserved(self, *, broad=None, narrowed=None, denied=None, admitted=None):
        broad_store = self.make_store(**(broad or {}))
        old = broad_store.emit(sample_event(delivery_id='revoked', **(denied or {})))
        before = broad_store.get_event(project_alias=(denied or {}).get('project_alias','homelab'),event_id=old['event_id'])
        selected = self.make_store(**(narrowed or {}))
        good = selected.emit(sample_event(delivery_id='still-admitted',build_number=2,**(admitted or {})))
        event,lease = selected.claim_next(('observation',),'scoped-consumer')
        self.assertEqual(event.event_id,good['event_id'])
        self.assertTrue(selected.complete_delivery(lease.delivery_row_id,lease.lease_token,RECEIPT_SHA))
        self.assertIsNone(selected.claim_next(('observation',),'scoped-consumer'))
        after = broad_store.get_event(project_alias=(denied or {}).get('project_alias','homelab'),event_id=old['event_id'])
        self.assertEqual(after,before,'Revoked deliveries remain intact and unclaimed, not failed or deleted')

    def test_claim_respects_current_project_allowlist(self):
        self._assert_narrowed_delivery_is_preserved(narrowed={'allowed_projects':['homelab'],
            'allowed_jobs':{'homelab':['forge/ci-main','forge/nightly']}},
            denied={'project_alias':'docs','job_name':'docs/build'})

    def test_claim_respects_current_instance_allowlist(self):
        self._assert_narrowed_delivery_is_preserved(broad={'allowed_instances':{
            'ci-primary':{'origin':ORIGIN},'ci-secondary':{'origin':'https://other.internal'}}},
            denied={'jenkins_instance':'ci-secondary','build_url':'https://other.internal/job/build/1/'})

    def test_claim_respects_current_job_allowlist(self):
        self._assert_narrowed_delivery_is_preserved(narrowed={'allowed_jobs':{
            'homelab':['forge/ci-main'],'docs':['docs/build']}},denied={'job_name':'forge/nightly'})

    def test_claim_respects_current_consumer_project_subscription(self):
        self._assert_narrowed_delivery_is_preserved(narrowed={'consumers':[
            {'name':'observation','handler':'observation','projects':['homelab']}]},
            denied={'project_alias':'docs','job_name':'docs/build'})

    def test_claim_respects_current_consumer_event_subscription(self):
        self._assert_narrowed_delivery_is_preserved(narrowed={'consumers':[
            {'name':'observation','handler':'observation','event_kinds':['build_completed']}]},
            admitted={'event_kind':'build_completed','build_result':'success'})

    def test_claim_respects_current_instance_origin(self):
        self._assert_narrowed_delivery_is_preserved(narrowed={'allowed_instances':{
            'ci-primary':{'origin':'https://new.internal'}}},
            admitted={'build_url':'https://new.internal/job/build/2/'})

    def test_explicit_consumer_selection_cannot_enable_disabled_or_unregistered_handlers(self):
        broad = self.make_store()
        result = broad.emit(sample_event())
        before = broad.get_event(project_alias='homelab',event_id=result['event_id'])
        selected = self.make_store(consumers=[{'name':'observation','handler':'observation','enabled':False},
            {'name':'audit','handler':'audit','enabled':True}])
        for names in (('observation',),('unknown',),('audit','audit'),('audit','observation')):
            with self.subTest(names=names),self.assertRaises(ValueError):selected.claim_next(names,'scoped-consumer')
        self.assertIsNone(selected.claim_next((),'scoped-consumer'))
        self.assertEqual(broad.get_event(project_alias='homelab',event_id=result['event_id']),before)

    def test_narrowed_store_cannot_renew_complete_fail_or_project_revoked_event(self):
        broad = self.make_store()
        broad.emit(sample_event(project_alias='docs',job_name='docs/build'))
        event,lease = broad.claim_next(('observation',),'original-consumer')
        before = broad.get_event(project_alias='docs',event_id=event.event_id)
        selected = self.make_store(allowed_projects=['homelab'],allowed_jobs={'homelab':['forge/ci-main','forge/nightly']})
        self.assertFalse(selected.renew_lease(lease.delivery_row_id,lease.lease_token))
        self.assertFalse(selected.complete_delivery(lease.delivery_row_id,lease.lease_token,RECEIPT_SHA))
        self.assertIsNone(selected.fail_delivery(lease.delivery_row_id,lease.lease_token,'revoked',retryable=False))
        with self.assertRaises(Rejected):selected.upsert_observation(event)
        with self.assertRaises(Rejected):selected.get_observation(project_alias='docs',jenkins_instance='ci-primary',
            job_name='docs/build',observation_key='build:1')
        self.assertEqual(broad.get_event(project_alias='docs',event_id=event.event_id),before)

    def test_atomic_emit_fanout_notify_and_zero_notify_on_rejection(self):
        store = self.make_store()
        with store.listen() as listener:
            # Rejection must not commit or notify.
            bad_event = sample_event(job_name="forge/unlisted")
            with self.assertRaises(Rejected) as ctx:
                store.emit(bad_event)
            self.assertEqual(ctx.exception.status, 403)
            self.assertFalse(store.wait_for_signal(listener, 0.15))

            receipt = store.emit(sample_event(delivery_id="del-notify-1"))
            self.assertFalse(receipt["deduplicated"])
            self.assertEqual(receipt["sequence"], 1)
            self.assertEqual(receipt["deliveries_queued"], 2)
            self.assertTrue(store.wait_for_signal(listener, 2.0))

            # docs project only subscribes observation consumer (audit is homelab-only).
            docs_receipt = store.emit(
                sample_event(
                    project_alias="docs",
                    job_name="docs/build",
                    delivery_id="del-docs-1",
                    build_url=f"{ORIGIN}/job/docs/job/build/1/",
                )
            )
            self.assertEqual(docs_receipt["sequence"], 1)
            self.assertEqual(docs_receipt["deliveries_queued"], 1)
            self.assertTrue(store.wait_for_signal(listener, 2.0))

    def test_idempotency_conflict_and_concurrent_duplicate_emit_race(self):
        store = self.make_store()
        event = sample_event(delivery_id="del-race-1")
        barrier = threading.Barrier(2)

        def _worker():
            barrier.wait(timeout=5)
            return store.emit(event)

        with ThreadPoolExecutor(max_workers=2) as pool:
            r1, r2 = list(pool.map(lambda _: _worker(), range(2)))

        self.assertEqual(r1["event_id"], r2["event_id"])
        self.assertEqual(r1["sequence"], r2["sequence"])
        self.assertEqual({r1["deduplicated"], r2["deduplicated"]}, {False, True})

        conflicting = sample_event(delivery_id="del-race-1", summary="Tampered summary")
        with self.assertRaises(Rejected) as ctx:
            store.emit(conflicting)
        self.assertEqual(ctx.exception.status, 409)
        self.assertEqual(ctx.exception.code, "idempotency_conflict")

    def test_queue_full_and_pagination_filters_and_no_lease_token_leak(self):
        store = self.make_store(max_pending_deliveries=2)
        first = store.emit(sample_event(delivery_id="del-q-1", build_number=1))
        self.assertEqual(first["deliveries_queued"], 2)

        # Next emit needs 2 deliveries -> exceeds max_pending_deliveries=2.
        with self.assertRaises(Rejected) as ctx:
            store.emit(sample_event(delivery_id="del-q-2", build_number=2))
        self.assertEqual(ctx.exception.status, 503)
        self.assertEqual(ctx.exception.code, "queue_full")

        # Claim one delivery to verify lease_token is hidden from public get/list views.
        claimed = store.claim_next(("observation",), "worker-1")
        self.assertIsNotNone(claimed)
        _, delivery = claimed
        self.assertIsNotNone(delivery.lease_token)

        opened = store.get_event(project_alias="homelab", event_id=first["event_id"])
        self.assertEqual(opened["event"]["event_id"], first["event_id"])
        self.assertEqual(len(opened["deliveries"]), 2)
        for item in opened["deliveries"]:
            self.assertNotIn("lease_token", item)

        listed_del = store.list_deliveries(project_alias="homelab", consumer_name="observation")
        self.assertEqual(len(listed_del["deliveries"]), 1)
        self.assertEqual(listed_del["deliveries"][0]["status"], "leased")
        self.assertNotIn("lease_token", listed_del["deliveries"][0])

        listed_ev = store.list_events(
            project_alias="homelab",
            job_name="forge/ci-main",
            event_kind=JenkinsEventKind.BUILD_STARTED,
            commit_sha=SHA1,
        )
        self.assertEqual(len(listed_ev["events"]), 1)
        self.assertFalse(listed_ev["has_more"])

        with self.assertRaises(ValueError):
            store.list_events(project_alias="homelab", limit=0)
        with self.assertRaises(ValueError):
            store.list_deliveries(project_alias="homelab", after_sequence=-1)

    def test_skip_locked_lease_reclaim_exhaustion_and_fencing(self):
        store = self.make_store()
        store.emit(sample_event(delivery_id="del-l-1", build_number=1))
        store.emit(sample_event(delivery_id="del-l-2", build_number=2))

        c1 = store.claim_next(("observation",), "worker-a", lease_seconds=30)
        c2 = store.claim_next(("observation",), "worker-b", lease_seconds=30)
        self.assertIsNotNone(c1)
        self.assertIsNotNone(c2)
        self.assertNotEqual(c1[1].delivery_row_id, c2[1].delivery_row_id)
        self.assertIsNone(store.claim_next(("observation",), "worker-c"))

        # Renew valid lease succeeds; wrong token rejected.
        self.assertTrue(store.renew_lease(c1[1].delivery_row_id, c1[1].lease_token))
        self.assertFalse(store.renew_lease(c1[1].delivery_row_id, uuid4()))

        # Force lease expiration on c1 -> worker-c reclaims with a new lease_token.
        self.cluster.admin_exec(
            f"UPDATE forge_agent.jenkins_event_deliveries "
            f"SET lease_expires_at = clock_timestamp() - interval '5 seconds' "
            f"WHERE delivery_row_id = '{c1[1].delivery_row_id}'::uuid"
        )
        reclaimed = store.claim_next(("observation",), "worker-c", lease_seconds=30)
        self.assertIsNotNone(reclaimed)
        self.assertEqual(reclaimed[1].delivery_row_id, c1[1].delivery_row_id)
        self.assertEqual(reclaimed[1].attempt_count, 2)
        self.assertNotEqual(reclaimed[1].lease_token, c1[1].lease_token)

        # Stale worker-a completion and failure are fenced out.
        self.assertFalse(store.complete_delivery(c1[1].delivery_row_id, c1[1].lease_token, RECEIPT_SHA))
        self.assertIsNone(
            store.fail_delivery(c1[1].delivery_row_id, c1[1].lease_token, "stale_err", retryable=True)
        )

        # Active worker-c completes cleanly.
        self.assertTrue(store.complete_delivery(reclaimed[1].delivery_row_id, reclaimed[1].lease_token, RECEIPT_SHA))

        # Expire c2 after reaching max_attempts=3 -> next claim transitions it to dead_lettered.
        self.cluster.admin_exec(
            f"UPDATE forge_agent.jenkins_event_deliveries "
            f"SET attempt_count = 3, lease_expires_at = clock_timestamp() - interval '5 seconds' "
            f"WHERE delivery_row_id = '{c2[1].delivery_row_id}'::uuid"
        )
        self.assertIsNone(store.claim_next(("observation",), "worker-d"))
        del_list = store.list_deliveries(project_alias="homelab", consumer_name="observation", status="dead_lettered")
        self.assertEqual(len(del_list["deliveries"]), 1)
        self.assertEqual(del_list["deliveries"][0]["delivery_row_id"], c2[1].delivery_row_id)

    def test_immutability_triggers_and_least_privilege_worker_isolation(self):
        store = self.make_store()
        emitted = store.emit(sample_event(delivery_id="del-imm-1"))
        claimed = store.claim_next(("observation",), "worker-imm")
        self.assertTrue(store.complete_delivery(claimed[1].delivery_row_id, claimed[1].lease_token, RECEIPT_SHA))

        # Even forge_agent_owner cannot UPDATE or DELETE jenkins_events (P0009 trigger).
        with self.assertRaises(psycopg2.Error) as upd_ctx:
            self.cluster.admin_exec(
                f"UPDATE forge_agent.jenkins_events SET summary = 'tampered' "
                f"WHERE event_id = '{emitted['event_id']}'::uuid"
            )
        self.assertEqual(upd_ctx.exception.pgcode, "P0009")

        with self.assertRaises(psycopg2.Error) as del_ctx:
            self.cluster.admin_exec(
                f"DELETE FROM forge_agent.jenkins_events WHERE event_id = '{emitted['event_id']}'::uuid"
            )
        self.assertEqual(del_ctx.exception.pgcode, "P0009")

        # Terminal delivery cannot be mutated even by forge_agent_owner (P0009 trigger).
        with self.assertRaises(psycopg2.Error) as term_ctx:
            self.cluster.admin_exec(
                f"UPDATE forge_agent.jenkins_event_deliveries SET status = 'pending', "
                f"receipt_digest = NULL, consumed_at = NULL "
                f"WHERE delivery_row_id = '{claimed[1].delivery_row_id}'::uuid"
            )
        self.assertEqual(term_ctx.exception.pgcode, "P0009")

        # Runner role cannot DELETE from any Jenkins table or UPDATE jenkins_events.
        with closing(self.cluster.connect("forge_agent")) as conn:
            with conn.cursor() as cur:
                for sql in (
                    "UPDATE forge_agent.jenkins_events SET summary = 'x'",
                    "DELETE FROM forge_agent.jenkins_events",
                    "DELETE FROM forge_agent.jenkins_event_deliveries",
                    "DELETE FROM forge_agent.jenkins_observations",
                ):
                    with self.subTest(sql=sql):
                        with self.assertRaises(psycopg2.errors.InsufficientPrivilege):
                            cur.execute(sql)
                        conn.rollback()

        # Worker roles have zero privileges on all Jenkins tables.
        for role in ("forge_gpu_worker", "forge_coding_worker", "forge_coding_reconciler"):
            with closing(self.cluster.connect(role)) as conn:
                with conn.cursor() as cur:
                    for table in ("jenkins_events", "jenkins_event_deliveries", "jenkins_observations"):
                        with self.subTest(role=role, table=table):
                            with self.assertRaises(psycopg2.errors.InsufficientPrivilege):
                                cur.execute(f"SELECT 1 FROM forge_agent.{table} LIMIT 1")
                            conn.rollback()


if __name__ == "__main__":
    unittest.main()
