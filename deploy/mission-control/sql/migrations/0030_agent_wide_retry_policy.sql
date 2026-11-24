-- A shared allowance widens counters; historical snapshots and receipts stay immutable.
ALTER TABLE forge_agent.coding_workflows DROP CONSTRAINT coding_workflows_plan_revisions_check;
ALTER TABLE forge_agent.coding_workflows ADD CONSTRAINT coding_workflows_plan_revisions_check CHECK(plan_revisions BETWEEN 0 AND 20);
ALTER TABLE forge_external_workflow.workflows DROP CONSTRAINT workflows_plan_revisions_check;
ALTER TABLE forge_external_workflow.workflows ADD CONSTRAINT workflows_plan_revisions_check CHECK(plan_revisions BETWEEN 0 AND 20);
ALTER TABLE forge_external_workflow.workflows DROP CONSTRAINT workflows_code_revisions_check;
ALTER TABLE forge_external_workflow.workflows ADD CONSTRAINT workflows_code_revisions_check CHECK(code_revisions BETWEEN 0 AND 20);
ALTER TABLE forge_agent.coding_review_policy DROP CONSTRAINT coding_review_policy_max_rounds_check;
ALTER TABLE forge_agent.coding_review_policy ADD CONSTRAINT coding_review_policy_max_rounds_check CHECK(max_rounds BETWEEN 0 AND 20);
ALTER TABLE forge_agent.coding_review_policy DROP CONSTRAINT coding_review_policy_max_stagnant_rounds_check;
ALTER TABLE forge_agent.coding_review_policy ADD CONSTRAINT coding_review_policy_max_stagnant_rounds_check CHECK(max_stagnant_rounds BETWEEN 1 AND 20);
ALTER TABLE forge_agent.coding_review_policy DROP CONSTRAINT coding_review_policy_max_infrastructure_retries_check;
ALTER TABLE forge_agent.coding_review_policy ADD CONSTRAINT coding_review_policy_max_infrastructure_retries_check CHECK(max_infrastructure_retries BETWEEN 0 AND 20);
ALTER TABLE forge_agent.coding_review_policy ALTER COLUMN max_rounds SET DEFAULT 20;
ALTER TABLE forge_agent.coding_review_policy ALTER COLUMN max_stagnant_rounds SET DEFAULT 20;
ALTER TABLE forge_agent.coding_review_policy ALTER COLUMN max_infrastructure_retries SET DEFAULT 20;
ALTER TABLE forge_agent.coding_review_budget DROP CONSTRAINT coding_review_budget_repair_rounds_check;
ALTER TABLE forge_agent.coding_review_budget ADD CONSTRAINT coding_review_budget_repair_rounds_check CHECK(repair_rounds BETWEEN 0 AND 20);
ALTER TABLE forge_agent.coding_review_budget DROP CONSTRAINT coding_review_budget_stagnant_rounds_check;
ALTER TABLE forge_agent.coding_review_budget ADD CONSTRAINT coding_review_budget_stagnant_rounds_check CHECK(stagnant_rounds BETWEEN 0 AND 20);
ALTER TABLE forge_agent.coding_review_budget ALTER COLUMN policy SET DEFAULT '{"version":"forge-repair-v1","max_rounds":20,"max_stagnant_rounds":20,"max_infrastructure_retries":20,"required_hosted_checks":["check","pr-fast-gate","real-browser"]}';

CREATE FUNCTION forge_agent.amend_agent_retry_policy(p_project text,p_root uuid,p_policy jsonb)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE t forge_agent.coding_tasks%ROWTYPE;w forge_agent.coding_workflows%ROWTYPE;
 b forge_agent.coding_review_budget%ROWTYPE;previous jsonb;packet jsonb;key text;
