-- M24 exact narrow source/activity APIs; no source or workflow authority changes.
DO $native_jenkins_verify$
DECLARE expected record;actual text;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=24 AND name='native_jenkins_context') THEN
  RAISE EXCEPTION 'Missing native Jenkins context migration24';
 END IF;
 FOR expected IN SELECT * FROM(VALUES
  ('forge_coding_reconciler_api.native_ci_context(uuid,uuid,text,uuid,uuid)','["forge_agent_owner", true, ["search_path=pg_catalog, pg_temp"], ["forge_agent_owner=X/forge_agent_owner", "forge_coding_reconciler=X/forge_agent_owner"]]'),
  ('forge_coding_reconciler_api.native_ci_status(uuid,text,uuid,uuid)','["forge_agent_owner", true, ["search_path=pg_catalog, pg_temp"], ["forge_agent_owner=X/forge_agent_owner", "forge_coding_reconciler=X/forge_agent_owner"]]')
 )x(signature,shape) LOOP
  SELECT jsonb_build_array(pg_get_userbyid(p.proowner),p.prosecdef,p.proconfig,ARRAY(SELECT unnest(p.proacl::text[]) ORDER BY 1))::text
  INTO actual FROM pg_proc p WHERE p.oid=expected.signature::regprocedure;
  IF actual IS DISTINCT FROM expected.shape THEN RAISE EXCEPTION 'Native Jenkins context function drift: %',expected.signature;END IF;
 END LOOP;
END;
$native_jenkins_verify$;
