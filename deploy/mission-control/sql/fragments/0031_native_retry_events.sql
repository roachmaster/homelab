-- All source-lineage totals remain auditable, without per-task ceilings.
ALTER TABLE forge_agent.coding_workflows DROP CONSTRAINT coding_workflows_plan_revisions_check;
ALTER TABLE forge_agent.coding_workflows ADD CONSTRAINT coding_workflows_plan_revisions_check CHECK(plan_revisions>=0);
ALTER TABLE forge_external_workflow.workflows DROP CONSTRAINT workflows_plan_revisions_check;
ALTER TABLE forge_external_workflow.workflows ADD CONSTRAINT workflows_plan_revisions_check CHECK(plan_revisions>=0);
ALTER TABLE forge_external_workflow.workflows DROP CONSTRAINT workflows_code_revisions_check;
ALTER TABLE forge_external_workflow.workflows ADD CONSTRAINT workflows_code_revisions_check CHECK(code_revisions>=0);
ALTER TABLE forge_agent.coding_review_budget DROP CONSTRAINT coding_review_budget_repair_rounds_check;
ALTER TABLE forge_agent.coding_review_budget ADD CONSTRAINT coding_review_budget_repair_rounds_check CHECK(repair_rounds>=0);
ALTER TABLE forge_agent.coding_review_budget DROP CONSTRAINT coding_review_budget_stagnant_rounds_check;
ALTER TABLE forge_agent.coding_review_budget ADD CONSTRAINT coding_review_budget_stagnant_rounds_check CHECK(stagnant_rounds>=0);
ALTER TABLE forge_agent.coding_reviews DROP CONSTRAINT coding_reviews_round_check;
ALTER TABLE forge_agent.coding_reviews ADD CONSTRAINT coding_reviews_round_check CHECK(round>=0);
ALTER TABLE forge_agent.coding_reviews DROP CONSTRAINT coding_reviews_stagnant_rounds_check;
ALTER TABLE forge_agent.coding_reviews ADD CONSTRAINT coding_reviews_stagnant_rounds_check CHECK(stagnant_rounds>=0);
ALTER TABLE forge_agent.coding_reviews DROP CONSTRAINT coding_reviews_infrastructure_retries_check;
ALTER TABLE forge_agent.coding_reviews ADD CONSTRAINT coding_reviews_infrastructure_retries_check CHECK(infrastructure_retries>=0);

CREATE FUNCTION forge_agent.retry_event_bind_review(p_review uuid,p_event uuid,p_action_key text) RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $f$
DECLARE q forge_agent.coding_reviews%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;e forge_agent.coding_retry_events%ROWTYPE;
BEGIN
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p_review AND owner_id=current_setting('forge.coding_owner',true);
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(q.owner_id,q.project_alias,q.task_id);
 SELECT * INTO e FROM forge_agent.coding_retry_events WHERE event_id=p_event;
 IF t.id IS NULL OR e.event_id IS NULL OR(e.owner_id,e.project_alias,e.scope_kind,e.scope_id) IS DISTINCT FROM
 (t.owner_id,t.project_alias,CASE WHEN t.workflow_id IS NULL THEN 'manual_task' ELSE 'native_workflow' END,coalesce(t.workflow_id,t.repair_root_task_id,t.id))
 OR p_action_key IS NULL OR octet_length(p_action_key) NOT BETWEEN 1 AND 256 THEN RAISE EXCEPTION 'Retry review binding mismatch' USING ERRCODE='42501';END IF;
 PERFORM forge_agent._retry_scope(e.project_alias,e.scope_kind,e.scope_id);
 INSERT INTO forge_agent.coding_retry_review_events(review_id,event_id,reservation_key) VALUES(p_review,p_event,p_action_key)
 ON CONFLICT(review_id) DO UPDATE SET event_id=excluded.event_id,reservation_key=excluded.reservation_key;
