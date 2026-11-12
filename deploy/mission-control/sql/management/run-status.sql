\set ON_ERROR_STOP on

BEGIN READ ONLY;

SELECT status, model_id, count(*) AS runs, min(created_at) AS oldest_run
FROM forge_agent.runs
GROUP BY status, model_id
ORDER BY status, model_id;

SELECT status, count(*) AS tool_calls, min(created_at) AS oldest_call
FROM forge_agent.tool_calls
WHERE status IN ('pending', 'running', 'outcome_unknown')
GROUP BY status
ORDER BY status;

COMMIT;
