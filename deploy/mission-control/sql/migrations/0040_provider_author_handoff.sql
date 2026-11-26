-- M40: Source-bound AGY -> Codex author delegation. Original workflow authority,
-- approvals, results, retry evidence and all predecessor migrations stay frozen.
-- The application coordinator admits one completed handoff action atomically
-- with the existing task transition. Workers receive no new mutation API.

CREATE FUNCTION forge_agent._coding_author_canonical_json(p_value jsonb)
 RETURNS text LANGUAGE sql IMMUTABLE STRICT SET search_path=pg_catalog,pg_temp AS $fn$
 SELECT CASE jsonb_typeof(p_value)
 WHEN 'object' THEN '{'||coalesce((SELECT string_agg(to_jsonb(key)::text||':'||forge_agent._coding_author_canonical_json(value),',' ORDER BY key COLLATE "C") FROM jsonb_each(p_value)),'')||'}'
 WHEN 'array' THEN '['||coalesce((SELECT string_agg(forge_agent._coding_author_canonical_json(value),',' ORDER BY ordinal) FROM jsonb_array_elements(p_value) WITH ORDINALITY AS a(value,ordinal)),'')||']'
 ELSE p_value::text END
$fn$;
REVOKE ALL ON FUNCTION forge_agent._coding_author_canonical_json(jsonb) FROM PUBLIC;

