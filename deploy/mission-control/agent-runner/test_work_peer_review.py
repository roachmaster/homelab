"""Authority-bound parser regressions, independent of a model or database."""

from copy import deepcopy
import hashlib
import json
import unittest
from unittest.mock import patch
from uuid import uuid4

from work_peer_review import (PeerReviewError, parse_review_target,
                              parse_review_verdict, validate_review_target,
                              IncompleteApprovalError, validate_live_review_verdict)


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()


def target(kind="plan", *, source="A concrete plan with acceptance checks."):
    value = {
        "version": 1, "workflow_id": str(uuid4()), "workflow_review_id": str(uuid4()),
        "policy_digest": "a" * 64, "target_kind": kind, "reviewer_task_id": str(uuid4()),
        "author_profile": "agy-primary", "reviewer_profile": "codex-primary",
        "source_task_id": str(uuid4()), "source_attempt_id": str(uuid4()),
        "source_result_sha256": sha(source), "source_revision": 3,
        "base_revision": "b" * 40, "plan_revision": 0, "plan_decision_id": None,
        "review_id": None, "candidate_commit": None, "gate_manifest_sha256": None,
        "workspace_digest": None, "integration_base": None, "source_text": source,
        "request_prompt": "Implement the requested capability using selected providers.", "acceptance_criteria": "Tests pass.",
        "findings": [], "verification": None, "approved_plan": None,
    }
    if kind == "code":
        plan = "The exact approved implementation plan."
        value.update(plan_decision_id=str(uuid4()), review_id=str(uuid4()),
            candidate_commit=value["base_revision"], gate_manifest_sha256="c" * 64,
            workspace_digest="d" * 64, integration_base="e" * 40, verification={"checks_passed": True,
                "toolchain": "rustc fixture", "checks": [{"check_id": "make-ci",
                    "status": "passed", "head_sha": value["base_revision"]}]},
            approved_plan={"attempt_id": str(uuid4()), "sha256": sha(plan), "content": plan})
    return value


def bind(value, *, pretty=False):
    raw = json.dumps(value, ensure_ascii=False, indent=2 if pretty else None)
    return parse_review_target(raw, sha(raw))


def finding(identity="missing-case"):
    return {"finding_id": identity, "category": "source", "summary": "Cover the missing edge."}


def verdict(value, decision="approve"):
    return {
        "version": 1, **{field: value[field] for field in (
            "workflow_id", "workflow_review_id", "target_kind", "source_task_id",
            "source_attempt_id", "source_result_sha256", "source_revision")},
        "context_sha256": value["_context_sha256"], "decision": decision,
        "summary": "Reviewed the exact artifact.",
        "findings": [finding()] if decision == "revise" else [], "attested_finding_ids": [],
    }


