-- M42 (phase 0d): stop re-hashing JSON inside PostgreSQL.
--
-- coding_workflow_author_handoff recomputed SHA-256 over a PL/pgSQL
-- re-serialization of stored jsonb (_coding_author_canonical_json) and
-- compared it with digests Python computed over json.dumps output. Any
-- difference between the two serializers (escaping, number rendering)
-- silently refused a valid handoff. The structural checks remain: stored
-- snapshots are compared as jsonb, the finish receipt is bound to the
-- intent's task, revision and recipe, digests are still compared as the
-- stored strings they are, and grants, attempts and receipts are re-read.
--
-- Additive and safe to apply after M41. Applied migrations stay unchanged;
-- owner and ACLs of the replaced function are preserved by CREATE OR REPLACE.

CREATE OR REPLACE FUNCTION forge_agent.coding_workflow_author_handoff(p_workflow uuid)
 RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE
 w forge_agent.coding_workflows%ROWTYPE;a forge_agent.coding_workflow_actions%ROWTYPE;
 t forge_agent.coding_tasks%ROWTYPE;q forge_agent.coding_attempts%ROWTYPE;
 own_attempt forge_agent.coding_attempts%ROWTYPE;r forge_agent.coding_reconciliation_receipts%ROWTYPE;
 i jsonb;f jsonb;s jsonb;e jsonb;entry text;handoff_mode text;recipe jsonb;model_route jsonb;number integer;
