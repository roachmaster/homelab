"""Public WorkStore projections over the actual external database workflow."""
import unittest
from uuid import UUID, uuid4

from test_external_flow import ExternalFlowPGTest as _BaseFlow


class WorkflowProjectionTests(_BaseFlow):
    def test_public_envelope_projects_real_review_links_and_filters_foreign_reports(self):
        service, root, flow, job = self.submit_plan()
        store = service._store
        root_id = UUID(root['task_id'])
        detail = store.get(project_alias='homelab',task_id=root_id)
        workflow = detail['workflow']
        self.assertTrue(detail['workflow_required'])
        self.assertEqual(workflow['kind'],'external_database')
        self.assertEqual(workflow['phase'],'plan_review')
        self.assertEqual(str(UUID(detail['correlation_id'])),detail['correlation_id'])
        self.assertIsNone(detail['attempt'])
        self.assertIsNone(detail['result'])
        native = self.rows('SELECT workflow_id,latest_attempt_id FROM forge_agent.coding_tasks WHERE id=%s',(root_id,))[0]
        self.assertIsNone(native['workflow_id'])
        self.assertIsNone(native['latest_attempt_id'])
        listed = store.list_tasks(project_alias='homelab')['tasks']
        self.assertEqual(len(listed),1)
        self.assertEqual(listed[0]['correlation_id'],detail['correlation_id'])
        self.assertEqual(listed[0]['workflow_phase'],'plan_review')
        child = store.get(project_alias='homelab',task_id=UUID(workflow['reviewer_task_id']))
        self.assertEqual(child['mode'],'review')
        events = store.events(project_alias='homelab',task_id=root_id)['events']
        self.assertEqual(len({e['event_id'] for e in events}),len(events))
        self.assertTrue(all(e['correlation_id']==detail['correlation_id'] for e in events))
        links = [e['workflow_progress']['related_task_id'] for e in events if e.get('workflow_progress',{}).get('related_task_id')]
        self.assertIn(workflow['reviewer_task_id'],links)
        # A genuine event identity cannot confer authority on a foreign link.
        payload = '{"workflow_id":"%s","phase":"plan_review","summary":"Foreign task must be hidden","related_task_id":"%s"}' % (workflow['workflow_id'],uuid4())
        self.pg.admin_exec("INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload) SELECT owner_id,project_alias,id,(SELECT max(sequence)+1 FROM forge_agent.coding_events WHERE task_id=t.id),'checkpoint','%s'::jsonb FROM forge_agent.coding_tasks t WHERE id='%s'"%(payload,root_id))
        last = store.events(project_alias='homelab',task_id=root_id)['events'][-1]
        self.assertNotIn('workflow_progress',last)
        self.assertNotIn('progress',last)
        self.assertNotIn('external_progress',last)

    def test_completed_projection_records_exact_merge_without_native_process_proofs(self):
        super().test_complete_actual_native_review_delivery_lifecycle()
        root = self.rows("SELECT task_id FROM forge_external_workflow.tasks WHERE role='root'")[0]
        store = self.service()._store
        detail = store.get(project_alias='homelab',task_id=root['task_id'])
        self.assertEqual(detail['status'],'completed')
        self.assertEqual(detail['workflow']['phase'],'completed')
        self.assertEqual(detail['workflow']['candidate_commit'],'c'*40)
        self.assertEqual(detail['workflow']['merge_commit'],'c'*40)
        self.assertFalse(detail['workflow']['can_cancel'])
        self.assertIsNone(detail['attempt'])
        self.assertIsNone(detail['result'])
        events = store.events(project_alias='homelab',task_id=root['task_id'])['events']
        # The workflow checkpoint precedes the root task's distinct terminal
        # event within the same transaction. A terminal event is not a
        # native progress report or a checkpoint-shaped payload.
        terminal = events[-1]
        self.assertEqual(terminal['kind'],'completed')
        self.assertNotIn('progress',terminal)
        self.assertNotIn('external_progress',terminal)
        self.assertNotIn('workflow_progress',terminal)
        completed = [e for e in events if e.get('workflow_progress',{}).get('phase')=='completed']
        self.assertEqual(len(completed),1)
        self.assertEqual(completed[0]['workflow_progress']['workflow_id'],detail['workflow']['workflow_id'])
        self.assertLess(completed[0]['sequence'],terminal['sequence'])
        self.assertEqual(events[-1]['correlation_id'],detail['correlation_id'])
        self.assertTrue(all(e['correlation_id']==detail['correlation_id'] for e in events))
        self.assertEqual(len({e['event_id'] for e in events}),len(events))
        self.assertEqual(len(store.list_tasks(project_alias='homelab')['tasks']),1)


def load_tests(loader, tests, pattern):
    return unittest.TestSuite(WorkflowProjectionTests(name) for name in (
        'test_public_envelope_projects_real_review_links_and_filters_foreign_reports',
        'test_completed_projection_records_exact_merge_without_native_process_proofs',
    ))


if __name__ == '__main__':
    unittest.main()
