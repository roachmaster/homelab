"""Lifecycle lineage is distinct from the reviewed code-repair capability."""
from types import SimpleNamespace
import unittest
from uuid import uuid4
from test_work_executor import lease
from work_lease_contract import validate_lease


class LeaseContractTest(unittest.TestCase):
    def value(self,mode='plan',**fields):
        return {**lease(SimpleNamespace(worker_id='fixture'),mode=mode),**fields}

    def test_workflow_plan_and_its_approved_implementation_have_lineage(self):
        plan=self.value(parent_task_id=str(uuid4()),workflow_id=str(uuid4()))
        self.assertEqual(validate_lease(plan),plan)
        implementation={**plan,'mode':'implement','plan_decision_id':str(uuid4())}
        self.assertEqual(validate_lease(implementation),implementation)
        legacy={**implementation,'plan_decision_id':None,'scope_plan_decision_id':str(uuid4())}
        self.assertEqual(validate_lease(legacy),legacy)

    def test_parentage_does_not_authorize_standalone_or_unapproved_implementation(self):
        for value in (self.value(parent_task_id=str(uuid4())),
                self.value('implement',parent_task_id=str(uuid4()),workflow_id=str(uuid4()))):
            with self.subTest(value=value['mode']),self.assertRaises(ValueError):validate_lease(value)

    def test_same_task_revision_keeps_plan_and_feedback_authorities_separate(self):
        value=self.value('implement',parent_task_id=str(uuid4()),workflow_id=str(uuid4()),
            plan_decision_id=str(uuid4()),feedback_id=str(uuid4()),feedback_sha256='b'*64)
        self.assertEqual(validate_lease(value),value)
        for change in ({'mode':'plan'},{'plan_decision_id':None},{'feedback_sha256':None},
                       {'repair_candidate_commit':'a'*40},{'repair_source_review_id':str(uuid4())}):
            with self.subTest(change=change),self.assertRaises(ValueError):validate_lease({**value,**change})

    def test_partial_code_repair_cannot_be_disguised_as_lifecycle_lineage(self):
        base=self.value('implement',parent_task_id=str(uuid4()),workflow_id=str(uuid4()),
            plan_decision_id=str(uuid4()),repair_source_review_id=str(uuid4()),
            repair_candidate_commit='a'*40,feedback_id=str(uuid4()),feedback_sha256='b'*64)
        self.assertEqual(validate_lease(base),base)
        for field in ('parent_task_id','repair_source_review_id','feedback_id'):
            with self.subTest(field=field),self.assertRaises(ValueError):validate_lease({**base,field:None})
        with self.assertRaises(ValueError):validate_lease({**base,'repair_candidate_commit':'c'*40})


if __name__=='__main__':unittest.main()