BEGIN
 SELECT * INTO w FROM forge_agent.coding_workflows WHERE workflow_id=p_workflow
 AND owner_id=current_setting('forge.coding_owner',true);
 IF NOT FOUND THEN RETURN NULL;END IF;
 SELECT count(*) INTO number FROM forge_agent.coding_workflow_actions
 WHERE workflow_id=w.workflow_id AND source_key LIKE 'provider-author-handoff:%';
 IF number<>1 THEN RETURN NULL;END IF;
 SELECT * INTO a FROM forge_agent.coding_workflow_actions WHERE workflow_id=w.workflow_id
 AND source_key LIKE 'provider-author-handoff:%';
 i:=a.intent;f:=a.finish_receipt;
 IF (a.owner_id=w.owner_id AND a.project_alias=w.project_alias AND a.kind='decide' AND a.status='completed'
 AND jsonb_typeof(i)='object' AND jsonb_typeof(f)='object'
 AND i?&ARRAY['policy','owner','project_alias','workflow_id','task_id','source_attempt_id','quota_source_attempt_id',
 'source_revision','source_status','source_mode','source_model','source_profile','target_profile','source_recipe_digest',
 'original_snapshot_sha256','source_snapshot','source_snapshot_sha256','effective_snapshot','reconciliation_receipt_id',
 'source_workspace_digest','plan_decision_id','scope_plan_decision_id','feedback_id','feedback_sha256']
 AND i->>'policy'='forge-provider-author-handoff-v1' AND i->>'owner'=w.owner_id
 AND i->>'project_alias'=w.project_alias AND i->>'workflow_id'=w.workflow_id::text
 AND i->>'task_id'~'^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
 AND a.source_key='provider-author-handoff:'||(i->>'task_id')
 AND i->>'original_snapshot_sha256'=w.policy_digest AND i->>'source_profile'=w.snapshot->>'author_profile'
 AND i->>'target_profile'~'^[A-Za-z0-9_.-]{1,64}$' AND i->>'target_profile'<>i->>'source_profile'
 AND i->>'target_profile'<>w.snapshot->>'reviewer_profile'
 AND i->>'source_mode' IN('plan','implement') AND i->>'source_status' IN('queued','quota_blocked')
 AND jsonb_typeof(i->'source_revision')='number' AND i->>'source_revision'~'^[1-9][0-9]{0,15}$'
 AND (i->>'source_revision')::bigint<9007199254740991
 AND f->>'status'='provider_author_handed_off'
 AND f->'receipt'->>'status'='queued' AND f->'receipt'->>'task_id'=i->>'task_id'
 AND f->'receipt'->>'project_alias'=w.project_alias
 AND f->'receipt'->'base_revision'=i->'source_revision'
 AND f->'receipt'->>'revision'=((i->>'source_revision')::bigint+1)::text) IS DISTINCT FROM true THEN RETURN NULL;END IF;
 s:=i->'source_snapshot';e:=i->'effective_snapshot';
 IF (jsonb_typeof(s)='object' AND jsonb_typeof(e)='object'
 AND s->>'policy_digest'=i->>'source_snapshot_sha256'
 AND e->>'author_profile'=i->>'target_profile' AND s->>'author_profile'=i->>'source_profile'
 AND (s-ARRAY['recipes','policy_digest'])=(w.snapshot-ARRAY['recipes','policy_digest'])
 AND (s->'recipes')-ARRAY['author_plan','author_implement']=(w.snapshot->'recipes')-ARRAY['author_plan','author_implement']
 AND (e-ARRAY['author_profile','recipes','policy_digest'])=(s-ARRAY['author_profile','recipes','policy_digest'])
 AND e->'recipes'->'reviewer_review'=s->'recipes'->'reviewer_review'
 AND (e->'recipes')-ARRAY['author_plan','author_implement']=(s->'recipes')-ARRAY['author_plan','author_implement']
 AND s->'recipes'->('author_'||(i->>'source_mode'))->>'digest'=i->>'source_recipe_digest'
 AND s->'recipes'->('author_'||(i->>'source_mode'))->'recipe'->>'model'=i->>'source_model'
 AND s->'recipes'->('author_'||(i->>'source_mode'))->'recipe'->>'backend'='antigravity'
 AND f->'receipt'->>'recipe_digest'=e->'recipes'->('author_'||(i->>'source_mode'))->>'digest') IS DISTINCT FROM true THEN RETURN NULL;END IF;
 IF s IS DISTINCT FROM w.snapshot THEN
  SELECT count(*) INTO number FROM forge_agent.coding_workflow_actions x WHERE x.workflow_id=w.workflow_id AND x.source_key LIKE 'provider-model-route:%';
  IF number<>1 THEN RETURN NULL;END IF;
  SELECT x.intent->'effective_snapshot' INTO model_route FROM forge_agent.coding_workflow_actions x
  WHERE x.workflow_id=w.workflow_id AND x.owner_id=w.owner_id AND x.project_alias=w.project_alias
   AND x.kind='decide' AND x.status='completed' AND x.source_key LIKE 'provider-model-route:%'
   AND x.intent->>'original_snapshot_sha256'=w.policy_digest
   AND x.finish_receipt->>'status'='provider_model_routed';
  IF s IS DISTINCT FROM model_route THEN RETURN NULL;END IF;
 END IF;
 FOREACH handoff_mode IN ARRAY ARRAY['plan','implement'] LOOP
  entry:='author_'||handoff_mode;recipe:=e->'recipes'->entry->'recipe';
  IF ((s->'recipes'->entry->'recipe')-'model'=(w.snapshot->'recipes'->entry->'recipe')-'model'
   AND s->'recipes'->entry->'recipe'->>'model'=i->>'source_model') IS DISTINCT FROM true THEN RETURN NULL;END IF;
  IF (jsonb_typeof(recipe)='object' AND recipe->'version'='2'::jsonb AND recipe->>'backend'='codex'
   AND recipe->>'mode'=handoff_mode AND recipe->'generation_token_limit'='null'::jsonb
   AND recipe->>'model'~'^[ -~]{1,128}$' AND recipe->>'resource_profile'~'^forge-coding-v[0-9]+$'
   AND recipe->>'policy_digest'~'^[0-9a-f]{64}$'
   AND recipe-ARRAY['version','backend','model','resource_profile','mode','generation_token_limit','policy_digest']='{}'::jsonb) IS DISTINCT FROM true THEN RETURN NULL;END IF;
  IF NOT EXISTS(SELECT FROM forge_agent.coding_worker_grants g WHERE(g.owner_id,g.project_alias,g.agent_profile,g.mode)=(w.owner_id,w.project_alias,i->>'target_profile',handoff_mode)) THEN RETURN NULL;END IF;
 END LOOP;
 IF NOT EXISTS(SELECT FROM forge_agent.coding_worker_grants g WHERE(g.owner_id,g.project_alias,g.agent_profile,g.mode)=(w.owner_id,w.project_alias,w.snapshot->>'reviewer_profile','review')) THEN RETURN NULL;END IF;
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,workflow_id,id)=(w.owner_id,w.project_alias,w.workflow_id,(i->>'task_id')::uuid);
 IF NOT FOUND OR t.workflow_review_id IS NOT NULL THEN RETURN NULL;END IF;
 IF i->>'source_mode'='implement' AND NOT EXISTS(SELECT FROM forge_agent.coding_plan_decisions pd
 WHERE(pd.owner_id,pd.project_alias,pd.task_id)=(w.owner_id,w.project_alias,w.root_task_id)
 AND pd.decision_id=coalesce((i->>'plan_decision_id')::uuid,(i->>'scope_plan_decision_id')::uuid) AND pd.decision='resume') THEN RETURN NULL;END IF;
 SELECT * INTO q FROM forge_agent.coding_attempts WHERE(owner_id,project_alias,attempt_id)=(w.owner_id,w.project_alias,(i->>'quota_source_attempt_id')::uuid);
 IF (q.agent_profile=i->>'source_profile' AND q.pinned_recipe->>'backend'='antigravity' AND q.pinned_recipe->>'model'=i->>'source_model'
 AND q.phase='stopped' AND q.outcome='quota_blocked' AND NOT q.reservation_active AND q.lease_expires_at IS NULL
 AND q.published_at IS NOT NULL AND q.finished_at IS NOT NULL AND q.result_sha256 IS NULL AND q.published_at<=a.created_at
 AND EXISTS(SELECT FROM forge_agent.coding_reconciliation_receipts qr
 WHERE(qr.owner_id,qr.project_alias,qr.task_id,qr.attempt_id)=(q.owner_id,q.project_alias,q.task_id,q.attempt_id)
 AND qr.stop_proof->'scope_stopped'='true'::jsonb AND qr.stop_proof->>'source'='systemd'
 AND qr.stop_proof->>'unit_name'=q.unit_name AND (qr.stop_proof->>'checked_at')::timestamptz>=q.started_at
 AND (qr.stop_proof->>'checked_at')::timestamptz<=a.created_at AND qr.created_at>=q.finished_at AND qr.created_at<=a.created_at)
 AND q.summary~'^Provider quota exhausted; waiting for allowance\. \[forge-provider-quota-v1 reset=(none|[1-9][0-9]{0,4}) error=[0-9a-f]{64}\]$') IS DISTINCT FROM true THEN RETURN NULL;END IF;
 IF i->'source_attempt_id'='null'::jsonb THEN
  IF i->>'source_status'<>'queued' OR i->'reconciliation_receipt_id' IS DISTINCT FROM 'null'::jsonb OR i->'source_workspace_digest' IS DISTINCT FROM 'null'::jsonb
   OR EXISTS(SELECT FROM forge_agent.coding_attempts p WHERE(p.owner_id,p.project_alias,p.task_id)=(w.owner_id,w.project_alias,t.id) AND p.started_at<=a.created_at) THEN RETURN NULL;END IF;
 ELSE
  SELECT * INTO own_attempt FROM forge_agent.coding_attempts WHERE(owner_id,project_alias,task_id,attempt_id)=(w.owner_id,w.project_alias,t.id,(i->>'source_attempt_id')::uuid);
  SELECT * INTO r FROM forge_agent.coding_reconciliation_receipts WHERE(owner_id,project_alias,task_id,attempt_id,receipt_id)=(w.owner_id,w.project_alias,t.id,own_attempt.attempt_id,(i->>'reconciliation_receipt_id')::uuid);
  IF (own_attempt.phase='stopped' AND NOT own_attempt.reservation_active AND own_attempt.lease_expires_at IS NULL
   AND own_attempt.agent_profile=i->>'source_profile' AND own_attempt.published_at IS NOT NULL
   AND own_attempt.finished_at IS NOT NULL AND own_attempt.published_at<=r.created_at AND r.created_at<=a.created_at
   AND r.workspace_digest=i->>'source_workspace_digest'
   AND (to_jsonb(own_attempt.plan_decision_id) IS NOT DISTINCT FROM nullif(i->'plan_decision_id','null'::jsonb)
    OR(i->>'source_status'='queued' AND own_attempt.pinned_recipe->>'mode'='plan'
    AND EXISTS(SELECT FROM forge_agent.coding_plan_decisions pd
     WHERE(pd.owner_id,pd.project_alias,pd.task_id,pd.plan_attempt_id)=(w.owner_id,w.project_alias,w.root_task_id,own_attempt.attempt_id)
     AND pd.decision_id=(i->>'plan_decision_id')::uuid
     AND pd.decision=CASE i->>'source_mode' WHEN 'implement' THEN 'resume' WHEN 'plan' THEN 'revise' END)))
   AND to_jsonb(own_attempt.scope_plan_decision_id) IS NOT DISTINCT FROM nullif(i->'scope_plan_decision_id','null'::jsonb)
   AND to_jsonb(own_attempt.feedback_id) IS NOT DISTINCT FROM nullif(i->'feedback_id','null'::jsonb)
   AND to_jsonb(own_attempt.feedback_sha256) IS NOT DISTINCT FROM nullif(i->'feedback_sha256','null'::jsonb)
   AND r.task_revision<=(i->>'source_revision')::bigint AND r.stop_proof->'scope_stopped'='true'::jsonb
   AND r.stop_proof->>'source'='systemd' AND r.stop_proof->>'unit_name'=own_attempt.unit_name
   AND (r.stop_proof->>'checked_at')::timestamptz>=own_attempt.started_at
   AND (r.stop_proof->>'checked_at')::timestamptz<=a.created_at) IS DISTINCT FROM true THEN RETURN NULL;END IF;
  IF i->>'source_status'='queued' AND r.task_revision>=(i->>'source_revision')::bigint THEN RETURN NULL;END IF;
  IF i->>'source_status'='quota_blocked' AND (own_attempt.outcome<>'quota_blocked' OR own_attempt.recipe_digest<>i->>'source_recipe_digest' OR r.task_revision<>(i->>'source_revision')::bigint) THEN RETURN NULL;END IF;
 END IF;
 RETURN jsonb_build_object('action_id',a.action_id,'intent',i,'finish_receipt',f);
EXCEPTION WHEN invalid_text_representation OR numeric_value_out_of_range OR datetime_field_overflow OR invalid_datetime_format THEN RETURN NULL;
END;
$fn$;

DROP FUNCTION forge_agent._coding_author_canonical_json(jsonb);
