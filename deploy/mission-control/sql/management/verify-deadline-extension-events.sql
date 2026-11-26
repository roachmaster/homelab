-- M47 deadline extension events: renew_deadline owner, security and privileges. Read only.
DO $deadline_extension_verify$
DECLARE actual jsonb;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=47) THEN RETURN;END IF;
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=47 AND name='deadline_extension_events') THEN RAISE EXCEPTION 'Deadline extension migration identity drift';END IF;
 SELECT to_jsonb(jsonb_build_array(pg_get_userbyid(p.proowner),p.prosecdef,p.proconfig,ARRAY(SELECT unnest(p.proacl::text[]) ORDER BY 1))) INTO actual FROM pg_proc p
  WHERE p.oid=to_regprocedure('forge_coding_worker_api.renew_deadline(uuid,uuid,text)');
 IF actual IS DISTINCT FROM '["forge_agent_owner", true, ["search_path=pg_catalog, pg_temp"], ["forge_agent_owner=X/forge_agent_owner", "forge_coding_worker=X/forge_agent_owner"]]'::jsonb THEN
  RAISE EXCEPTION 'Deadline extension function drift';END IF;
END;
$deadline_extension_verify$;
