"""Dormant external intake contracts; fake ports and owned disposable PG only."""
from contextlib import closing
from dataclasses import replace
import hashlib
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
from uuid import UUID, uuid4

from work_muse_consumer import (MuseIntakeConsumer, MuseIntakeConsumerFactory,
                                apply_publish_result, default_store_factory,
                                default_intake_factory, main)
from work_muse_intake import (MUSE_MODEL, MUSE_PROFILE, ExternalIntakeUnsupported,
    IntakeError, IntakeStore, WorkerIntakeStore, chunk_content, parse_intake_config,
    load_intake_config, policy_digest, result_manifest, validate_progress,
    validate_publish_result, validate_submit_plan)
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE

BASE_REV = "a" * 40


def raw_config():
    return {"worker_id": "future-muse-intake", "owner": "mission-control",
            "database": {"host": "/var/run/postgresql", "port": 5432,
                         "name": "forge_agent", "user": "muse_submission_proposed"},
            "profiles": {MUSE_PROFILE: {"backend": "muse", "model": MUSE_MODEL,
                                      "modes": ["plan"]}},
            "projects": {"homelab": BASE_REV}}


class IntakeValidationTest(unittest.TestCase):
    def test_submit_plan_valid_and_defaults(self):
        result = validate_submit_plan({
            "project_alias": "homelab", "base_revision": BASE_REV,
            "plan": "# Title\n\nBody text.",
        })
        self.assertEqual(result["reviewer_profile"], "codex-primary")
        self.assertEqual(result["plan"], "# Title\n\nBody text.")

    def test_submit_plan_rejects_bad_payloads(self):
        good = {"project_alias": "homelab", "base_revision": BASE_REV, "plan": "Plan."}
        cases = [
            {**good, "base_revision": "xyz"},
            {**good, "base_revision": BASE_REV.upper()},
            {**good, "plan": "   "},
            {**good, "plan": "x" * (1024 * 1024 + 1)},
            {**good, "reviewer_profile": "muse-primary"},
            {**good, "reviewer_profile": "not an identifier!"},
            {**good, "title": "extra field"},
            {"project_alias": "homelab", "base_revision": BASE_REV},
            {"project_alias": "homelab", "plan": "Plan."},
            "not a dict",
        ]
        for payload in cases:
            with self.subTest(payload=str(payload)[:40]), self.assertRaises(IntakeError):
                validate_submit_plan(payload)

    def test_publish_result_valid_and_rejects(self):
        result = validate_publish_result({"chunks": ["hello ", "world"]})
        self.assertEqual(result["content"], "hello world")
        self.assertEqual(result["outcome"], "awaiting_review")
        for payload in (
            {"chunks": []},
            {"chunks": ["x" * (16384 + 1)]},
            {"chunks": ["ok", "bad\0nul"]},
            {"chunks": ["ok"], "outcome": "failed"},
            {"chunks": "not a list"},
            {"outcome": "awaiting_review"},
        ):
            with self.subTest(payload=str(payload)[:40]), self.assertRaises(IntakeError):
                validate_publish_result(payload)

    def test_progress_valid_and_rejects(self):
        self.assertEqual(validate_progress({"summary": "Working on it."})["summary"], "Working on it.")
        for payload in ({"summary": "  "}, {"summary": "x" * 4097}, {"summary": "bad\0"}, {}):
            with self.subTest(payload=str(payload)[:20]), self.assertRaises(IntakeError):
                validate_progress(payload)

    def test_chunk_content_boundaries(self):
        self.assertEqual(chunk_content("short"), ["short"])
        text = "é" * 9000  # 2 bytes each: 18000 bytes -> 2 chunks
        chunks = chunk_content(text)
        self.assertEqual(len(chunks), 2)
        self.assertEqual("".join(chunks), text)
        self.assertTrue(all(len(chunk.encode()) <= 16384 for chunk in chunks))
        with self.assertRaises(IntakeError):
            chunk_content("")

    def test_result_manifest(self):
        manifest = result_manifest("abc")
        self.assertEqual(manifest["sha256"], hashlib.sha256(b"abc").hexdigest())
        self.assertEqual(manifest["byte_count"], 3)
        self.assertEqual(manifest["chunk_count"], 1)

    def test_intake_config(self):
        config = parse_intake_config({
            "worker_id": "muse-intake-consumer", "owner": "mission-control",
            "database": {"host": "/var/run/postgresql", "port": 5432,
                         "name": "forge_agent", "user": "forge_coding_worker"},
            "profiles": {"muse-primary": {"backend": "muse", "model": MUSE_MODEL,
                                          "modes": ["plan", "review", "implement"]}},
            "projects": {"homelab": BASE_REV},
        })
        self.assertEqual(config.advertised_profiles(), [])
        self.assertFalse(config.enabled)
        self.assertEqual(config.advertised_projects(), [])
        bad = {
            "worker_id": "muse-intake-consumer", "owner": "mission-control",
            "database": {"host": "/var/run/postgresql", "port": 5432,
                         "name": "forge_agent", "user": "forge_coding_worker"},
            "profiles": {"codex-primary": {"backend": "codex", "model": "gpt-5.4",
                                           "modes": ["plan"]}},
            "projects": {"homelab": None},
        }
        with self.assertRaises(ValueError):
            parse_intake_config(bad)


