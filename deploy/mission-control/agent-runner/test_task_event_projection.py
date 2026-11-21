"""Strict public trace projections never trust arbitrary event payloads."""
import unittest
from uuid import uuid4

from work_store import _public_progress, _public_external_progress, _public_workflow_progress


class EventProjectionTests(unittest.TestCase):
    def test_external_progress_requires_real_assignment_and_bounded_generation(self):
        identity = str(uuid4())
        payload = {"provider":"muse-db", "assignment_id":identity, "generation":1, "summary":"Public checkpoint"}
        binding = {"assignment_id":identity, "generation":2}
        self.assertEqual(_public_external_progress(payload,binding),payload)
        for patch in ({"assignment_id":str(uuid4())},{"generation":0},{"generation":3},{"generation":True},{"provider":"native"},{"summary":"x"*4097},{"summary":"private\x00control"},{"lease_token":"secret"}):
            self.assertIsNone(_public_external_progress({**payload,**patch},binding))
        self.assertIsNone(_public_external_progress(payload,None))

    def test_workflow_progress_requires_real_relation_and_verified_link(self):
        identity, related = str(uuid4()), str(uuid4())
        payload = {"workflow_id":identity,"phase":"plan_review","summary":"Independent review dispatched"}
        workflow = {"workflow_id":identity}
        self.assertEqual(_public_workflow_progress(payload,workflow),payload)
        linked = {**payload,"related_task_id":related}
        self.assertIsNone(_public_workflow_progress(linked,workflow))
        self.assertEqual(_public_workflow_progress(linked,workflow,True),linked)
        for patch in ({"workflow_id":str(uuid4())},{"phase":"made_up"},{"related_task_id":"arbitrary"},{"summary":"x"*4097},{"attempt_id":related}):
            self.assertIsNone(_public_workflow_progress({**payload,**patch},workflow,True))
        self.assertIsNone(_public_workflow_progress(payload,None))

    def test_native_progress_is_separate_from_external_and_coordinator_payloads(self):
        identity = str(uuid4())
        native = {"attempt_id":identity,"progress_sequence":1,"stage":"plan","summary":"Approved public report"}
        self.assertEqual(_public_progress(native)["attempt_id"],identity)
        self.assertIsNone(_public_progress({**native,"assignment_id":identity}))
        self.assertIsNone(_public_progress({**native,"workflow_id":identity}))
        self.assertIsNone(_public_progress({**native,"progress_sequence":True}))


if __name__ == "__main__":
    unittest.main()
