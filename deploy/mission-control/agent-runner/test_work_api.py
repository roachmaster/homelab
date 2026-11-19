"""Unit and authenticated HTTP contract tests for Forge coding-job admission."""

from contextlib import closing, contextmanager
from dataclasses import FrozenInstanceError
from http.client import HTTPConnection
import ast
import hashlib
import json
import re
from pathlib import Path
from tempfile import TemporaryDirectory
import threading
import unittest
from unittest.mock import Mock, patch
from uuid import UUID

import psycopg2

import chat_history_api
import runner
from queue_store import Rejected
import usage_store
import work_api
import work_config
import work_policy

TOKEN = "a" * 32
REQUEST_ID = "work-req-1"
TASK_ID = UUID("4fb285b2-5a53-4caa-90b6-d0aff012c0a4")
CURSOR_ID = UUID("72c85704-8c75-4e7e-982f-19cd5587842f")
BASE_REV = "2e1300a00da92e9f97bdb1a656c2b182d4219a6c"
RECIPE_DIGEST = "b" * 64
CREATED_AT = "2026-11-19T17:00:00Z"


def sample_raw_config():
    return {
        "owner": "mission-control",
        "allowed_projects": ["homelab", "docs"],
        "allowed_modes": ["plan", "review"],
        "max_pending": 16,
        "profiles": {
            "agy-plan": {
                "backend": "antigravity",
                "model": "gemini-2.5-pro",
                "resource_profile": "plan-default",
            },
            "codex-review": {
                "backend": "codex",
                "model": "gpt-5.4",
                "resource_profile": "review-default",
            },
        },
    }


def submit_body(**overrides):
    body = {
        "kind": "work_submit",
        "request_id": REQUEST_ID,
        "project_alias": "homelab",
        "base_revision": BASE_REV,
        "mode": "plan",
        "agent_profile": "agy-plan",
        "prompt": "Draft the admission plan.\nInclude bounds.",
        "acceptance_criteria": "Return a concise checklist.\tVerify limits.",
        "idempotency_key": "submit-key-1",
    }
    body.update(overrides)
    return body


def sample_summary(**overrides):
    summary = {
        "task_id": str(TASK_ID),
        "project_alias": "homelab",
        "base_revision": BASE_REV,
        "mode": "plan",
        "agent_profile": "agy-plan",
        "recipe_digest": RECIPE_DIGEST,
        "status": "queued",
        "revision": 1,
        "created_at": CREATED_AT,
    }
    summary.update(overrides)
    return summary


def reply(kind, request_id=REQUEST_ID, **fields):
    return {"protocol_version": 1, "event": {"kind": kind, "request_id": request_id, **fields}}


@contextmanager
def runner_server(*, work_service=None, usage_service=None, distributed=False):
    ready = threading.Event()
    ready.set()
    with runner.Server(
        TOKEN, ready, None, distributed=distributed,
        work_service=work_service, usage_service=usage_service, port=0
    ) as server:
        thread = threading.Thread(
            target=lambda: server.serve_forever(poll_interval=0.01), daemon=True
        )
        thread.start()
        try:
            yield server.server_port
        finally:
            server.shutdown()
            thread.join()


def work_post(port, destination, body, *, extra_headers=None, path=chat_history_api.PATH):
    headers = {
        "Authorization": f"Bearer {TOKEN}",
        "Content-Type": "application/json",
        "Forge-Envelope-Version": "1",
        "Forge-Contract-Version": "1",
        "Forge-Destination": destination.value if isinstance(destination, work_api.Destination) else destination,
        "Forge-Request-Id": REQUEST_ID,
    }
    if extra_headers:
        headers.update(extra_headers)
    payload = body if isinstance(body, bytes) else runner.workspace_json(body)
    with closing(HTTPConnection("127.0.0.1", port, timeout=3)) as connection:
        connection.request("POST", path, payload, headers)
        response = connection.getresponse()
        return response.status, json.loads(response.read()), response.getheader("Cache-Control")


