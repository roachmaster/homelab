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
COMMIT;
