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
        actor = {"kind":"agent", "id":"muse-db-primary"}
        self.assertEqual(_public_external_progress({**payload,"actor":actor},binding),payload)
        for patch in ({"assignment_id":str(uuid4())},{"generation":0},{"generation":3},{"generation":True},{"provider":"native"},{"summary":"x"*4097},{"summary":"private\x00control"},{"lease_token":"secret"}):
            self.assertIsNone(_public_external_progress({**payload,**patch},binding))
        for bad_actor in ({"kind":"worker","id":"muse-db-primary"},{"kind":"agent","id":""},
                          {"kind":"agent"},{"kind":"agent","id":"x","extra":1},None,"agent"):
            self.assertIsNone(_public_external_progress({**payload,"actor":bad_actor},binding))
        self.assertIsNone(_public_external_progress(payload,None))

    def test_workflow_progress_requires_real_relation_and_verified_link(self):
        identity, related = str(uuid4()), str(uuid4())
        payload = {"workflow_id":identity,"phase":"plan_review","summary":"Independent review dispatched"}
        workflow = {"workflow_id":identity}
        self.assertEqual(_public_workflow_progress(payload,workflow),payload)
        actor = {"kind":"coordinator","id":"feedback-loop"}
        self.assertEqual(_public_workflow_progress({**payload,"actor":actor},workflow),payload)
        for bad_actor in ({"kind":"worker","id":"feedback-loop"},{"kind":"coordinator","id":""},
                          {"kind":"coordinator"},None):
            self.assertIsNone(_public_workflow_progress({**payload,"actor":bad_actor},workflow))
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
        actor = {"kind":"worker","id":"progress-worker"}
        projected = _public_progress({**native,"actor":actor})
        self.assertEqual(projected,{"attempt_id":identity,"sequence":1,"stage":"plan","summary":"Approved public report"})
        for bad_actor in ({"kind":"agent","id":"progress-worker"},{"kind":"worker","id":""},
                          {"kind":"worker"},None):
            self.assertIsNone(_public_progress({**native,"actor":bad_actor}))
        self.assertIsNone(_public_progress({**native,"assignment_id":identity}))
        self.assertIsNone(_public_progress({**native,"workflow_id":identity}))
        self.assertIsNone(_public_progress({**native,"progress_sequence":True}))


if __name__ == "__main__":
    unittest.main()
