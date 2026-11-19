"""Real PostgreSQL regressions for immutable usage attribution and poll ordering."""

from contextlib import closing
import unittest
from uuid import UUID, uuid4

from test_work_execution_and_usage import (
    BASE_REV,
    WORKSPACE_DIGEST,
    make_snapshot,
    make_stop_proof,
    make_submit,
    make_v2_config,
)
import work_api
import work_policy
from usage_store import UsageStore
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE
from work_worker_store import WorkerDB


@unittest.skipUnless(PG16_AVAILABLE, "Requires a non-root disposable PostgreSQL16 cluster")
class UsageStorageFencesTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.cluster.reset_coding_tables()
        self.config = make_v2_config()
        connect = lambda: self.cluster.connect("forge_agent")
        self.service = work_api.WorkServiceFactory(self.config, connect).create()
        self.usage = UsageStore(self.config, connect)
        self.db = WorkerDB(
            lambda: self.cluster.connect("forge_coding_worker"),
            self.config.owner,
            reconcile_connect_factory=lambda: self.cluster.connect("forge_coding_reconciler"),
        )
        for profile in ("codex-v2", "agy-v2"):
            self.cluster.grant_worker(self.config.owner, "homelab", profile)

    def _providers(self):
        return {row["alias"]: row for row in self.usage.overview(
            "homelab", ("codex-v2", "agy-v2")
        )["providers"]}

    def _record(self, poll, snapshot):
        return self.db.record_usage(
            "codex-v2", poll["generation"], poll["configuration_epoch"], snapshot
        )

    def _fail(self, poll, code):
        return self.db.fail_usage_poll(
            "codex-v2", poll["generation"], poll["configuration_epoch"], code
        )

    def test_resume_keeps_historic_tokens_counts_and_duration_on_original_profile(self):
        incarnation = str(uuid4())
        self.db.advertise(
            "usage-worker", incarnation,
            [{"alias": "homelab", "base_revision": BASE_REV}],
            [
                {"alias": "codex-v2", "backend": "codex", "model": "gpt-5.4", "modes": ["plan"]},
                {"alias": "agy-v2", "backend": "antigravity", "model": "gemini-2.5-pro", "modes": ["plan"]},
            ],
            work_policy.POLICY_DIGEST,
        )
        submitted = self.service.submit(make_submit(idempotency_key="usage-resume-profile"))
        task_id = UUID(submitted["task_id"])
        first = self.db.claim("usage-worker", incarnation, ["codex-v2"])
        self.assertIsNotNone(first)
        first_usage = {
            "input_tokens": "101", "output_tokens": "23", "cached_input_tokens": "7",
            "total_tokens": "124", "source": "codex-exec-turn-v1", "coverage": "complete",
            "session_provenance": "fresh_session",
        }
        proof = make_stop_proof(first.unit_name)
        original_publication = self.db.publish(first, "failed", None, first_usage, "review needed", proof)
        before = self._providers()["codex-v2"]["jobs"]
        self.assertEqual((before["known_attempts"], before["unknown_attempts"]), (1, 0))
        self.assertEqual(before["total_tokens"], "124")
        self.assertIsNotNone(before["elapsed_ms"])

        receipt_id = UUID(self.db.reconcile(first, WORKSPACE_DIGEST, proof))
        opened = self.service.get(work_api.WorkGet("homelab", task_id))
        self.service.resume(work_api.WorkResume(
            "homelab", task_id, opened["revision"], "usage-switch-to-agy",
            "agy-v2", receipt_id, "Explicit provider handoff",
        ))

        queued = self._providers()
        self.assertEqual(queued["codex-v2"]["jobs"], before)
        self.assertEqual(queued["agy-v2"]["jobs"]["known_attempts"], 0)
        self.assertIsNone(queued["agy-v2"]["jobs"]["total_tokens"])
        self.assertIsNone(queued["agy-v2"]["jobs"]["elapsed_ms"])

        second = self.db.claim("usage-worker", incarnation, ["agy-v2"])
        self.assertIsNotNone(second)
        self.assertEqual(second.task_id, first.task_id)
        self.db.publish(second, "failed", None, None, "usage unavailable", make_stop_proof(second.unit_name))
        after = self._providers()
        self.assertEqual(after["codex-v2"]["jobs"], before)
        self.assertEqual((after["agy-v2"]["jobs"]["known_attempts"],
                          after["agy-v2"]["jobs"]["unknown_attempts"]), (0, 1))
        self.assertIsNone(after["agy-v2"]["jobs"]["total_tokens"])
        self.assertIsNotNone(after["agy-v2"]["jobs"]["elapsed_ms"])
        self.assertEqual(self.usage.overview("homelab", ("codex-v2",))["providers"][0]["jobs"], before)

        with closing(self.cluster.connect("forge_agent")) as connection, connection.cursor() as cursor:
            cursor.execute(
                "SELECT agent_profile, pinned_recipe, recipe_digest FROM forge_agent.coding_attempts "
                "WHERE owner_id = %s AND project_alias = 'homelab' AND attempt_id = %s",
                (self.config.owner, first.attempt_id),
            )
            original_profile, original_recipe, original_digest = cursor.fetchone()
        self.assertEqual(original_profile, "codex-v2")
        self.assertEqual(original_recipe["backend"], "codex")
        self.assertEqual(original_digest, submitted["recipe_digest"])
        self.assertEqual(self.db.publish(first, "failed", None, first_usage, "review needed", proof),
                         original_publication)
        self.assertEqual(self._providers()["codex-v2"]["jobs"], before)

    def test_late_success_cannot_erase_a_newer_poll_failure_or_prior_good_snapshot(self):
        baseline = self.db.begin_usage_poll("codex-v2")
        self.assertTrue(self._record(baseline, make_snapshot(used_percent=10.0)))
        old_poll = self.db.begin_usage_poll("codex-v2")
        current_poll = self.db.begin_usage_poll("codex-v2")
        self._fail(current_poll, "timeout")

        self.assertFalse(self._record(old_poll, make_snapshot(used_percent=99.0)))
        observed = self._providers()["codex-v2"]
        self.assertEqual(observed["last_error"], "timeout")
        self.assertEqual(observed["allowance"]["buckets"][0]["used_percent"], 10.0)
        self.assertEqual(observed["snapshot_state"], "fresh")

        next_poll = self.db.begin_usage_poll("codex-v2")
        self.assertTrue(self._record(next_poll, make_snapshot(used_percent=20.0)))
        observed = self._providers()["codex-v2"]
        self.assertIsNone(observed["last_error"])
        self.assertEqual(observed["allowance"]["buckets"][0]["used_percent"], 20.0)

    def test_late_failure_cannot_replace_a_newer_poll_failure(self):
        baseline = self.db.begin_usage_poll("codex-v2")
        self.assertTrue(self._record(baseline, make_snapshot(used_percent=10.0)))
        old_poll = self.db.begin_usage_poll("codex-v2")
        current_poll = self.db.begin_usage_poll("codex-v2")
        self._fail(current_poll, "unavailable")
        self._fail(old_poll, "auth_required")

        observed = self._providers()["codex-v2"]
        self.assertEqual(observed["last_error"], "unavailable")
        self.assertEqual(observed["allowance"]["buckets"][0]["used_percent"], 10.0)


if __name__ == "__main__":
    unittest.main()
