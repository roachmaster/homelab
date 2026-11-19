"""Only explicitly public progress enters the API, never opaque CLI payloads."""
import unittest
from work_store import _public_progress

class PublicProgressTest(unittest.TestCase):
    def report(self):
        return {"attempt_id":"11111111-1111-4111-8111-111111111111", "progress_sequence":1,
                "stage":"plan", "summary":"## Public plan\n1. Inspect\n2. Test"}

    def test_projects_the_complete_public_plan_without_opaque_fields(self):
        report = self.report()
        self.assertEqual(_public_progress(report), {"attempt_id":report["attempt_id"],
            "sequence":1, "stage":"plan", "summary":report["summary"]})
        for key in ["tool_arguments", "thought", "credentials"]:
            self.assertIsNone(_public_progress({**report,key:"DO_NOT_EXPOSE"}))
        self.assertIsNone(_public_progress({"summary":"Legacy checkpoint"}))

    def test_rejects_malformed_or_unbounded_public_fields(self):
        for key, value in [("attempt_id","elsewhere"), ("progress_sequence",True),
            ("progress_sequence",129), ("progress_sequence",2), ("stage","completed"),
            ("stage",[]), ("summary"," "), ("summary","x"*4097),
            ("summary","é"*2049), ("summary","hidden\x85control"),
            ("summary","hidden\rcontrol"), ("summary","\ud800")]:
            with self.subTest(key=key):
                self.assertIsNone(_public_progress({**self.report(),key:value}))
