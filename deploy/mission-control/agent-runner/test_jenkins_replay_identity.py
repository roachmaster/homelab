"""GB-3: Jenkins emit replay identity is the typed event fields, not payload_sha256.

payload_sha256 is still recorded on the row as evidence, but a replay of the
same event must deduplicate even when the stored digest was produced by a
different canonical encoding, and a real field change must still conflict.
"""

import unittest

from jenkins_events import JenkinsEventKind
from jenkins_store import _same_semantic_event
from queue_store import Rejected
from test_jenkins_store import JenkinsStorePostgresTest, sample_event


class JenkinsReplayIdentityTest(JenkinsStorePostgresTest):
    def _rewrite_stored_digest(self, event_id, digest):
        assert len(digest) == 64 and set(digest) <= set("0123456789abcdef")
        self.cluster.admin_exec(
            "ALTER TABLE forge_agent.jenkins_events DISABLE TRIGGER USER",
            f"UPDATE forge_agent.jenkins_events SET payload_sha256='{digest}' WHERE event_id='{event_id}'::uuid",
            "ALTER TABLE forge_agent.jenkins_events ENABLE TRIGGER USER",
        )

    def test_replay_deduplicates_when_recorded_digest_differs(self):
        store = self.make_store()
        first = store.emit(sample_event(delivery_id="del-replay-1"))
        self._rewrite_stored_digest(first["event_id"], "e" * 64)

        replay = store.emit(sample_event(delivery_id="del-replay-1"))

        self.assertTrue(replay["deduplicated"])
        self.assertEqual(replay["event_id"], first["event_id"])
        self.assertEqual(replay["sequence"], first["sequence"])
        self.assertEqual(replay["payload_sha256"], "e" * 64)

    def test_equivalent_timestamp_spelling_deduplicates(self):
        store = self.make_store()
        first = store.emit(sample_event(delivery_id="del-replay-ts"))
        replay = store.emit(sample_event(delivery_id="del-replay-ts", occurred_at="2026-11-20T23:00:00-06:00"))
        self.assertTrue(replay["deduplicated"])
        self.assertEqual(replay["event_id"], first["event_id"])

    def test_changed_typed_fields_still_conflict(self):
        store = self.make_store()
        store.emit(sample_event(delivery_id="del-replay-2"))
        for change in (
            {"summary": "Different summary"},
            {"build_number": 2, "build_url": "https://jenkins.internal:8443/job/forge/job/ci-main/2/"},
            {"commit_sha": "2" * 40},
            {"occurred_at": "2026-11-21T05:00:01Z"},
            {"branch": "other"},
            {"correlation_id": "corr-1"},
        ):
            with self.subTest(change=change):
                with self.assertRaises(Rejected) as ctx:
                    store.emit(sample_event(delivery_id="del-replay-2", **change))
                self.assertEqual((ctx.exception.status, ctx.exception.code), (409, "idempotency_conflict"))

    def test_kind_change_conflicts(self):
        store = self.make_store()
        store.emit(sample_event(delivery_id="del-replay-3"))
        with self.assertRaises(Rejected) as ctx:
            store.emit(sample_event(
                delivery_id="del-replay-3",
                event_kind=JenkinsEventKind.BUILD_QUEUED,
                build_number=None,
                queue_id=7,
                build_url=None,
            ))
        self.assertEqual(ctx.exception.code, "idempotency_conflict")


class SemanticComparisonUnitTest(unittest.TestCase):
    def test_missing_optional_columns_compare_as_absent(self):
        event = sample_event()
        row = dict(event.semantic_payload())
        self.assertTrue(_same_semantic_event(event, row))
        row["branch_scope"] = "main"
        self.assertFalse(_same_semantic_event(event, row))


def load_tests(loader, tests, pattern):
    suite = unittest.TestSuite()
    suite.addTests(loader.loadTestsFromNames([
        f"{__name__}.JenkinsReplayIdentityTest.test_replay_deduplicates_when_recorded_digest_differs",
        f"{__name__}.JenkinsReplayIdentityTest.test_equivalent_timestamp_spelling_deduplicates",
        f"{__name__}.JenkinsReplayIdentityTest.test_changed_typed_fields_still_conflict",
        f"{__name__}.JenkinsReplayIdentityTest.test_kind_change_conflicts",
    ]))
    suite.addTests(loader.loadTestsFromTestCase(SemanticComparisonUnitTest))
    return suite


if __name__ == "__main__":
    unittest.main()
