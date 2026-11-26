-- Existing service principals only; no account or credential mutation.
GRANT USAGE ON SCHEMA forge_external_workflow,forge_external_workflow_api TO forge_agent;
GRANT SELECT,INSERT,UPDATE ON forge_external_workflow.workflows,forge_external_workflow.tasks,forge_external_workflow.contexts,forge_external_workflow.reviews,forge_external_workflow.actions TO forge_agent;
GRANT SELECT ON forge_external_workflow.delivery_grants TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_external_workflow_api.enqueue_v2(text,text,uuid),forge_external_workflow_api.is_internal(text,text,uuid),forge_external_workflow_api.service_scope(text,text,text,boolean) TO forge_agent;
GRANT USAGE ON SCHEMA forge_external_workflow_worker_api TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_external_workflow_worker_api.read_context(uuid,uuid) TO forge_coding_worker;
GRANT USAGE ON SCHEMA forge_external_workflow_delivery_api TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_external_workflow_delivery_api.claim(text,text),forge_external_workflow_delivery_api.renew(uuid,integer,uuid),forge_external_workflow_delivery_api.checkpoint(uuid,integer,uuid,jsonb) TO forge_coding_reconciler;
DO $grant_external_plan_override$
BEGIN
 IF to_regclass('forge_external_workflow.plan_approve_overrides') IS NOT NULL THEN
  GRANT SELECT,INSERT ON forge_external_workflow.plan_approve_overrides TO forge_agent;
 END IF;
END;
$grant_external_plan_override$;

-- M41 cutover: revoke the direct forge_agent table writes above; the
-- proxy is the only writer from here on.
DO $grant_external_workflows_cutover$
BEGIN
 IF EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=52 AND name='pg_proxy_cutover') THEN
  REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON ALL TABLES IN SCHEMA forge_external_workflow FROM forge_agent, forge_coding_worker, forge_coding_reconciler;
  IF EXISTS(SELECT FROM pg_roles WHERE rolname='muse_events') THEN
   REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON ALL TABLES IN SCHEMA forge_external_workflow FROM muse_events;
  END IF;
 END IF;
END;
$grant_external_workflows_cutover$;
