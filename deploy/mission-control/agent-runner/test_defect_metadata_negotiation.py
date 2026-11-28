"""Strict old browser task DTOs survive the defect API rollout without reload."""
from contextlib import closing
from email.message import Message
from http.client import HTTPConnection
import json
import unittest

from queue_store import Rejected
import test_work_api as fixtures
import work_api


class DefectMetadataNegotiationTest(unittest.TestCase):
    def test_legacy_shapes_are_copies_and_optin_preserves_metadata(self):
        task=fixtures.sample_summary(task_kind='defect',defect={'summary':'Owned test'},filed_defects=[])
        for kind,fields in (('work_opened',{'task':task}),('work_listed',{'tasks':[task]})):
            original=(200,fixtures.reply(kind,**fields))
            preserved=json.loads(json.dumps(original[1]))
            old=work_api.defect_metadata_response(original,None)[1]['event']
            selected=old.get('task',old.get('tasks',[None])[0])
            self.assertFalse({'task_kind','defect','filed_defects'} & selected.keys())
            self.assertEqual(original[1],preserved)
            self.assertIs(work_api.defect_metadata_response(original,1),original)
        failure=(400,fixtures.reply('work_failed',code='invalid_request',reason='defect.fields'))
        self.assertNotIn('reason',work_api.defect_metadata_response(failure,None)[1]['event'])
        self.assertEqual(work_api.defect_metadata_response(failure,1),failure)

    def test_unknown_duplicate_or_noninteger_negotiation_rejects_before_mutation(self):
        for values in (['2'],['1','1'],['true'],['01'],['']):
            headers=Message()
            for value in values:headers['Forge-Defect-Metadata-Version']=value
            with self.subTest(values=values),self.assertRaises(Rejected):work_api.destination(headers)
        for value in (None,True,'1',0,2):
            with self.subTest(value=value),self.assertRaises(Rejected):
                work_api.decode(work_api.Destination.SUBMIT,fixtures.REQUEST_ID,
                                fixtures.submit_body(defect_metadata_version=value))

    def test_presentation_version_does_not_change_submission_identity(self):
        old=work_api.decode(work_api.Destination.SUBMIT,fixtures.REQUEST_ID,fixtures.submit_body())
        new=work_api.decode(work_api.Destination.SUBMIT,fixtures.REQUEST_ID,
                            fixtures.submit_body(defect_metadata_version=1))
        self.assertEqual(old,new)
        service,store=fixtures.WorkHttpApiTest()._service_with_mock_store()
        store.submit.return_value=dict(task_id=str(fixtures.TASK_ID),project_alias='homelab',
                                       revision=1,status='queued',recipe_digest=fixtures.RECIPE_DIGEST)
        with fixtures.runner_server(work_service=service) as port:
            for version in ('2','1,1'):
                self.assertEqual(fixtures.work_post(port,work_api.Destination.SUBMIT,
                    fixtures.submit_body(),extra_headers={'Forge-Defect-Metadata-Version':version})[0],400)
        store.submit.assert_not_called()

    def test_decode_failure_obeys_capability_before_submission(self):
        service,store=fixtures.WorkHttpApiTest()._service_with_mock_store()
        invalid=fixtures.submit_body(task_kind='defect',defect={
            'summary':'Build fails','location':{'repo_path':'src/build.rs','function':'build','commit':'a'*40},
            'observed_behavior':'panic','expected_behavior':'success','severity':'urgent','evidence':'owned fixture'})
        with fixtures.runner_server(work_service=service) as port:
            for version in (None,'1'):
                status,response,_=fixtures.work_post(port,work_api.Destination.SUBMIT,invalid,
                    extra_headers={'Forge-Defect-Metadata-Version':version} if version else None)
                self.assertEqual(status,400)
                self.assertEqual(response['event'].get('reason'),'defect.severity' if version else None)
        store.submit.assert_not_called()

    def test_http_list_get_and_failure_use_caller_selected_capability(self):
        service,store=fixtures.WorkHttpApiTest()._service_with_mock_store()
        task=fixtures.sample_summary(task_kind='feature',defect=None,filed_defects=[])
        store.get.return_value=task
        store.list_tasks.return_value=dict(project_alias='homelab',tasks=[task],next_cursor=None)
        with fixtures.runner_server(work_service=service) as port:
            for route,body,key in (
                (work_api.Destination.GET,dict(kind='work_get',request_id=fixtures.REQUEST_ID,
                    project_alias='homelab',task_id=str(fixtures.TASK_ID)),'task'),
                (work_api.Destination.LIST,dict(kind='work_list',request_id=fixtures.REQUEST_ID,
                    project_alias='homelab',status_filter='all',limit=20,cursor=None),'tasks')):
                for version in (None,'1'):
                    status,response,_=fixtures.work_post(port,route,body,
                        extra_headers={'Forge-Defect-Metadata-Version':version} if version else None)
                    self.assertEqual(status,200)
                    value=response['event'][key];value=value[0] if isinstance(value,list) else value
                    self.assertEqual('task_kind' in value,version=='1')
            store.submit.reset_mock()
            with closing(HTTPConnection('127.0.0.1',port,timeout=3)) as connection:
                connection.putrequest('POST','/v1/dispatch')
                payload=json.dumps(fixtures.submit_body()).encode()
                for name,value in {'Authorization':'Bearer '+fixtures.TOKEN,'Content-Type':'application/json',
                    'Forge-Envelope-Version':'1','Forge-Contract-Version':'1','Forge-Destination':'ide.work.submit',
                    'Forge-Request-Id':fixtures.REQUEST_ID,'Content-Length':str(len(payload))}.items():
                    connection.putheader(name,value)
                connection.putheader('Forge-Defect-Metadata-Version','1')
                connection.putheader('Forge-Defect-Metadata-Version','1')
                connection.endheaders(payload)
                response=connection.getresponse();self.assertEqual(response.status,400);response.read()
            store.submit.assert_not_called()


if __name__=='__main__':unittest.main()
