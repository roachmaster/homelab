import ast
import builtins
from dataclasses import FrozenInstanceError
from pathlib import Path
import re
import unittest
from unittest.mock import patch

import work_repair_policy as policy_mod
from work_repair_policy import (
    CATEGORIES,
    DISPOSITIONS,
    MAX_INFRASTRUCTURE_RETRIES_LIMIT,
    MAX_ROUNDS_LIMIT,
    MAX_STAGNANT_ROUNDS_LIMIT,
    RepairDecision,
    RepairObservation,
    RepairPolicy,
    decide_repair,
    failure_fingerprint,
)


class RepairPolicyValidationTest(unittest.TestCase):
    def test_defaults_and_frozen_dataclasses(self):
        policy = RepairPolicy()
        self.assertEqual(policy.max_rounds, 20)
        self.assertEqual(policy.max_stagnant_rounds, 20)
        self.assertEqual(policy.max_infrastructure_retries, 20)

        obs = RepairObservation("source")
        self.assertEqual(obs.category, "source")
        self.assertEqual(obs.repair_round, 0)
        self.assertEqual(obs.stagnant_rounds, 0)
        self.assertEqual(obs.infrastructure_retries, 0)
        self.assertFalse(obs.verified_progress)
        self.assertFalse(obs.checks_passed)
        self.assertFalse(obs.cancelled)
        self.assertFalse(obs.quota_blocked)
        self.assertFalse(obs.scope_changed)
        self.assertTrue(obs.cleanup_confirmed)
        self.assertFalse(obs.budget_exhausted)

        decision = decide_repair(obs)
        self.assertEqual(decision, RepairDecision("repair", "source_repair", True))

        with self.assertRaises(FrozenInstanceError):
            policy.max_rounds = 4  # type: ignore[misc]
        with self.assertRaises(FrozenInstanceError):
            obs.repair_round = 1  # type: ignore[misc]
        with self.assertRaises(FrozenInstanceError):
            decision.consume_round = False  # type: ignore[misc]

    def test_policy_rejects_bool_float_nonfinite_negative_and_excessive_values(self):
        bad_types = [True, False, 1.0, 1.5, float("nan"), float("inf"), float("-inf"), "3", None]
        for field in ("max_rounds", "max_stagnant_rounds", "max_infrastructure_retries"):
            for value in bad_types:
                with self.subTest(field=field, value=value):
                    with self.assertRaises(TypeError):
                        RepairPolicy(**{field: value})

        with self.assertRaises(ValueError):
            RepairPolicy(max_rounds=-1)
        with self.assertRaises(ValueError):
            RepairPolicy(max_rounds=MAX_ROUNDS_LIMIT + 1)
        with self.assertRaises(ValueError):
            RepairPolicy(max_stagnant_rounds=0)
        with self.assertRaises(ValueError):
            RepairPolicy(max_stagnant_rounds=-1)
        with self.assertRaises(ValueError):
            RepairPolicy(max_stagnant_rounds=MAX_STAGNANT_ROUNDS_LIMIT + 1)
        with self.assertRaises(ValueError):
            RepairPolicy(max_infrastructure_retries=-1)
        with self.assertRaises(ValueError):
            RepairPolicy(max_infrastructure_retries=MAX_INFRASTRUCTURE_RETRIES_LIMIT + 1)

        # Boundary minimum and maximum values are accepted.
        self.assertEqual(
            RepairPolicy(
                max_rounds=0,
                max_stagnant_rounds=1,
                max_infrastructure_retries=0,
            ),
            RepairPolicy(0, 1, 0),
        )
        self.assertEqual(
            RepairPolicy(
                max_rounds=MAX_ROUNDS_LIMIT,
                max_stagnant_rounds=MAX_STAGNANT_ROUNDS_LIMIT,
                max_infrastructure_retries=MAX_INFRASTRUCTURE_RETRIES_LIMIT,
            ),
            RepairPolicy(
                MAX_ROUNDS_LIMIT,
                MAX_STAGNANT_ROUNDS_LIMIT,
                MAX_INFRASTRUCTURE_RETRIES_LIMIT,
            ),
        )

    def test_observation_and_decision_strict_validation(self):
        for bad_category in ("unknown", "", " ", "SOURCE", "source\x00"):
            with self.subTest(category=bad_category):
                with self.assertRaises(ValueError):
                    RepairObservation(category=bad_category)
        with self.assertRaises(TypeError):
            RepairObservation(category=123)  # type: ignore[arg-type]

        for int_field in ("repair_round", "stagnant_rounds", "infrastructure_retries"):
            for bad in (True, False, 1.0, float("nan"), float("inf"), float("-inf"), "0", None):
                with self.subTest(field=int_field, bad=bad):
                    with self.assertRaises(TypeError):
                        RepairObservation(category="source", **{int_field: bad})
            with self.assertRaises(ValueError):
                RepairObservation(category="source", **{int_field: -1})

        bool_fields = (
            "verified_progress",
            "checks_passed",
            "cancelled",
            "quota_blocked",
            "scope_changed",
            "cleanup_confirmed",
            "budget_exhausted",
        )
        for bool_field in bool_fields:
            for bad in (0, 1, "true", None):
                with self.subTest(field=bool_field, bad=bad):
                    with self.assertRaises(TypeError):
                        RepairObservation(category="source", **{bool_field: bad})

        with self.assertRaises(ValueError):
            RepairDecision("accepted", "done", False)
        with self.assertRaises(ValueError):
            RepairDecision("handoff", "   ", False)
        with self.assertRaises(TypeError):
            RepairDecision("handoff", "reason", 1)  # type: ignore[arg-type]
        with self.assertRaises(TypeError):
            decide_repair("not-an-observation")  # type: ignore[arg-type]
        with self.assertRaises(TypeError):
            decide_repair(RepairObservation("source"), policy="not-a-policy")  # type: ignore[arg-type]

    def test_vocabularies_match_contract(self):
        self.assertEqual(
            CATEGORIES,
            frozenset({"source", "infrastructure", "quota", "authentication", "permission", "scope", "pass"}),
        )
        self.assertEqual(
            DISPOSITIONS,
            frozenset(
                {
                    "repair",
                    "retry_infrastructure",
                    "awaiting_review",
                    "handoff",
                    "cancelled",
                    "needs_reconcile",
                    "revise_plan",
                }
            ),
        )


