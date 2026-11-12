\set ON_ERROR_STOP on

BEGIN;

DO $test$
DECLARE
    conversation uuid := gen_random_uuid();
    other_conversation uuid := gen_random_uuid();
    run uuid := gen_random_uuid();
    call text := 'call-example';
BEGIN
    INSERT INTO forge_agent.conversations (id, owner_id)
    VALUES (conversation, 'fixture-owner'), (other_conversation, 'fixture-owner');

    INSERT INTO forge_agent.runs (id, conversation_id, request_id, model_id)
    VALUES (run, conversation, 'request-example', 'gpt-oss-20b');

    BEGIN
        INSERT INTO forge_agent.runs (id, conversation_id, request_id, model_id)
        VALUES (gen_random_uuid(), conversation, 'request-example', 'gpt-oss-20b');
        RAISE EXCEPTION 'Accepted a duplicate request ID for the conversation';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    INSERT INTO forge_agent.messages (conversation_id, position, run_id, role, content)
    VALUES (conversation, 1, run, 'user', 'Count the lines in README.md.');

    BEGIN
        INSERT INTO forge_agent.messages (conversation_id, position, run_id, role, content)
        VALUES (other_conversation, 1, run, 'user', 'Wrong conversation');
        RAISE EXCEPTION 'Accepted a message for a different conversation';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;

    INSERT INTO forge_agent.tool_calls (run_id, call_id, tool_name, arguments, status)
    VALUES (run, call, 'bash', '{"command": "wc -l README.md"}', 'running');

    INSERT INTO forge_agent.messages (conversation_id, position, run_id, role, tool_call_id)
    VALUES (conversation, 2, run, 'assistant', call);

    BEGIN
        INSERT INTO forge_agent.messages (conversation_id, position, run_id, role, tool_result_call_id)
        VALUES (conversation, 3, run, 'tool', call);
        RAISE EXCEPTION 'Accepted a tool message before its result was recorded';
    EXCEPTION WHEN foreign_key_violation THEN
        NULL;
    END;

    INSERT INTO forge_agent.tool_results (run_id, call_id, outcome, exit_code, stdout)
    VALUES (run, call, 'exited', 0, '12 README.md');

    BEGIN
        INSERT INTO forge_agent.tool_results (run_id, call_id, outcome, exit_code)
        VALUES (run, call, 'exited', 0);
        RAISE EXCEPTION 'Accepted a duplicate tool result';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    INSERT INTO forge_agent.messages (conversation_id, position, run_id, role, tool_result_call_id)
    VALUES (conversation, 3, run, 'tool', call);

    INSERT INTO forge_agent.messages (conversation_id, position, run_id, role, content)
    VALUES (conversation, 4, run, 'assistant', 'README.md has 12 lines.');

    INSERT INTO forge_agent.run_events (run_id, sequence, event_type)
    VALUES (run, 1, 'queued'), (run, 2, 'tool_finished'), (run, 3, 'completed');

    IF (SELECT count(*) FROM forge_agent.messages WHERE conversation_id = conversation) <> 4 THEN
        RAISE EXCEPTION 'Expected four transcript entries';
    END IF;
END;
$test$;

ROLLBACK;
