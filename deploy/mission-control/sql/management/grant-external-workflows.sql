-- Existing service principals only; no account or credential mutation.
GRANT USAGE ON SCHEMA forge_external_workflow,forge_external_workflow_api TO forge_agent;
GRANT SELECT,INSERT,UPDATE ON forge_external_workflow.workflows,forge_external_workflow.tasks,forge_external_workflow.contexts,forge_external_workflow.reviews,forge_external_workflow.actions TO forge_agent;
GRANT SELECT ON forge_external_workflow.delivery_grants TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_external_workflow_api.enqueue_v2(text,text,uuid),forge_external_workflow_api.is_internal(text,text,uuid),forge_external_workflow_api.service_scope(text,text,text,boolean) TO forge_agent;
GRANT USAGE ON SCHEMA forge_external_workflow_worker_api TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_external_workflow_worker_api.read_context(uuid,uuid) TO forge_coding_worker;
GRANT USAGE ON SCHEMA forge_external_workflow_delivery_api TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_external_workflow_delivery_api.claim(text,text),forge_external_workflow_delivery_api.renew(uuid,integer,uuid),forge_external_workflow_delivery_api.checkpoint(uuid,integer,uuid,jsonb) TO forge_coding_reconciler;
