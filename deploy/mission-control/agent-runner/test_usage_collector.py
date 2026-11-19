"""Injected collector lifecycle/generation/outage tests, no live calls."""

from datetime import UTC, datetime
import os
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from usage_collector import UsageCollector, UsageCollectorFactory, _read_hmac_key, create_worker_usage_collector
from usage_provider import AntigravityUsageProvider, UsageProviderConfig
from usage_rpc import ProviderFailure

REFRESH_ID = "18d5f913-cab9-453f-bbf6-84cccf8df9ea"


def provider(alias="agy-primary"):
    return AntigravityUsageProvider(UsageProviderConfig(alias, alias, "antigravity", "fixture-model"),
                                    clock=lambda: datetime(2026, 11, 19, tzinfo=UTC))


def store():
    result = Mock()
    result.next_refresh.return_value = None
    result.begin_usage_poll.return_value = {"generation": 1, "configuration_epoch": 1}
    result.record_usage.return_value = True
    return result


class CollectorTest(unittest.TestCase):
    def test_factory_deferred_selected_instances_and_disabled_zero_effects(self):
        selected = provider()
        factory = Mock(config=selected.config)
        factory.create.return_value = selected
        db = store()
        staged = UsageCollectorFactory([factory], db)
        factory.create.assert_not_called()
        collector = staged.create()
        factory.create.assert_called_once()
        self.assertIs(collector._providers[selected.config.alias], selected)
        self.assertIsNone(UsageCollectorFactory(None, db).create())
        self.assertEqual(db.mock_calls, [])
        with self.assertRaises(ValueError):
            UsageCollectorFactory([factory, factory], db)
        factory.create.assert_called_once()
        with self.assertRaises(ValueError):
            UsageCollectorFactory([factory], db, poll_interval=0)

    def test_generation_is_allocated_before_io_and_periodic_poll_is_bounded(self):
        selected = provider()
        clock = [100.0]
        db = store()
        observed = []
        actual_read = selected.read_snapshot
        selected.read_snapshot = lambda: (observed.append(db.begin_usage_poll.call_count), actual_read())[1]
        collector = UsageCollector({selected.config.alias: selected}, db, monotonic=lambda: clock[0])
        self.assertTrue(collector.run_once())
        self.assertEqual(observed, [1])
        self.assertFalse(collector.run_once())
        self.assertEqual(db.begin_usage_poll.call_count, 1)
        clock[0] += 61
        self.assertTrue(collector.run_once())
        self.assertEqual(db.begin_usage_poll.call_count, 2)
        self.assertEqual(db.record_usage.call_args.kwargs["configuration_epoch"], 1)

    def test_queued_refresh_uses_original_generation_not_new_poll(self):
        db = store()
        db.next_refresh.return_value = {"refresh_id": REFRESH_ID, "profile_alias": "agy-primary",
                                        "generation": 4, "configuration_epoch": 2}
        selected = provider()
        collector = UsageCollector({"agy-primary": selected}, db)
        self.assertTrue(collector.run_once())
        db.begin_usage_poll.assert_not_called()
        self.assertEqual(db.record_usage.call_args.kwargs["generation"], 4)
        self.assertEqual(db.record_usage.call_args.kwargs["configuration_epoch"], 2)

    def test_stale_generation_is_discarded_without_retry_or_new_provider_call(self):
        db = store()
        db.record_usage.return_value = False
        selected = provider()
        selected.read_snapshot = Mock(wraps=selected.read_snapshot)
        collector = UsageCollector({"agy-primary": selected}, db)
        self.assertTrue(collector.run_once())
        self.assertFalse(collector.run_once())
        selected.read_snapshot.assert_called_once()
        db.fail_usage_poll.assert_not_called()

    def test_database_outbox_retries_same_snapshot_without_repolling_provider(self):
        db = store()
        db.record_usage.side_effect = [RuntimeError("PRIVATE_DATABASE_DETAIL"), True]
        selected = provider()
        selected.read_snapshot = Mock(wraps=selected.read_snapshot)
        collector = UsageCollector({"agy-primary": selected}, db)
        self.assertFalse(collector.run_once())
        self.assertTrue(collector.run_once())
        selected.read_snapshot.assert_called_once()
        self.assertEqual(db.record_usage.call_args_list[0], db.record_usage.call_args_list[1])
        self.assertEqual(db.next_refresh.call_count, 1)

    def test_expired_outbox_records_coverage_error_after_database_recovery(self):
        db = store()
        db.record_usage.side_effect = RuntimeError("unavailable")
        now = [100.0]
        selected = provider()
        collector = UsageCollector({"agy-primary": selected}, db, outbox_ttl=5, monotonic=lambda: now[0])
        self.assertFalse(collector.run_once())
        now[0] += 6
        self.assertTrue(collector.run_once())
        self.assertEqual(db.record_usage.call_count, 1)
        db.fail_usage_poll.assert_called_once_with(profile_alias="agy-primary", generation=1,
                                                   configuration_epoch=1, code="unavailable")

    def test_pending_snapshot_ownership_survives_a_mutating_store_override(self):
        db = store()
        attempts = [0]
        def write(**fields):
            attempts[0] += 1
            if attempts[0] == 1:
                fields["snapshot"]["source"] = "PRIVATE_OVERRIDE"
                raise RuntimeError("retry")
            self.assertEqual(fields["snapshot"]["source"], "agy-headless/allowance-capability")
            return True
        db.record_usage.side_effect = write
        collector = UsageCollector({"agy-primary": provider()}, db)
        self.assertFalse(collector.run_once())
        self.assertTrue(collector.run_once())

    def test_provider_auth_or_unexpected_errors_are_sanitized_without_harming_worker(self):
        for error, code in ((ProviderFailure("auth_required"), "auth_required"),
                             (RuntimeError("PRIVATE_PROVIDER_DETAIL"), "unavailable")):
            db = store()
            selected = provider()
            selected.read_snapshot = Mock(side_effect=error)
            collector = UsageCollector({"agy-primary": selected}, db)
            self.assertTrue(collector.run_once())
            self.assertEqual(db.fail_usage_poll.call_args.kwargs["code"], code)
            db.record_usage.assert_not_called()
        db = store()
        db.next_refresh.side_effect = RuntimeError("PRIVATE")
        self.assertFalse(UsageCollector({"agy-primary": provider()}, db).run_once())

    def test_unknown_profile_never_invokes_any_provider(self):
        db = store()
        db.next_refresh.return_value = {"refresh_id": REFRESH_ID, "profile_alias": "foreign",
                                        "generation": 1, "configuration_epoch": 1}
        selected = provider()
        selected.read_snapshot = Mock(side_effect=AssertionError("wrong provider"))
        self.assertTrue(UsageCollector({"agy-primary": selected}, db).run_once())
        selected.read_snapshot.assert_not_called()
        self.assertEqual(db.fail_usage_poll.call_args.kwargs["code"], "unknown_profile")

    def test_disabled_bootstrap_never_reads_any_secret_or_starts_process(self):
        with patch("os.open", side_effect=AssertionError("no key read")):
            self.assertIsNone(create_worker_usage_collector(SimpleNamespace(usage=None), store()))

    def test_worker_bootstrap_constructs_selected_unsupported_adapter_without_cli(self):
        with TemporaryDirectory(prefix="forge-usage-bootstrap-") as directory:
            key = Path(directory) / "fixture.key"
            key.write_bytes(b"x" * 32)
            key.chmod(0o600)
            cfg = SimpleNamespace(
                usage=SimpleNamespace(hmac_key_file=key, interval_seconds=60),
                account=SimpleNamespace(uid=os.geteuid() or 1000, gid=os.getegid() or 1000, home=Path(directory)),
                profiles={"agy-primary": SimpleNamespace(alias="agy-primary", backend="antigravity", model="fixture-model")},
                binaries={"antigravity": Path(directory) / "unused-agi-binary"},
            )
            with patch("subprocess.Popen", side_effect=AssertionError("no quota model process")):
                db = store()
                collector = create_worker_usage_collector(cfg, db)
                db.begin_usage_poll.assert_not_called()
                self.assertTrue(collector.run_once())
            self.assertEqual(db.record_usage.call_args.kwargs["snapshot"]["allowance"]["reason"], "unsupported_headless")

    def test_private_hmac_key_fixture_formats_permissions_and_no_symlink(self):
        with TemporaryDirectory(prefix="forge-usage-key-") as directory:
            key = Path(directory) / "fixture.key"
            key.write_bytes(b"x" * 32)
            key.chmod(0o600)
            self.assertEqual(_read_hmac_key(key), b"x" * 32)
            key.write_text("78" * 32 + "\n")
            self.assertEqual(_read_hmac_key(key), b"x" * 32)
            key.chmod(0o644)
            with self.assertRaises(ValueError):
                _read_hmac_key(key)
            key.chmod(0o600)
            link = Path(directory) / "link.key"
            link.symlink_to(key)
            with self.assertRaises(ValueError):
                _read_hmac_key(link)
            fifo = Path(directory) / "fifo.key"
            os.mkfifo(fifo, 0o600)
            with self.assertRaises(ValueError):
                _read_hmac_key(fifo)


if __name__ == "__main__":
    unittest.main()
