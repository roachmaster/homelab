-- M49 gate contract names: gate checks accept a digest or a named contract. Read only.
DO $gate_contract_names_verify$
DECLARE fn text;gate_pattern text:='forge-sdlc-jenkins-v[1-3]:(main|detached|branch/';
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=49) THEN RETURN;END IF;
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=49 AND name='gate_contract_names') THEN RAISE EXCEPTION 'Gate contract names migration identity drift';END IF;
 IF EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=56 AND name='native_ci_verifier') THEN gate_pattern:='forge-sdlc-jenkins-v[1-4]:(main|detached|branch/';END IF;
 FOREACH fn IN ARRAY ARRAY['forge_agent.delivery_request_valid(jsonb,uuid,uuid,text,text,text,uuid)',
   'forge_coding_reconciler_api.post_merge_checkpoint(uuid,integer,uuid,jsonb)',
   'forge_coding_reconciler_api.review_finish(uuid,uuid,jsonb)',
   'forge_coding_reconciler_api.native_merge_failure(uuid,uuid,jsonb)'] LOOP
  IF position(gate_pattern IN (SELECT prosrc FROM pg_proc WHERE oid=fn::regprocedure))=0 THEN
   RAISE EXCEPTION 'Gate contract function drift: %',fn;
  END IF;
 END LOOP;
 IF (SELECT count(*) FROM pg_constraint WHERE conname IN('coding_reviews_gate_manifest_sha256_check','coding_workflow_reviews_gate_manifest_sha256_check')
     AND position(gate_pattern IN pg_get_constraintdef(oid))>0)<>2 THEN
  RAISE EXCEPTION 'Gate contract constraint drift';
 END IF;
END;
$gate_contract_names_verify$;
