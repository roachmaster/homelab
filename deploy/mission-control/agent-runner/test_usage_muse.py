"""Owned fake children and injected metadata ports; no native model/auth calls."""

from copy import deepcopy
from datetime import UTC, datetime
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from usage_collector import UsageCollector, create_worker_usage_collector
from usage_muse import MuseUsageProcess
from usage_provider import (AntigravityUsageProvider, MuseUsageProvider, UnavailableUsageProvider,
                            UsageProviderConfig, UsageProviderFactory)
from usage_rpc import MetadataProcessConfig, ProviderFailure
from work_native_auth import AuthLease

RAW = {"schemaVersion": 1, "allowance": None, "reason": "no_allowance", "hint": None}
NOW = datetime(2026, 11, 20, 21, 0, tzinfo=UTC)
REFRESH_ID = "18d5f913-cab9-453f-bbf6-84cccf8df9ea"


def selected(process=None):
    return UsageProviderConfig("muse-primary", "Muse", "muse", "muse-model",
                               process or MetadataProcessConfig(Path("/opt/muse-pinned"), Path("/home/fixture")))


def store():
    db = Mock()
    db.next_refresh.return_value = None
    db.begin_usage_poll.return_value = {"generation": 1, "configuration_epoch": 1}
    db.record_usage.return_value = True
    return db


