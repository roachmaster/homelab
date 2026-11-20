"""Provider tests use fake RPC/process ports, never account credentials."""

from copy import deepcopy
from datetime import UTC, datetime
import io
import json
import os
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from usage_provider import (
    AntigravityUsageProvider, CodexUsageProvider, UnavailableUsageProvider,
    UsageProviderConfig, UsageProviderFactory,
)
from usage_rpc import CodexMetadataRPC, MetadataProcessConfig, ProviderFailure, _Connection
from usage_types import InvalidUsage, ProviderUsage, normalize_attempt_usage, validate_snapshot

NOW = datetime(2026, 11, 19, 18, 30, tzinfo=UTC)
KEY = b"fixture-private-hmac-key-32-bytes!!"


def process_config():
    return MetadataProcessConfig(Path("/opt/codex"), Path("/home/fixture"), Path("/home/fixture/.codex"))


def config(provider="codex", alias="codex-primary"):
    return UsageProviderConfig(alias, "Fixture agent", provider, "fixture-model",
                               process_config() if provider == "codex" else None)


def quota(**overrides):
    result = {
        "accountId": "PRIVATE_ACCOUNT_ID", "ordinaryUsageAllowed": None,
        "rateLimits": {"limitId": "codex", "limitName": None,
                       "primary": {"usedPercent": 35.5, "windowDurationMins": 60, "resetsAt": 1795113600},
                       "secondary": None},
        "rateLimitsByLimitId": None,
        "rateLimitUpsell": {"private": "DO_NOT_STORE"},
        "rateLimitResetCredits": {"credits": [{"id": "DO_NOT_STORE_CREDIT"}]},
    }
    result.update(overrides)
    return result


def metadata():
    return {"account": {"account": {"type": "chatgpt", "email": "PRIVATE_EMAIL"}},
            "quota": quota(), "quota_after": quota(),
            "activity": {"summary": {"lifetimeTokens": 9007199254740999, "peakDailyTokens": None},
                         "dailyUsageBuckets": [{"startDate": "2026-11-18", "tokens": 100}]}}


def provider_row():
    return {
        "alias": "codex-primary", "label": "Codex", "provider": "codex", "model": "fixture-model",
        "worker_state": "idle", "active_task_id": None, "snapshot_state": "unknown",
        "observed_at": None, "source": "none", "version": "unknown", "last_error": None,
        "refresh_pending": False,
        "allowance": {"state": "unknown", "reason": None, "ordinary_usage_allowed": None, "buckets": []},
        "jobs": {"scope": "forge_project", "input_tokens": None, "output_tokens": None,
                 "cached_input_tokens": None, "total_tokens": None, "known_attempts": 0,
                 "unknown_attempts": 0, "elapsed_ms": None},
        "account_activity": {"state": "unknown", "lifetime_tokens": None, "daily": []},
    }


