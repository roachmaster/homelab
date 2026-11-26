-- M26 exact source/owner/ACL pins; no source normalization precedes this guard.
DO $owner_continuation_verify$
DECLARE expected record;actual text;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=26 AND name='coding_owner_continuation') THEN
  RAISE EXCEPTION 'Missing owner continuation migration26';END IF;
 FOR expected IN SELECT * FROM(VALUES
  ('forge_agent._coding_owner_continuation_fenced(uuid)','["forge_agent_owner", true, ["search_path=pg_catalog, pg_temp"], ["forge_agent_owner=X/forge_agent_owner"]]'),
  ('forge_agent.coding_schedule_repair(uuid,uuid)','["forge_agent_owner", true, ["search_path=pg_catalog, pg_temp"], ["forge_agent=X/forge_agent_owner", "forge_agent_owner=X/forge_agent_owner"]]'),
  ('forge_agent.owner_continuation_capability()','["forge_agent_owner", true, ["search_path=pg_catalog, pg_temp"], ["forge_agent=X/forge_agent_owner", "forge_agent_owner=X/forge_agent_owner"]]'),
  ('forge_coding_reconciler_api.native_branch_first_replay(uuid,bigint)','["forge_agent_owner", true, ["search_path=pg_catalog, pg_temp"], ["forge_agent_owner=X/forge_agent_owner", "forge_coding_reconciler=X/forge_agent_owner"]]'),
  ('forge_coding_reconciler_api.native_merge_failure(uuid,uuid,jsonb)','["forge_agent_owner", true, ["search_path=pg_catalog, pg_temp"], ["forge_agent_owner=X/forge_agent_owner", "forge_coding_reconciler=X/forge_agent_owner"]]'),
  ('forge_coding_reconciler_api.owner_continuation_capability()','["forge_agent_owner", true, ["search_path=pg_catalog, pg_temp"], ["forge_agent_owner=X/forge_agent_owner", "forge_coding_reconciler=X/forge_agent_owner"]]'),
  ('forge_coding_reconciler_api.owner_continuation_fenced(jsonb)','["forge_agent_owner", true, ["search_path=pg_catalog, pg_temp"], ["forge_agent_owner=X/forge_agent_owner", "forge_coding_reconciler=X/forge_agent_owner"]]'),
  ('forge_coding_reconciler_api.review_finish(uuid,uuid,jsonb)','["forge_agent_owner", true, ["search_path=pg_catalog, pg_temp"], ["forge_agent_owner=X/forge_agent_owner", "forge_coding_reconciler=X/forge_agent_owner"]]')
 )x(signature,shape) LOOP
  SELECT jsonb_build_array(pg_get_userbyid(p.proowner),p.prosecdef,p.proconfig,ARRAY(SELECT unnest(p.proacl::text[]) ORDER BY 1))::text
  INTO actual FROM pg_proc p WHERE p.oid=expected.signature::regprocedure;
  IF actual IS DISTINCT FROM expected.shape THEN RAISE EXCEPTION 'Owner continuation function drift: %',expected.signature;END IF;
 END LOOP;
END;
$owner_continuation_verify$;