class WorkConfigAndFactoryTest(unittest.TestCase):
    def test_runner_installer_includes_transitive_local_imports(self):
        root = Path(__file__).resolve().parent
        installer = (root / "setup-runner.sh").read_text()
        required = set(re.findall(r'(?<!_)publish "\$directory/([^"/]+\.py)" "\$target/\1"', installer))
        optional = set(re.findall(r'publish_if_present "\$directory/([^"/]+\.py)" "\$target/\1"', installer))
        published = required | {name for name in optional if (root / name).is_file()}
        self.assertIn("runner.py", required)
        self.assertIn("work_policy.py", required)
        self.assertIn("usage_store.py", required)
        for name in published:
            for node in ast.walk(ast.parse((root / name).read_text())):
                modules = ([item.name for item in node.names] if isinstance(node, ast.Import)
                           else [node.module] if isinstance(node, ast.ImportFrom) and node.module else [])
                for module in modules:
                    local = module.split(".")[0] + ".py"
                    if (root / local).is_file():
                        self.assertIn(local, published, f"{name} imports a module missing from the runner release")

    def test_parse_freezes_defensively_and_defaults_max_pending(self):
        raw = sample_raw_config()
        del raw["max_pending"]
        config = work_config.parse_work_config(raw)
        self.assertEqual(config.owner, "mission-control")
        self.assertEqual(config.allowed_projects, frozenset({"homelab", "docs"}))
        self.assertEqual(config.allowed_modes, frozenset({"plan", "review"}))
        self.assertEqual(config.max_pending, 32)
        self.assertEqual(set(config.profiles.keys()), {"agy-plan", "codex-review"})

        # Mutating caller structures must not affect live frozen grants.
        raw["allowed_projects"].append("mutated")
        raw["allowed_modes"].append("implement")
        raw["profiles"]["agy-plan"]["model"] = "tampered"
        raw["profiles"]["injected"] = {
            "backend": "codex", "model": "evil", "resource_profile": "x"
        }
        self.assertNotIn("mutated", config.allowed_projects)
        self.assertNotIn("implement", config.allowed_modes)
        self.assertEqual(config.profiles["agy-plan"].model, "gemini-2.5-pro")
        self.assertNotIn("injected", config.profiles)

        with self.assertRaises(FrozenInstanceError):
            config.owner = "other"
        with self.assertRaises(TypeError):
            config.profiles["x"] = config.profiles["agy-plan"]

    def test_parse_rejects_arbitrary_keys_and_invalid_bounds(self):
        valid = sample_raw_config()
        bad_cases = [
            "not-a-dict",
            {**valid, "path": "/etc/passwd"},
            {**valid, "env": {"SECRET": "1"}},
            {**valid, "command": ["agy"]},
            {**valid, "options": ["--yolo"]},
            {k: v for k, v in valid.items() if k != "owner"},
            {**valid, "owner": ""},
            {**valid, "owner": "   "},
            {**valid, "owner": "a" * 129},
            {**valid, "owner": "café"},
            {**valid, "owner": "owner\nline2"},
            {**valid, "allowed_projects": []},
            {**valid, "allowed_projects": ["homelab", "homelab"]},
            {**valid, "allowed_projects": ["bad project"]},
            {**valid, "allowed_modes": []},
            {**valid, "allowed_modes": ["plan", "plan"]},
            {**valid, "allowed_modes": ["execute"]},
            {**valid, "max_pending": 0},
            {**valid, "max_pending": 257},
            {**valid, "max_pending": True},
            {**valid, "max_pending": "32"},
            {**valid, "profiles": {}},
            {**valid, "profiles": {"bad space": valid["profiles"]["agy-plan"]}},
            {**valid, "profiles": {"p": {**valid["profiles"]["agy-plan"], "env": {}}}},
            {**valid, "profiles": {"p": {**valid["profiles"]["agy-plan"], "path": "/bin/sh"}}},
            {**valid, "profiles": {"p": {**valid["profiles"]["agy-plan"], "command": "agy"}}},
            {**valid, "profiles": {"p": {**valid["profiles"]["agy-plan"], "generation_token_limit": 256}}},
            {**valid, "profiles": {"p": {**valid["profiles"]["agy-plan"], "backend": "llama"}}},
            {**valid, "profiles": {"p": {**valid["profiles"]["agy-plan"], "model": "  "}}},
            {**valid, "profiles": {"p": {**valid["profiles"]["agy-plan"], "model": "m" * 129}}},
            {**valid, "profiles": {"p": {**valid["profiles"]["agy-plan"], "resource_profile": "bad/id"}}},
        ]
        for case in bad_cases:
            with self.subTest(case=case):
                with self.assertRaises(ValueError):
                    work_config.parse_work_config(case)

    def test_load_work_config_absent_has_zero_effects_and_errors_hide_secrets(self):
        with patch.object(Path, "stat", side_effect=AssertionError("must not touch fs")):
            self.assertIsNone(work_config.load_work_config(None))

        with TemporaryDirectory(prefix="forge-work-cfg-") as tmp:
            root = Path(tmp)
            valid_file = root / "work.json"
            valid_file.write_text(json.dumps(sample_raw_config()), encoding="utf-8")
            loaded = work_config.load_work_config(str(valid_file))
            self.assertEqual(loaded.owner, "mission-control")

            secret_bad = root / "bad.json"
            secret_bad.write_text(
                json.dumps({**sample_raw_config(), "env": {"API_KEY": "TOP_SECRET_VALUE"}}),
                encoding="utf-8",
            )
            with self.assertRaises(ValueError) as ctx:
                work_config.load_work_config(str(secret_bad))
            self.assertNotIn("TOP_SECRET_VALUE", str(ctx.exception))

            dup_file = root / "dup.json"
            dup_file.write_text('{"owner":"a","owner":"b"}', encoding="utf-8")
            nan_file = root / "nan.json"
            nan_file.write_text('{"owner":NaN}', encoding="utf-8")
            big_file = root / "big.json"
            big_file.write_bytes(b" " * (work_config.MAX_CONFIG_BYTES + 1))

            for bad_path in ("", "relative/work.json", str(root / "missing.json"), str(root),
                             str(dup_file), str(nan_file), str(big_file)):
                with self.subTest(bad_path=bad_path):
                    with self.assertRaises(ValueError):
                        work_config.load_work_config(bad_path)

    def test_pure_catalog_pins_nonsecret_recipe_with_null_token_limit(self):
        config = work_config.parse_work_config(sample_raw_config())
        catalog = config.catalog()
        recipe = catalog.resolve("agy-plan", "plan")
        expected_dict = {
            "version": 1,
            "backend": "antigravity",
            "model": "gemini-2.5-pro",
            "resource_profile": "plan-default",
            "mode": "plan",
            "generation_token_limit": None,
        }
        expected_digest = hashlib.sha256(
            json.dumps(expected_dict, sort_keys=True, separators=(",", ":")).encode("utf-8")
        ).hexdigest()
        self.assertEqual(recipe.to_dict(), expected_dict)
        self.assertEqual(recipe.digest, expected_digest)
        self.assertIsNone(recipe.generation_token_limit)

        copy = recipe.to_dict()
        copy["generation_token_limit"] = 999
        self.assertIsNone(recipe.to_dict()["generation_token_limit"])

        with self.assertRaises(ValueError):
            work_config.PinnedRecipe(
                version=1, backend="codex", model="gpt-5.4", resource_profile="r",
                mode="plan", generation_token_limit=256, digest="0" * 64,
            )

    def test_v2_policy_profile_pins_immutable_policy_digest_and_title_bounds(self):
        raw = sample_raw_config()
        raw["project_labels"] = {"homelab": "Homelab Repo"}
        raw["profiles"]["codex-v2"] = {
            "backend": "codex",
            "model": "gpt-5.4",
            "resource_profile": work_policy.POLICY_ID,
            "label": "Codex 5.4",
        }
        config = work_config.parse_work_config(raw)
        self.assertEqual(config.project_label("homelab"), "Homelab Repo")
        self.assertEqual(config.project_label("docs"), "docs")
        self.assertEqual(config.profile_label("codex-v2"), "Codex 5.4")
        self.assertEqual(config.profile_label("agy-plan"), "agy-plan")

        recipe_v2 = config.catalog().resolve("codex-v2", "review")
        self.assertEqual(recipe_v2.version, 2)
        self.assertEqual(recipe_v2.policy_digest, work_policy.POLICY_DIGEST)
        self.assertEqual(set(recipe_v2.to_dict().keys()), {
            "version", "backend", "model", "resource_profile",
            "mode", "generation_token_limit", "policy_digest",
        })
        self.assertIsNone(recipe_v2.to_dict()["generation_token_limit"])

        # Title preparation picks first non-blank line, collapses whitespace, and caps at 128 chars / 512 bytes.
        self.assertEqual(work_config.prepare_title("\n  \t  First   line\there \nSecond line"), "First line here")
        emoji_title = work_config.prepare_title("😀" * 200)
        self.assertEqual(len(emoji_title), 128)
        self.assertEqual(len(emoji_title.encode("utf-8")), 512)

    def test_factory_is_effect_free_and_zero_config_never_connects(self):
        connect = Mock(side_effect=AssertionError("must not connect during factory construction"))
        self.assertIsNone(work_api.WorkServiceFactory(None, connect).create())
        connect.assert_not_called()

        config = work_config.parse_work_config(sample_raw_config())
        service = work_api.WorkServiceFactory(config, connect).create()
        self.assertIsInstance(service, work_api.WorkService)
        connect.assert_not_called()


