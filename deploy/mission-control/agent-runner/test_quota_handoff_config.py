"""Pure startup selection and role boundaries for an explicit AGY quota handoff."""

from dataclasses import FrozenInstanceError
import json
from pathlib import Path
import unittest
from unittest.mock import patch

from work_config import (
    DEFAULT_REPAIR_POLICY, WorkCatalog, WorkProfile, parse_work_config,
)
from work_policy import AGY_POLICY_ID, POLICY_ID, POLICY_DIGEST


def raw_config():
    return {
        "owner": "mission-control",
        "allowed_projects": ["homelab"],
        "allowed_modes": ["plan", "implement", "review"],
        "profiles": {
            "agy-primary": {
                "backend": "antigravity", "model": "fixture-gemini",
                "resource_profile": AGY_POLICY_ID,
                "backup_models": ["fixture-opus"],
                "quota_handoff_profile": "codex-author",
            },
            "codex-author": {
                "backend": "codex", "model": "fixture-codex",
                "resource_profile": POLICY_ID,
            },
            "codex-reviewer": {
                "backend": "codex", "model": "fixture-codex",
                "resource_profile": POLICY_ID,
            },
        },
        "project_workflows": {"homelab": {
            "author_profile": "agy-primary", "reviewer_profile": "codex-reviewer",
        }},
    }


