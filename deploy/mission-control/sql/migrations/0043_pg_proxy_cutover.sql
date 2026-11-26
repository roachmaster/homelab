-- M43: Postgres proxy cutover. The proxy is now the only writer.
-- Revokes the muse_events mutating EXECUTE and the direct table
-- writes for the caller roles and muse_events. Preserves SELECT,
-- function EXECUTE, schema USAGE, and the muse_events read-only
-- path (capabilities, artifact, SELECT). The grant files are
-- stage-aware on this migration: re-running them after cutover
-- revokes (does not re-grant) the direct writes.

-- muse_events loses the mutating external EXECUTE (keeps the
-- read-only capabilities and artifact functions). The role is
-- created manually (it holds a SCRAM password, so provision.sql
-- cannot create it); a fresh install that has not created it yet
-- must still apply this migration, so the revokes are conditional
-- on the role existing. The post-cutover verifiers fail loudly if
-- muse_events later appears holding a mutating EXECUTE.
DO $m43_muse_events$
BEGIN
 IF to_regrole('muse_events') IS NOT NULL THEN
  REVOKE EXECUTE ON FUNCTION forge_external_agent_api.advertise(text,text,text) FROM muse_events;
  REVOKE EXECUTE ON FUNCTION forge_external_agent_api.claim(text,text) FROM muse_events;
  REVOKE EXECUTE ON FUNCTION forge_external_agent_api.renew(uuid,integer,uuid) FROM muse_events;
  REVOKE EXECUTE ON FUNCTION forge_external_agent_api.progress(uuid,integer,uuid,text) FROM muse_events;
  REVOKE EXECUTE ON FUNCTION forge_external_agent_api.finish(uuid,integer,uuid,text,text) FROM muse_events;
  REVOKE EXECUTE ON FUNCTION forge_external_agent_api.release(uuid,integer,uuid) FROM muse_events;
  REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON ALL TABLES IN SCHEMA forge_agent FROM muse_events;
  REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON ALL TABLES IN SCHEMA forge_external_workflow FROM muse_events;
  REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON ALL TABLES IN SCHEMA forge_external_agent FROM muse_events;
 END IF;
END
$m43_muse_events$;

-- Direct table writes revoked for the caller roles.
-- SELECT, function EXECUTE, and schema USAGE are preserved.
REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON ALL TABLES IN SCHEMA forge_agent FROM forge_agent, forge_coding_worker, forge_coding_reconciler;
REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON ALL TABLES IN SCHEMA forge_external_workflow FROM forge_agent, forge_coding_worker, forge_coding_reconciler;
REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON ALL TABLES IN SCHEMA forge_external_agent FROM forge_agent, forge_coding_worker, forge_coding_reconciler;