class WorkCatalogSubstitutionTest(unittest.TestCase):
    def test_catalog_substitution_cannot_expand_configured_profile_grants(self):
        config = work_config.parse_work_config(sample_raw_config())
        other = work_config.WorkProfile("ungranted", "codex", "fixture-model", "fixture")
        catalog = work_config.WorkCatalog({**config.profiles, other.alias: other})
        store = Mock()
        service = work_api.WorkService(config, catalog, store)
        command = work_api.decode(work_api.Destination.SUBMIT, REQUEST_ID,
                                  submit_body(agent_profile="ungranted"))
        with self.assertRaises(Rejected) as error:
            service.submit(command)
        self.assertEqual((error.exception.status, error.exception.code), (403, "denied"))
        store.submit.assert_not_called()

    def test_receipt_replay_does_not_require_recipe_in_replacement_catalog(self):
        config = work_config.parse_work_config(sample_raw_config())
        catalog = work_config.WorkCatalog({"codex-review": config.profiles["codex-review"]})
        command = work_api.decode(work_api.Destination.SUBMIT, REQUEST_ID, submit_body())
        receipt = {"task_id": str(TASK_ID), "recipe_digest": RECIPE_DIGEST}
        store = Mock()
        store.submit.return_value = receipt
        service = work_api.WorkService(config, catalog, store)
        self.assertEqual(service.submit(command), receipt)
        # Simulate new admission, which resolves only after the store's replay lookup.
        store.submit.side_effect = lambda **fields: fields["resolve_recipe"]()
        with self.assertRaises(Rejected) as error:
            service.submit(command)
        self.assertEqual((error.exception.status, error.exception.code), (503, "unavailable"))