def decide_legacy_repair(observation, policy=RepairPolicy(3, 2, 1)):
    """Retain coverage of persisted pre-shared-policy repair ledgers."""
    return decide_repair(observation, policy)


class DecideRepairPrecedenceAndBoundaryTest(unittest.TestCase):
    def test_unconfirmed_cleanup_always_needs_reconcile_even_after_cancellation(self):
        cases = [
            RepairObservation(category="source", cleanup_confirmed=False),
            RepairObservation(category="source", cancelled=True, cleanup_confirmed=False),
            RepairObservation(category="pass", checks_passed=True, cancelled=True, cleanup_confirmed=False),
            RepairObservation(category="quota", quota_blocked=True, cleanup_confirmed=False),
            RepairObservation(category="scope", scope_changed=True, cleanup_confirmed=False),
            RepairObservation(category="infrastructure", budget_exhausted=True, cleanup_confirmed=False),
        ]
        for obs in cases:
            with self.subTest(obs=obs):
                decision = decide_legacy_repair(obs)
                self.assertEqual(decision.disposition, "needs_reconcile")
                self.assertEqual(decision.reason, "cleanup_unconfirmed")
                self.assertFalse(decision.consume_round)

    def test_confirmed_cancellation_precedes_other_dispositions(self):
        cases = [
            RepairObservation(category="source", cancelled=True, cleanup_confirmed=True),
            RepairObservation(category="pass", checks_passed=True, cancelled=True, cleanup_confirmed=True),
            RepairObservation(category="quota", quota_blocked=True, cancelled=True, cleanup_confirmed=True),
            RepairObservation(category="scope", scope_changed=True, cancelled=True, cleanup_confirmed=True),
            RepairObservation(category="infrastructure", cancelled=True, cleanup_confirmed=True),
        ]
        for obs in cases:
            with self.subTest(obs=obs):
                decision = decide_legacy_repair(obs)
                self.assertEqual(decision.disposition, "cancelled")
                self.assertEqual(decision.reason, "cancelled")
                self.assertFalse(decision.consume_round)

    def test_quota_authentication_and_permission_handoff_without_consuming_round(self):
        cases = [
            (RepairObservation(category="quota"), "quota_blocked"),
            (RepairObservation(category="source", quota_blocked=True), "quota_blocked"),
            (RepairObservation(category="pass", checks_passed=True, quota_blocked=True), "quota_blocked"),
            (RepairObservation(category="scope", scope_changed=True, quota_blocked=True), "quota_blocked"),
            (RepairObservation(category="authentication"), "authentication_required"),
            (RepairObservation(category="permission"), "permission_required"),
        ]
        for obs, expected_reason in cases:
            with self.subTest(obs=obs):
                decision = decide_legacy_repair(obs)
                self.assertEqual(decision.disposition, "handoff")
                self.assertEqual(decision.reason, expected_reason)
                self.assertFalse(decision.consume_round)

    def test_scope_change_requests_plan_revision(self):
        cases = [
            RepairObservation(category="scope"),
            RepairObservation(category="source", scope_changed=True),
            RepairObservation(category="pass", checks_passed=True, scope_changed=True),
            RepairObservation(category="infrastructure", scope_changed=True),
        ]
        for obs in cases:
            with self.subTest(obs=obs):
                decision = decide_legacy_repair(obs)
                self.assertEqual(decision.disposition, "revise_plan")
                self.assertEqual(decision.reason, "scope_changed")
                self.assertFalse(decision.consume_round)

    def test_passing_checks_enter_awaiting_review_and_require_verification(self):
        # Verified pass enters awaiting_review (not auto-accepted), even after final repair round.
        for completed_rounds in (0, 1, 2, 3):
            with self.subTest(completed_rounds=completed_rounds):
                decision = decide_legacy_repair(
                    RepairObservation(
                        category="pass",
                        repair_round=completed_rounds,
                        checks_passed=True,
                    )
                )
                self.assertEqual(decision.disposition, "awaiting_review")
                self.assertEqual(decision.reason, "checks_passed")
                self.assertFalse(decision.consume_round)

        # Unverified pass (checks_passed=False) cannot advance to awaiting_review.
        unverified = decide_legacy_repair(RepairObservation(category="pass", checks_passed=False))
        self.assertEqual(unverified.disposition, "handoff")
        self.assertEqual(unverified.reason, "checks_not_passed")
        self.assertFalse(unverified.consume_round)

    def test_wall_budget_exhaustion_hands_off_failures(self):
        for category in ("source", "infrastructure"):
            with self.subTest(category=category):
                decision = decide_legacy_repair(
                    RepairObservation(category=category, budget_exhausted=True)
                )
                self.assertEqual(decision.disposition, "handoff")
                self.assertEqual(decision.reason, "budget_exhausted")
                self.assertFalse(decision.consume_round)

    def test_infrastructure_retries_bounded_without_consuming_model_round(self):
        first = decide_legacy_repair(
            RepairObservation(category="infrastructure", repair_round=0, infrastructure_retries=0)
        )
        self.assertEqual(first, RepairDecision("retry_infrastructure", "infrastructure_retry", False))

        # Still allowed on the final repair round's artifact without consuming a model round.
        on_last_round = decide_legacy_repair(
            RepairObservation(category="infrastructure", repair_round=3, infrastructure_retries=0)
        )
        self.assertEqual(
            on_last_round,
            RepairDecision("retry_infrastructure", "infrastructure_retry", False),
        )

        # Exhausted after 1 retry under default policy.
        second = decide_legacy_repair(
            RepairObservation(category="infrastructure", repair_round=0, infrastructure_retries=1)
        )
        self.assertEqual(
            second,
            RepairDecision("handoff", "infrastructure_retries_exhausted", False),
        )

        # Zero retry policy hands off immediately.
        no_retry = decide_legacy_repair(
            RepairObservation(category="infrastructure", infrastructure_retries=0),
            policy=RepairPolicy(max_infrastructure_retries=0),
        )
        self.assertEqual(no_retry.disposition, "handoff")
        self.assertFalse(no_retry.consume_round)

    def test_source_repair_round_boundaries(self):
        # Completed rounds 0, 1, 2 are allowed when max_rounds=3 (3 repair rounds total).
        for completed in (0, 1, 2):
            with self.subTest(completed=completed):
                decision = decide_legacy_repair(
                    RepairObservation(category="source", repair_round=completed, verified_progress=True)
                )
                self.assertEqual(decision, RepairDecision("repair", "source_repair", True))

        # After 3 completed repairs, round budget is exhausted.
        exhausted = decide_legacy_repair(
            RepairObservation(category="source", repair_round=3, verified_progress=True)
        )
        self.assertEqual(exhausted, RepairDecision("handoff", "max_rounds_exhausted", False))

        # Policy with max_rounds=0 disables automatic repair.
        disabled = decide_legacy_repair(
            RepairObservation(category="source", repair_round=0),
            policy=RepairPolicy(max_rounds=0),
        )
        self.assertEqual(disabled, RepairDecision("handoff", "max_rounds_exhausted", False))

    def test_two_successive_stagnant_rounds_trigger_handoff_unless_verified_progress(self):
        # Initial attempt (0 repairs completed) -> repair round 1.
        r0 = decide_legacy_repair(
            RepairObservation(category="source", repair_round=0, stagnant_rounds=0, verified_progress=False)
        )
        self.assertEqual(r0, RepairDecision("repair", "source_repair", True))

        # First repair round with no verified progress (1 stagnant round < 2) -> repair round 2.
        r1 = decide_legacy_repair(
            RepairObservation(category="source", repair_round=1, stagnant_rounds=1, verified_progress=False)
        )
        self.assertEqual(r1, RepairDecision("repair", "source_repair", True))

        # Second successive repair round without verified progress (2 >= 2) -> handoff.
        r2_stagnant = decide_legacy_repair(
            RepairObservation(category="source", repair_round=2, stagnant_rounds=2, verified_progress=False)
        )
        self.assertEqual(r2_stagnant, RepairDecision("handoff", "no_progress", False))

        # Independently verified progress resets stagnation and allows the remaining round.
        r2_progress = decide_legacy_repair(
            RepairObservation(category="source", repair_round=2, stagnant_rounds=2, verified_progress=True)
        )
        self.assertEqual(r2_progress, RepairDecision("repair", "source_repair", True))

    def test_repeated_error_fingerprints_simulate_no_progress_vs_verified_closure(self):
        finding_a = failure_fingerprint(
            "cargo-clippy",
            "unreachable_code",
            path="src/rust/forge-remote-https/examples/loopback.rs",
            summary="unreachable expression in pid=1234 at 2026-11-20T11:30:05Z",
        )
        finding_a_repeat = failure_fingerprint(
            "cargo-clippy",
            "unreachable_code",
            path="./src/rust/forge-remote-https/examples/loopback.rs",
            summary="unreachable expression in pid=9876 at 2026-11-20T11:35:00Z",
        )
        self.assertEqual(finding_a, finding_a_repeat)

        # Two successive repair rounds where the open fingerprint set does not shrink
        # means no independently verified progress -> handoff after round 2.
        open_before = {finding_a}
        open_after_r1 = {finding_a_repeat}
        verified_r1 = bool(open_before - open_after_r1)
        self.assertFalse(verified_r1)
        d1 = decide_legacy_repair(
            RepairObservation(
                category="source",
                repair_round=1,
                stagnant_rounds=1,
                verified_progress=verified_r1,
            )
        )
        self.assertEqual(d1.disposition, "repair")

        open_after_r2 = {finding_a_repeat}
        verified_r2 = bool(open_after_r1 - open_after_r2)
        self.assertFalse(verified_r2)
        d2 = decide_legacy_repair(
            RepairObservation(
                category="source",
                repair_round=2,
                stagnant_rounds=2,
                verified_progress=verified_r2,
            )
        )
        self.assertEqual(d2, RepairDecision("handoff", "no_progress", False))


