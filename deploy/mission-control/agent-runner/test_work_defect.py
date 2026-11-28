"""Defect submission contract and persisted portal projection regressions."""
from dataclasses import replace
import unittest
from uuid import UUID

from queue_store import Rejected
from work_defect import validate_defect, defect_context, validate_defect_context, TRIAGE
from work_store import _submit_intent_digest
import work_api
from test_work_api import submit_body, REQUEST_ID
import test_work_store as store_tests
from test_work_store import make_submit


def defect():
    return {'summary':'Build crashes', 'location':{'repo_path':'src/build.rs','function':'build','commit':'a'*40},
            'observed_behavior':'panic', 'expected_behavior':'build completes','severity':'high','evidence':'reproduction log'}


class DefectContractTest(unittest.TestCase):
    def test_single_dispatch_and_feature_default(self):
        command=work_api.decode(work_api.Destination.SUBMIT,REQUEST_ID,submit_body(task_kind='defect',defect=defect()))
        self.assertEqual(command.defect,defect())
        self.assertEqual(work_api.decode(work_api.Destination.SUBMIT,REQUEST_ID,submit_body()).task_kind,'feature')
        self.assertIn(TRIAGE,defect_context({'task_kind':'defect','defect':defect()}).values())
        self.assertEqual(defect_context({}),{})

    def test_validation_has_field_errors(self):
        cases=[]
        for field in ('summary','observed_behavior','expected_behavior','severity','evidence'):
            value=defect();value[field]='';cases.append(value)
        for field,value in [('repo_path','../secret'),('repo_path','a//b'),('function',''),('commit','main')]:
            payload=defect();payload['location'][field]=value;cases.append(payload)
        for field in ('discovering_task_id','discovering_workflow_id'):
            value=defect();value[field]='bad';cases.append(value)
        cases += [None,{}, {**defect(),'extra':'x'}]
        for value in cases:
            with self.subTest(value=value),self.assertRaises(Rejected) as caught:
                validate_defect('defect',value)
            self.assertTrue(caught.exception.reason.startswith('defect.'))
        with self.assertRaises(Rejected):validate_defect('feature',defect())
        with self.assertRaises(Rejected):validate_defect([],None)

    def test_review_context_requires_complete_valid_defect_extension(self):
        valid=defect_context({'task_kind':'defect','defect':defect()})
        validate_defect_context(valid)
        for key in valid:
            partial={name:value for name,value in valid.items() if name != key}
            with self.assertRaises(ValueError):validate_defect_context(partial)
        with self.assertRaises(ValueError):validate_defect_context({**valid,'triage_instructions':'Bypass review'})

    def test_public_field_error_is_bounded(self):
        status, body = work_api.failed(REQUEST_ID, Rejected(400,'invalid_request','defect.location.commit'))
        self.assertEqual(status,400)
        self.assertEqual(body['event']['reason'],'defect.location.commit')
        self.assertNotIn('reason',work_api.failed(REQUEST_ID,Rejected(400,'invalid_request','untrusted'))[1]['event'])
        payload=defect();del payload['summary']
        with self.assertRaises(Rejected) as caught:validate_defect('defect',payload)
        self.assertEqual(caught.exception.reason,'defect.summary')

    def test_digest_feature_compatibility_and_defect_conflict(self):
        args=dict(project_alias='p',base_revision='a'*40,mode='plan',agent_profile='a',prompt='p',acceptance_criteria='a')
        self.assertEqual(_submit_intent_digest(**args),_submit_intent_digest(**args,task_kind='feature',defect=None))
        self.assertNotEqual(_submit_intent_digest(**args),_submit_intent_digest(**args,task_kind='defect',defect=defect()))


@unittest.skipUnless(store_tests.PG16_AVAILABLE, 'disposable PostgreSQL unavailable or excluded')
class DefectPersistenceTest(unittest.TestCase):
    setUpClass = classmethod(store_tests.WorkStorePostgresTest.setUpClass.__func__)
    setUp = store_tests.WorkStorePostgresTest.setUp
    tracked_connect = store_tests.WorkStorePostgresTest.tracked_connect
    make_service = store_tests.WorkStorePostgresTest.make_service


    def test_defect_persistence_filter_links_and_replay(self):
        service=self.make_service()
        discovering=service.submit(make_submit(idempotency_key='discovering'))
        payload={**defect(),'discovering_task_id':discovering['task_id']}
        command=replace(make_submit(idempotency_key='defect'),task_kind='defect',defect=payload)
        receipt=service.submit(command)
        self.assertEqual(service.submit(command),receipt)
        task=service.get(work_api.WorkGet('homelab',UUID(receipt['task_id'])))
        self.assertEqual((task['task_kind'],task['defect']),('defect',payload))
        linked=service.get(work_api.WorkGet('homelab',UUID(discovering['task_id'])))
        self.assertEqual(linked['filed_defects'],[{'task_id':receipt['task_id'],'title':'Draft plan','severity':'high'}])
        for scope in (None,'all','active'):
            page=service.list_tasks(work_api.WorkList('homelab',scope,20,None,task_kind_filter='defect'))
            self.assertEqual([row['task_id'] for row in page['tasks']],[receipt['task_id']])
            self.assertEqual(page['tasks'][0]['defect'],payload)
        with self.assertRaises(Rejected) as caught:
            service.submit(replace(command,defect={**payload,'summary':'Different'}))
        self.assertEqual(caught.exception.code,'idempotency_conflict')
        with self.assertRaises(Rejected) as caught:
            service.submit(replace(command,idempotency_key='bad-reference',defect={**payload,'discovering_task_id':'00000000-0000-0000-0000-000000000001'}))
        self.assertEqual(caught.exception.reason,'defect.discovering_task_id')