class MuseProviderTest(unittest.TestCase):
    def test_factory_selection_and_creation_have_no_native_or_auth_effects(self):
        native = Mock(read=Mock(return_value=RAW))
        factory = Mock(return_value=native)
        configured = selected()
        with patch("subprocess.Popen", side_effect=AssertionError("native process")), \
                patch("work_muse.stage_auth", side_effect=AssertionError("auth access")):
            staged = UsageProviderFactory(configured, hmac_key=b"x" * 32,
                                          muse_factory=factory, clock=lambda: NOW)
            factory.assert_not_called()
            provider = staged.create()
            factory.assert_called_once_with(configured.process)
            native.read.assert_not_called()
        self.assertIsInstance(provider, MuseUsageProvider)
        item = provider.read_snapshot()
        self.assertEqual(item["source"], "muse-cli/usage-json-v1")
        self.assertEqual(item["observed_at"], "2026-11-20T21:00:00Z")
        self.assertEqual(item["allowance"], {"state": "unsupported", "reason": "no_allowance",
                                            "ordinary_usage_allowed": None, "buckets": []})
        self.assertIsNone(item["account_identity"])
        self.assertEqual(item["account_activity"], {"state": "unknown", "lifetime_tokens": None, "daily": []})

    def test_unverified_schemas_and_allowance_values_never_become_zero_or_unlimited(self):
        for raw in (None, [], {}, dict(RAW, schemaVersion=True), dict(RAW, schemaVersion=2),
                    dict(RAW, allowance={"usedPercent": 0}), dict(RAW, reason="unlimited"),
                    dict(RAW, hint="PRIVATE"), dict(RAW, unexpected="PRIVATE")):
            with self.subTest(raw=raw):
                provider = MuseUsageProvider(selected(), Mock(read=Mock(return_value=raw)))
                with self.assertRaises(ProviderFailure) as error:
                    provider.read_snapshot()
                self.assertEqual(error.exception.code, "malformed_metadata")
        with self.assertRaises(ValueError):
            UsageProviderConfig("muse", "Muse", "muse", "muse-model")
        with self.assertRaises(ValueError):
            selected(MetadataProcessConfig(Path("/opt/muse"), Path("/home/fixture"), Path("/home/fixture/.codex")))

    def test_periodic_collection_never_admits_muse_but_manual_refresh_runs_once(self):
        native = Mock(read=Mock(return_value=RAW))
        provider = MuseUsageProvider(selected(), native, clock=lambda: NOW)
        db = store()
        collector = UsageCollector({"muse-primary": provider}, db, monotonic=lambda: 1_000_000)
        for _ in range(4):
            self.assertFalse(collector.run_once())
        db.begin_usage_poll.assert_not_called()
        native.read.assert_not_called()
        db.next_refresh.return_value = {"refresh_id": REFRESH_ID, "profile_alias": "muse-primary",
                                        "generation": 4, "configuration_epoch": 2}
        self.assertTrue(collector.run_once())
        native.read.assert_called_once()
        db.begin_usage_poll.assert_not_called()
        self.assertEqual(db.record_usage.call_args.kwargs["generation"], 4)
        db.next_refresh.return_value = None
        self.assertFalse(collector.run_once())
        native.read.assert_called_once()

    def test_manual_only_unavailable_provider_is_not_polled_or_starving_other_profiles(self):
        muse = UnavailableUsageProvider(selected(), "auth_required")
        agy_cfg = UsageProviderConfig("agy", "AGY", "antigravity", "agy-model")
        agy = AntigravityUsageProvider(agy_cfg, clock=lambda: NOW)
        db = store()
        collector = UsageCollector({"muse-primary": muse, "agy": agy}, db)
        self.assertTrue(collector.run_once())
        db.begin_usage_poll.assert_called_once_with(profile_alias="agy")
        db.fail_usage_poll.assert_not_called()
        db.next_refresh.return_value = {"refresh_id": REFRESH_ID, "profile_alias": "muse-primary",
                                        "generation": 4, "configuration_epoch": 2}
        self.assertTrue(collector.run_once())
        self.assertEqual(db.fail_usage_poll.call_args.kwargs["code"], "auth_required")

    def test_manual_outbox_retries_persisted_observation_without_another_native_call(self):
        native = Mock(read=Mock(return_value=RAW))
        db = store()
        db.next_refresh.return_value = {"refresh_id": REFRESH_ID, "profile_alias": "muse-primary",
                                        "generation": 4, "configuration_epoch": 2}
        db.record_usage.side_effect = [RuntimeError("PRIVATE"), True]
        provider = MuseUsageProvider(selected(), native, clock=lambda: NOW)
        collector = UsageCollector({"muse-primary": provider}, db)
        self.assertFalse(collector.run_once())
        self.assertTrue(collector.run_once())
        native.read.assert_called_once()
        self.assertEqual(db.next_refresh.call_count, 1)

    def test_worker_bootstrap_passes_muse_process_config_without_starting_it(self):
        with TemporaryDirectory(prefix="forge-muse-bootstrap-") as directory:
            root = Path(directory)
            key = root / "hmac"
            key.write_bytes(b"x" * 32)
            key.chmod(0o600)
            cfg = SimpleNamespace(
                usage=SimpleNamespace(hmac_key_file=key, interval_seconds=60),
                account=SimpleNamespace(uid=os.geteuid() or 1000, gid=os.getegid() or 1000, home=root),
                profiles={"muse-primary": SimpleNamespace(alias="muse-primary", backend="muse", model="muse-model")},
                binaries={"muse": root / "must-not-start"},
            )
            db = store()
            with patch("subprocess.Popen", side_effect=AssertionError("native process")):
                collector = create_worker_usage_collector(cfg, db)
                self.assertFalse(collector.run_once())
            process = collector._providers["muse-primary"].config.process
            self.assertEqual(process.binary, cfg.binaries["muse"])
            self.assertIsNone(process.code_home)
            self.assertEqual(process.home, root)
            db.begin_usage_poll.assert_not_called()


    def test_usage_only_profile_is_visible_without_execution_registration_or_polling(self):
        with TemporaryDirectory(prefix="forge-muse-usage-only-") as directory:
            root = Path(directory)
            key = root / "hmac"
            key.write_bytes(b"x" * 32)
            key.chmod(0o600)
            profile = SimpleNamespace(alias="muse-primary", backend="muse", model="muse-model",
                                      binary=root / "pinned-muse-metadata-only")
            cfg = SimpleNamespace(
                usage=SimpleNamespace(hmac_key_file=key, interval_seconds=60),
                account=SimpleNamespace(uid=os.geteuid() or 1000, gid=os.getegid() or 1000, home=root),
                profiles={"agy": SimpleNamespace(alias="agy", backend="antigravity", model="agy-model")},
                binaries={"antigravity": root / "not-started-agy"},
                usage_only_profiles={"muse-primary": profile},
            )
            db = store()
            with patch("subprocess.Popen", side_effect=AssertionError("native process")):
                collector = create_worker_usage_collector(cfg, db)
                self.assertEqual(set(collector._providers), {"agy", "muse-primary"})
                self.assertTrue(collector.run_once())  # AGY's non-native unsupported snapshot.
                for _ in range(3):
                    self.assertFalse(collector.run_once())
            muse = collector._providers["muse-primary"]
            self.assertEqual(muse.config.process.binary, profile.binary)
            self.assertTrue(muse.manual_refresh_only)
            self.assertEqual(set(cfg.profiles), {"agy"})
            self.assertEqual(set(cfg.binaries), {"antigravity"})
            db.begin_usage_poll.assert_called_once_with(profile_alias="agy")


