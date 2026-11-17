CREATE FUNCTION forge_agent_worker_api.next_model(p_models text[])
RETURNS text
LANGUAGE sql SECURITY DEFINER STRICT SET search_path = pg_catalog, pg_temp
AS $fn$
    -- This is a scheduling hint. The existing claim function checks eligibility
    -- again and acquires the chat and run locks before issuing an attempt.
    SELECT r.model_id
    FROM forge_agent.runs r
    JOIN forge_agent.conversations c ON c.id = r.conversation_id
    LEFT JOIN forge_agent.ide_chats h
        ON (h.owner_id, h.id) = (r.ide_chat_owner_id, r.ide_chat_id)
    WHERE c.owner_id = 'mission-control' AND r.distributed AND r.status = 'queued'
      AND r.model_id IN ('gpt-oss-20b', 'qwen2.5-coder-7b') AND r.model_id = ANY(p_models)
      AND r.attempt_no < 3 AND r.next_attempt_at <= clock_timestamp()
      AND r.run_deadline > clock_timestamp()
      AND (r.ide_chat_id IS NULL OR (h.id IS NOT NULL AND h.removed_at IS NULL
          AND (h.expires_at IS NULL OR h.expires_at > clock_timestamp())))
    ORDER BY r.created_at, r.id
    LIMIT 1;
$fn$;

REVOKE ALL ON FUNCTION forge_agent_worker_api.next_model(text[]) FROM PUBLIC;