BEGIN
 IF session_user<>'forge_agent' OR current_setting('forge.coding_owner',true) IS NULL THEN
  RAISE EXCEPTION 'Retry allowance requires the scoped owner service' USING ERRCODE='42501';END IF;
 PERFORM pg_advisory_xact_lock(hashtextextended(current_setting('forge.coding_owner',true)||E'\n'||p_project,2026111901));
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE owner_id=current_setting('forge.coding_owner',true)
 AND project_alias=p_project AND id=p_root FOR UPDATE;
 IF NOT FOUND THEN RAISE EXCEPTION 'Retry source is not active' USING ERRCODE='P0009';END IF;
 IF t.workflow_id IS NOT NULL THEN
  SELECT * INTO w FROM forge_agent.coding_workflows WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND workflow_id=t.workflow_id;
  IF NOT FOUND OR w.phase IN('completed','cancelled') OR w.root_task_id<>t.id
  OR NOT EXISTS(SELECT FROM forge_agent.coding_worker_grants g WHERE(g.owner_id,g.project_alias,g.agent_profile,g.mode)=(w.owner_id,w.project_alias,w.snapshot->>'author_profile','implement'))
  OR NOT EXISTS(SELECT FROM forge_agent.coding_worker_grants g WHERE(g.owner_id,g.project_alias,g.agent_profile,g.mode)=(w.owner_id,w.project_alias,w.snapshot->>'reviewer_profile','review'))
  THEN RAISE EXCEPTION 'Retry workflow scope is no longer delegated' USING ERRCODE='P0009';END IF;
 ELSIF NOT EXISTS(SELECT FROM forge_agent.coding_worker_grants g WHERE(g.owner_id,g.project_alias,g.agent_profile,g.mode)=(t.owner_id,t.project_alias,t.agent_profile,t.mode))
 THEN RAISE EXCEPTION 'Retry source grant is no longer delegated' USING ERRCODE='P0009';
 END IF;
 IF t.status IN('completed','cancelled') AND w.workflow_id IS NULL
 AND NOT EXISTS(SELECT FROM forge_agent.coding_tasks child WHERE(child.owner_id,child.project_alias,child.repair_root_task_id)=(t.owner_id,t.project_alias,t.id)
                AND child.status NOT IN('completed','cancelled'))
 THEN RAISE EXCEPTION 'Retry source has no active lineage' USING ERRCODE='P0009';END IF;
 SELECT * INTO b FROM forge_agent.coding_review_budget WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND root_task_id=t.id FOR UPDATE;
 IF NOT FOUND OR jsonb_typeof(p_policy)<>'object' OR p_policy->>'version'<>'forge-repair-v1'
 OR (SELECT count(*) FROM jsonb_object_keys(p_policy))<>5
 OR p_policy->'max_rounds'<>'20'::jsonb OR p_policy->'max_stagnant_rounds'<>'20'::jsonb
 OR p_policy->'max_infrastructure_retries'<>'20'::jsonb
 OR (p_policy-ARRAY['max_rounds','max_stagnant_rounds','max_infrastructure_retries']) IS DISTINCT FROM
    (b.policy-ARRAY['max_rounds','max_stagnant_rounds','max_infrastructure_retries'])
 OR EXISTS(SELECT FROM unnest(ARRAY['max_rounds','max_stagnant_rounds','max_infrastructure_retries']) k
           WHERE jsonb_typeof(b.policy->k)<>'number' OR (b.policy->>k)::integer NOT BETWEEN 0 AND 20)
 THEN RAISE EXCEPTION 'Retry allowance can only widen numeric limits' USING ERRCODE='P0009';END IF;
 IF b.policy=p_policy AND(w.workflow_id IS NULL OR(w.snapshot->>'max_plan_revisions')::integer>=20 AND(w.snapshot->>'max_code_revisions')::integer>=20) THEN RETURN jsonb_build_object('policy','forge-agent-retry-v1','status','unchanged');END IF;
 key:=coalesce(w.workflow_id,t.id)::text;
 SELECT payload INTO previous FROM forge_agent.coding_events WHERE owner_id=t.owner_id AND project_alias=t.project_alias
 AND task_id=t.id AND event_kind='checkpoint' AND payload->>'stage'='retry_policy_updated'
 AND payload->>'policy'='forge-agent-retry-v1' AND payload->>'scope_id'=key ORDER BY sequence DESC LIMIT 1;
 IF FOUND THEN
  IF previous->'new_policy' IS DISTINCT FROM p_policy OR b.policy IS DISTINCT FROM p_policy
  THEN RAISE EXCEPTION 'Recorded retry allowance no longer matches' USING ERRCODE='P0006';END IF;
  RETURN previous;
 END IF;
 UPDATE forge_agent.coding_review_budget SET policy=p_policy WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND root_task_id=t.id;
 packet:=jsonb_build_object('stage','retry_policy_updated','policy','forge-agent-retry-v1','scope_id',key,
  'workflow_id',w.workflow_id,'snapshot_digest',w.policy_digest,'old_policy',b.policy,'new_policy',p_policy,
  'max_plan_revisions',20,'max_code_revisions',20,'repair_rounds',b.repair_rounds,'stagnant_rounds',b.stagnant_rounds,
  'summary','The shared agent retry allowance is now twenty; existing counters and evidence are preserved.');
 INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
 SELECT t.owner_id,t.project_alias,t.id,coalesce(max(sequence),0)+1,'checkpoint',packet FROM forge_agent.coding_events
 WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND task_id=t.id;
 PERFORM pg_notify('forge_coding_jobs','');
 RETURN packet;
END;$fn$;
REVOKE ALL ON FUNCTION forge_agent.amend_agent_retry_policy(text,uuid,jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_agent.amend_agent_retry_policy(text,uuid,jsonb) TO forge_agent;
