-- Existing account only; no passwords, role changes or table grants.
DO $external_grants$
BEGIN
 GRANT USAGE ON SCHEMA forge_external_agent_api TO forge_agent;
 GRANT EXECUTE ON FUNCTION forge_external_agent_api.enqueue(text,text,uuid) TO forge_agent;
 GRANT EXECUTE ON FUNCTION forge_external_agent_api.catalogue(text) TO forge_agent;
 GRANT EXECUTE ON FUNCTION forge_external_agent_api.reconcile(text) TO forge_agent;
 GRANT EXECUTE ON FUNCTION forge_external_agent_api.artifact_by_task(text,text,uuid) TO forge_agent;
 IF EXISTS(SELECT FROM pg_roles WHERE rolname='muse_events') THEN
 GRANT USAGE ON SCHEMA forge_external_agent_api TO muse_events;
 GRANT EXECUTE ON FUNCTION forge_external_agent_api.capabilities() TO muse_events;
 GRANT EXECUTE ON FUNCTION forge_external_agent_api.advertise(text,text,text) TO muse_events;
 GRANT EXECUTE ON FUNCTION forge_external_agent_api.claim(text,text) TO muse_events;
 GRANT EXECUTE ON FUNCTION forge_external_agent_api.renew(uuid,integer,uuid) TO muse_events;
 GRANT EXECUTE ON FUNCTION forge_external_agent_api.progress(uuid,integer,uuid,text) TO muse_events;
 GRANT EXECUTE ON FUNCTION forge_external_agent_api.finish(uuid,integer,uuid,text,text) TO muse_events;
 GRANT EXECUTE ON FUNCTION forge_external_agent_api.release(uuid,integer,uuid) TO muse_events;
 GRANT EXECUTE ON FUNCTION forge_external_agent_api.artifact(uuid) TO muse_events;
 END IF;
END;
$external_grants$;