class FailureFingerprintTest(unittest.TestCase):
    def test_deterministic_sha256_and_conservative_normalization(self):
        fp1 = failure_fingerprint(
            "  Cargo-Clippy ",
            " E0308 ",
            path="./src\\rust//forge-remote-https/examples/loopback.rs",
            summary="Expected i32, found u64 at 2026-11-20T11:30:05Z pid=4120 commit 0cfd79c2c5485094d94c9bd17c4f8bb2ef6f24bb",
        )
        fp2 = failure_fingerprint(
            "cargo-clippy",
            "e0308",
            path="src/rust/forge-remote-https/examples/loopback.rs",
            summary="expected  i32,\tfound u64 at 2026-11-20 12:00:00+00:00 pid: 9999 commit c7b211c61aa0f03ca5fe9db1ef420440f7fbf3d5",
        )
        self.assertRegex(fp1, re.compile(r"^[0-9a-f]{64}$"))
        self.assertEqual(fp1, fp2)

    def test_preserves_meaningful_numeric_codes_and_path_case(self):
        self.assertNotEqual(
            failure_fingerprint("cargo-check", "E0308", "src/lib.rs", "expected 2 arguments"),
            failure_fingerprint("cargo-check", "E0309", "src/lib.rs", "expected 2 arguments"),
        )
        self.assertNotEqual(
            failure_fingerprint("http-check", "status", "/api", "HTTP 403 forbidden"),
            failure_fingerprint("http-check", "status", "/api", "HTTP 404 forbidden"),
        )
        self.assertNotEqual(
            failure_fingerprint("cargo-clippy", "unreachable_code", "examples/loopback.rs", "rustc 1.100.0 line 111"),
            failure_fingerprint("cargo-clippy", "unreachable_code", "examples/loopback.rs", "rustc 1.97.1 line 111"),
        )
        self.assertNotEqual(
            failure_fingerprint("style", "lint", "Makefile", "missing target"),
            failure_fingerprint("style", "lint", "makefile", "missing target"),
        )
        # Field boundary safety against delimiter injection.
        self.assertNotEqual(
            failure_fingerprint("a:b", "c"),
            failure_fingerprint("a", "b:c"),
        )

    def test_rejects_invalid_inputs_and_extra_identity_params(self):
        for bad in ("", "   ", "bad\x00id"):
            with self.subTest(bad=bad):
                with self.assertRaises(ValueError):
                    failure_fingerprint(bad, "E0308")
                with self.assertRaises(ValueError):
                    failure_fingerprint("check", bad)
        with self.assertRaises(ValueError):
            failure_fingerprint("check", "E0308", path="a\x00b")
        with self.assertRaises(ValueError):
            failure_fingerprint("check", "E0308", summary="a\x00b")
        with self.assertRaises(TypeError):
            failure_fingerprint(123, "E0308")  # type: ignore[arg-type]
        with self.assertRaises(TypeError):
            failure_fingerprint("check", None)  # type: ignore[arg-type]
        with self.assertRaises(TypeError):
            failure_fingerprint("check", "E0308", path=123)  # type: ignore[arg-type]
        with self.assertRaises(TypeError):
            failure_fingerprint("check", "E0308", summary=123)  # type: ignore[arg-type]
        with self.assertRaises(TypeError):
            failure_fingerprint("check", "E0308", source_sha="0cfd79c")  # type: ignore[call-arg]


