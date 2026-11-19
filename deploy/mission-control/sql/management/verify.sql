\set ON_ERROR_STOP on

BEGIN READ ONLY;
SET LOCAL search_path = pg_catalog;

DO $verify$
DECLARE
    relation text;
    att_archive_reason smallint;
    att_archive_base_revision smallint;
    expected record;
    actual_expression text;
    actual_tokens text[];
    expected_tokens text[];
    matching_checks bigint;
    check_ready boolean;
    -- Preserve quoted strings/identifiers and every non-whitespace token.
    token_pattern constant text := $tokens$([Ee]?'(?:[^']|'')*'|"(?:[^"]|"")*"|[[:alpha:]_][[:alnum:]_$]*|[[:digit:]]+|::|<=|>=|<>|!=|[^[:space:]])$tokens$;
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

    IF NOT EXISTS (
        SELECT 1 FROM forge_agent.schema_migrations
        WHERE version = 8 AND name = 'mail_archive'
    ) THEN
        RAISE EXCEPTION 'Forge IDE Mail archive migration 0008 is missing';
    END IF;

    SELECT attnum INTO att_archive_reason
    FROM pg_catalog.pg_attribute
    WHERE attrelid = 'forge_agent.mail_threads'::regclass
      AND attname = 'archive_reason'
      AND NOT attisdropped
      AND atttypid = 'text'::regtype
      AND NOT attnotnull;
    IF att_archive_reason IS NULL THEN
        RAISE EXCEPTION 'forge_agent.mail_threads.archive_reason must be nullable text';
    END IF;

    SELECT attnum INTO att_archive_base_revision
    FROM pg_catalog.pg_attribute
    WHERE attrelid = 'forge_agent.mail_threads'::regclass
      AND attname = 'archive_base_revision'
      AND NOT attisdropped
      AND atttypid = 'bigint'::regtype
      AND NOT attnotnull;
    IF att_archive_base_revision IS NULL THEN
        RAISE EXCEPTION 'forge_agent.mail_threads.archive_base_revision must be nullable bigint';
    END IF;

    -- PostgreSQL 16 deparses these immutable migration 0007/0008 expressions.
    -- Compare tokens, ignoring only whitespace outside quotes. Unexpected but
    -- equivalent DDL fails closed and requires review when migrations or the
    -- supported PostgreSQL major version change. Never execute catalog code:
    -- setup runs this verifier as an administrator. Boundary behavior is tested
    -- against actual constraints only in the disposable PostgreSQL fixture.
    FOR expected IN
        SELECT * FROM (VALUES
            (ARRAY['archive_reason'], NULL::text,
             $expr$(archive_reason = ANY (ARRAY['context_limit'::text, 'manual'::text]))$expr$),
            (ARRAY['archive_base_revision'], NULL::text,
             $expr$((archive_base_revision > 0) AND (archive_base_revision <= '9007199254740991'::bigint))$expr$),
            (ARRAY['revision'], NULL::text,
             $expr$((revision >= 0) AND (revision <= '9007199254740991'::bigint))$expr$),
            (ARRAY['archive_base_revision', 'archive_reason', 'archived_at', 'revision'], 'mail_archive_reason_check',
             $expr$((((archived_at IS NULL) AND (archive_reason IS NULL) AND (archive_base_revision IS NULL)) OR ((archived_at IS NOT NULL) AND (archive_reason = 'context_limit'::text) AND (archive_base_revision IS NULL)) OR ((archived_at IS NOT NULL) AND (archive_reason = 'manual'::text) AND (archive_base_revision IS NOT NULL) AND (archive_base_revision = (revision - 1)))) IS TRUE)$expr$)
        ) AS checks(columns, required_name, expression)
    LOOP
        SELECT count(*), min(pg_catalog.pg_get_expr(c.conbin, c.conrelid)),
               bool_and(c.convalidated AND
                   (expected.required_name IS NULL OR c.conname = expected.required_name))
        INTO matching_checks, actual_expression, check_ready
        FROM pg_catalog.pg_constraint c
        WHERE c.conrelid = 'forge_agent.mail_threads'::regclass
          AND c.contype = 'c'
          AND ARRAY(
              SELECT a.attname::text
              FROM pg_catalog.pg_attribute a
              WHERE a.attrelid = c.conrelid AND a.attnum = ANY(c.conkey)
              ORDER BY a.attname::text COLLATE "C"
          ) = expected.columns;

        IF matching_checks <> 1 OR check_ready IS DISTINCT FROM true THEN
            RAISE EXCEPTION 'Missing or unexpected validated archive CHECK on forge_agent.mail_threads columns % (required name %)',
                expected.columns, coalesce(expected.required_name, 'any');
        END IF;

        actual_tokens := ARRAY(
            SELECT token.parts[1]
            FROM pg_catalog.regexp_matches(actual_expression, token_pattern, 'g')
                WITH ORDINALITY AS token(parts, position)
            ORDER BY token.position
        );
        expected_tokens := ARRAY(
            SELECT token.parts[1]
            FROM pg_catalog.regexp_matches(expected.expression, token_pattern, 'g')
                WITH ORDINALITY AS token(parts, position)
            ORDER BY token.position
        );
        IF actual_tokens IS DISTINCT FROM expected_tokens THEN
            RAISE EXCEPTION 'Unexpected archive CHECK expression on forge_agent.mail_threads columns %',
                expected.columns;
        END IF;
    END LOOP;

    IF (SELECT count(*) FROM pg_catalog.pg_constraint
        WHERE conrelid = 'forge_agent.mail_threads'::regclass AND contype = 'c'
          AND conkey && ARRAY[att_archive_reason, att_archive_base_revision]) <> 3
    THEN
        RAISE EXCEPTION 'Unexpected archive CHECK constraints on forge_agent.mail_threads';
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