CREATE FUNCTION forge_agent.coding_workflow_author_handoff(p_workflow uuid)
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
 AND f->>'intent_sha256'=encode(sha256(convert_to(forge_agent._coding_author_canonical_json(i),'UTF8')),'hex')
 AND f->'receipt'->>'status'='queued' AND f->'receipt'->>'task_id'=i->>'task_id'
 AND f->'receipt'->>'project_alias'=w.project_alias
 AND f->'receipt'->'base_revision'=i->'source_revision'
 AND f->'receipt'->>'revision'=((i->>'source_revision')::bigint+1)::text) IS DISTINCT FROM true THEN RETURN NULL;END IF;
 s:=i->'source_snapshot';e:=i->'effective_snapshot';
 IF (jsonb_typeof(s)='object' AND jsonb_typeof(e)='object'
 AND s->>'policy_digest'=i->>'source_snapshot_sha256'
 AND s->>'policy_digest'=encode(sha256(convert_to(forge_agent._coding_author_canonical_json(s-'policy_digest'),'UTF8')),'hex')
 AND e->>'policy_digest'=encode(sha256(convert_to(forge_agent._coding_author_canonical_json(e-'policy_digest'),'UTF8')),'hex')
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
   AND s->'recipes'->entry->'recipe'->>'model'=i->>'source_model'
   AND s->'recipes'->entry->>'digest'=encode(sha256(convert_to(forge_agent._coding_author_canonical_json(s->'recipes'->entry->'recipe'),'UTF8')),'hex')) IS DISTINCT FROM true THEN RETURN NULL;END IF;
  IF (jsonb_typeof(recipe)='object' AND recipe->'version'='2'::jsonb AND recipe->>'backend'='codex'
   AND recipe->>'mode'=handoff_mode AND recipe->'generation_token_limit'='null'::jsonb
   AND recipe->>'model'~'^[ -~]{1,128}$' AND recipe->>'resource_profile'~'^forge-coding-v[0-9]+$'
   AND recipe->>'policy_digest'~'^[0-9a-f]{64}$'
   AND recipe-ARRAY['version','backend','model','resource_profile','mode','generation_token_limit','policy_digest']='{}'::jsonb
   AND e->'recipes'->entry->>'digest'=encode(sha256(convert_to(forge_agent._coding_author_canonical_json(recipe),'UTF8')),'hex')) IS DISTINCT FROM true THEN RETURN NULL;END IF;
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
    OR(i->>'source_status'='queued' AND i->>'source_mode'='implement' AND own_attempt.pinned_recipe->>'mode'='plan'
    AND EXISTS(SELECT FROM forge_agent.coding_plan_decisions pd
     WHERE(pd.owner_id,pd.project_alias,pd.task_id,pd.plan_attempt_id)=(w.owner_id,w.project_alias,w.root_task_id,own_attempt.attempt_id)
     AND pd.decision_id=(i->>'plan_decision_id')::uuid AND pd.decision='resume')))
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
REVOKE ALL ON FUNCTION forge_agent.coding_workflow_author_handoff(uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_agent.coding_workflow_author_handoff(uuid) TO forge_agent;

CREATE FUNCTION forge_agent.coding_workflow_author_profile(p_workflow uuid)
 RETURNS text LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE w forge_agent.coding_workflows%ROWTYPE;h jsonb;
BEGIN
 SELECT * INTO w FROM forge_agent.coding_workflows WHERE workflow_id=p_workflow AND owner_id=current_setting('forge.coding_owner',true)
 AND enabled AND phase NOT IN('completed','cancelled','escalated');
 IF NOT FOUND THEN RETURN NULL;END IF;
 IF NOT EXISTS(SELECT FROM forge_agent.coding_workflow_actions WHERE workflow_id=p_workflow AND source_key LIKE 'provider-author-handoff:%') THEN RETURN w.snapshot->>'author_profile';END IF;
 h:=forge_agent.coding_workflow_author_handoff(p_workflow);
 RETURN h->'intent'->>'target_profile';
END;
$fn$;
REVOKE ALL ON FUNCTION forge_agent.coding_workflow_author_profile(uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_agent.coding_workflow_author_profile(uuid) TO forge_agent;

CREATE FUNCTION forge_agent._coding_author_feedback_proof(p_attempt uuid,p_kind text)
 RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.coding_attempts%ROWTYPE;h jsonb;source text;value jsonb;previous forge_agent.coding_attempts%ROWTYPE;
 original forge_agent.coding_attempts%ROWTYPE;receipt forge_agent.coding_reconciliation_receipts%ROWTYPE;
BEGIN
 SELECT * INTO a FROM forge_agent.coding_attempts WHERE attempt_id=p_attempt AND owner_id=current_setting('forge.coding_owner',true);
 IF NOT FOUND OR NOT EXISTS(SELECT FROM forge_agent.coding_tasks t
 WHERE(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)=(a.owner_id,a.project_alias,a.task_id,a.attempt_id)
 AND((t.status='running' AND a.lease_expires_at>clock_timestamp() AND a.deadline_at>clock_timestamp())
 OR(t.status='needs_reconcile' AND a.phase='stopped' AND NOT a.reservation_active AND a.lease_expires_at IS NULL AND a.published_at IS NOT NULL
 AND EXISTS(SELECT FROM forge_agent.coding_reconciliation_receipts r WHERE(r.owner_id,r.project_alias,r.task_id,r.attempt_id,r.task_revision)=(a.owner_id,a.project_alias,a.task_id,a.attempt_id,t.revision)
  AND r.stop_proof->'scope_stopped'='true'::jsonb AND r.stop_proof->>'source'='systemd' AND r.stop_proof->>'unit_name'=a.unit_name AND r.created_at>=a.published_at)))) THEN RAISE EXCEPTION 'Handoff feedback requires a live or reconciled owned attempt' USING ERRCODE='P0009';END IF;
 IF p_kind='peer' THEN
  SELECT target_json::jsonb->>'author_profile' INTO source FROM forge_agent.coding_workflow_reviews
   WHERE workflow_review_id=a.workflow_feedback_review_id AND workflow_id=a.workflow_id AND owner_id=a.owner_id AND project_alias=a.project_alias;
 ELSIF p_kind='build' THEN
  SELECT feedback_json::jsonb->>'agent_profile' INTO source FROM forge_agent.coding_feedback
   WHERE feedback_id=a.feedback_id AND owner_id=a.owner_id AND project_alias=a.project_alias AND task_id=a.task_id;
 ELSE RAISE EXCEPTION 'Unknown author feedback kind' USING ERRCODE='22023';END IF;
 IF source IS NOT DISTINCT FROM a.agent_profile THEN RETURN NULL;END IF;
 h:=forge_agent.coding_workflow_author_handoff(a.workflow_id);
 IF h IS NULL OR h->'intent'->>'source_profile' IS DISTINCT FROM source
 OR h->'intent'->>'target_profile' IS DISTINCT FROM a.agent_profile THEN RAISE EXCEPTION 'Handoff feedback no longer authorized' USING ERRCODE='P0009';END IF;
 value:=jsonb_build_object('version',1,'policy','forge-provider-author-handoff-v1',
  'workflow_id',a.workflow_id,'task_id',a.task_id,'attempt_id',a.attempt_id,
  'source_author_profile',source,'target_author_profile',a.agent_profile,
  'action_id',h->>'action_id','intent_sha256',h->'finish_receipt'->>'intent_sha256',
  'original_snapshot_sha256',h->'intent'->>'original_snapshot_sha256');
 IF p_kind='peer' THEN
  RETURN value||jsonb_build_object('workflow_feedback_review_id',a.workflow_feedback_review_id,'workflow_feedback_sha256',a.workflow_feedback_sha256);
 END IF;
 SELECT * INTO original FROM forge_agent.coding_attempts
 WHERE(owner_id,project_alias,task_id,attempt_id)=(a.owner_id,a.project_alias,a.task_id,(h->'intent'->>'source_attempt_id')::uuid);
 SELECT * INTO previous FROM forge_agent.coding_attempts
 WHERE(owner_id,project_alias,task_id,attempt_no)=(a.owner_id,a.project_alias,a.task_id,a.attempt_no-1);
 SELECT * INTO receipt FROM forge_agent.coding_reconciliation_receipts
 WHERE(owner_id,project_alias,task_id,attempt_id)=(a.owner_id,a.project_alias,a.task_id,previous.attempt_id)
 ORDER BY created_at DESC,receipt_id DESC LIMIT 1;
 IF (original.feedback_id=a.feedback_id AND original.feedback_sha256=a.feedback_sha256
 AND original.plan_decision_id IS NOT DISTINCT FROM a.plan_decision_id
 AND original.scope_plan_decision_id IS NOT DISTINCT FROM a.scope_plan_decision_id
 AND previous.phase='stopped' AND NOT previous.reservation_active AND previous.lease_expires_at IS NULL
 AND previous.published_at IS NOT NULL AND previous.feedback_id=a.feedback_id AND previous.feedback_sha256=a.feedback_sha256
 AND previous.plan_decision_id IS NOT DISTINCT FROM a.plan_decision_id
 AND previous.scope_plan_decision_id IS NOT DISTINCT FROM a.scope_plan_decision_id
 AND receipt.workspace_digest=a.resume_workspace_digest AND receipt.stop_proof->'scope_stopped'='true'::jsonb
 AND receipt.stop_proof->>'source'='systemd' AND receipt.stop_proof->>'unit_name'=previous.unit_name
 AND receipt.created_at>=previous.published_at) IS DISTINCT FROM true THEN RAISE EXCEPTION 'Handoff feedback source lineage changed' USING ERRCODE='P0009';END IF;
 RETURN value||jsonb_build_object('feedback_id',a.feedback_id,'feedback_sha256',a.feedback_sha256,
  'parent_attempt_id',(SELECT feedback_json::jsonb->>'parent_attempt_id' FROM forge_agent.coding_feedback WHERE feedback_id=a.feedback_id),
  'resume_attempt_id',previous.attempt_id,'resume_workspace_digest',a.resume_workspace_digest,
  'feedback_workspace_digest',(SELECT feedback_json::jsonb->>'workspace_digest' FROM forge_agent.coding_feedback WHERE feedback_id=a.feedback_id));
END;
$fn$;
REVOKE ALL ON FUNCTION forge_agent._coding_author_feedback_proof(uuid,text) FROM PUBLIC;

CREATE FUNCTION forge_agent.coding_workflow_author_feedback(p_attempt uuid,p_kind text)
 RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
 SELECT forge_agent._coding_author_feedback_proof(p_attempt,p_kind)
$fn$;
REVOKE ALL ON FUNCTION forge_agent.coding_workflow_author_feedback(uuid,text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_agent.coding_workflow_author_feedback(uuid,text) TO forge_agent;

-- Only the exact additive changes are reversed for historical verification;
-- the complete installed definitions are independently pinned by M40 first.
CREATE FUNCTION forge_agent._coding_author_handoff_legacy_source(p_oid oid,p_source text)
 RETURNS text LANGUAGE plpgsql IMMUTABLE STRICT SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 IF p_oid='forge_agent.coding_workflow_role_granted(uuid,text,text)'::regprocedure THEN
  RETURN replace(p_source,$new$p_profile=forge_agent.coding_workflow_author_profile(w.workflow_id)$new$,$old$p_profile=w.snapshot->>'author_profile'$old$);
 END IF;
 IF p_oid='forge_coding_worker_api.read_workflow_feedback(uuid,uuid)'::regprocedure THEN
  RETURN replace(replace(p_source,$new$forge_agent.coding_workflow_author_profile(w.workflow_id)=a.agent_profile$new$,$old$w.snapshot->>'author_profile'=a.agent_profile$old$),
   $new$||coalesce((SELECT jsonb_build_object('author_handoff',proof) FROM(SELECT forge_agent._coding_author_feedback_proof(a.attempt_id,'peer') AS proof) x WHERE proof IS NOT NULL),'{}'::jsonb)$new$,'');
 END IF;
 IF p_oid='forge_coding_worker_api.read_feedback(uuid,uuid)'::regprocedure THEN
  RETURN replace(replace(p_source,
   $new$(f.feedback_json::jsonb->>'workspace_digest' IS DISTINCT FROM a.resume_workspace_digest AND forge_agent._coding_author_feedback_proof(a.attempt_id,'build') IS NULL)$new$,
   $old$f.feedback_json::jsonb->>'workspace_digest' IS DISTINCT FROM a.resume_workspace_digest$old$),$new$||coalesce((SELECT jsonb_build_object('author_handoff',proof) FROM(SELECT forge_agent._coding_author_feedback_proof(a.attempt_id,'build') AS proof) x WHERE proof IS NOT NULL),'{}'::jsonb)$new$,'');
 END IF;
 RETURN p_source;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_agent._coding_author_handoff_legacy_source(oid,text) FROM PUBLIC;

DO $upgrade$
DECLARE definition text;source text;replacement text;signature text;kind text;
BEGIN
 definition:=pg_get_functiondef('forge_agent.coding_workflow_role_granted(uuid,text,text)'::regprocedure);
 source:=$old$p_profile=w.snapshot->>'author_profile'$old$;
 replacement:=$new$p_profile=forge_agent.coding_workflow_author_profile(w.workflow_id)$new$;
 IF position(source in definition)=0 THEN RAISE EXCEPTION 'Unexpected author-role source';END IF;
 EXECUTE replace(definition,source,replacement);
 FOREACH kind IN ARRAY ARRAY['peer','build'] LOOP
  signature:=CASE kind WHEN 'peer' THEN 'forge_coding_worker_api.read_workflow_feedback(uuid,uuid)' ELSE 'forge_coding_worker_api.read_feedback(uuid,uuid)' END;
  definition:=pg_get_functiondef(signature::regprocedure);
  IF kind='peer' THEN
   source:=$old$w.snapshot->>'author_profile'=a.agent_profile$old$;
   replacement:=$new$forge_agent.coding_workflow_author_profile(w.workflow_id)=a.agent_profile$new$;
   IF position(source in definition)=0 THEN RAISE EXCEPTION 'Unexpected feedback-author source';END IF;
   definition:=replace(definition,source,replacement);
   source:=$old$ RETURN jsonb_build_object('target_json',r.target_json,'target_sha256',r.target_sha256,'verdict_json',r.verdict_json,'verdict_sha256',r.verdict_sha256);$old$;
  ELSE
   source:=$old$f.feedback_json::jsonb->>'workspace_digest' IS DISTINCT FROM a.resume_workspace_digest$old$;
   replacement:=$new$(f.feedback_json::jsonb->>'workspace_digest' IS DISTINCT FROM a.resume_workspace_digest AND forge_agent._coding_author_feedback_proof(a.attempt_id,'build') IS NULL)$new$;
   IF position(source in definition)=0 THEN RAISE EXCEPTION 'Unexpected feedback-workspace source';END IF;
   definition:=replace(definition,source,replacement);
   source:=$old$ RETURN jsonb_build_object('feedback',f.feedback_json::jsonb,'feedback_json',f.feedback_json,'legacy_resume_note',a.legacy_resume_note);$old$;
  END IF;
  IF position(source in definition)=0 THEN RAISE EXCEPTION 'Unexpected feedback-envelope source';END IF;
  replacement:=left(source,length(source)-1)||format($patch$||coalesce((SELECT jsonb_build_object('author_handoff',proof) FROM(SELECT forge_agent._coding_author_feedback_proof(a.attempt_id,%L) AS proof) x WHERE proof IS NOT NULL),'{}'::jsonb);$patch$,kind);
  EXECUTE replace(definition,source,replacement);
 END LOOP;
END;
$upgrade$;
