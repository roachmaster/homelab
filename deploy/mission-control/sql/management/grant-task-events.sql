-- Scoped projection only; no new external writer or native execution rights.
GRANT EXECUTE ON FUNCTION forge_external_agent_api.event_binding(text,text,uuid)
    TO forge_agent;
