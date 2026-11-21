"""Disabled profile denials preserve immutable receipt lookup ordering."""
from dataclasses import replace
import json
import unittest
from unittest.mock import Mock
from uuid import uuid4

from queue_store import Rejected
from test_work_api import runner_server, work_post, REQUEST_ID, TASK_ID
from test_work_execution_and_usage import make_v2_config,make_submit
from work_api import WorkService,WorkRepair,WorkResume,WorkPlanDecide,WorkReviewDecide,Destination
from work_config import WorkCatalog,WorkProfile
from work_policy import POLICY_ID


def config(enabled=False):
 value=make_v2_config()
 return replace(value,profiles={**value.profiles,'grok-off':WorkProfile('grok-off','grok','fixture-grok',POLICY_ID,execution_enabled=enabled)})

def repair():
 return WorkRepair('homelab',TASK_ID,2,'a'*40,'grok-off','Check selected recipe.',(),'repair-key')

class DisabledProfileApiTest(unittest.TestCase):
 def test_new_disabled_repair_typed_http_denial_before_store_effects(self):
  cfg=config();store=Mock();effects=[]
  def mutate(**kw):
   kw['resolve_recipe']()
   effects.append('must not write')
  store.repair.side_effect=mutate
  service=WorkService(cfg,cfg.catalog(),store)
  with runner_server(work_service=service)as port:
   status,body,_=work_post(port,Destination.REPAIR,{'kind':'work_repair','request_id':REQUEST_ID,
    'project_alias':'homelab','task_id':str(TASK_ID),'expected_revision':2,'idempotency_key':'repair-key',
    'candidate_commit':'a'*40,'agent_profile':'grok-off','note':'Check selected recipe.','findings':[]})
  self.assertEqual(status,403);self.assertIn('denied',json.dumps(body));self.assertEqual(effects,[])
 def test_enabled_catalog_drift_is_typed_unavailable(self):
  class MissingCatalog(WorkCatalog):
   def resolve(self,*_):raise KeyError('stale selected profile')
  cfg=config(True);store=Mock();store.repair.side_effect=lambda **kw:kw['resolve_recipe']()
  service=WorkService(cfg,MissingCatalog(cfg.profiles),store)
  with self.assertRaises(Rejected)as context:service.repair(repair())
  self.assertEqual(context.exception.status,503)
 def test_immutable_repair_replay_precedes_disabled_catalog(self):
  cfg=config();store=Mock();receipt={'status':'completed','repair_task_id':str(uuid4())}
  store.repair.return_value=receipt
  self.assertEqual(WorkService(cfg,cfg.catalog(),store).repair(repair()),receipt)
  self.assertIn('resolve_recipe',store.repair.call_args.kwargs)
 def test_plan_and_review_new_recipe_selection_checks_enabled(self):
  cfg=config();store=Mock();service=WorkService(cfg,cfg.catalog(),store)
  store.plan_decide.side_effect=lambda **kw:kw['resolve_recipe']('implement')
  store.review_decide.side_effect=lambda **kw:kw['resolve_recipe']()
  for operation,command in ((service.plan_decide,WorkPlanDecide('homelab',TASK_ID,2,uuid4(),'a'*64,'resume','grok-off','plan-key')),
    (service.review_decide,WorkReviewDecide('homelab',TASK_ID,2,uuid4(),'repair','grok-off','Follow review.','review-key'))):
   with self.assertRaises(Rejected)as context:operation(command)
   self.assertEqual(context.exception.status,403)
 def test_unconfigured_profile_denied_before_store_across_submit_resume_and_repair(self):
  cfg=config();store=Mock();service=WorkService(cfg,cfg.catalog(),store)
  for operation,command in (
    (service.submit,make_submit(agent_profile='unconfigured-profile')),
    (service.resume,WorkResume('homelab',TASK_ID,2,'resume-key','unconfigured-profile',uuid4(),'Continue.')),
    (service.repair,WorkRepair('homelab',TASK_ID,2,'a'*40,'unconfigured-profile','Fix.',(),'repair-key')),
  ):
   with self.subTest(operation=operation.__name__),self.assertRaises(Rejected)as context:operation(command)
   self.assertEqual((context.exception.status,context.exception.code),(403,'denied'))
  store.submit.assert_not_called();store.resume.assert_not_called();store.repair.assert_not_called()