class WorkDecodeTest(unittest.TestCase):
    def test_valid_requests_and_defaults_decode_strictly(self):
        cmd = work_api.decode(work_api.Destination.SUBMIT, REQUEST_ID, submit_body(
            prompt="é" * 4096,  # exact 8192 UTF-8 bytes
            acceptance_criteria="é" * 1024,  # exact 2048 UTF-8 bytes
        ))
        self.assertEqual(cmd.project_alias, "homelab")
        self.assertEqual(cmd.mode, "plan")

        get_cmd = work_api.decode(work_api.Destination.GET, REQUEST_ID, {
            "kind": "work_get", "request_id": REQUEST_ID, "project_alias": "homelab", "task_id": str(TASK_ID),
        })
        self.assertEqual(get_cmd, work_api.WorkGet("homelab", TASK_ID))

        list_default = work_api.decode(work_api.Destination.LIST, REQUEST_ID, {
            "kind": "work_list", "request_id": REQUEST_ID, "project_alias": "homelab",
        })
        self.assertEqual(list_default, work_api.WorkList("homelab", None, 20, None))

        list_explicit_nulls = work_api.decode(work_api.Destination.LIST, REQUEST_ID, {
            "kind": "work_list", "request_id": REQUEST_ID, "project_alias": "homelab",
            "status_filter": None, "limit": 50, "cursor": None,
        })
        self.assertEqual(list_explicit_nulls, work_api.WorkList("homelab", None, 50, None))

        list_cursor = work_api.decode(work_api.Destination.LIST, REQUEST_ID, {
            "kind": "work_list", "request_id": REQUEST_ID, "project_alias": "homelab",
            "status_filter": "cancelled", "limit": 1, "cursor": str(CURSOR_ID),
        })
        self.assertEqual(list_cursor, work_api.WorkList("homelab", "cancelled", 1, CURSOR_ID))

        events_default = work_api.decode(work_api.Destination.EVENTS, REQUEST_ID, {
            "kind": "work_events", "request_id": REQUEST_ID, "project_alias": "homelab", "task_id": str(TASK_ID),
        })
        self.assertEqual(events_default, work_api.WorkEvents("homelab", TASK_ID, 0, 50))

        events_max = work_api.decode(work_api.Destination.EVENTS, REQUEST_ID, {
            "kind": "work_events", "request_id": REQUEST_ID, "project_alias": "homelab", "task_id": str(TASK_ID),
            "after_sequence": 2**53 - 1, "limit": 100,
        })
        self.assertEqual(events_max, work_api.WorkEvents("homelab", TASK_ID, 2**53 - 1, 100))

        cancel_cmd = work_api.decode(work_api.Destination.CANCEL, REQUEST_ID, {
            "kind": "work_cancel", "request_id": REQUEST_ID, "project_alias": "homelab",
            "task_id": str(TASK_ID), "expected_revision": 2**53 - 1, "idempotency_key": "cancel-1",
        })
        self.assertEqual(cancel_cmd, work_api.WorkCancel("homelab", TASK_ID, 2**53 - 1, "cancel-1"))

    def test_invalid_payloads_rejected_with_400(self):
        cases = [
            (work_api.Destination.SUBMIT, submit_body(request_id="other")),
            (work_api.Destination.SUBMIT, submit_body(owner="attacker")),
            (work_api.Destination.SUBMIT, submit_body(env={"A": "1"})),
            (work_api.Destination.SUBMIT, submit_body(path="/tmp")),
            (work_api.Destination.SUBMIT, submit_body(binary="agy")),
            (work_api.Destination.SUBMIT, submit_body(project_alias="bad space")),
            (work_api.Destination.SUBMIT, submit_body(project_alias="a" * 65)),
            (work_api.Destination.SUBMIT, submit_body(project_alias="café")),
            (work_api.Destination.SUBMIT, submit_body(agent_profile="")),
            (work_api.Destination.SUBMIT, submit_body(idempotency_key="k" * 129)),
            (work_api.Destination.SUBMIT, submit_body(base_revision=BASE_REV.upper())),
            (work_api.Destination.SUBMIT, submit_body(base_revision=BASE_REV[:-1])),
            (work_api.Destination.SUBMIT, submit_body(mode="execute")),
            (work_api.Destination.SUBMIT, submit_body(prompt="   \t\r\n ")),
            (work_api.Destination.SUBMIT, submit_body(prompt="ok\x00bad")),
            (work_api.Destination.SUBMIT, submit_body(prompt="ok\x07bad")),
            (work_api.Destination.SUBMIT, submit_body(prompt="ok\ud800bad")),
            (work_api.Destination.SUBMIT, submit_body(prompt="a" * 8193)),
            (work_api.Destination.SUBMIT, submit_body(acceptance_criteria="a" * 2049)),
            (work_api.Destination.SUBMIT, submit_body(acceptance_criteria="\x7f")),
            (work_api.Destination.GET, {
                "kind": "work_get", "request_id": REQUEST_ID, "project_alias": "homelab",
                "task_id": str(TASK_ID).upper(),
            }),
            (work_api.Destination.GET, {
                "kind": "work_get", "request_id": REQUEST_ID, "project_alias": "homelab",
                "task_id": TASK_ID.hex,
            }),
            (work_api.Destination.LIST, {
                "kind": "work_list", "request_id": REQUEST_ID, "project_alias": "homelab",
                "status_filter": "bogus_status",
            }),
            (work_api.Destination.LIST, {
                "kind": "work_list", "request_id": REQUEST_ID, "project_alias": "homelab",
                "limit": 0,
            }),
            (work_api.Destination.LIST, {
                "kind": "work_list", "request_id": REQUEST_ID, "project_alias": "homelab",
                "limit": 51,
            }),
            (work_api.Destination.LIST, {
                "kind": "work_list", "request_id": REQUEST_ID, "project_alias": "homelab",
                "limit": None,
            }),
            (work_api.Destination.LIST, {
                "kind": "work_list", "request_id": REQUEST_ID, "project_alias": "homelab",
                "limit": True,
            }),
            (work_api.Destination.LIST, {
                "kind": "work_list", "request_id": REQUEST_ID, "project_alias": "homelab",
                "cursor": str(CURSOR_ID).upper(),
            }),
            (work_api.Destination.EVENTS, {
                "kind": "work_events", "request_id": REQUEST_ID, "project_alias": "homelab",
                "task_id": str(TASK_ID), "after_sequence": -1,
            }),
            (work_api.Destination.EVENTS, {
                "kind": "work_events", "request_id": REQUEST_ID, "project_alias": "homelab",
                "task_id": str(TASK_ID), "after_sequence": 2**53,
            }),
            (work_api.Destination.EVENTS, {
                "kind": "work_events", "request_id": REQUEST_ID, "project_alias": "homelab",
                "task_id": str(TASK_ID), "limit": 101,
            }),
            (work_api.Destination.CANCEL, {
                "kind": "work_cancel", "request_id": REQUEST_ID, "project_alias": "homelab",
                "task_id": str(TASK_ID), "expected_revision": 0, "idempotency_key": "k1",
            }),
            (work_api.Destination.CANCEL, {
                "kind": "work_cancel", "request_id": REQUEST_ID, "project_alias": "homelab",
                "task_id": str(TASK_ID), "expected_revision": True, "idempotency_key": "k1",
            }),
            (work_api.Destination.CANCEL, {
                "kind": "work_cancel", "request_id": REQUEST_ID, "project_alias": "homelab",
                "task_id": str(TASK_ID), "expected_revision": 2**53, "idempotency_key": "k1",
            }),
        ]
        for route, body in cases:
            with self.subTest(route=route, body=body):
                with self.assertRaises(Rejected) as ctx:
                    work_api.decode(route, REQUEST_ID, body)
                self.assertEqual((ctx.exception.status, ctx.exception.code), (400, "invalid_request"))