class AccountingTest(unittest.TestCase):
    def test_codex_per_turn_total_excludes_overlapping_categories(self):
        observed = normalize_attempt_usage("codex", {"input_tokens": 100, "output_tokens": 20,
                  "cached_input_tokens": 80, "cache_write_input_tokens": 10, "reasoning_output_tokens": 12})
        self.assertEqual(observed["total_tokens"], "120")
        self.assertEqual(observed["cached_input_tokens"], "80")
        self.assertEqual(observed["coverage"], "complete")
        self.assertEqual(observed["source"], "codex-exec-turn-v1")

    def test_agy_cumulative_final_is_not_added_to_step_or_cache_usage(self):
        raw = {"input_tokens": 100, "output_tokens": 20, "total_tokens": 120,
               "cache_read_tokens": 1000, "thinking_tokens": 12}
        observed = normalize_attempt_usage("antigravity", raw)
        self.assertEqual(observed["total_tokens"], "120")
        self.assertEqual(observed["cached_input_tokens"], "1000")
        unknown = normalize_attempt_usage("antigravity", raw, fresh_session=False)
        self.assertIsNone(unknown["input_tokens"])
        self.assertEqual(unknown["coverage"], "unknown")
        self.assertEqual(unknown["session_provenance"], "unverified_baseline")

    def test_agy_cache_aliases_agree_without_adding_overlapping_metrics(self):
        for cache in ({"cache_read_tokens": 80}, {"cached_tokens": 80},
                      {"cache_read_tokens": 80, "cached_tokens": "80"},
                      {"cache_read_tokens": None, "cached_tokens": 80}):
            with self.subTest(cache=cache):
                observed = normalize_attempt_usage("antigravity", {
                    "input_tokens": 100, "output_tokens": 20, "total_tokens": 120,
                    "thinking_tokens": 10, "cache_write_tokens": 30, **cache,
                })
                self.assertEqual(observed["cached_input_tokens"], "80")
                self.assertEqual(observed["total_tokens"], "120")
                self.assertEqual(observed["coverage"], "complete")

    def test_agy_conflicting_or_invalid_cache_does_not_discard_primary_metrics(self):
        for cache in ({"cache_read_tokens": 80, "cached_tokens": 70},
                      {"cache_read_tokens": 80, "cached_tokens": True},
                      {"cache_read_tokens": -1, "cached_tokens": 80},
                      {"cached_tokens": 1.5}, {"cache_read_tokens": "01"}):
            with self.subTest(cache=cache):
                observed = normalize_attempt_usage("antigravity", {
                    "input_tokens": 100, "output_tokens": 20, "total_tokens": 120, **cache,
                })
                self.assertIsNone(observed["cached_input_tokens"])
                self.assertEqual(observed["input_tokens"], "100")
                self.assertEqual(observed["output_tokens"], "20")
                self.assertEqual(observed["total_tokens"], "120")
                self.assertEqual(observed["coverage"], "complete")

    def test_agy_cache_only_is_partial_and_keeps_exact_zero_and_wide_counters(self):
        for field in ("cache_read_tokens", "cached_tokens"):
            for value in (0, 9007199254740999, 10**40 - 1, "9007199254740999"):
                with self.subTest(field=field, value=value):
                    observed = normalize_attempt_usage("antigravity", {field: value})
                    self.assertEqual(observed["cached_input_tokens"], str(value))
                    self.assertEqual(observed["coverage"], "partial")
                    for primary in ("input_tokens", "output_tokens", "total_tokens"):
                        self.assertIsNone(observed[primary])
        zero = normalize_attempt_usage("antigravity", {
            "input_tokens": 0, "output_tokens": 0, "total_tokens": 0, "cached_tokens": 0,
        })
        self.assertEqual(zero["coverage"], "complete")
        self.assertTrue(all(zero[key] == "0" for key in (
            "input_tokens", "output_tokens", "cached_input_tokens", "total_tokens")))

    def test_agy_resumed_or_unverified_usage_never_attributes_cumulative_counts(self):
        raw = {"input_tokens": 1000, "output_tokens": 200, "total_tokens": 1200,
               "cache_read_tokens": 900, "cached_tokens": 900}
        for provenance in (False, None, 1, "fresh_session"):
            with self.subTest(provenance=provenance):
                observed = normalize_attempt_usage("antigravity", raw, fresh_session=provenance)
                self.assertEqual(observed["coverage"], "unknown")
                self.assertEqual(observed["session_provenance"], "unverified_baseline")
                self.assertTrue(all(observed[key] is None for key in (
                    "input_tokens", "output_tokens", "cached_input_tokens", "total_tokens")))

    def test_agy_explicit_total_is_not_invented_or_replaced_by_subdivisions(self):
        without_total = normalize_attempt_usage("antigravity", {
            "input_tokens": 100, "output_tokens": 20, "cached_tokens": 1000, "thinking_tokens": 12,
        })
        self.assertIsNone(without_total["total_tokens"])
        self.assertEqual(without_total["coverage"], "partial")
        codex = normalize_attempt_usage("codex", {
            "input_tokens": 100, "output_tokens": 20, "cached_tokens": 80,
        })
        self.assertIsNone(codex["cached_input_tokens"])
        self.assertEqual(codex["total_tokens"], "120")

    def test_large_null_invalid_and_partial_counters_preserve_uncertainty(self):
        large = 9007199254740999
        observed = normalize_attempt_usage("codex", {"input_tokens": large, "output_tokens": 1})
        self.assertEqual(observed["total_tokens"], str(large + 1))
        self.assertIsNone(observed["cached_input_tokens"])
        for invalid in (-1, True, 1.5, "01", "-1", "secret", 10**40):
            item = normalize_attempt_usage("codex", {"input_tokens": invalid, "output_tokens": 1})
            self.assertIsNone(item["input_tokens"])
            self.assertIsNone(item["total_tokens"])
            self.assertEqual(item["coverage"], "partial")
        self.assertEqual(normalize_attempt_usage("codex", None)["coverage"], "unknown")