class PeerReviewContractTest(unittest.TestCase):
    def test_live_approval_requires_all_open_ids_without_changing_historical_parser(self):
        source = target('code')
        source['findings'] = [finding('open-' + str(n)) for n in range(8)] + [{**finding('already'), 'status': 'verified'}]
        parsed = bind(source)
        value = verdict(parsed)
        value['attested_finding_ids'] = ['open-' + str(n) for n in range(3)]
        raw = json.dumps(value)
        self.assertEqual(parse_review_verdict(raw, parsed), value)
        with self.assertRaises(IncompleteApprovalError) as caught:
            validate_live_review_verdict(raw, parsed)
        self.assertEqual(caught.exception.missing_finding_ids, tuple('open-' + str(n) for n in range(3, 8)))
        self.assertEqual(caught.exception.verdict, value)
        value['attested_finding_ids'] = ['open-' + str(n) for n in range(8)]
        self.assertEqual(validate_live_review_verdict(json.dumps(value), parsed), value)
        value['source_attempt_id'] = str(uuid4())
        with self.assertRaises(PeerReviewError) as invalid:
            validate_live_review_verdict(json.dumps(value), parsed)
        self.assertNotIsInstance(invalid.exception, IncompleteApprovalError)

    def test_live_plan_revise_and_escalate_do_not_require_approval_attestations(self):
        for kind in ('plan', 'code'):
            source = target(kind)
            if kind == 'code':
                source['findings'] = [finding()]
            parsed = bind(source)
            for decision in ('revise', 'escalate') + (('approve',) if kind == 'plan' else ()):
                with self.subTest(kind=kind, decision=decision):
                    value = verdict(parsed, decision)
                    self.assertEqual(validate_live_review_verdict(json.dumps(value), parsed), parse_review_verdict(json.dumps(value), parsed))

    def test_long_correction_note_is_bounded_and_retains_full_target_instruction(self):
        from work_reviewer_correction import correction_note
        identities = [str(n).zfill(64) for n in range(32)]
        note = correction_note(identities, identities[3:], 'a' * 64)
        self.assertLessEqual(len(note.encode()), 2048)
        self.assertIn('ALL target findings whose status is open', note)
        self.assertIn('a' * 64, note)
        self.assertIn('Missing count: 29', note)
        self.assertTrue(note.endswith('Return ONLY the same exact source-bound JSON verdict, no Markdown.'))
        small = correction_note(identities[:8], identities[3:8], 'b' * 64)
        for identity in identities[:8]:
            self.assertIn(identity, small)
        self.assertTrue(small.endswith('Return ONLY the same exact source-bound JSON verdict, no Markdown.'))

    def test_selected_correction_factory_is_injected_without_constructing_default(self):
        from types import SimpleNamespace
        from work_workflow import WorkflowStoreFactory
        selected = SimpleNamespace(correct=lambda *a: None, resume=lambda c: None)
        acquired = []
        class Selected:
            def __init__(self, flow):
                acquired.append(flow)
            def create(self):
                return selected
        with patch('work_reviewer_correction.ReviewerCorrectionFactory', side_effect=AssertionError('unused default')):
            flow = WorkflowStoreFactory(None, None, object(), reviewer_correction_factory=Selected).create()
        self.assertIs(flow.reviewer_correction, selected)
        self.assertEqual(acquired, [flow])

    def test_only_peer_correctness_wire_alias_is_normalized_without_mutating_raw(self):
        from work_feedback import validate_findings
        parsed = bind(target("code"))
        value = verdict(parsed, "revise")
        value["findings"][0]["category"] = "correctness"
        raw = json.dumps(value)
        original = sha(raw)
        self.assertEqual(parse_review_verdict(raw, parsed)["findings"][0]["category"], "source")
        self.assertEqual(sha(raw), original)
        self.assertEqual(value["findings"][0]["category"], "correctness")
        with self.assertRaises(ValueError):
            validate_findings(value["findings"], public=True)
        for category in ("Correctness", "bug", "security", "correctness ", [], None):
            value["findings"][0]["category"] = category
            with self.subTest(category=category), self.assertRaises(PeerReviewError):
                parse_review_verdict(json.dumps(value), parsed)

    def test_exact_unicode_plan_and_three_revision_context_remain_intact(self):
        value = target(source="Plan: preserve José’s draft → review → execute.")
        value["plan_revision"] = 3
        parsed = bind(value, pretty=True)
        result = parse_review_verdict(json.dumps(verdict(parsed)), parsed)
        self.assertEqual(result["decision"], "approve")
        self.assertEqual(parsed["source_text"], value["source_text"])
        self.assertEqual(parsed["request_prompt"], value["request_prompt"])

    def test_large_plan_is_reviewed_whole_without_feedback_sized_truncation(self):
        text = "Full plan line.\n" * 6000
        self.assertGreater(len(text.encode()), 65536)
        parsed = bind(target(source=text))
        self.assertEqual(parsed["source_text"], text)
        self.assertEqual(parsed["source_result_sha256"], sha(text))

    def test_same_semantic_context_with_different_raw_bytes_is_not_same_authority(self):
        source = target()
        compact, pretty = bind(source), bind(source, pretty=True)
        self.assertNotEqual(compact["_context_sha256"], pretty["_context_sha256"])
        with self.assertRaises(PeerReviewError):
            parse_review_verdict(json.dumps(verdict(compact)), pretty)

    def test_distinct_reviewer_and_author_task_are_required(self):
        for field, replacement in (("reviewer_profile", "agy-primary"), ("reviewer_task_id", None)):
            value = target()
            value[field] = replacement if replacement else value["source_task_id"]
            with self.subTest(field=field), self.assertRaises(PeerReviewError):
                bind(value)

    def test_identity_revision_and_hash_mismatches_never_approve(self):
        parsed = bind(target())
        for field, replacement in (("workflow_id", str(uuid4())), ("workflow_review_id", str(uuid4())),
                ("source_task_id", str(uuid4())), ("source_attempt_id", str(uuid4())),
                ("source_result_sha256", "f" * 64), ("context_sha256", "f" * 64),
                ("source_revision", 4), ("target_kind", "code")):
            value = verdict(parsed)
            value[field] = replacement
            with self.subTest(field=field), self.assertRaises(PeerReviewError):
                parse_review_verdict(json.dumps(value), parsed)

    def test_code_requires_exact_passed_candidate_and_approved_plan(self):
        original = target("code")
        self.assertEqual(bind(original)["candidate_commit"], original["candidate_commit"])
        for change in (lambda v: v["verification"].update(checks_passed=False),
                       lambda v: v["verification"]["checks"][0].update(head_sha="f" * 40),
                       lambda v: v["verification"].update(checks=[]),
                       lambda v: v["approved_plan"].update(content="A different plan"),
                       lambda v: v.update(base_revision="f" * 40)):
            value = deepcopy(original)
            change(value)
            with self.assertRaises(PeerReviewError):
                bind(value)

    def test_source_text_cannot_change_while_retaining_old_manifest(self):
        value = target()
        value["source_text"] += " Secret extra instruction."
        with self.assertRaises(PeerReviewError):
            bind(value)

    def test_only_specific_existing_open_code_findings_can_be_attested(self):
        value = target("code")
        value["findings"] = [finding("fixed-case"), {**finding("already-closed"), "status": "verified"}]
        parsed = bind(value)
        decision = verdict(parsed)
        decision["attested_finding_ids"] = ["fixed-case"]
        self.assertEqual(parse_review_verdict(json.dumps(decision), parsed)["attested_finding_ids"], ["fixed-case"])
        for ids in (["unknown"], ["already-closed"], ["fixed-case", "fixed-case"]):
            decision["attested_finding_ids"] = ids
            with self.assertRaises(PeerReviewError):
                parse_review_verdict(json.dumps(decision), parsed)

    def test_revision_needs_actionable_open_findings_not_forged_verified_flags(self):
        parsed = bind(target())
        value = verdict(parsed, "revise")
        self.assertEqual(parse_review_verdict(json.dumps(value), parsed)["decision"], "revise")
        for bad in ([], [{**finding(), "status": "verified"}], [{**finding(), "verified": True}]):
            value["findings"] = bad
            with self.assertRaises(PeerReviewError):
                parse_review_verdict(json.dumps(value), parsed)

    def test_unknown_fields_duplicate_keys_and_non_json_are_rejected(self):
        parsed = bind(target())
        good = json.dumps(verdict(parsed))
        bad = [good[:-1] + ',"decision":"approve"}',
               json.dumps({**verdict(parsed), "next_command": "merge main"}),
               "```json\n" + good + "\n```", "null", "{}", "[]",
               good.replace('"version": 1', '"version": NaN'), "All fixed, approve."]
        for raw in bad:
            with self.subTest(raw=raw[:40]), self.assertRaises(PeerReviewError):
                parse_review_verdict(raw, parsed)

    def test_context_digest_helper_cannot_be_smuggled_in_stored_json(self):
        value = target()
        value["_context_sha256"] = "f" * 64
        with self.assertRaises(PeerReviewError):
            bind(value)
        with self.assertRaises(PeerReviewError):
            parse_review_verdict("{}", target())

    def test_types_resources_and_secrets_fail_closed_without_io(self):
        for key, replacement in (("source_revision", True), ("source_revision", 2**53),
                                 ("plan_revision", 4), ("source_text", "\ud800"),
                                 ("acceptance_criteria", "ghp_" + "A" * 36)):
            value = target()
            value[key] = replacement
            with self.subTest(key=key), self.assertRaises(PeerReviewError):
                validate_review_target(value)
        parsed = bind(target())
        with self.assertRaises(PeerReviewError):
            parse_review_verdict(" " * 65537, parsed)
        with patch("builtins.open", side_effect=AssertionError("No parser I/O")):
            self.assertEqual(parse_review_verdict(json.dumps(verdict(parsed)), parsed)["decision"], "approve")


if __name__ == "__main__":
    unittest.main()