END;$f$;
CREATE FUNCTION forge_agent._retry_native_context(p_task uuid) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $f$
DECLARE t forge_agent.coding_tasks%ROWTYPE;v_scope_kind text;v_scope_id uuid;events jsonb;result_sha text;
BEGIN
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE id=p_task AND owner_id=current_setting('forge.coding_owner',true);
 IF NOT FOUND THEN RAISE EXCEPTION 'Retry task scope mismatch' USING ERRCODE='42501';END IF;
 v_scope_kind:=CASE WHEN t.workflow_id IS NULL THEN 'manual_task' ELSE 'native_workflow' END;v_scope_id:=coalesce(t.workflow_id,t.repair_root_task_id,t.id);
 IF t.feedback_id IS NOT NULL THEN
  SELECT a.result_sha256 INTO result_sha FROM forge_agent.coding_attempts a WHERE(a.owner_id,a.project_alias,a.task_id,a.attempt_id,a.feedback_id)=(t.owner_id,t.project_alias,t.id,t.latest_attempt_id,t.feedback_id) AND a.phase='stopped' AND NOT a.reservation_active AND a.lease_expires_at IS NULL AND a.published_at IS NOT NULL AND a.result_sha256 IS NOT NULL;
  IF FOUND THEN PERFORM forge_agent.retry_event_resolve(t.project_alias,v_scope_kind,v_scope_id,'feedback_delivery',t.latest_attempt_id::text,result_sha,'feedback.'||t.feedback_id::text);END IF;
 END IF;
 SELECT coalesce(jsonb_agg(forge_agent._retry_event_dto(e)||jsonb_build_object('phase_id',e.phase_id) ORDER BY e.updated_at DESC),'[]'::jsonb) INTO events
 FROM forge_agent.coding_retry_events e WHERE(e.owner_id,e.project_alias,e.scope_kind,e.scope_id)=(t.owner_id,t.project_alias,v_scope_kind,v_scope_id) AND e.status IN('open','exhausted');
 RETURN jsonb_build_object('retry_policy','event-v1','retry_events',events);
END;$f$;
CREATE FUNCTION forge_agent._retry_native_observe(p_review uuid,p_observation jsonb) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $f$
DECLARE q forge_agent.coding_reviews%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;v_kind text;v_scope_kind text;v_scope_id uuid;digest text;e jsonb;k text;phase text;passed text[];passed_phases text[];
BEGIN
 IF p_observation IS NULL THEN RETURN NULL;END IF;
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p_review AND owner_id=current_setting('forge.coding_owner',true);
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(q.owner_id,q.project_alias,q.task_id);
 IF t.id IS NULL OR jsonb_typeof(p_observation)<>'object' OR EXISTS(SELECT FROM jsonb_object_keys(p_observation) AS keys(name) WHERE keys.name NOT IN('failure_kind','passed_kinds','stage_evidence_digest','phase_id','passed_phase_ids'))
 OR jsonb_typeof(coalesce(p_observation->'passed_kinds','[]'::jsonb))<>'array' OR jsonb_typeof(coalesce(p_observation->'passed_phase_ids','[]'::jsonb))<>'array'
 THEN RAISE EXCEPTION 'Invalid trusted retry observation' USING ERRCODE='22023';END IF;
 v_scope_kind:=CASE WHEN t.workflow_id IS NULL THEN 'manual_task' ELSE 'native_workflow' END;v_scope_id:=coalesce(t.workflow_id,t.repair_root_task_id,t.id);
 v_kind:=p_observation->>'failure_kind';digest:=p_observation->>'stage_evidence_digest';phase:=coalesce(p_observation->>'phase_id','');
 IF digest IS NULL OR digest!~'^[0-9a-f]{64}$' THEN RAISE EXCEPTION 'Invalid trusted retry proof digest' USING ERRCODE='22023';END IF;
 passed:=ARRAY(SELECT jsonb_array_elements_text(coalesce(p_observation->'passed_kinds','[]'::jsonb)));passed_phases:=ARRAY(SELECT jsonb_array_elements_text(coalesce(p_observation->'passed_phase_ids','[]'::jsonb)));
 IF v_kind IS NOT NULL THEN
  e:=forge_agent.retry_event_observe(t.project_alias,v_scope_kind,v_scope_id,v_kind,q.review_id::text,digest,passed,phase,passed_phases);
  PERFORM forge_agent.retry_event_bind_review(q.review_id,(e->>'event_id')::uuid,'review-fix.'||q.review_id::text);
 ELSE
  FOREACH k IN ARRAY passed LOOP PERFORM forge_agent.retry_event_resolve(t.project_alias,v_scope_kind,v_scope_id,k,q.review_id::text,digest);END LOOP;
  FOREACH phase IN ARRAY passed_phases LOOP
   FOR k IN SELECT DISTINCT x.kind FROM forge_agent.coding_retry_events x WHERE(x.owner_id,x.project_alias,x.scope_kind,x.scope_id,x.phase_id)=(t.owner_id,t.project_alias,v_scope_kind,v_scope_id,phase) AND x.status IN('open','exhausted') LOOP
    PERFORM forge_agent.retry_event_resolve(t.project_alias,v_scope_kind,v_scope_id,k,q.review_id::text,digest,phase);
   END LOOP;
  END LOOP;
 END IF;
 RETURN e;
