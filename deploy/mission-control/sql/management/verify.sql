\set ON_ERROR_STOP on

BEGIN READ ONLY;

DO $verify$
DECLARE
    relation text;
BEGIN
    FOREACH relation IN ARRAY ARRAY[
        'schema_migrations', 'conversations', 'runs', 'messages',
        'tool_calls', 'tool_results', 'run_events'
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
END;
$verify$;

SELECT version, name, applied_at
FROM forge_agent.schema_migrations
ORDER BY version;

COMMIT;