class PureModuleNoIoTest(unittest.TestCase):
    def test_module_imports_only_pure_stdlib(self):
        source_path = Path(policy_mod.__file__).resolve()
        tree = ast.parse(source_path.read_text(encoding="utf-8"))
        imported_roots = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                for alias in node.names:
                    imported_roots.add(alias.name.split(".")[0])
            elif isinstance(node, ast.ImportFrom) and node.module:
                imported_roots.add(node.module.split(".")[0])

        allowed_pure_stdlib = {"__future__", "dataclasses", "hashlib", "json", "re", "work_retry_policy"}
        self.assertTrue(
            imported_roots.issubset(allowed_pure_stdlib),
            f"unexpected imports in pure policy module: {sorted(imported_roots - allowed_pure_stdlib)}",
        )

    def test_policy_and_fingerprint_perform_no_filesystem_io(self):
        def forbid_open(*args, **kwargs):
            raise AssertionError("pure repair policy must not open files")

        with patch.object(builtins, "open", side_effect=forbid_open):
            policy = RepairPolicy(max_rounds=3, max_stagnant_rounds=2, max_infrastructure_retries=1)
            obs = RepairObservation(category="source", repair_round=1, stagnant_rounds=1)
            decision = decide_repair(obs, policy)
            fp = failure_fingerprint("cargo-clippy", "unreachable_code", "examples/loopback.rs", "infallible")
        self.assertEqual(decision.disposition, "repair")
        self.assertEqual(len(fp), 64)


if __name__ == "__main__":
    unittest.main()