class ProviderTest(unittest.TestCase):
    def test_factory_is_effect_free_and_uses_selected_rpc_after_creation(self):
        rpc = Mock()
        rpc.version.return_value = "0.160.0"
        rpc.read.return_value = metadata()
        selected = Mock(return_value=rpc)
        factory = UsageProviderFactory(config(), hmac_key=KEY, rpc_factory=selected, clock=lambda: NOW)
        selected.assert_not_called()
        provider = factory.create()
        selected.assert_called_once_with(config().process)
        rpc.read.assert_not_called()
        snapshot = provider.read_snapshot()
        self.assertEqual(snapshot["snapshot_state"], "fresh")
        self.assertEqual(snapshot["allowance"]["buckets"][0]["used_percent"], 35.5)
        self.assertIsNone(snapshot["allowance"]["ordinary_usage_allowed"])
        self.assertEqual(snapshot["account_activity"]["lifetime_tokens"], "9007199254740999")
        self.assertRegex(snapshot["account_identity"], r"^[a-f0-9]{64}$")
        encoded = json.dumps(snapshot)
        for secret in ("PRIVATE_ACCOUNT_ID", "PRIVATE_EMAIL", "DO_NOT_STORE", "DO_NOT_STORE_CREDIT"):
            self.assertNotIn(secret, encoded)

    def test_multi_meter_view_wins_without_adding_backward_compatible_duplicate(self):
        raw = metadata()
        raw["quota_after"]["rateLimitsByLimitId"] = {
            "codex": {"limitId": "codex", "limitName": "Codex", "primary": {
                "usedPercent": 0, "windowDurationMins": None, "resetsAt": None},
                "secondary": {"usedPercent": 100, "windowDurationMins": 10080, "resetsAt": 0}},
            "other": {"limitId": "other", "primary": None, "secondary": None},
        }
        raw["quota_after"]["ordinaryUsageAllowed"] = False
        item = CodexUsageProvider(config(), Mock(read=Mock(return_value=raw)), KEY,
                                  version="0.160.0", clock=lambda: NOW).read_snapshot()
        self.assertEqual(len(item["allowance"]["buckets"]), 2)
        self.assertEqual([row["used_percent"] for row in item["allowance"]["buckets"]], [0, 100])
        self.assertFalse(item["allowance"]["ordinary_usage_allowed"])
        self.assertEqual(item["allowance"]["buckets"][1]["resets_at"], "1970-01-01T00:00:00Z")

    def test_unknown_identity_is_not_derived_from_email_and_switch_rejected(self):
        raw = metadata()
        raw["quota"]["accountId"] = raw["quota_after"]["accountId"] = None
        selected = CodexUsageProvider(config(), Mock(read=Mock(return_value=raw)), KEY,
                                      version="0.160.0", clock=lambda: NOW)
        self.assertIsNone(selected.read_snapshot()["account_identity"])
        raw["quota_after"]["accountId"] = "different"
        with self.assertRaises(ProviderFailure) as error:
            selected.read_snapshot()
        self.assertEqual(error.exception.code, "account_changed")

    def test_activity_failure_does_not_fabricate_zero_or_discard_allowance(self):
        for activity in (None, {"summary": {"lifetimeTokens": -1}, "dailyUsageBuckets": []}):
            raw = metadata()
            raw["activity"] = activity
            selected = CodexUsageProvider(config(), Mock(read=Mock(return_value=raw)), KEY,
                                          version="0.160.0", clock=lambda: NOW)
            item = selected.read_snapshot()
            self.assertEqual(item["allowance"]["state"], "available")
            self.assertEqual(item["account_activity"]["state"], "unavailable")
            self.assertIsNone(item["account_activity"]["lifetime_tokens"])

    def test_bad_provider_numbers_dates_and_bounds_are_rejected(self):
        mutations = [lambda raw: raw["quota_after"]["rateLimits"]["primary"].update(usedPercent=float("nan")),
                     lambda raw: raw["quota_after"]["rateLimits"]["primary"].update(usedPercent=True),
                     lambda raw: raw["quota_after"].update(ordinaryUsageAllowed="yes"),
                     lambda raw: raw["quota_after"]["rateLimits"]["primary"].update(resetsAt=-1),
                     lambda raw: raw["quota_after"]["rateLimits"]["primary"].update(windowDurationMins=0)]
        for mutate in mutations:
            raw = metadata()
            mutate(raw)
            selected = CodexUsageProvider(config(), Mock(read=Mock(return_value=raw)), KEY,
                                          version="0.160.0", clock=lambda: NOW)
            with self.assertRaises(ProviderFailure):
                selected.read_snapshot()

    def test_agy_allowance_is_explicitly_unsupported_without_process_effects(self):
        rpc = Mock(side_effect=AssertionError("no AGY quota process"))
        selected = UsageProviderFactory(config("antigravity", "agy-primary"), hmac_key=KEY, rpc_factory=rpc)
        provider = selected.create()
        snapshot = provider.read_snapshot()
        rpc.assert_not_called()
        self.assertEqual(snapshot["allowance"]["state"], "unsupported")
        self.assertEqual(snapshot["allowance"]["reason"], "unsupported_headless")
        self.assertIsNone(snapshot["account_identity"])

    def test_unsupported_version_and_probe_failure_remain_unavailable(self):
        rpc = Mock(version=Mock(return_value="0.999.0"))
        selected = UsageProviderFactory(config(), hmac_key=KEY, rpc_factory=Mock(return_value=rpc)).create()
        self.assertIsInstance(selected, UnavailableUsageProvider)
        with self.assertRaises(ProviderFailure) as error:
            selected.read_snapshot()
        self.assertEqual(error.exception.code, "unsupported_version")
        rpc.read.assert_not_called()
        selected = UsageProviderFactory(config(), hmac_key=KEY,
                                        rpc_factory=Mock(side_effect=RuntimeError("PRIVATE"))).create()
        with self.assertRaises(ProviderFailure) as error:
            selected.read_snapshot()
        self.assertNotIn("PRIVATE", str(error.exception))

    def test_public_dto_is_bounded_exact_and_defensively_copied(self):
        raw = provider_row()
        value = ProviderUsage.parse(raw)
        raw["jobs"]["total_tokens"] = "999"
        self.assertIsNone(value.to_dict()["jobs"]["total_tokens"])
        for mutate in (lambda row: row.update(email="PRIVATE"),
                       lambda row: row["jobs"].update(total_tokens=9007199254740999),
                       lambda row: row.update(observed_at="2026-02-30T00:00:00Z"),
                       lambda row: row.update(last_error="PRIVATE_DSN"),
                       lambda row: row.update(label="\ud800")):
            raw = provider_row()
            mutate(raw)
            with self.assertRaises(InvalidUsage):
                ProviderUsage.parse(raw)