class DormantBoundaryTest(unittest.TestCase):
    def test_activation_and_startup_fail_before_any_port_or_config_access(self):
        worker, intake = Mock(), Mock()
        config = parse_intake_config(raw_config())
        factory = MuseIntakeConsumerFactory(config, worker, intake)
        worker.assert_not_called()
        intake.assert_not_called()
        with patch("pathlib.Path.open", side_effect=AssertionError("no config read")), \
                patch("subprocess.Popen", side_effect=AssertionError("no child process")):
            with self.assertRaises(ExternalIntakeUnsupported):
                factory.create()
            with self.assertRaises(ExternalIntakeUnsupported):
                load_intake_config("/must/not/read/private.json")
            with self.assertRaises(ExternalIntakeUnsupported):
                default_store_factory(config)
            with self.assertRaises(ExternalIntakeUnsupported):
                default_intake_factory(config)
            with self.assertRaises(ExternalIntakeUnsupported):
                policy_digest()
            with self.assertRaises(SystemExit):
                main()
        for enabled in (True, 1, "true", None):
            with self.subTest(enabled=enabled), self.assertRaises(ValueError):
                parse_intake_config({**raw_config(), "enabled": enabled})
        with self.assertRaises(ExternalIntakeUnsupported):
            MuseIntakeConsumerFactory(replace(config, enabled=True), worker, intake)
        self.assertEqual(worker.mock_calls, [])
        self.assertEqual(intake.mock_calls, [])

    def test_direct_consumer_cannot_claim_publish_reconcile_or_emit_progress(self):
        worker, intake = Mock(), Mock()
        config = parse_intake_config(raw_config())
        consumer = MuseIntakeConsumer(config, worker, intake)
        row = SimpleNamespace(task_id=uuid4(), payload={"chunks": ["Untrusted result"]})
        for action in (consumer.advertise, consumer.run_once, consumer.run_forever,
                       lambda: apply_publish_result(worker, config, uuid4(), row)):
            for _retry in range(2):
                with self.assertRaises(ExternalIntakeUnsupported):
                    action()
        self.assertEqual(worker.mock_calls, [])
        self.assertEqual(intake.mock_calls, [])

    def test_store_surfaces_reject_before_connecting_even_for_stale_claims(self):
        connect = Mock(side_effect=AssertionError("no database connection"))
        for store in (IntakeStore(connect), WorkerIntakeStore(connect)):
            for action in (lambda: store.claim_row(("submit_plan",), "caller"),
                           lambda: store.release_row(uuid4()),
                           lambda: store.finish_row(uuid4(), "applied", {})):
                with self.assertRaises(ExternalIntakeUnsupported):
                    action()
        connect.assert_not_called()

    def test_untrusted_oversized_and_invalid_unicode_payloads_rejected(self):
        with self.assertRaises(IntakeError):
            validate_publish_result({"chunks": ["x" * 16384] * 65})
        for content in ("bad\ud800", "bad\x00"):
            with self.assertRaises(IntakeError):
                validate_progress({"summary": content})


@unittest.skipUnless(PG16_AVAILABLE, "Requires non-root disposable PostgreSQL16")
class DormantIntakePostgresTest(unittest.TestCase):
    def test_apply_twice_adds_no_external_intake_authority(self):
        cluster = DisposablePostgresCluster().start()
        self.addCleanup(cluster.stop)
        cluster.apply_and_verify()
        cluster.apply_and_verify()
        with closing(cluster.connect("postgres")) as connection, connection.cursor() as cursor:
            cursor.execute("SELECT max(version) FROM forge_agent.schema_migrations")
            self.assertEqual(cursor.fetchone()[0], 19)
            cursor.execute("SELECT to_regclass('forge_agent.coding_muse_intake')")
            self.assertIsNone(cursor.fetchone()[0])
            cursor.execute("SELECT count(*) FROM pg_roles WHERE rolname='muse_events'")
            self.assertEqual(cursor.fetchone()[0], 0)
            cursor.execute("SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace "
                           "WHERE n.nspname='forge_coding_worker_api' "
                           "AND p.proname IN ('claim_task','claim_intake','finish_intake','release_intake')")
            self.assertEqual(cursor.fetchone()[0], 0)


if __name__ == "__main__":
    unittest.main()
