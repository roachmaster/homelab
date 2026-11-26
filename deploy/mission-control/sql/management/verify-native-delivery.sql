-- M25 exact source/owner/ACL pins; run in the full strict verifier closure.
DO $native_delivery_verify$
DECLARE expected record;actual text;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=25 AND name='native_delivery_repair') THEN
  RAISE EXCEPTION 'Missing native delivery repair migration25';
 END IF;
 FOR expected IN SELECT * FROM(VALUES
  ('forge_coding_reconciler_api.native_merge_failure(uuid,uuid,jsonb)','["forge_agent_owner", true, ["search_path=pg_catalog, pg_temp"], ["forge_agent_owner=X/forge_agent_owner", "forge_coding_reconciler=X/forge_agent_owner"]]'),
  ('forge_coding_reconciler_api.native_branch_first_replay(uuid,bigint)','["forge_agent_owner", true, ["search_path=pg_catalog, pg_temp"], ["forge_agent_owner=X/forge_agent_owner", "forge_coding_reconciler=X/forge_agent_owner"]]'),
  ('forge_coding_reconciler_api.native_branch_published(uuid,uuid,jsonb)','["forge_agent_owner", true, ["search_path=pg_catalog, pg_temp"], ["forge_agent_owner=X/forge_agent_owner", "forge_coding_reconciler=X/forge_agent_owner"]]'),
  ('forge_coding_worker_api.read_native_delivery_context(uuid,uuid)','["forge_agent_owner", true, ["search_path=pg_catalog, pg_temp"], ["forge_agent_owner=X/forge_agent_owner", "forge_coding_worker=X/forge_agent_owner"]]')
 )x(signature,shape) LOOP
  SELECT jsonb_build_array(pg_get_userbyid(p.proowner),p.prosecdef,p.proconfig,ARRAY(SELECT unnest(p.proacl::text[]) ORDER BY 1))::text
  INTO actual FROM pg_proc p WHERE p.oid=expected.signature::regprocedure;
  IF actual IS DISTINCT FROM expected.shape THEN RAISE EXCEPTION 'Native delivery function drift: %',expected.signature;END IF;
 END LOOP;
END;
$native_delivery_verify$;
