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
GRANT SELECT, INSERT ON forge_agent.coding_events, forge_agent.coding_control_receipts TO forge_agent;
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
    forge_coding_reconciler_api.reconcile(uuid, uuid, text, jsonb)
TO forge_coding_reconciler;
COMMIT;
