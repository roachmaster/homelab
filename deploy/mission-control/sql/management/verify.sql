\set ON_ERROR_STOP on

BEGIN READ ONLY;

DO $verify$
DECLARE
    relation text;
BEGIN
    FOREACH relation IN ARRAY ARRAY[
        'schema_migrations', 'conversations', 'runs', 'messages',
        'tool_calls', 'tool_results', 'run_events',
        'ide_workspace_history', 'ide_workspace_tabs', 'ide_chats',
        'inference_results', 'gpu_worker_readiness', 'mail_threads', 'mail_messages'
    ] LOOP
        IF pg_catalog.to_regclass('forge_agent.' || relation) IS NULL THEN
            RAISE EXCEPTION 'Missing relation forge_agent.%', relation;
        END IF;
    END LOOP;

    IF NOT EXISTS (
        SELECT 1 FROM forge_agent.schema_migrations
        WHERE version = 1 AND name = 'agent'
    ) THEN
        RAISE EXCEPTION 'Forge agent migration 0001 is missing';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM forge_agent.schema_migrations
        WHERE version = 2 AND name = 'ide_workspace'
    ) THEN
        RAISE EXCEPTION 'Forge IDE workspace migration 0002 is missing';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM forge_agent.schema_migrations
        WHERE version = 3 AND name = 'ide_chat_history'
    ) THEN
        RAISE EXCEPTION 'Forge IDE chat history migration 0003 is missing';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM forge_agent.schema_migrations
        WHERE version = 4 AND name = 'ide_chat_default_expiration'
    ) THEN
        RAISE EXCEPTION 'Forge IDE chat default expiration migration 0004 is missing';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM forge_agent.schema_migrations
        WHERE version = 5 AND name = 'inference_dispatch'
    ) THEN
        RAISE EXCEPTION 'Forge inference dispatch migration 0005 is missing';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM forge_agent.schema_migrations
        WHERE version = 6 AND name = 'inference_model_selection'
    ) THEN
        RAISE EXCEPTION 'Forge inference model selection migration 0006 is missing';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM forge_agent.schema_migrations
        WHERE version = 7 AND name = 'ide_mail'
    ) THEN
        RAISE EXCEPTION 'Forge IDE Mail migration 0007 is missing';
    END IF;

    IF has_schema_privilege('forge_gpu_worker', 'forge_agent', 'USAGE, CREATE')
       OR has_schema_privilege('forge_gpu_worker', 'forge_agent_worker_api', 'CREATE')
       OR NOT has_schema_privilege('forge_gpu_worker', 'forge_agent_worker_api', 'USAGE')
       OR has_database_privilege('forge_gpu_worker', current_database(), 'CREATE, TEMP')
       OR has_function_privilege('forge_gpu_worker', 'forge_agent_worker_api.lock_attempt(uuid,uuid)', 'EXECUTE')
       OR NOT (has_function_privilege('forge_gpu_worker', 'forge_agent_worker_api.advertise(text,boolean)', 'EXECUTE')
           AND has_function_privilege('forge_gpu_worker', 'forge_agent_worker_api.next_model(text[])', 'EXECUTE')
           AND has_function_privilege('forge_gpu_worker', 'forge_agent_worker_api.claim(text)', 'EXECUTE')
           AND has_function_privilege('forge_gpu_worker', 'forge_agent_worker_api.renew(uuid,uuid)', 'EXECUTE')
           AND has_function_privilege('forge_gpu_worker', 'forge_agent_worker_api.publish(uuid,uuid,jsonb)', 'EXECUTE'))
       OR EXISTS (
           SELECT FROM pg_catalog.pg_class rel
           WHERE rel.relnamespace = 'forge_agent'::regnamespace AND rel.relkind IN ('r', 'p')
             AND has_table_privilege('forge_gpu_worker', rel.oid, 'SELECT, INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER')
       )
    THEN
        RAISE EXCEPTION 'Unexpected Forge GPU worker privileges';
    END IF;
END;
$verify$;

SELECT version, name, applied_at
FROM forge_agent.schema_migrations
ORDER BY version;

COMMIT;
