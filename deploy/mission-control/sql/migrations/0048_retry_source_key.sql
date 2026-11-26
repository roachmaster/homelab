-- M48 (phase 0d): retry evidence is keyed by its source, not by a digest.
--
-- retry_event_observe() and retry_event_resolve() deduplicated observations
-- by (source_id, source_digest). A source re-observed with a recomputed digest
-- (the JSON formatting false failure) counted as a NEW failure and spent a
-- retry, and a stale "passed" witness with a new digest could resolve a newer
-- failure. Both now deduplicate by the source id alone; the digest is still
-- validated and recorded as evidence. The live definitions are patched in
-- place (owner and grants unchanged) and the patch must apply exactly.

DO $m48$
DECLARE src text;patched text;
BEGIN
 src:=pg_get_functiondef('forge_agent.retry_event_observe(text,text,uuid,text,text,text,text[],text,text[])'::regprocedure);
 patched:=replace(src,$o1$WHERE(o.owner_id,o.project_alias,o.scope_kind,o.scope_id,o.kind,o.phase_id,o.source_id,o.source_digest,o.observation_kind)=(current_setting('forge.coding_owner',true),p_project,p_scope_kind,p_scope_id,p_kind,phase,p_source_id,p_digest,'failure');$o1$,$n1$WHERE(o.owner_id,o.project_alias,o.scope_kind,o.scope_id,o.kind,o.phase_id,o.source_id,o.observation_kind)=(current_setting('forge.coding_owner',true),p_project,p_scope_kind,p_scope_id,p_kind,phase,p_source_id,'failure') ORDER BY o.created_at LIMIT 1;$n1$);
 IF patched=src THEN RAISE EXCEPTION 'M48 retry_event_observe patch did not apply';END IF;
 EXECUTE patched;
 src:=pg_get_functiondef('forge_agent.retry_event_resolve(text,text,uuid,text,text,text,text)'::regprocedure);
 patched:=replace(replace(src,$o2$ INSERT INTO forge_agent.coding_retry_event_observations(owner_id,project_alias,scope_kind,scope_id,kind,phase_id,source_id,source_digest,event_id,observation_kind)
 VALUES(current_setting('forge.coding_owner',true),p_project,p_scope_kind,p_scope_id,p_kind,witness_phase,p_source_id,p_digest,NULL,'passed') ON CONFLICT DO NOTHING;
 GET DIAGNOSTICS inserted=ROW_COUNT;$o2$,$n2$ IF EXISTS(SELECT FROM forge_agent.coding_retry_event_observations WHERE(owner_id,project_alias,scope_kind,scope_id,kind,phase_id,source_id,observation_kind)=(current_setting('forge.coding_owner',true),p_project,p_scope_kind,p_scope_id,p_kind,witness_phase,p_source_id,'passed')) THEN
  inserted:=0;
 ELSE
 INSERT INTO forge_agent.coding_retry_event_observations(owner_id,project_alias,scope_kind,scope_id,kind,phase_id,source_id,source_digest,event_id,observation_kind)
  VALUES(current_setting('forge.coding_owner',true),p_project,p_scope_kind,p_scope_id,p_kind,witness_phase,p_source_id,p_digest,NULL,'passed') ON CONFLICT DO NOTHING;
  GET DIAGNOSTICS inserted=ROW_COUNT;
 END IF;$n2$),$o3$  SELECT event_id INTO prior_event FROM forge_agent.coding_retry_event_observations WHERE(owner_id,project_alias,scope_kind,scope_id,kind,phase_id,source_id,source_digest,observation_kind)=(current_setting('forge.coding_owner',true),p_project,p_scope_kind,p_scope_id,p_kind,witness_phase,p_source_id,p_digest,'passed');$o3$,$n3$  SELECT event_id INTO prior_event FROM forge_agent.coding_retry_event_observations WHERE(owner_id,project_alias,scope_kind,scope_id,kind,phase_id,source_id,observation_kind)=(current_setting('forge.coding_owner',true),p_project,p_scope_kind,p_scope_id,p_kind,witness_phase,p_source_id,'passed') ORDER BY created_at LIMIT 1;$n3$);
 IF position('inserted:=0;' IN patched)=0 OR position('ORDER BY created_at LIMIT 1;' IN patched)=0 THEN
  RAISE EXCEPTION 'M48 retry_event_resolve patch did not apply';
 END IF;
 EXECUTE patched;
END
$m48$;
