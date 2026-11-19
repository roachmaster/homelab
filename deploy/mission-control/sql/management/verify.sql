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
    actual_index oid;
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
        'inference_results', 'gpu_worker_readiness', 'mail_threads', 'mail_messages',
        'coding_tasks', 'coding_events', 'coding_control_receipts'
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

    IF NOT EXISTS (
        SELECT 1 FROM forge_agent.schema_migrations
        WHERE version = 9 AND name = 'coding_admission'
    ) THEN
        RAISE EXCEPTION 'Forge coding admission migration 0009 is missing';
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

    -- Pin PostgreSQL 16 deparsed definitions without executing stored code.
    -- Whitespace outside quoted values is ignored; other changes fail closed.
    FOR expected IN
        SELECT * FROM (VALUES
            ('coding_tasks_by_project_created', $def$CREATE INDEX coding_tasks_by_project_created ON forge_agent.coding_tasks USING btree (owner_id, project_alias, created_at, id)$def$),
            ('coding_tasks_by_project_status_created', $def$CREATE INDEX coding_tasks_by_project_status_created ON forge_agent.coding_tasks USING btree (owner_id, project_alias, status, created_at, id)$def$)
        ) AS indexes(relname, definition)
    LOOP
        SELECT pg_catalog.pg_get_indexdef(i.indexrelid, 0, false)
        INTO actual_expression
        FROM pg_catalog.pg_index i
        WHERE i.indexrelid = pg_catalog.to_regclass('forge_agent.' || expected.relname)
          AND i.indrelid = 'forge_agent.coding_tasks'::regclass
          AND i.indisvalid AND i.indisready AND i.indislive;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'Missing or invalid index forge_agent.%', expected.relname;
        END IF;
        actual_tokens := ARRAY(
            SELECT token.parts[1]
            FROM pg_catalog.regexp_matches(actual_expression, token_pattern, 'g')
                WITH ORDINALITY AS token(parts, position)
            ORDER BY token.position
        );
        expected_tokens := ARRAY(
            SELECT token.parts[1]
            FROM pg_catalog.regexp_matches(expected.definition, token_pattern, 'g')
                WITH ORDINALITY AS token(parts, position)
            ORDER BY token.position
        );
        IF actual_tokens IS DISTINCT FROM expected_tokens THEN
            RAISE EXCEPTION 'Unexpected index definition forge_agent.%', expected.relname;
        END IF;
    END LOOP;

    FOR expected IN
        SELECT * FROM (VALUES
            ('coding_tasks', 'coding_tasks_pkey', 'p', $def$PRIMARY KEY (owner_id, project_alias, id)$def$),
            ('coding_tasks', 'coding_tasks_id_unique', 'u', $def$UNIQUE (id)$def$),
            ('coding_tasks', 'coding_tasks_idempotency_unique', 'u', $def$UNIQUE (owner_id, project_alias, idempotency_key)$def$),
            ('coding_tasks', 'coding_tasks_owner_id_check', 'c', $def$CHECK ((((octet_length(owner_id) >= 1)
                 AND (octet_length(owner_id) <= 128))
                 AND (btrim(owner_id) <> ''::text)
                 AND (owner_id ~ '^[ -~]+$'::text)))$def$),
            ('coding_tasks', 'coding_tasks_project_alias_check', 'c', $def$CHECK ((project_alias ~ '^[A-Za-z0-9_.-]{1,64}$'::text))$def$),
            ('coding_tasks', 'coding_tasks_idempotency_key_check', 'c', $def$CHECK ((idempotency_key ~ '^[A-Za-z0-9_.-]{1,128}$'::text))$def$),
            ('coding_tasks', 'coding_tasks_intent_digest_check', 'c', $def$CHECK ((intent_digest ~ '^[0-9a-f]{64}$'::text))$def$),
            ('coding_tasks', 'coding_tasks_base_revision_check', 'c', $def$CHECK ((base_revision ~ '^[0-9a-f]{40}$'::text))$def$),
            ('coding_tasks', 'coding_tasks_mode_check', 'c', $def$CHECK ((mode = ANY (ARRAY['plan'::text, 'review'::text, 'implement'::text])))$def$),
            ('coding_tasks', 'coding_tasks_agent_profile_check', 'c', $def$CHECK ((agent_profile ~ '^[A-Za-z0-9_.-]{1,64}$'::text))$def$),
            ('coding_tasks', 'coding_tasks_prompt_check', 'c', E'CHECK ((((octet_length(prompt) >= 1) AND (octet_length(prompt) <= 8192)) AND (btrim(prompt, '' \t\r\n''::text) <> ''''::text)))'),
            ('coding_tasks', 'coding_tasks_acceptance_criteria_check', 'c', E'CHECK ((((octet_length(acceptance_criteria) >= 1) AND (octet_length(acceptance_criteria) <= 2048)) AND (btrim(acceptance_criteria, '' \t\r\n''::text) <> ''''::text)))'),
            ('coding_tasks', 'coding_tasks_pinned_recipe_check', 'c', $def$CHECK ((((jsonb_typeof(pinned_recipe) = 'object'::text)
                 AND ((pinned_recipe - ARRAY['version'::text, 'backend'::text, 'model'::text, 'resource_profile'::text, 'mode'::text, 'generation_token_limit'::text]) = '{}'::jsonb)
                 AND (pinned_recipe ? 'version'::text)
                 AND ((pinned_recipe -> 'version'::text) = '1'::jsonb)
                 AND (pinned_recipe ? 'backend'::text)
                 AND (jsonb_typeof((pinned_recipe -> 'backend'::text)) = 'string'::text)
                 AND ((pinned_recipe ->> 'backend'::text) = ANY (ARRAY['antigravity'::text, 'codex'::text]))
                 AND (pinned_recipe ? 'model'::text)
                 AND (jsonb_typeof((pinned_recipe -> 'model'::text)) = 'string'::text)
                 AND ((octet_length((pinned_recipe ->> 'model'::text)) >= 1)
                 AND (octet_length((pinned_recipe ->> 'model'::text)) <= 128))
                 AND (btrim((pinned_recipe ->> 'model'::text)) <> ''::text)
                 AND ((pinned_recipe ->> 'model'::text) ~ '^[ -~]+$'::text)
                 AND (pinned_recipe ? 'resource_profile'::text)
                 AND (jsonb_typeof((pinned_recipe -> 'resource_profile'::text)) = 'string'::text)
                 AND ((pinned_recipe ->> 'resource_profile'::text) ~ '^[A-Za-z0-9_.-]{1,64}$'::text)
                 AND (pinned_recipe ? 'mode'::text)
                 AND (jsonb_typeof((pinned_recipe -> 'mode'::text)) = 'string'::text)
                 AND ((pinned_recipe ->> 'mode'::text) = ANY (ARRAY['plan'::text, 'review'::text, 'implement'::text]))
                 AND (pinned_recipe ? 'generation_token_limit'::text)
                 AND (jsonb_typeof((pinned_recipe -> 'generation_token_limit'::text)) = 'null'::text)) IS TRUE))$def$),
            ('coding_tasks', 'coding_tasks_recipe_digest_check', 'c', $def$CHECK ((recipe_digest ~ '^[0-9a-f]{64}$'::text))$def$),
            ('coding_tasks', 'coding_tasks_status_check', 'c', $def$CHECK ((status = ANY (ARRAY['queued'::text, 'cancelled'::text])))$def$),
            ('coding_tasks', 'coding_tasks_revision_check', 'c', $def$CHECK (((revision >= 1)
                 AND (revision <= '9007199254740991'::bigint)))$def$),
            ('coding_tasks', 'coding_tasks_status_revision_check', 'c', $def$CHECK (((((status = 'queued'::text)
                 AND (cancelled_at IS NULL)
                 AND (revision = 1)) OR ((status = 'cancelled'::text)
                 AND (cancelled_at IS NOT NULL)
                 AND (revision > 1))) IS TRUE))$def$),
            ('coding_tasks', 'coding_tasks_recipe_mode_check', 'c', $def$CHECK (((pinned_recipe ->> 'mode'::text) = mode))$def$),
            ('coding_events', 'coding_events_pkey', 'p', $def$PRIMARY KEY (owner_id, project_alias, task_id, sequence)$def$),
            ('coding_events', 'coding_events_task_fk', 'f', $def$FOREIGN KEY (owner_id, project_alias, task_id) REFERENCES forge_agent.coding_tasks(owner_id, project_alias, id)$def$),
            ('coding_events', 'coding_events_sequence_check', 'c', $def$CHECK (((sequence >= 1)
                 AND (sequence <= '9007199254740991'::bigint)))$def$),
            ('coding_events', 'coding_events_kind_check', 'c', $def$CHECK ((event_kind = ANY (ARRAY['queued'::text, 'cancelled'::text])))$def$),
            ('coding_events', 'coding_events_payload_check', 'c', $def$CHECK ((jsonb_typeof(payload) = 'object'::text))$def$),
            ('coding_events', 'coding_events_sequence_kind_check', 'c', $def$CHECK (((((sequence = 1)
                 AND (event_kind = 'queued'::text)) OR ((sequence > 1)
                 AND (event_kind = 'cancelled'::text))) IS TRUE))$def$),
            ('coding_control_receipts', 'coding_control_receipts_pkey', 'p', $def$PRIMARY KEY (owner_id, project_alias, task_id, action, idempotency_key)$def$),
            ('coding_control_receipts', 'coding_control_receipts_task_fk', 'f', $def$FOREIGN KEY (owner_id, project_alias, task_id) REFERENCES forge_agent.coding_tasks(owner_id, project_alias, id)$def$),
            ('coding_control_receipts', 'coding_control_receipts_action_check', 'c', $def$CHECK ((action = 'cancel'::text))$def$),
            ('coding_control_receipts', 'coding_control_receipts_idempotency_key_check', 'c', $def$CHECK ((idempotency_key ~ '^[A-Za-z0-9_.-]{1,128}$'::text))$def$),
            ('coding_control_receipts', 'coding_control_receipts_intent_digest_check', 'c', $def$CHECK ((intent_digest ~ '^[0-9a-f]{64}$'::text))$def$),
            ('coding_control_receipts', 'coding_control_receipts_base_revision_check', 'c', $def$CHECK (((base_revision >= 1)
                 AND (base_revision <= '9007199254740991'::bigint)))$def$),
            ('coding_control_receipts', 'coding_control_receipts_result_revision_check', 'c', $def$CHECK (((result_revision >= 2)
                 AND (result_revision <= '9007199254740991'::bigint)))$def$),
            ('coding_control_receipts', 'coding_control_receipts_receipt_json_check', 'c', $def$CHECK ((jsonb_typeof(receipt_json) = 'object'::text))$def$),
            ('coding_control_receipts', 'coding_control_receipts_revision_step_check', 'c', $def$CHECK ((result_revision = (base_revision + 1)))$def$)
        ) AS req(relname, conname, contype, definition)
    LOOP
        SELECT pg_catalog.pg_get_constraintdef(c.oid, false), c.conindid
        INTO actual_expression, actual_index
        FROM pg_catalog.pg_constraint c
        WHERE c.conrelid = ('forge_agent.' || expected.relname)::regclass
          AND c.conname = expected.conname
          AND c.contype::text = expected.contype
          AND c.convalidated;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'Missing or unvalidated constraint % on forge_agent.%',
                expected.conname, expected.relname;
        END IF;
        IF expected.contype IN ('p', 'u') AND NOT EXISTS (
            SELECT 1 FROM pg_catalog.pg_index i
            WHERE i.indexrelid = actual_index
              AND i.indrelid = ('forge_agent.' || expected.relname)::regclass
              AND i.indisunique AND i.indisvalid AND i.indisready AND i.indislive
        ) THEN
            RAISE EXCEPTION 'Invalid constraint index % on forge_agent.%',
                expected.conname, expected.relname;
        END IF;
        IF expected.definition IS NOT NULL THEN
            actual_tokens := ARRAY(
                SELECT token.parts[1]
                FROM pg_catalog.regexp_matches(actual_expression, token_pattern, 'g')
                    WITH ORDINALITY AS token(parts, position)
                ORDER BY token.position
            );
            expected_tokens := ARRAY(
                SELECT token.parts[1]
                FROM pg_catalog.regexp_matches(expected.definition, token_pattern, 'g')
                    WITH ORDINALITY AS token(parts, position)
                ORDER BY token.position
            );
            IF actual_tokens IS DISTINCT FROM expected_tokens THEN
                RAISE EXCEPTION 'Unexpected constraint definition % on forge_agent.%',
                    expected.conname, expected.relname;
            END IF;
        END IF;
    END LOOP;

    IF EXISTS (
        SELECT 1
        FROM unnest(ARRAY[
            'coding_tasks', 'coding_events', 'coding_control_receipts'
        ]) AS relname
        WHERE has_table_privilege(
                  'public',
                  'forge_agent.' || relname,
                  'SELECT, INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER'
              )
           OR has_any_column_privilege(
                  'public',
                  'forge_agent.' || relname,
                  'SELECT, INSERT, UPDATE, REFERENCES'
              )
    ) THEN
        RAISE EXCEPTION 'Unexpected PUBLIC privileges on Forge coding admission tables';
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
             AND (has_table_privilege('forge_gpu_worker', rel.oid, 'SELECT, INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER')
                  OR has_any_column_privilege('forge_gpu_worker', rel.oid, 'SELECT, INSERT, UPDATE, REFERENCES'))
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