class RpcTest(unittest.TestCase):
    def fake_process(self):
        return SimpleNamespace(pid=424242, stdin=io.BytesIO(), stdout=io.BytesIO(), stderr=io.BytesIO(),
                               wait=Mock(return_value=0))

    def test_exact_metadata_only_methods_framing_env_and_cleanup(self):
        process = self.fake_process()
        selected = Mock(return_value=process)
        connection = Mock()
        connection.request.side_effect = [ {}, {"account": {"type": "chatgpt"}}, quota(),
                                           metadata()["activity"], quota() ]
        signals = Mock()
        rpc = CodexMetadataRPC(process_config(), process_factory=selected,
                               signal_group=signals, cleanup_delay=lambda _delay: None)
        with patch("usage_rpc._Connection", return_value=connection), patch("os.geteuid", return_value=1000):
            raw = rpc.read()
        calls = connection.request.call_args_list
        self.assertEqual([args.args[1] for args in calls],
                         ["initialize", "account/read", "account/rateLimits/read",
                          "account/usage/read", "account/rateLimits/read"])
        self.assertEqual(calls[1].args[2], {"refreshToken": False})
        self.assertEqual(calls[2].args[2], {"supportsLunaReserve": False, "excludeResetCreditDetails": True})
        connection.send.assert_called_once_with({"method": "initialized"})
        args, kwargs = selected.call_args
        self.assertEqual(args[0], ["/opt/codex", "--no-daemon", "app-server", "--stdio", "-c", "analytics.enabled=false"])
        self.assertEqual(set(kwargs["env"]), {"HOME", "PATH", "LANG", "LC_ALL", "RUST_LOG", "CODEX_HOME"})
        self.assertTrue(kwargs["start_new_session"])
        self.assertEqual(signals.call_count, 2)
        process.wait.assert_called_once()
        connection.close.assert_called_once()
        self.assertIsNotNone(raw["quota"])

    def test_root_requires_explicit_signed_account_and_drops_groups(self):
        selected = Mock(return_value=self.fake_process())
        rpc = CodexMetadataRPC(process_config(), process_factory=selected, signal_group=Mock(), cleanup_delay=lambda _: None)
        with patch("os.geteuid", return_value=0):
            with self.assertRaises(ProviderFailure):
                rpc._start(["--version"])
        selected.assert_not_called()
        cfg = MetadataProcessConfig(Path("/opt/codex"), Path("/home/fixture"), run_uid=1000, run_gid=1000)
        rpc = CodexMetadataRPC(cfg, process_factory=selected, signal_group=Mock(), cleanup_delay=lambda _: None)
        with patch("os.geteuid", return_value=0):
            rpc._start(["--version"])
        self.assertEqual(selected.call_args.kwargs["user"], 1000)
        self.assertEqual(selected.call_args.kwargs["group"], 1000)
        self.assertEqual(selected.call_args.kwargs["extra_groups"], [])

    def test_no_account_starts_no_quota_call_and_cleans_up(self):
        connection = Mock()
        connection.request.side_effect = [{}, {"account": None, "requiresOpenaiAuth": True}]
        process = self.fake_process()
        rpc = CodexMetadataRPC(process_config(), process_factory=Mock(return_value=process),
                               signal_group=Mock(), cleanup_delay=lambda _: None)
        with patch("usage_rpc._Connection", return_value=connection), patch("os.geteuid", return_value=1000):
            with self.assertRaises(ProviderFailure) as error:
                rpc.read()
        self.assertEqual(error.exception.code, "auth_required")
        self.assertEqual(connection.request.call_count, 2)
        process.wait.assert_called_once()

    def test_connection_rejects_duplicate_json_nonfinite_and_unexpected_response(self):
        for line in (b'{"id":1,"id":1,"result":{}}', b'{"id":1,"result":{"x":NaN}}', b'[]', b'notjson'):
            connection = object.__new__(_Connection)
            connection.send = Mock()
            connection.line = Mock(return_value=line)
            with self.assertRaises(ProviderFailure) as error:
                connection.request(1, "initialize", {})
            self.assertEqual(error.exception.code, "malformed_metadata")

    def test_pipe_transport_bounds_timeout_eof_and_stderr_without_provider(self):
        for mode, expected in (("frame", "response_limit"), ("stream", "response_limit"),
                               ("silent", "timeout"), ("eof", "unavailable")):
            out_read, out_write = os.pipe()
            err_read, err_write = os.pipe()
            process = SimpleNamespace(stdout=os.fdopen(out_read, "rb", buffering=0),
                                      stderr=os.fdopen(err_read, "rb", buffering=0))
            connection = _Connection(process, timeout=0.01)
            try:
                if mode == "frame":
                    connection.MAX_FRAME = 4
                    os.write(out_write, b"12345\n")
                elif mode == "stream":
                    connection.MAX_STREAM = 4
                    os.write(err_write, b"PRIVATE STDERR")
                elif mode == "eof":
                    os.close(out_write)
                    out_write = None
                with self.assertRaises(ProviderFailure) as error:
                    connection.line()
                self.assertEqual(error.exception.code, expected)
            finally:
                connection.close()
                process.stdout.close()
                process.stderr.close()
                if out_write is not None:
                    os.close(out_write)
                os.close(err_write)

    def test_server_requests_never_execute_auth_or_process_actions(self):
        connection = object.__new__(_Connection)
        connection.send = Mock()
        connection.line = Mock(side_effect=[
            b'{"id":9,"method":"account/chatgptAuthTokens/refresh","params":{"private":"PRIVATE"}}',
            b'{"id":1,"result":{}}'])
        self.assertEqual(connection.request(1, "initialize", {}), {})
        self.assertEqual(connection.send.call_count, 2)
        self.assertEqual(connection.send.call_args.args[0]["error"]["code"], -32601)
        self.assertNotIn("PRIVATE", json.dumps(connection.send.call_args.args[0]))


if __name__ == "__main__":
    unittest.main()