class QuotaHandoffConfigurationTest(unittest.TestCase):
    def test_selection_is_pure_and_resolves_an_independent_author(self):
        with patch.object(Path, "open", side_effect=AssertionError("Unexpected startup I/O")):
            config = parse_work_config(raw_config())
            for mode in ("plan", "implement"):
                alias, recipe = config.catalog().resolve_handoff("agy-primary", mode)
                self.assertEqual(alias, "codex-author")
                self.assertEqual(recipe.backend, "codex")
                self.assertEqual(recipe.mode, mode)
                self.assertEqual(recipe.version, 2)
                self.assertEqual(recipe.policy_digest, POLICY_DIGEST)
                self.assertEqual(recipe, config.catalog().resolve(alias, mode))
                self.assertNotEqual(alias, config.project_workflows["homelab"].reviewer_profile)

    def test_omitting_the_selection_keeps_every_original_recipe_and_snapshot(self):
        original = raw_config()
        del original["profiles"]["agy-primary"]["quota_handoff_profile"]
        before = parse_work_config(original)
        after = parse_work_config(raw_config())
        for alias in before.profiles:
            for mode in ("plan", "implement", "review"):
                self.assertEqual(before.catalog().resolve(alias, mode), after.catalog().resolve(alias, mode))
        self.assertEqual(
            before.project_workflows["homelab"].snapshot(before.catalog(), DEFAULT_REPAIR_POLICY),
            after.project_workflows["homelab"].snapshot(after.catalog(), DEFAULT_REPAIR_POLICY),
        )
        with self.assertRaises(KeyError):
            before.catalog().resolve_handoff("agy-primary", "plan")

    def test_missing_or_non_author_modes_cannot_select_a_handoff(self):
        catalog = parse_work_config(raw_config()).catalog()
        for alias in ("missing", "codex-author", "codex-reviewer"):
            with self.subTest(alias=alias), self.assertRaises(KeyError):
                catalog.resolve_handoff(alias, "plan")
        for mode in ("review", "anything", None):
            with self.subTest(mode=mode), self.assertRaises(ValueError):
                catalog.resolve_handoff("agy-primary", mode)

    def test_same_provider_model_backup_still_resolves_when_handoff_is_configured(self):
        catalog = parse_work_config(raw_config()).catalog()
        for mode in ("plan", "implement"):
            recipe = catalog.resolve_backup("agy-primary", mode, "fixture-opus")
            self.assertEqual(recipe.backend, "antigravity")
            self.assertEqual(recipe.model, "fixture-opus")
            self.assertEqual(recipe.mode, mode)
        self.assertEqual(catalog.profile("agy-primary").quota_handoff_profile, "codex-author")
        self.assertEqual(catalog.profile("agy-primary").backup_models, ("fixture-opus",))

    def test_strict_profile_schema_rejects_invalid_handoff_identifiers(self):
        for target in (True, 1, [], {}, "", "with space", "../codex", "x" * 65):
            with self.subTest(target=target), self.assertRaises(ValueError):
                raw = raw_config()
                raw["profiles"]["agy-primary"]["quota_handoff_profile"] = target
                parse_work_config(raw)

    def test_unknown_disabled_or_non_codex_targets_fail_before_runtime(self):
        cases = []
        raw = raw_config()
        raw["profiles"]["agy-primary"]["quota_handoff_profile"] = "missing"
        cases.append(raw)
        raw = raw_config()
        raw["profiles"]["codex-author"]["execution_enabled"] = False
        cases.append(raw)
        for backend in ("antigravity", "grok", "muse"):
            raw = raw_config()
            raw["profiles"]["codex-author"]["backend"] = backend
            cases.append(raw)
        for raw in cases:
            with self.subTest(target=raw["profiles"]["codex-author"]), self.assertRaises(ValueError):
                parse_work_config(raw)

    def test_legacy_source_or_target_policy_cannot_authorize_automatic_handoff(self):
        for alias in ("agy-primary", "codex-author"):
            with self.subTest(alias=alias), self.assertRaises(ValueError):
                raw = raw_config()
                raw["profiles"][alias]["resource_profile"] = "legacy-profile"
                parse_work_config(raw)

    def test_only_antigravity_may_select_the_one_way_quota_handoff(self):
        for backend in ("codex", "grok", "muse"):
            with self.subTest(backend=backend), self.assertRaises(ValueError):
                WorkProfile("source", backend, "fixture", POLICY_ID,
                            quota_handoff_profile="codex-author")
        with self.assertRaises(ValueError):
            WorkProfile("source", "antigravity", "fixture", AGY_POLICY_ID,
                        quota_handoff_profile="source")

    def test_target_cannot_be_the_current_workflow_reviewer(self):
        raw = raw_config()
        raw["profiles"]["agy-primary"]["quota_handoff_profile"] = "codex-reviewer"
        with self.assertRaisesRegex(ValueError, "distinct profiles"):
            parse_work_config(raw)

    def test_catalog_validates_references_without_an_automatic_project(self):
        raw = raw_config()
        raw.pop("project_workflows")
        raw["profiles"]["agy-primary"]["quota_handoff_profile"] = "missing"
        with self.assertRaises(ValueError):
            parse_work_config(raw)
        source = WorkProfile("source", "antigravity", "fixture", AGY_POLICY_ID,
                             quota_handoff_profile="missing")
        with self.assertRaises(ValueError):
            WorkCatalog({"source": source})

    def test_frozen_selection_does_not_follow_caller_dictionary_changes(self):
        raw = raw_config()
        config = parse_work_config(raw)
        selected = config.catalog().resolve_handoff("agy-primary", "plan")
        raw["profiles"]["codex-author"]["model"] = "changed"
        raw["profiles"]["agy-primary"]["quota_handoff_profile"] = "codex-reviewer"
        self.assertEqual(config.catalog().resolve_handoff("agy-primary", "plan"), selected)
        with self.assertRaises(FrozenInstanceError):
            config.profiles["agy-primary"].quota_handoff_profile = "changed"

    def test_startup_example_selects_distinct_author_and_reviewer(self):
        raw = json.loads(Path(__file__).with_name("quota-handoff-config.example.json").read_text())
        config = parse_work_config(raw)
        target, _ = config.catalog().resolve_handoff("agy-primary", "implement")
        self.assertEqual(target, "codex-author")
        self.assertEqual(config.project_workflows["homelab"].reviewer_profile, "codex-primary")

    def test_existing_worker_grants_bind_author_and_reviewer_aliases_separately(self):
        from test_work_executor import raw_config as worker_raw_config
        from work_worker_config import parse_config, verify_recipe
        raw = worker_raw_config("/tmp/forge-handoff-worker-fixture")
        raw["profiles"] = {
            "codex-author": {"backend": "codex", "model": "fixture-codex", "modes": ["plan", "implement"]},
            "codex-reviewer": {"backend": "codex", "model": "fixture-codex", "modes": ["review"]},
        }
        raw["binaries"] = {"codex": "/usr/bin/python3"}
        raw["allow_implement"] = True
        worker = parse_config(raw)
        self.assertEqual(worker.advertised_profiles(), [
            {"alias": "codex-author", "backend": "codex", "model": "fixture-codex", "modes": ["plan", "implement"]},
            {"alias": "codex-reviewer", "backend": "codex", "model": "fixture-codex", "modes": ["review"]},
        ])
        _, recipe = parse_work_config(raw_config()).catalog().resolve_handoff("agy-primary", "implement")
        lease = {"project_alias": "homelab", "agent_profile": "codex-author", "mode": "implement", "recipe": recipe.to_dict()}
        self.assertTrue(verify_recipe(worker, lease))
        self.assertFalse(verify_recipe(worker, {**lease, "agent_profile": "codex-reviewer"}))


if __name__ == "__main__":
    unittest.main()