class WorkHttpApiTest(unittest.TestCase):
    def _service_with_mock_store(self):
        config = work_config.parse_work_config(sample_raw_config())
        fake_store = Mock()
        return work_api.WorkService(config, config.catalog(), fake_store), fake_store

    def test_all_five_work_routes_succeed_with_distributed_false_without_enabling_mail_or_chat_runs(self):
        service, fake_store = self._service_with_mock_store()
        fake_store.submit.return_value = {
            "task_id": str(TASK_ID),
            "project_alias": "homelab",
            "revision": 1,
            "status": "queued",
            "recipe_digest": RECIPE_DIGEST,
        }
        fake_store.get.return_value = {
            **sample_summary(),
            "prompt": "Draft the admission plan.",
            "acceptance_criteria": "Return a concise checklist.",
        }
        fake_store.list_tasks.return_value = {
            "project_alias": "homelab",
            "tasks": [sample_summary()],
            "next_cursor": str(TASK_ID),
        }
        fake_store.events.return_value = {
            "project_alias": "homelab",
            "task_id": str(TASK_ID),
            "revision": 1,
            "status": "queued",
            "events": [{"sequence": 1, "kind": "queued", "created_at": CREATED_AT}],
            "next_sequence": 1,
        }
        fake_store.cancel.return_value = {
            "project_alias": "homelab",
            "task_id": str(TASK_ID),
            "base_revision": 1,
            "revision": 2,
            "status": "cancelled",
        }

        with (runner_server(work_service=service, distributed=False) as port,
              patch("subprocess.Popen", side_effect=AssertionError("no subprocess")),
              patch("subprocess.run", side_effect=AssertionError("no subprocess"))):
            self.assertEqual(
                work_post(port, work_api.Destination.SUBMIT, submit_body()),
                (202, reply("work_submitted", **fake_store.submit.return_value), "no-store"),
            )
            self.assertEqual(
                work_post(port, work_api.Destination.GET, {
                    "kind": "work_get", "request_id": REQUEST_ID,
                    "project_alias": "homelab", "task_id": str(TASK_ID),
                }),
                (200, reply("work_opened", task=fake_store.get.return_value), "no-store"),
            )
            self.assertEqual(
                work_post(port, work_api.Destination.LIST, {
                    "kind": "work_list", "request_id": REQUEST_ID, "project_alias": "homelab",
                }),
                (200, reply("work_listed", **fake_store.list_tasks.return_value), "no-store"),
            )
            self.assertEqual(
                work_post(port, work_api.Destination.EVENTS, {
                    "kind": "work_events", "request_id": REQUEST_ID,
                    "project_alias": "homelab", "task_id": str(TASK_ID),
                }),
                (200, reply("work_events", **fake_store.events.return_value), "no-store"),
            )
            self.assertEqual(
                work_post(port, work_api.Destination.CANCEL, {
                    "kind": "work_cancel", "request_id": REQUEST_ID,
                    "project_alias": "homelab", "task_id": str(TASK_ID),
                    "expected_revision": 1, "idempotency_key": "cancel-1",
                }),
                (200, reply("work_cancel_recorded", **fake_store.cancel.return_value), "no-store"),
            )
            # Distributed=False still gates Mail and async Chat routes unchanged.
            self.assertEqual(
                work_post(port, "ide.mail.list", {"kind": "mail_list", "request_id": REQUEST_ID}),
                (503, reply("mail_failed", code="unavailable"), "no-store"),
            )
            self.assertEqual(
                work_post(port, "ide.chat.run.models", {"kind": "chat_run_models", "request_id": REQUEST_ID}),
                (503, reply("chat_run_failed", code="unavailable"), "no-store"),
            )

    def test_default_off_returns_503_for_supported_and_404_for_unsupported_work_destinations(self):
        with runner_server(work_service=None, distributed=True) as port:
            for destination, body in [
                (work_api.Destination.SUBMIT, submit_body()),
                (work_api.Destination.GET, {
                    "kind": "work_get", "request_id": REQUEST_ID,
                    "project_alias": "homelab", "task_id": str(TASK_ID),
                }),
                (work_api.Destination.LIST, {
                    "kind": "work_list", "request_id": REQUEST_ID, "project_alias": "homelab",
                }),
                (work_api.Destination.EVENTS, {
                    "kind": "work_events", "request_id": REQUEST_ID,
                    "project_alias": "homelab", "task_id": str(TASK_ID),
                }),
                (work_api.Destination.CANCEL, {
                    "kind": "work_cancel", "request_id": REQUEST_ID,
                    "project_alias": "homelab", "task_id": str(TASK_ID),
                    "expected_revision": 1, "idempotency_key": "cancel-1",
                }),
            ]:
                with self.subTest(destination=destination):
                    self.assertEqual(
                        work_post(port, destination, body),
                        (503, reply("work_failed", code="unavailable"), "no-store"),
                    )

            for unsupported in ("ide.work.unknown", "ide.work.delete"):
                with self.subTest(unsupported=unsupported):
                    self.assertEqual(
                        work_post(port, unsupported, submit_body()),
                        (404, reply("work_failed", code="not_found"), "no-store"),
                    )

            for extended_dest, body in [
                (work_api.Destination.CATALOG, {"kind": "work_catalog", "request_id": REQUEST_ID}),
                (work_api.Destination.RESULT, {
                    "kind": "work_result", "request_id": REQUEST_ID,
                    "project_alias": "homelab", "task_id": str(TASK_ID), "attempt_id": str(CURSOR_ID),
                }),
                (work_api.Destination.ACCEPT, {
                    "kind": "work_accept", "request_id": REQUEST_ID,
                    "project_alias": "homelab", "task_id": str(TASK_ID),
                    "expected_revision": 2, "idempotency_key": "acc-1",
                }),
                (work_api.Destination.RESUME, {
                    "kind": "work_resume", "request_id": REQUEST_ID,
                    "project_alias": "homelab", "task_id": str(TASK_ID),
                    "expected_revision": 2, "idempotency_key": "res-1",
                    "agent_profile": "agy-plan", "reconciliation_receipt_id": str(CURSOR_ID),
                }),
            ]:
                with self.subTest(extended_dest=extended_dest):
                    self.assertEqual(
                        work_post(port, extended_dest, body),
                        (503, reply("work_failed", code="unavailable"), "no-store"),
                    )

    def test_denied_grants_and_invalid_ingress_have_zero_store_effects(self):
        config = work_config.parse_work_config(sample_raw_config())
        connect = Mock(side_effect=AssertionError("store connection must not be opened"))
        service = work_api.WorkServiceFactory(config, connect).create()

        with runner_server(work_service=service, distributed=False) as port:
            # Denied project, mode, and profile -> 403 denied before store.
            for body in [
                submit_body(project_alias="unconfigured-project"),
                submit_body(mode="implement"),
                submit_body(agent_profile="unconfigured-profile"),
            ]:
                with self.subTest(denied_submit=body):
                    self.assertEqual(
                        work_post(port, work_api.Destination.SUBMIT, body),
                        (403, reply("work_failed", code="denied"), "no-store"),
                    )

            for destination, body in [
                (work_api.Destination.GET, {
                    "kind": "work_get", "request_id": REQUEST_ID,
                    "project_alias": "denied-proj", "task_id": str(TASK_ID),
                }),
                (work_api.Destination.LIST, {
                    "kind": "work_list", "request_id": REQUEST_ID,
                    "project_alias": "denied-proj", "cursor": str(CURSOR_ID),
                }),
                (work_api.Destination.EVENTS, {
                    "kind": "work_events", "request_id": REQUEST_ID,
                    "project_alias": "denied-proj", "task_id": str(TASK_ID),
                }),
                (work_api.Destination.CANCEL, {
                    "kind": "work_cancel", "request_id": REQUEST_ID,
                    "project_alias": "denied-proj", "task_id": str(TASK_ID),
                    "expected_revision": 1, "idempotency_key": "k1",
                }),
            ]:
                with self.subTest(denied_route=destination):
                    self.assertEqual(
                        work_post(port, destination, body),
                        (403, reply("work_failed", code="denied"), "no-store"),
                    )

            # Unsupported Work destination with active service still returns 404.
            self.assertEqual(
                work_post(port, "ide.work.unknown", submit_body()),
                (404, reply("work_failed", code="not_found"), "no-store"),
            )

            # Auth, header, size, duplicate JSON, surrogate, and non-UTF8 rejections.
            dup_json = json.dumps(submit_body()).replace(
                '"mode": "plan"', '"mode": "plan", "mode": "review"'
            ).encode("utf-8")
            overescaped = json.dumps(submit_body(prompt="a" * 3000)).replace("a", "\\u0061").encode("utf-8")
            self.assertGreater(len(overescaped), work_api.MAX_REQUEST_BYTES)

            for body, extra_headers, status, expected in [
                (submit_body(), {"Authorization": "Bearer wrong"}, 401, {"error": "unauthorized"}),
                (submit_body(), {"Forge-Request-Id": "bad,id"}, 400, {"error": "invalid_request"}),
                (submit_body(), {"Forge-Contract-Version": "2"}, 400, reply("work_failed", code="invalid_request")),
                (submit_body(), {"Forge-Envelope-Version": "2"}, 400, reply("work_failed", code="invalid_request")),
                (submit_body(), {"Forge-Extra": "1"}, 400, reply("work_failed", code="invalid_request")),
                (submit_body(), {"Content-Type": "text/plain"}, 415, reply("work_failed", code="invalid_request")),
                (b"x" * (work_api.MAX_REQUEST_BYTES + 1), {}, 413, reply("work_failed", code="invalid_request")),
                (overescaped, {}, 413, reply("work_failed", code="invalid_request")),
                (dup_json, {}, 400, reply("work_failed", code="invalid_request")),
                (b'{"kind":"work_submit","request_id":"work-req-1","prompt":"\\ud800"}', {},
                 400, reply("work_failed", code="invalid_request")),
                (b"\xff\xfe", {}, 400, reply("work_failed", code="invalid_request")),
                (submit_body(owner="spoofed"), {}, 400, reply("work_failed", code="invalid_request")),
            ]:
                with self.subTest(status=status, extra_headers=extra_headers):
                    actual_status, actual_body, cache = work_post(
                        port, work_api.Destination.SUBMIT, body, extra_headers=extra_headers
                    )
                    self.assertEqual((actual_status, actual_body, cache), (status, expected, "no-store"))

            # Duplicate Forge-Destination and Authorization headers.
            payload = runner.workspace_json(submit_body())
            for dup_name, dup_value, expected_status in [
                ("Authorization", f"Bearer {TOKEN}", 401),
                ("Forge-Destination", "ide.work.get", 400),
                ("Forge-Request-Id", REQUEST_ID, 400),
            ]:
                with closing(HTTPConnection("127.0.0.1", port, timeout=3)) as conn:
                    conn.putrequest("POST", chat_history_api.PATH)
                    for k, v in {
                        "Authorization": f"Bearer {TOKEN}",
                        "Content-Type": "application/json",
                        "Content-Length": str(len(payload)),
                        "Forge-Envelope-Version": "1",
                        "Forge-Contract-Version": "1",
                        "Forge-Destination": work_api.Destination.SUBMIT.value,
                        "Forge-Request-Id": REQUEST_ID,
                    }.items():
                        conn.putheader(k, v)
                    conn.putheader(dup_name, dup_value)
                    conn.endheaders(payload)
                    self.assertEqual(conn.getresponse().status, expected_status)

            connect.assert_not_called()

    def test_typed_store_errors_and_db_outages_never_leak_internal_details(self):
        service, fake_store = self._service_with_mock_store()
        with runner_server(work_service=service, distributed=False) as port:
            for error, status, code in [
                (Rejected(404, "not_found"), 404, "not_found"),
                (Rejected(409, "idempotency_conflict"), 409, "idempotency_conflict"),
                (Rejected(409, "revision_conflict"), 409, "revision_conflict"),
                (Rejected(409, "not_cancellable"), 409, "not_cancellable"),
                (Rejected(503, "queue_full"), 503, "queue_full"),
                (Rejected(500, "internal_secret_code"), 503, "unavailable"),
                (psycopg2.OperationalError("password=SUPER_SECRET_DSN"), 503, "unavailable"),
                (TimeoutError("secret_timeout_detail"), 503, "unavailable"),
            ]:
                with self.subTest(code=code, error=type(error).__name__):
                    fake_store.submit.side_effect = error
                    actual_status, body, cache = work_post(port, work_api.Destination.SUBMIT, submit_body())
                    self.assertEqual((actual_status, body, cache), (status, reply("work_failed", code=code), "no-store"))
                    serialized = json.dumps(body)
                    self.assertNotIn("SUPER_SECRET_DSN", serialized)
                    self.assertNotIn("internal_secret_code", serialized)
                    self.assertNotIn("secret_timeout_detail", serialized)

    def test_extended_work_routes_and_new_conflict_codes(self):
        service, fake_store = self._service_with_mock_store()
        fake_store.catalog.return_value = {
            "projects": [{"alias": "homelab", "label": "homelab", "base_revision": BASE_REV}],
            "profiles": [{
                "alias": "agy-plan", "label": "agy-plan",
                "backend": "antigravity", "model": "gemini-2.5-pro", "modes": ["plan"],
            }],
            "worker_status": "ready",
            "observed_at": CREATED_AT,
        }
        fake_store.result.return_value = {
            "project_alias": "homelab",
            "task_id": str(TASK_ID),
            "attempt_id": str(CURSOR_ID),
            "result_digest": RECIPE_DIGEST,
            "content": "hello",
            "offset_bytes": 0,
            "next_cursor": None,
            "complete": True,
            "total_bytes": 5,
        }
        fake_store.accept.return_value = {
            "project_alias": "homelab",
            "task_id": str(TASK_ID),
            "base_revision": 3,
            "revision": 4,
            "status": "completed",
        }
        fake_store.resume.return_value = {
            "project_alias": "homelab",
            "task_id": str(TASK_ID),
            "base_revision": 3,
            "revision": 4,
            "status": "queued",
            "recipe_digest": RECIPE_DIGEST,
        }

        with runner_server(work_service=service, distributed=False) as port:
            self.assertEqual(
                work_post(port, work_api.Destination.CATALOG, {
                    "kind": "work_catalog", "request_id": REQUEST_ID,
                }),
                (200, reply("work_catalog", **fake_store.catalog.return_value), "no-store"),
            )
            self.assertEqual(
                work_post(port, work_api.Destination.RESULT, {
                    "kind": "work_result", "request_id": REQUEST_ID,
                    "project_alias": "homelab", "task_id": str(TASK_ID), "attempt_id": str(CURSOR_ID),
                    "cursor": f"{RECIPE_DIGEST}.0", "limit_bytes": 16,
                }),
                (200, reply("work_result_page", **fake_store.result.return_value), "no-store"),
            )
            self.assertEqual(
                work_post(port, work_api.Destination.ACCEPT, {
                    "kind": "work_accept", "request_id": REQUEST_ID,
                    "project_alias": "homelab", "task_id": str(TASK_ID),
                    "expected_revision": 3, "idempotency_key": "acc-1", "review_note": "Looks good",
                }),
                (200, reply("work_accepted", **fake_store.accept.return_value), "no-store"),
            )
            self.assertEqual(
                work_post(port, work_api.Destination.RESUME, {
                    "kind": "work_resume", "request_id": REQUEST_ID,
                    "project_alias": "homelab", "task_id": str(TASK_ID),
                    "expected_revision": 3, "idempotency_key": "res-1",
                    "agent_profile": "agy-plan", "reconciliation_receipt_id": str(CURSOR_ID),
                    "note": "Retry after stop",
                }),
                (202, reply("work_resumed", **fake_store.resume.return_value), "no-store"),
            )
            for code in ("reconcile_required", "not_reviewable", "stale_reconciliation", "result_mismatch"):
                fake_store.resume.side_effect = Rejected(409, code)
                self.assertEqual(
                    work_post(port, work_api.Destination.RESUME, {
                        "kind": "work_resume", "request_id": REQUEST_ID,
                        "project_alias": "homelab", "task_id": str(TASK_ID),
                        "expected_revision": 3, "idempotency_key": "res-err",
                        "agent_profile": "agy-plan", "reconciliation_receipt_id": str(CURSOR_ID),
                    }),
                    (409, reply("work_failed", code=code), "no-store"),
                )

    def test_usage_factory_injection_independent_config_and_routing_before_gpu_flags(self):
        created_factories = []

        class FakeUsageService:
            def __init__(self, config, store):
                self.config = config
                self.store = store

        class FakeUsageServiceFactory:
            def __init__(self, config, store_factory):
                self.config = config
                self.store_factory = store_factory
                created_factories.append(self)

            def create(self):
                if self.config is None:
                    return None
                return FakeUsageService(self.config, self.store_factory(self.config))

        class FakeUsageApi:
            MAX_REQUEST_BYTES = 16 * 1024
            UsageServiceFactory = FakeUsageServiceFactory

            @staticmethod
            def handles(headers):
                return any(v.startswith("ide.usage.") for v in headers.get_all("Forge-Destination", []))

            @staticmethod
            def destination(headers):
                dest = chat_history_api.dispatch_destination(headers)
                if dest not in {"ide.usage.overview", "ide.usage.detail", "ide.usage.refresh"}:
                    raise Rejected(404, "not_found")
                return dest

            @staticmethod
            def decode(route, correlation, body):
                if not isinstance(body, dict) or body.get("request_id") != correlation:
                    raise Rejected(400, "invalid_request")
                return (route, body)

            @staticmethod
            def failed(correlation, error):
                code = "not_found" if error.status == 404 else "unavailable"
                status = 404 if error.status == 404 else 503
                return status, chat_history_api.envelope("usage_failed", correlation, code=code)

            @staticmethod
            def dispatch(route, correlation, command, service):
                return 200, chat_history_api.envelope(
                    "usage_overview", correlation,
                    project_alias=command[1]["project_alias"],
                    providers=[],
                    owner=service.store.owner,
                )

        no_connect = Mock(side_effect=AssertionError("must not connect during startup factory"))
        # Both disabled -> zero file/DB effects.
        with patch.object(Path, "stat", side_effect=AssertionError("must not touch fs")):
            work_svc, usage_svc = runner.build_work_and_usage_services(
                {}, no_connect, usage_api_module=FakeUsageApi
            )
        self.assertIsNone(work_svc)
        self.assertIsNone(usage_svc)
        no_connect.assert_not_called()

        with TemporaryDirectory(prefix="forge-usage-cfg-") as tmp:
            cfg_path = Path(tmp) / "usage.json"
            cfg_path.write_text(json.dumps(sample_raw_config()), encoding="utf-8")

            # Work enabled alone does NOT implicitly enable usage.
            work_only, usage_off = runner.build_work_and_usage_services(
                {work_config.CONFIG_ENV_VAR: str(cfg_path)}, no_connect, usage_api_module=FakeUsageApi
            )
            self.assertIsInstance(work_only, work_api.WorkService)
            self.assertIsNone(usage_off)

            # Usage enabled alone works independently and injects UsageStore via lambda.
            work_off, usage_only = runner.build_work_and_usage_services(
                {work_config.USAGE_CONFIG_ENV_VAR: str(cfg_path)}, no_connect, usage_api_module=FakeUsageApi
            )
            self.assertIsNone(work_off)
            self.assertIsInstance(usage_only, FakeUsageService)
            self.assertIsInstance(usage_only.store, usage_store.UsageStore)
            no_connect.assert_not_called()

            # Usage routes run before GPU distributed=False check and default-off returns 503/404.
            with patch.object(runner, "usage_api", FakeUsageApi):
                with runner_server(work_service=None, usage_service=usage_only, distributed=False) as port:
                    self.assertEqual(
                        work_post(port, "ide.usage.overview", {
                            "kind": "usage_overview", "request_id": REQUEST_ID, "project_alias": "homelab",
                        }),
                        (200, reply("usage_overview", project_alias="homelab", providers=[], owner="mission-control"), "no-store"),
                    )
                    self.assertEqual(
                        work_post(port, "ide.usage.unknown", {
                            "kind": "usage_overview", "request_id": REQUEST_ID, "project_alias": "homelab",
                        }),
                        (404, reply("usage_failed", code="not_found"), "no-store"),
                    )
                with runner_server(work_service=None, usage_service=None, distributed=True) as port:
                    self.assertEqual(
                        work_post(port, "ide.usage.overview", {
                            "kind": "usage_overview", "request_id": REQUEST_ID, "project_alias": "homelab",
                        }),
                        (503, reply("usage_failed", code="unavailable"), "no-store"),
                    )


if __name__ == "__main__":
    unittest.main()


class RunnerListenerConfigurationTests(unittest.TestCase):
    def test_default_and_staged_api_only_listener(self):
        self.assertEqual(runner.configured_listener({}), (18083, False))
        self.assertEqual(runner.configured_listener({"FORGE_RUNNER_PORT": "18084", "FORGE_RUNNER_API_ONLY": "1"}), (18084, True))

    def test_invalid_listener_is_rejected(self):
        for value in ("", "0", "65536", "-1", " 18084", "１８０８４", "18084x"):
            with self.subTest(port=value), self.assertRaises(ValueError):
                runner.configured_listener({"FORGE_RUNNER_PORT": value})
        with self.assertRaises(ValueError):
            runner.configured_listener({"FORGE_RUNNER_API_ONLY": "true"})
