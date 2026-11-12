\set ON_ERROR_STOP on

BEGIN;
GRANT USAGE ON SCHEMA forge_agent TO forge_agent;
GRANT SELECT, INSERT, UPDATE, DELETE ON
    forge_agent.conversations, forge_agent.runs, forge_agent.messages,
    forge_agent.tool_calls, forge_agent.tool_results, forge_agent.run_events
TO forge_agent;
COMMIT;