END;$f$;
REVOKE ALL ON FUNCTION forge_agent.retry_event_bind_review(uuid,uuid,text),forge_agent._retry_native_context(uuid),forge_agent._retry_native_observe(uuid,jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_agent.retry_event_bind_review(uuid,uuid,text) TO forge_agent,forge_coding_reconciler;

-- Exact predecessor identities prevent a policy patch from replacing authority guards.
CREATE FUNCTION forge_agent._retry_native_patches() RETURNS jsonb LANGUAGE sql IMMUTABLE SET search_path=pg_catalog,pg_temp AS $f$
 SELECT $patches31$[{"name":"forge_agent.coding_schedule_repair(uuid,uuid)","old_sha":"2cd2e122b738d4284ce758486a9ea0edf146691f050f6ec7a87581fa097878e3","new_sha":"8a9400c2e953dc2a4ad73783930a8a9b14611c9bfd54bbc04970760b0e5b33e8","patches":[["root_id uuid;","root_id uuid;event_link forge_agent.coding_retry_review_events%ROWTYPE;event_receipt jsonb;"],[" IF b.repair_rounds>=p.max_rounds OR greatest(b.stagnant_rounds,q.stagnant_rounds)>=p.max_stagnant_rounds THEN\n RAISE EXCEPTION 'Repair budget exhausted' USING ERRCODE='P0009';END IF;"," SELECT * INTO event_link FROM forge_agent.coding_retry_review_events WHERE review_id=q.review_id;\n IF FOUND THEN\n  event_receipt:=forge_agent.retry_event_reserve(event_link.event_id,event_link.reservation_key);\n  IF event_receipt->>'status'='exhausted' THEN RAISE EXCEPTION 'Retry event exhausted' USING ERRCODE='P0009';END IF;\n ELSE\n IF b.repair_rounds>=p.max_rounds OR greatest(b.stagnant_rounds,q.stagnant_rounds)>=p.max_stagnant_rounds THEN\n RAISE EXCEPTION 'Repair budget exhausted' USING ERRCODE='P0009';END IF;\n END IF;"]]},{"name":"forge_coding_reconciler_api.review_finish(uuid,uuid,jsonb)","old_sha":"5392008b8b2750787fd19df3f85b9e35f74a65c7f6478063e1f9eaa3d8364390","new_sha":"adbd755abbe35ed3f92648acaa85e21460be8f62975d99e627c14bc05a1b563f","patches":[["v_candidate text;v_gate text;","v_candidate text;v_gate text;event_receipt jsonb;has_retry boolean;"],[" SELECT * INTO p FROM forge_agent.coding_review_policy WHERE owner_id=t.owner_id AND project_alias=t.project_alias;"," event_receipt:=forge_agent._retry_native_observe(q.review_id,p_outcome->'retry_observation');\n has_retry:=p_outcome->'retry_observation' IS NOT NULL;\n SELECT * INTO p FROM forge_agent.coding_review_policy WHERE owner_id=t.owner_id AND project_alias=t.project_alias;"],[" IF disposition='repair' AND(greatest(q.round,coalesce((SELECT b.repair_rounds FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),0))>=p.max_rounds OR stagnant>=p.max_stagnant_rounds) THEN disposition:='handoff';END IF;"," IF disposition='repair' AND((has_retry AND event_receipt->>'status'='exhausted') OR(NOT has_retry AND(greatest(q.round,coalesce((SELECT b.repair_rounds FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),0))>=p.max_rounds OR stagnant>=p.max_stagnant_rounds))) THEN disposition:='handoff';END IF;"],[" IF disposition='retry_infrastructure' AND q.infrastructure_retries>=p.max_infrastructure_retries THEN disposition:='handoff';END IF;"," IF disposition='retry_infrastructure' AND((has_retry AND event_receipt->>'status'='exhausted') OR(NOT has_retry AND q.infrastructure_retries>=p.max_infrastructure_retries)) THEN disposition:='handoff';END IF;"],[" ELSIF disposition='retry_infrastructure' AND(t.auto_repair OR(q.phase='source' AND q.owner_decision->>'decision'='repair_source')) THEN"," ELSIF disposition='retry_infrastructure' AND(t.auto_repair OR(q.phase='source' AND q.owner_decision->>'decision'='repair_source')) THEN\n  IF has_retry THEN PERFORM forge_agent.retry_event_reserve((event_receipt->>'event_id')::uuid,'infra-review.'||q.review_id::text);END IF;"]]},{"name":"forge_coding_reconciler_api.review_claim(text)","old_sha":"a147c3e2325ba6948f3e28270a457e60abc9e0e59dec5f2880ed8038af260ecd","new_sha":"e601931e658dc87b9df9192edcfeb24a39e3594e725782335a12b920415752e6","patches":[[" RETURN jsonb_build_object("," RETURN forge_agent._retry_native_context(j.task_id)||jsonb_build_object("]]},{"name":"forge_coding_worker_api.read_workflow_feedback(uuid,uuid)","old_sha":"889bef227a31a8db0f3818fcb175838a5295bee7d9ff780496a9aec0333ad9b0","new_sha":"875f39531d50753410e7f3607b185efcceb9318960610cf1ce0753ace9b93dbc","patches":[["(a.repair_source_review_id=r.source_review_id OR(t.parent_task_id IS NULL AND r.source_task_id=t.id))","(a.repair_source_review_id=r.source_review_id OR(t.parent_task_id IS NULL AND r.source_task_id=t.id)\n OR(t.parent_task_id IS NOT NULL AND r.source_task_id=t.id AND EXISTS(\n  SELECT 1 FROM forge_agent.coding_retry_review_events link\n  JOIN forge_agent.coding_retry_event_admissions admitted ON(admitted.event_id,admitted.event_key)=(link.event_id,link.reservation_key)\n  JOIN forge_agent.coding_retry_events event ON event.event_id=link.event_id\n  JOIN forge_agent.coding_reviews source ON source.review_id=link.review_id\n  WHERE link.review_id=r.source_review_id AND link.reservation_key='peer-revision.'||r.workflow_review_id::text\n  AND(event.owner_id,event.project_alias,event.scope_kind,event.scope_id,event.kind)=(a.owner_id,a.project_alias,'native_workflow',a.workflow_id,'review')\n  AND(source.owner_id,source.project_alias,source.task_id,source.attempt_id,source.parent_result_sha256,source.status)=\n   (a.owner_id,a.project_alias,t.id,r.source_attempt_id,r.source_result_sha256,'repair'))))"]]}]$patches31$::jsonb
$f$;
CREATE FUNCTION forge_agent._retry_legacy_source(p_oid oid,p_source text) RETURNS text LANGUAGE plpgsql STABLE SET search_path=pg_catalog,pg_temp AS $f$
DECLARE item jsonb;pair jsonb;source text:=p_source;
BEGIN
 FOR item IN SELECT jsonb_array_elements(forge_agent._retry_native_patches()) LOOP
  IF p_oid=(item->>'name')::regprocedure THEN
   IF encode(sha256(convert_to(source,'UTF8')),'hex')<>item->>'new_sha' THEN RAISE EXCEPTION 'Retry native authority source drift';END IF;
   FOR pair IN SELECT value FROM jsonb_array_elements(item->'patches') WITH ORDINALITY p(value,n) ORDER BY n DESC LOOP source:=replace(source,pair->>1,pair->>0);END LOOP;
   IF encode(sha256(convert_to(source,'UTF8')),'hex')<>item->>'old_sha' THEN RAISE EXCEPTION 'Retry predecessor restoration drift';END IF;
   RETURN source;
  END IF;
 END LOOP;
 RETURN source;
END;$f$;
DO $patch$
DECLARE item jsonb;pair jsonb;source text;definition text;
BEGIN
 FOR item IN SELECT jsonb_array_elements(forge_agent._retry_native_patches()) LOOP
  SELECT prosrc,pg_get_functiondef(oid) INTO source,definition FROM pg_proc WHERE oid=(item->>'name')::regprocedure;
  IF encode(sha256(convert_to(source,'UTF8')),'hex')<>item->>'old_sha' THEN RAISE EXCEPTION 'Unexpected predecessor retry source: %',item->>'name';END IF;
  FOR pair IN SELECT jsonb_array_elements(item->'patches') LOOP source:=replace(source,pair->>0,pair->>1);END LOOP;
  IF encode(sha256(convert_to(source,'UTF8')),'hex')<>item->>'new_sha' THEN RAISE EXCEPTION 'Unexpected event retry source: %',item->>'name';END IF;
  EXECUTE replace(definition,(SELECT prosrc FROM pg_proc WHERE oid=(item->>'name')::regprocedure),source);
 END LOOP;
END;$patch$;
REVOKE ALL ON FUNCTION forge_agent._retry_native_patches(),forge_agent._retry_legacy_source(oid,text) FROM PUBLIC;
CREATE FUNCTION forge_agent._retry_legacy_constraint(p_relation oid,p_name text,p_definition text) RETURNS text LANGUAGE plpgsql STABLE SET search_path=pg_catalog,pg_temp AS $f$
DECLARE column_name text;bound integer;
BEGIN
 SELECT x.column_name,x.bound INTO column_name,bound FROM(VALUES
 ('forge_agent.coding_workflows'::regclass,'coding_workflows_plan_revisions_check','plan_revisions',20),
 ('forge_external_workflow.workflows'::regclass,'workflows_plan_revisions_check','plan_revisions',20),
 ('forge_external_workflow.workflows'::regclass,'workflows_code_revisions_check','code_revisions',20),
 ('forge_agent.coding_review_budget'::regclass,'coding_review_budget_repair_rounds_check','repair_rounds',20),
 ('forge_agent.coding_review_budget'::regclass,'coding_review_budget_stagnant_rounds_check','stagnant_rounds',20),
 ('forge_agent.coding_reviews'::regclass,'coding_reviews_round_check','round',32),
 ('forge_agent.coding_reviews'::regclass,'coding_reviews_stagnant_rounds_check','stagnant_rounds',32),
 ('forge_agent.coding_reviews'::regclass,'coding_reviews_infrastructure_retries_check','infrastructure_retries',32),
 ('forge_agent.delivery_lineages'::regclass,'delivery_lineages_rounds_check','rounds',20),
 ('forge_agent.delivery_task_lineages'::regclass,'delivery_task_lineages_round_check','round',20),
 ('forge_agent.delivery_operations'::regclass,'delivery_operations_round_check','round',20)
 )x(relation,name,column_name,bound) WHERE(x.relation,x.name)=(p_relation,p_name);
 IF NOT FOUND THEN RETURN p_definition;END IF;
 IF p_definition<>format('CHECK ((%s >= 0))',column_name) THEN RAISE EXCEPTION 'Event retry telemetry constraint drift: %',p_name;END IF;
 RETURN format('CHECK (((%s >= 0) AND (%s <= %s)))',column_name,column_name,bound);
END;$f$;
REVOKE ALL ON FUNCTION forge_agent._retry_legacy_constraint(oid,text,text) FROM PUBLIC;
