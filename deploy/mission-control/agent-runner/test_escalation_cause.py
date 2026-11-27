"""GB-3: workflow escalation events carry a machine-readable cause code."""

from pathlib import Path
import re
import unittest
from uuid import uuid4

from work_events import ESCALATION_CAUSES, escalation_cause
from work_store import _public_workflow_progress
from test_external_flow import AGENT_RETRY_LIMIT, ExternalFlowPGTest
from test_work_workflow import AutonomousWorkflowTest
from test_work_execution_and_usage import make_v2_config
from work_api import WorkServiceFactory

HERE = Path(__file__).resolve().parent


class EscalationCauseVocabularyTest(unittest.TestCase):
    def test_known_reason_literals_map_to_codes(self):
        cases = {
            "Native execution deadline event exhausted its twenty retry attempts. The stopped workspace and approved Plan are retained.": "retry_exhausted",
            "Independent reviewer context transcription exhausted twenty attempts for this event.": "retry_exhausted",
            "The plan revision budget is exhausted. Revise again.": "budget_exhausted",
            "The bounded main integration budget is exhausted.": "budget_exhausted",
            "The code revision budget is exhausted. Fix it.": "budget_exhausted",
            "The external execution grant was revoked.": "grant_revoked",
            "Workflow delegation or role grant is no longer current.": "grant_revoked",
            "Agent stopped: needs_reconcile": "agent_stopped",
            "External author cannot continue: failed": "agent_stopped",
            "Workflow task was cancelled.": "agent_stopped",
            "Trusted publication stopped; retained source and remote metadata require reconciliation.": "publication_failed",
            "Trusted merge could not be verified: x": "verification_failed",
            "All protected phases passed but Jenkins did not report SUCCESS. The terminal delivery mismatch requires reconciliation; no source repair was admitted.": "verification_failed",
            "Peer review could not safely continue: denied": "review_unavailable",
            "The author returned the identical candidate manifest at base abc": "candidate_repeated",
            "handoff": "handoff",
            "Something nobody anticipated": "unclassified",
            "": "unclassified",
        }
        for reason, code in cases.items():
            with self.subTest(reason=reason):
                self.assertEqual(escalation_cause(reason), code)
                self.assertIn(code, ESCALATION_CAUSES)

    def test_explicit_cause_wins_and_is_validated(self):
        self.assertEqual(escalation_cause("The external execution grant was revoked.", "review_verdict"), "review_verdict")
        with self.assertRaises(ValueError):
            escalation_cause("anything", "sha_mismatch")

    def test_every_literal_escalation_reason_in_source_is_classified(self):
        pattern = re.compile(r"_escalate\(\s*cursor\s*,\s*w\s*,\s*\(?\s*'([^']+)'")
        seen = 0
        for path in sorted(HERE.glob("work_*.py")):
            for literal in pattern.findall(path.read_text()):
                if len(literal) < 12:
                    continue  # a bare prefix such as 'The '+kind+' revision budget...'
                seen += 1
                with self.subTest(path=path.name, literal=literal):
                    self.assertNotEqual(escalation_cause(literal), "unclassified")
        self.assertGreaterEqual(seen, 15)


class EscalationCauseProjectionTest(unittest.TestCase):
    def setUp(self):
        self.workflow_id = str(uuid4())
        self.payload = {
            "actor": {"kind": "coordinator", "id": "forge-coordinator"},
            "workflow_id": self.workflow_id,
            "phase": "escalated",
            "summary": "The bounded main integration budget is exhausted.",
            "escalation_cause": "budget_exhausted",
        }

    def project(self, payload):
        return _public_workflow_progress(payload, {"workflow_id": self.workflow_id})

    def test_escalated_checkpoint_projects_cause(self):
        projected = self.project(self.payload)
        self.assertEqual(projected["escalation_cause"], "budget_exhausted")
        self.assertEqual(projected["phase"], "escalated")

    def test_legacy_checkpoint_without_cause_still_projects(self):
        legacy = {k: v for k, v in self.payload.items() if k != "escalation_cause"}
        projected = self.project(legacy)
        self.assertIsNotNone(projected)
        self.assertNotIn("escalation_cause", projected)

    def test_unknown_cause_or_non_escalated_phase_is_not_projected(self):
        self.assertIsNone(self.project({**self.payload, "escalation_cause": "sha_mismatch"}))
        self.assertIsNone(self.project({**self.payload, "phase": "implement"}))


class ExternalEscalationCauseTest(ExternalFlowPGTest):
    def test_budget_escalation_checkpoint_names_cause(self):
        _service, root, flow, _ = self.submit_plan()
        for n in range(AGENT_RETRY_LIMIT):
            self.review(flow, 'revise')
            self.author(flow, 'Revised plan ' + str(n))
        self.review(flow, 'revise')
        w = self.rows('SELECT * FROM forge_external_workflow.workflows')[0]
        self.assertEqual(w['phase'], 'escalated')
        events = self.rows(
            "SELECT payload FROM forge_agent.coding_events WHERE task_id=%s AND event_kind='checkpoint' "
            "AND payload->>'phase'='escalated' ORDER BY sequence", (root['task_id'],))
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0]['payload']['escalation_cause'], 'budget_exhausted')
        self.assertTrue(events[0]['payload']['summary'].startswith('The plan revision budget is exhausted. '))


class NativeEscalationCauseTest(AutonomousWorkflowTest):
    def test_grant_removal_escalation_events_name_cause(self):
        self.submit()
        self.claim('agy-v2')
        removed = WorkServiceFactory(make_v2_config(), lambda: self.cluster.connect('forge_agent')).create()
        removed.workflow_worker.run_once()
        with self.cluster.connect('postgres') as conn, conn.cursor() as cur:
            cur.execute("SELECT payload FROM forge_agent.coding_events WHERE event_kind IN('cancel_requested','cancelled') "
                        "AND payload ? 'workflow_id'")
            payloads = [row[0] for row in cur.fetchall()]
        self.assertTrue(payloads)
        for payload in payloads:
            self.assertEqual(payload['cause'], 'workflow_cancelled')
            self.assertEqual(payload['escalation_cause'], 'grant_revoked')


def load_tests(loader, tests, pattern):
    suite = unittest.TestSuite()
    for case in (EscalationCauseVocabularyTest, EscalationCauseProjectionTest):
        suite.addTests(loader.loadTestsFromTestCase(case))
    suite.addTests(loader.loadTestsFromNames([
        f"{__name__}.ExternalEscalationCauseTest.test_budget_escalation_checkpoint_names_cause",
        f"{__name__}.NativeEscalationCauseTest.test_grant_removal_escalation_events_name_cause",
    ]))
    return suite


if __name__ == "__main__":
    unittest.main()
