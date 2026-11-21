\set ON_ERROR_STOP on

BEGIN;
GRANT USAGE ON SCHEMA forge_agent TO forge_agent;
GRANT SELECT, INSERT, UPDATE, DELETE ON
    forge_agent.conversations, forge_agent.runs, forge_agent.messages,
    forge_agent.tool_calls, forge_agent.tool_results, forge_agent.run_events,
    forge_agent.ide_workspace_tabs
TO forge_agent;
GRANT SELECT, INSERT, UPDATE ON forge_agent.ide_workspace_history, forge_agent.ide_chats TO forge_agent;
GRANT SELECT, INSERT, UPDATE ON forge_agent.mail_threads TO forge_agent;
GRANT SELECT, INSERT ON forge_agent.mail_messages TO forge_agent;
GRANT SELECT, INSERT, UPDATE ON forge_agent.coding_tasks TO forge_agent;
GRANT SELECT, INSERT ON forge_agent.coding_events, forge_agent.coding_control_receipts,
    forge_agent.coding_plan_decisions TO forge_agent;
GRANT SELECT ON forge_agent.coding_publications TO forge_agent;
GRANT SELECT ON
    forge_agent.coding_worker_readiness,
    forge_agent.coding_attempts,
    forge_agent.coding_result_chunks,
    forge_agent.coding_reconciliation_receipts,
    forge_agent.coding_provider_usage
TO forge_agent;
GRANT SELECT, INSERT ON forge_agent.coding_usage_refresh_requests TO forge_agent;
GRANT SELECT, DELETE ON forge_agent.inference_results TO forge_agent;
GRANT SELECT ON forge_agent.gpu_worker_readiness TO forge_agent;
GRANT USAGE ON SCHEMA forge_agent_worker_api TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION
    forge_agent_worker_api.advertise(text, boolean),
    forge_agent_worker_api.next_model(text[]),
    forge_agent_worker_api.claim(text),
    forge_agent_worker_api.renew(uuid, uuid),
    forge_agent_worker_api.publish(uuid, uuid, jsonb)
TO forge_gpu_worker;
GRANT USAGE ON SCHEMA forge_coding_worker_api TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION
    forge_coding_worker_api.advertise(text, uuid, jsonb, jsonb, text),
    forge_coding_worker_api.claim(text, uuid, text[]),
    forge_coding_worker_api.renew(uuid, uuid),
    forge_coding_worker_api.checkpoint(uuid, uuid, text),
    forge_coding_worker_api.report_progress(uuid, uuid, integer, text, text),
    forge_coding_worker_api.read_plan(uuid, uuid),
    forge_coding_worker_api.queue_publication(uuid, uuid, uuid),
    forge_coding_worker_api.append_result(uuid, uuid, integer, text),
    forge_coding_worker_api.publish(uuid, uuid, text, jsonb, jsonb, text, jsonb),
    forge_coding_worker_api.record_stop(uuid, uuid, jsonb, text),
    forge_coding_worker_api.cancellation_requested(uuid, uuid),
    forge_coding_worker_api.begin_usage_poll(text),
    forge_coding_worker_api.next_refresh(),
    forge_coding_worker_api.record_usage(text, bigint, bigint, jsonb),
    forge_coding_worker_api.fail_usage_poll(text, bigint, bigint, text)
TO forge_coding_worker;
GRANT USAGE ON SCHEMA forge_coding_reconciler_api TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION
    forge_coding_reconciler_api.expire_leases(),
    forge_coding_reconciler_api.pending_reconciliations(integer),
    forge_coding_reconciler_api.reconcile(uuid, uuid, text, jsonb),
    forge_coding_reconciler_api.publication_claim(text),
    forge_coding_reconciler_api.publication_renew(uuid, uuid),
    forge_coding_reconciler_api.publication_finish(uuid, uuid, text, text, text, text, text)
TO forge_coding_reconciler;
COMMIT;
-- M15: ready owner service and narrowly scoped native parent routines only.
BEGIN;
GRANT SELECT,INSERT ON forge_agent.coding_feedback TO forge_agent;
GRANT SELECT,INSERT,UPDATE ON forge_agent.coding_reviews,forge_agent.coding_review_policy TO forge_agent;
GRANT SELECT,INSERT ON forge_agent.coding_review_budget TO forge_agent;
GRANT SELECT ON forge_agent.coding_hosted_checks TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_agent.coding_make_feedback(uuid,uuid,text,text,jsonb),
 forge_agent.coding_schedule_repair(uuid,uuid),forge_agent.coding_queue_approved_review(uuid) TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_coding_worker_api.read_feedback(uuid,uuid),forge_coding_worker_api.queue_review(uuid,uuid,uuid) TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_coding_reconciler_api.review_claim(text),forge_coding_reconciler_api.review_renew(uuid,uuid),
 forge_coding_reconciler_api.review_finish(uuid,uuid,jsonb),forge_coding_reconciler_api.record_hosted_checks(uuid,text,jsonb) TO forge_coding_reconciler;
COMMIT;

-- M16: coordinator tables stay unavailable to native and publication processes.
BEGIN;
GRANT EXECUTE ON FUNCTION forge_agent.coding_workflow_role_granted(uuid,text,text) TO forge_agent;
GRANT SELECT,INSERT,UPDATE ON forge_agent.coding_workflows,forge_agent.coding_workflow_reviews,forge_agent.coding_workflow_actions TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_coding_worker_api.project_revisions() TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_coding_worker_api.read_review_target(uuid,uuid),forge_coding_worker_api.read_workflow_feedback(uuid,uuid) TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_coding_reconciler_api.merge_claim(text),forge_coding_reconciler_api.merge_renew(uuid,uuid),forge_coding_reconciler_api.merge_intent(uuid,uuid,jsonb),forge_coding_reconciler_api.merge_finish(uuid,uuid,text,jsonb) TO forge_coding_reconciler;
COMMIT;

-- M20: scoped external DB API only.
\ir grant-external-agent.sql

-- M21: scoped trace projection; keep historical fixtures below this marker.
SELECT EXISTS(SELECT 1 FROM forge_agent.schema_migrations WHERE version=21) AS has_task_events
\gset
\if :has_task_events
\ir grant-task-events.sql
\endif

-- M22: isolated external workflow coordination, native review and delivery ports.
SELECT EXISTS(SELECT 1 FROM forge_agent.schema_migrations WHERE version=22) AS has_external_workflows
\gset
\if :has_external_workflows
\ir grant-external-workflows.sql
\endif

-- M24: narrow native Jenkins identity/status ports; no direct table access.
SELECT EXISTS(SELECT 1 FROM forge_agent.schema_migrations WHERE version=24) AS has_native_jenkins
\gset
\if :has_native_jenkins
GRANT EXECUTE ON FUNCTION forge_coding_reconciler_api.native_ci_status(uuid,text,uuid,uuid),
 forge_coding_reconciler_api.native_ci_context(uuid,uuid,text,uuid,uuid) TO forge_coding_reconciler;
\endif