class MuseProcessTest(unittest.TestCase):
    def setUp(self):
        self.temporary = TemporaryDirectory(prefix="forge-fake-muse-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.binary = self.root / "fake-muse"
        self.children = []
        self.uid, self.gid = os.geteuid() or 1000, os.getegid() or 1000

    def fixture(self, program, *, auth=True, timeout=1):
        self.binary.write_text("#!/usr/bin/python3\n" + program)
        self.binary.chmod(0o700)
        if os.geteuid() == 0:
            os.chown(self.root, self.uid, self.gid)
            os.chown(self.binary, self.uid, self.gid)
        digest = hashlib.sha256(self.binary.read_bytes()).hexdigest()
        self.patch = patch("usage_muse.BINARY_SHA256", digest)
        self.patch.start()
        self.addCleanup(self.patch.stop)
        cfg = MetadataProcessConfig(self.binary, self.root / "original-home",
                                    run_uid=self.uid if os.geteuid() == 0 else None,
                                    run_gid=self.gid if os.geteuid() == 0 else None, timeout_seconds=timeout)
        captures = []
        def spawn(*args, **options):
            captures.append((deepcopy(args), deepcopy(options)))
            child = subprocess.Popen(*args, **options)
            self.children.append(child)
            return child
        def stage(account, destination):
            self.assertEqual((account.home, account.uid), (cfg.home, self.uid))
            if auth:
                descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o400)
                os.write(descriptor, b"FAKE_PRIVATE_AUTH")
                directory = os.open(destination.parent, os.O_RDONLY | os.O_DIRECTORY)
                return AuthLease(directory, descriptor, destination.name, "home/.config/muse")
            return False
        signals = []
        def group_signal(group, signum):
            signals.append(signum)
            os.killpg(group, signum)
        return MuseUsageProcess(cfg, process_factory=spawn, auth_stager=stage,
                                signal_group=group_signal), captures, signals

    def test_owned_fake_child_sees_only_isolated_state_and_exact_metadata_argv(self):
        program = '''import json,os,sys
from pathlib import Path
assert sys.argv[1:]==["usage","--json"]
assert os.geteuid()!=0
assert set(os.environ)=={"HOME","XDG_CONFIG_HOME","MUSE_AUTH_PATH","PATH","LANG","LC_ALL","RUST_LOG"}
assert Path(os.environ["MUSE_AUTH_PATH"]).read_bytes()==b"FAKE_PRIVATE_AUTH"
assert not (Path(os.environ["HOME"])/"settings.json").exists()
print(json.dumps({"schemaVersion":1,"allowance":None,"reason":"no_allowance","hint":None}))
print("PRIVATE_STDERR",file=sys.stderr)
'''
        process, calls, signals = self.fixture(program)
        with patch.dict(os.environ, {"PRIVATE_TOKEN": "MUST_NOT_INHERIT", "META_API_KEY": "PRIVATE"}):
            self.assertEqual(process.read(), RAW)
        self.assertEqual(calls[0][0][0], [str(self.binary), "usage", "--json"])
        options = calls[0][1]
        self.assertEqual(options["cwd"], "/")
        self.assertEqual(options["stdin"], subprocess.DEVNULL)
        self.assertTrue(options["start_new_session"])
        self.assertTrue(options["executable"].startswith("/proc/self/fd/"))
        self.assertEqual(len(options["pass_fds"]), 1)
        self.assertFalse(Path(options["env"]["HOME"]).exists())
        self.assertEqual(signals, [signal.SIGTERM, signal.SIGKILL, 0])

    def test_auth_lease_is_owned_until_stop_and_closed_after_success(self):
        process, calls, _signals = self.fixture('import json;print('+repr(json.dumps(RAW))+')')
        stage = process._stage_auth
        acquired = []
        def acquire(*args):
            credential = stage(*args)
            acquired.append(credential)
            return credential
        process._stage_auth = acquire
        stop = process._stop
        def stopped(child):
            self.assertFalse(acquired[0].cleaned)
            self.assertTrue(Path(calls[0][1]["env"]["MUSE_AUTH_PATH"]).exists())
            return stop(child)
        process._stop = stopped
        self.assertEqual(process.read(), RAW)
        self.assertTrue(acquired[0].cleaned)
        self.assertIsNone(acquired[0].credential)
        self.assertIsNone(acquired[0].directory)

    def test_uncertain_metadata_stop_retains_owned_auth_for_reconciliation(self):
        import shutil
        process, calls, _signals = self.fixture('print("{}")')
        stop = process._stop
        def uncertain(child):
            stop(child)
            raise ProviderFailure("unavailable")
        process._stop = uncertain
        with self.assertRaises(ProviderFailure):
            process.read()
        home = Path(calls[0][1]["env"]["HOME"])
        try:
            self.assertEqual((home / ".config/muse/auth.json").read_bytes(), b"FAKE_PRIVATE_AUTH")
            self.assertIsNotNone(self.children[-1].returncode)
        finally:
            shutil.rmtree(home)

    def test_missing_auth_binary_mismatch_and_root_without_account_never_spawn(self):
        process, calls, _signals = self.fixture("raise AssertionError('must not run')", auth=False)
        with self.assertRaises(ProviderFailure) as error:
            process.read()
        self.assertEqual(error.exception.code, "auth_required")
        self.assertEqual(calls, [])
        process, calls, _signals = self.fixture("raise AssertionError('must not run')")
        with patch("usage_muse.BINARY_SHA256", "0" * 64), self.assertRaises(ProviderFailure) as error:
            process.read()
        self.assertEqual(error.exception.code, "unsupported_version")
        self.assertEqual(calls, [])
        root_process = MuseUsageProcess(MetadataProcessConfig(self.binary, self.root), process_factory=Mock())
        with patch("os.geteuid", return_value=0), self.assertRaises(ProviderFailure):
            root_process.read()
        root_process._spawn.assert_not_called()

    def test_output_errors_limits_timeout_and_exit_failure_are_bounded_and_sanitized(self):
        for program, code in (
            ('print("{\\"schemaVersion\\":1,\\"schemaVersion\\":1}")', "malformed_metadata"),
            ('print("NaN")', "malformed_metadata"),
            ('print("x"*65537)', "response_limit"),
            ('import time;time.sleep(10)', "timeout"),
            ('import sys;print("PRIVATE_FAILURE",file=sys.stderr);sys.exit(1)', "unavailable"),
        ):
            with self.subTest(code=code):
                process, calls, signals = self.fixture(program)
                with self.assertRaises(ProviderFailure) as error:
                    process.read()
                self.assertEqual(error.exception.code, code)
                self.assertNotIn("PRIVATE", str(error.exception))
                self.assertEqual(signals, [signal.SIGTERM, signal.SIGKILL, 0])
                self.assertFalse(Path(calls[0][1]["env"]["HOME"]).exists())
                self.assertIsNotNone(self.children[-1].returncode)
                with self.assertRaises(ProcessLookupError):
                    os.kill(self.children[-1].pid, 0)


if __name__ == "__main__":
    unittest.main()
