\set ON_ERROR_STOP on

BEGIN READ ONLY;
SET LOCAL search_path = pg_catalog;

-- Validate M33 review lineage before exact predecessor normalization.
\ir verify-superseding-reviews.sql

-- Validate M32 lineage authority before exact predecessor normalization.
\ir verify-delivery-lineage.sql

-- Validate M31 event authority before exact predecessor normalization.
\ir verify-retry-events.sql

-- Validate M30 numeric allowances before historical catalog normalization.
\ir verify-agent-retries.sql

-- Validate the M23 replacement before any historical-catalog normalization.
\ir verify-owner-continuation.sql
\ir verify-agy-unattended.sql
\ir verify-native-jenkins.sql
\ir verify-native-delivery.sql
\ir verify-jenkins-events.sql
\ir verify-cicd-stages.sql
-- Verify M28 additions before excluding only its known catalog additions.
\ir verify-delivery.sql
\ir verify-post-merge.sql

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
        'coding_tasks', 'coding_events', 'coding_control_receipts',
        'coding_worker_grants', 'coding_worker_readiness', 'coding_attempts',
        'coding_result_chunks', 'coding_reconciliation_receipts',
        'coding_provider_usage', 'coding_provider_usage_identities',
        'coding_usage_refresh_requests', 'coding_plan_decisions', 'coding_publications'
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

    IF NOT EXISTS (
        SELECT 1 FROM forge_agent.schema_migrations
        WHERE version = 10 AND name = 'coding_execution_and_usage'
    ) THEN
        RAISE EXCEPTION 'Forge coding execution and usage migration 0010 is missing';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM forge_agent.schema_migrations
        WHERE version = 11 AND name = 'coding_elevated_tool_policy'
    ) THEN
        RAISE EXCEPTION 'Forge elevated coding tool policy migration 0011 is missing';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM forge_agent.schema_migrations WHERE version = 12 AND name = 'coding_progress'
    ) THEN
        RAISE EXCEPTION 'Forge coding progress migration 0012 is missing';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM forge_agent.schema_migrations WHERE version = 13 AND name = 'coding_task_session_policy'
    ) THEN
        RAISE EXCEPTION 'Forge coding task session policy migration 0013 is missing';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM forge_agent.schema_migrations WHERE version = 17 AND name = 'coding_codex_warm_sessions'
    ) THEN
        RAISE EXCEPTION 'Forge coding Codex warm sessions migration 0017 is missing';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=18 AND name='coding_grok_provider') THEN
        RAISE EXCEPTION 'Forge Grok provider migration 0018 is missing';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=19 AND name='coding_muse_provider') THEN
        RAISE EXCEPTION 'Forge Muse provider migration 0019 is missing';
    END IF;
    FOR expected IN
        SELECT * FROM (VALUES ('progress_attempt_id', 'uuid'), ('progress_sequence', 'integer')) AS columns(name, type)
    LOOP
        IF NOT EXISTS (
            SELECT 1 FROM pg_catalog.pg_attribute a
            WHERE a.attrelid = 'forge_agent.coding_events'::regclass
              AND a.attname = expected.name AND a.atttypid = expected.type::regtype
              AND NOT a.attnotnull AND NOT a.attisdropped
        ) THEN
            RAISE EXCEPTION 'Missing or unexpected nullable progress column %', expected.name;
        END IF;
    END LOOP;
    IF NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_proc p
        WHERE p.oid = 'forge_coding_worker_api.report_progress(uuid,uuid,integer,text,text)'::regprocedure
          AND p.prosecdef AND 'search_path=pg_catalog, pg_temp' = ANY(p.proconfig)
    ) THEN
        RAISE EXCEPTION 'Missing or unexpected coding progress function';
    END IF;

    -- This is a capability/version guard, not permission inferred from a
    -- comment or from a recipe ID alone. Keep exact worker/recipe digest
    -- matching and the SECURITY DEFINER search path when replacing claim.
    IF NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_proc p
        WHERE p.oid = 'forge_coding_worker_api.claim(text,uuid,text[])'::regprocedure
          AND p.prosecdef
          AND 'search_path=pg_catalog, pg_temp' = ANY(p.proconfig)
          AND position(
              $predicate$(t.pinned_recipe->>'resource_profile')IN('forge-coding-v1','forge-coding-v2','forge-coding-v3','forge-coding-v4','forge-coding-v5','forge-coding-v6','forge-coding-v7','forge-coding-v8','forge-coding-v9','forge-coding-v10')$predicate$
              IN pg_catalog.regexp_replace(replace(p.prosrc, ', ''forge-coding-agy-unattended-v1''', ''), '[[:space:]]+', '', 'g')
          ) > 0
          AND position(
              $predicate$(t.pinned_recipe->>'policy_digest')=r.policy_digest$predicate$
              IN pg_catalog.regexp_replace(p.prosrc, '[[:space:]]+', '', 'g')
          ) > 0
          AND position(
              $predicate$'plan_decision_id',v_task.plan_decision_id$predicate$
              IN pg_catalog.regexp_replace(p.prosrc, '[[:space:]]+', '', 'g')
          ) > 0
          AND position(
              $predicate$'resume_attempt_id',CASEWHENv_resume_digestISNOTNULLTHENcoalesce(v_task.latest_attempt_id,(SELECTf.parent_attempt_idFROMforge_agent.coding_feedbackfWHEREf.feedback_id=v_task.feedback_id))ELSENULLEND$predicate$
              IN pg_catalog.regexp_replace(p.prosrc, '[[:space:]]+', '', 'g')
          ) > 0
    ) THEN
        RAISE EXCEPTION 'Missing or unexpected migration 0013 claim policy predicates';
    END IF;

    IF NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=14 AND name='coding_plan_workflow') THEN
        RAISE EXCEPTION 'Forge coding Plan workflow migration 0014 is missing';
    END IF;
    FOR expected IN SELECT * FROM (VALUES
        ('forge_coding_worker_api.read_plan(uuid,uuid)'),
        ('forge_coding_worker_api.queue_publication(uuid,uuid,uuid)'),
        ('forge_coding_reconciler_api.publication_claim(text)'),
        ('forge_coding_reconciler_api.publication_renew(uuid,uuid)'),
        ('forge_coding_reconciler_api.publication_finish(uuid,uuid,text,text,text,text,text)')
    ) AS functions(signature) LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_proc p WHERE p.oid=expected.signature::regprocedure
                       AND p.prosecdef AND 'search_path=pg_catalog, pg_temp'=ANY(p.proconfig)) THEN
            RAISE EXCEPTION 'Missing or unsafe Plan workflow function %',expected.signature;
        END IF;
    END LOOP;
    FOR expected IN SELECT * FROM (VALUES ('coding_tasks'),('coding_attempts')) AS tables(name) LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_attribute a WHERE a.attrelid=('forge_agent.'||expected.name)::regclass
                       AND a.attname='plan_decision_id' AND a.atttypid='uuid'::regtype AND NOT a.attnotnull AND NOT a.attisdropped) THEN
            RAISE EXCEPTION 'Missing nullable Plan decision pointer %',expected.name;
        END IF;
    END LOOP;

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
            ('coding_events', 'coding_events_progress_identity', E'CREATE UNIQUE INDEX coding_events_progress_identity ON forge_agent.coding_events USING btree (owner_id, project_alias, task_id, progress_attempt_id, progress_sequence) WHERE (progress_attempt_id IS NOT NULL)'),
            ('coding_tasks', 'coding_tasks_by_project_created', $def$CREATE INDEX coding_tasks_by_project_created ON forge_agent.coding_tasks USING btree (owner_id, project_alias, created_at, id)$def$),
            ('coding_tasks', 'coding_tasks_by_project_status_created', $def$CREATE INDEX coding_tasks_by_project_status_created ON forge_agent.coding_tasks USING btree (owner_id, project_alias, status, created_at, id)$def$),
            ('coding_worker_readiness', 'coding_worker_readiness_by_owner_observed', $def$CREATE INDEX coding_worker_readiness_by_owner_observed ON forge_agent.coding_worker_readiness USING btree (owner_id, observed_at DESC)$def$),
            ('coding_attempts', 'coding_attempts_one_open_per_task', $def$CREATE UNIQUE INDEX coding_attempts_one_open_per_task ON forge_agent.coding_attempts USING btree (owner_id, project_alias, task_id) WHERE (lease_expires_at IS NOT NULL)$def$),
            ('coding_attempts', 'coding_attempts_one_reserved_per_task', $def$CREATE UNIQUE INDEX coding_attempts_one_reserved_per_task ON forge_agent.coding_attempts USING btree (owner_id, project_alias, task_id) WHERE reservation_active$def$),
            ('coding_attempts', 'coding_attempts_by_task_no', $def$CREATE INDEX coding_attempts_by_task_no ON forge_agent.coding_attempts USING btree (owner_id, project_alias, task_id, attempt_no DESC)$def$),
            ('coding_attempts', 'coding_attempts_by_lease_expiry', $def$CREATE INDEX coding_attempts_by_lease_expiry ON forge_agent.coding_attempts USING btree (lease_expires_at) WHERE (lease_expires_at IS NOT NULL)$def$),
            ('coding_attempts', 'coding_attempts_by_project_usage', $def$CREATE INDEX coding_attempts_by_project_usage ON forge_agent.coding_attempts USING btree (owner_id, project_alias, published_at) WHERE (published_at IS NOT NULL)$def$),
            ('coding_reconciliation_receipts', 'coding_reconciliation_receipts_by_task_rev', $def$CREATE INDEX coding_reconciliation_receipts_by_task_rev ON forge_agent.coding_reconciliation_receipts USING btree (owner_id, project_alias, task_id, task_revision DESC, created_at DESC)$def$),
            ('coding_usage_refresh_requests', 'coding_usage_refresh_one_open_per_profile', $def$CREATE UNIQUE INDEX coding_usage_refresh_one_open_per_profile ON forge_agent.coding_usage_refresh_requests USING btree (owner_id, profile_alias) WHERE (status = ANY (ARRAY['pending'::text, 'in_progress'::text]))$def$),
            ('coding_usage_refresh_requests', 'coding_usage_refresh_pending_order', $def$CREATE INDEX coding_usage_refresh_pending_order ON forge_agent.coding_usage_refresh_requests USING btree (owner_id, requested_at, profile_alias) WHERE (status = 'pending'::text)$def$)
        ) AS indexes(tabname, relname, definition)
    LOOP
        SELECT pg_catalog.pg_get_indexdef(i.indexrelid, 0, false)
        INTO actual_expression
        FROM pg_catalog.pg_index i
        WHERE i.indexrelid = pg_catalog.to_regclass('forge_agent.' || expected.relname)
          AND i.indrelid = ('forge_agent.' || expected.tabname)::regclass
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
            ('coding_tasks', 'coding_tasks_title_check', 'c', $def$CHECK (((title IS NULL) OR ((char_length(title) >= 1) AND (char_length(title) <= 128) AND (octet_length(title) >= 1) AND (octet_length(title) <= 512) AND (btrim(title) <> ''::text))))$def$),
            ('coding_tasks', 'coding_tasks_resume_workspace_digest_check', 'c', $def$CHECK (((resume_workspace_digest IS NULL) OR (resume_workspace_digest ~ '^[0-9a-f]{64}$'::text)))$def$),
            ('coding_tasks', 'coding_tasks_review_note_check', 'c', $def$CHECK (((review_note IS NULL) OR (octet_length(review_note) <= 2048)))$def$),
            ('coding_tasks', 'coding_tasks_pinned_recipe_check', 'c', $def$CHECK ((((jsonb_typeof(pinned_recipe) = 'object'::text)
                 AND (pinned_recipe ? 'version'::text)
                 AND (pinned_recipe ? 'backend'::text)
                 AND (jsonb_typeof((pinned_recipe -> 'backend'::text)) = 'string'::text)
                 AND ((pinned_recipe ->> 'backend'::text) = ANY (ARRAY['antigravity'::text, 'codex'::text, 'grok'::text, 'muse'::text]))
                 AND (pinned_recipe ? 'model'::text)
                 AND (jsonb_typeof((pinned_recipe -> 'model'::text)) = 'string'::text)
                 AND (octet_length((pinned_recipe ->> 'model'::text)) >= 1)
                 AND (octet_length((pinned_recipe ->> 'model'::text)) <= 128)
                 AND (btrim((pinned_recipe ->> 'model'::text)) <> ''::text)
                 AND ((pinned_recipe ->> 'model'::text) ~ '^[ -~]+$'::text)
                 AND (pinned_recipe ? 'resource_profile'::text)
                 AND (jsonb_typeof((pinned_recipe -> 'resource_profile'::text)) = 'string'::text)
                 AND ((pinned_recipe ->> 'resource_profile'::text) ~ '^[A-Za-z0-9_.-]{1,64}$'::text)
                 AND (pinned_recipe ? 'mode'::text)
                 AND (jsonb_typeof((pinned_recipe -> 'mode'::text)) = 'string'::text)
                 AND ((pinned_recipe ->> 'mode'::text) = ANY (ARRAY['plan'::text, 'review'::text, 'implement'::text]))
                 AND (pinned_recipe ? 'generation_token_limit'::text)
                 AND (jsonb_typeof((pinned_recipe -> 'generation_token_limit'::text)) = 'null'::text)
                 AND ((((pinned_recipe -> 'version'::text) = '1'::jsonb)
                     AND ((pinned_recipe - ARRAY['version'::text, 'backend'::text, 'model'::text, 'resource_profile'::text, 'mode'::text, 'generation_token_limit'::text]) = '{}'::jsonb))
                 OR (((pinned_recipe -> 'version'::text) = '2'::jsonb)
                     AND ((pinned_recipe - ARRAY['version'::text, 'backend'::text, 'model'::text, 'resource_profile'::text, 'mode'::text, 'generation_token_limit'::text, 'policy_digest'::text]) = '{}'::jsonb)
                     AND (pinned_recipe ? 'policy_digest'::text)
                     AND (jsonb_typeof((pinned_recipe -> 'policy_digest'::text)) = 'string'::text)
                     AND ((pinned_recipe ->> 'policy_digest'::text) ~ '^[0-9a-f]{64}$'::text)))) IS TRUE))$def$),
            ('coding_tasks', 'coding_tasks_recipe_digest_check', 'c', $def$CHECK ((recipe_digest ~ '^[0-9a-f]{64}$'::text))$def$),
            ('coding_tasks', 'coding_tasks_status_check', 'c', $def$CHECK ((status = ANY (ARRAY['queued'::text, 'running'::text, 'cancel_requested'::text, 'awaiting_review'::text, 'completed'::text, 'cancelled'::text, 'failed'::text, 'quota_blocked'::text, 'blocked_auth'::text, 'blocked_permission'::text, 'needs_reconcile'::text])))$def$),
            ('coding_tasks', 'coding_tasks_revision_check', 'c', $def$CHECK (((revision >= 1)
                 AND (revision <= '9007199254740991'::bigint)))$def$),
            ('coding_tasks', 'coding_tasks_status_revision_check', 'c', $def$CHECK (((((status = 'queued'::text)
                 AND (cancelled_at IS NULL)
                 AND (accepted_at IS NULL)) OR ((status = 'cancelled'::text)
                 AND (cancelled_at IS NOT NULL)
                 AND (accepted_at IS NULL)
                 AND (revision > 1)) OR ((status = 'completed'::text)
                 AND (accepted_at IS NOT NULL)
                 AND (cancelled_at IS NULL)
                 AND (revision > 1)) OR ((status = ANY (ARRAY['running'::text, 'cancel_requested'::text, 'awaiting_review'::text, 'failed'::text, 'quota_blocked'::text, 'blocked_auth'::text, 'blocked_permission'::text, 'needs_reconcile'::text]))
                 AND (cancelled_at IS NULL)
                 AND (accepted_at IS NULL)
                 AND (revision > 1))) IS TRUE))$def$),
            ('coding_tasks', 'coding_tasks_recipe_mode_check', 'c', $def$CHECK (((pinned_recipe ->> 'mode'::text) = mode))$def$),
            ('coding_events', 'coding_events_progress_attempt_fk', 'f', E'FOREIGN KEY (owner_id, project_alias, task_id, progress_attempt_id) REFERENCES forge_agent.coding_attempts(owner_id, project_alias, task_id, attempt_id)'),
            ('coding_events', 'coding_events_progress_check', 'c', E'CHECK (((((progress_attempt_id IS NULL) AND (progress_sequence IS NULL)) OR ((progress_attempt_id IS NOT NULL) AND ((progress_sequence >= 1) AND (progress_sequence <= 128)) AND (event_kind = ''checkpoint''::text) AND ((payload - ARRAY[''attempt_id''::text, ''progress_sequence''::text, ''stage''::text, ''summary''::text, ''actor''::text]) = ''{}''::jsonb) AND (payload ?& ARRAY[''attempt_id''::text, ''progress_sequence''::text, ''stage''::text, ''summary''::text, ''actor''::text]) AND (jsonb_typeof((payload -> ''attempt_id''::text)) = ''string''::text) AND ((payload ->> ''attempt_id''::text) = (progress_attempt_id)::text) AND (jsonb_typeof((payload -> ''progress_sequence''::text)) = ''number''::text) AND ((payload ->> ''progress_sequence''::text) = (progress_sequence)::text) AND (jsonb_typeof((payload -> ''stage''::text)) = ''string''::text) AND (((progress_sequence = 1) AND ((payload ->> ''stage''::text) = ''plan''::text)) OR ((progress_sequence > 1) AND ((payload ->> ''stage''::text) = ''progress''::text))) AND (jsonb_typeof((payload -> ''summary''::text)) = ''string''::text) AND ((octet_length((payload ->> ''summary''::text)) >= 1) AND (octet_length((payload ->> ''summary''::text)) <= 4096)) AND (btrim((payload ->> ''summary''::text), '' \u0009\u000d\u000a''::text) <> ''''::text) AND ((payload ->> ''summary''::text) !~ ''[\u0001-\u0008\u000b-\u001f\u007f-\u009f]''::text) AND (jsonb_typeof((payload -> ''actor''::text)) = ''object''::text) AND ((payload -> ''actor''::text) ?& ARRAY[''kind''::text, ''id''::text]) AND (((payload -> ''actor''::text) ->> ''kind''::text) = ''worker''::text) AND ((octet_length(btrim(((payload -> ''actor''::text) ->> ''id''::text))) >= 1) AND (octet_length(btrim(((payload -> ''actor''::text) ->> ''id''::text))) <= 128)))) IS TRUE))'),
            ('coding_events', 'coding_events_pkey', 'p', $def$PRIMARY KEY (owner_id, project_alias, task_id, sequence)$def$),
            ('coding_events', 'coding_events_task_fk', 'f', $def$FOREIGN KEY (owner_id, project_alias, task_id) REFERENCES forge_agent.coding_tasks(owner_id, project_alias, id)$def$),
            ('coding_events', 'coding_events_sequence_check', 'c', $def$CHECK (((sequence >= 1)
                 AND (sequence <= '9007199254740991'::bigint)))$def$),
            ('coding_events', 'coding_events_kind_check', 'c', $def$CHECK ((event_kind = ANY (ARRAY['queued'::text, 'started'::text, 'checkpoint'::text, 'cancel_requested'::text, 'cancelled'::text, 'awaiting_review'::text, 'completed'::text, 'failed'::text, 'quota_blocked'::text, 'blocked_auth'::text, 'blocked_permission'::text, 'needs_reconcile'::text, 'resumed'::text])))$def$),
            ('coding_events', 'coding_events_payload_check', 'c', $def$CHECK ((jsonb_typeof(payload) = 'object'::text))$def$),
            ('coding_events', 'coding_events_sequence_kind_check', 'c', $def$CHECK (((((sequence = 1)
                 AND (event_kind = 'queued'::text)) OR ((sequence > 1)
                 AND (event_kind <> 'queued'::text))) IS TRUE))$def$),
            ('coding_control_receipts', 'coding_control_receipts_pkey', 'p', $def$PRIMARY KEY (owner_id, project_alias, task_id, action, idempotency_key)$def$),
            ('coding_control_receipts', 'coding_control_receipts_task_key_unique', 'u', $def$UNIQUE (owner_id, project_alias, task_id, idempotency_key)$def$),
            ('coding_control_receipts', 'coding_control_receipts_task_fk', 'f', $def$FOREIGN KEY (owner_id, project_alias, task_id) REFERENCES forge_agent.coding_tasks(owner_id, project_alias, id)$def$),
            ('coding_control_receipts', 'coding_control_receipts_action_check', 'c', $def$CHECK ((action = ANY (ARRAY['cancel'::text, 'accept'::text, 'resume'::text, 'plan_decide'::text, 'review_decide'::text, 'repair'::text, 'publication_retry'::text])))$def$),
            ('coding_control_receipts', 'coding_control_receipts_idempotency_key_check', 'c', $def$CHECK ((idempotency_key ~ '^[A-Za-z0-9_.-]{1,128}$'::text))$def$),
            ('coding_control_receipts', 'coding_control_receipts_intent_digest_check', 'c', $def$CHECK ((intent_digest ~ '^[0-9a-f]{64}$'::text))$def$),
            ('coding_control_receipts', 'coding_control_receipts_base_revision_check', 'c', $def$CHECK (((base_revision >= 1)
                 AND (base_revision <= '9007199254740991'::bigint)))$def$),
            ('coding_control_receipts', 'coding_control_receipts_result_revision_check', 'c', $def$CHECK (((result_revision >= 2)
                 AND (result_revision <= '9007199254740991'::bigint)))$def$),
            ('coding_control_receipts', 'coding_control_receipts_receipt_json_check', 'c', $def$CHECK ((jsonb_typeof(receipt_json) = 'object'::text))$def$),
            ('coding_control_receipts', 'coding_control_receipts_revision_step_check', 'c', $def$CHECK ((result_revision = (base_revision + 1)))$def$),
            ('coding_worker_grants', 'coding_worker_grants_pkey', 'p', $def$PRIMARY KEY (worker_role, owner_id, project_alias, agent_profile, mode)$def$),
            ('coding_worker_grants', 'coding_worker_grants_mode_check', 'c', $def$CHECK ((mode = ANY (ARRAY['plan'::text, 'review'::text, 'implement'::text])))$def$),
            ('coding_worker_readiness', 'coding_worker_readiness_pkey', 'p', $def$PRIMARY KEY (worker_role, owner_id, worker_id)$def$),
            ('coding_worker_readiness', 'coding_worker_readiness_policy_digest_check', 'c', $def$CHECK ((policy_digest ~ '^[0-9a-f]{64}$'::text))$def$),
            ('coding_attempts', 'coding_attempts_agent_profile_check', 'c', $def$CHECK ((agent_profile ~ '^[A-Za-z0-9_.-]{1,64}$'::text))$def$),
            ('coding_attempts', 'coding_attempts_pinned_recipe_check', 'c', $def$CHECK ((((jsonb_typeof(pinned_recipe) = 'object'::text) AND ((pinned_recipe - ARRAY['version'::text, 'backend'::text, 'model'::text, 'resource_profile'::text, 'mode'::text, 'generation_token_limit'::text, 'policy_digest'::text]) = '{}'::jsonb) AND (pinned_recipe ?& ARRAY['version'::text, 'backend'::text, 'model'::text, 'resource_profile'::text, 'mode'::text, 'generation_token_limit'::text, 'policy_digest'::text]) AND ((pinned_recipe -> 'version'::text) = '2'::jsonb) AND ((pinned_recipe ->> 'backend'::text) = ANY (ARRAY['antigravity'::text, 'codex'::text, 'grok'::text, 'muse'::text])) AND (jsonb_typeof((pinned_recipe -> 'model'::text)) = 'string'::text) AND ((octet_length((pinned_recipe ->> 'model'::text)) >= 1) AND (octet_length((pinned_recipe ->> 'model'::text)) <= 128)) AND ((pinned_recipe ->> 'resource_profile'::text) = ANY (ARRAY['forge-coding-v1'::text, 'forge-coding-v2'::text, 'forge-coding-v3'::text, 'forge-coding-v4'::text, 'forge-coding-v5'::text, 'forge-coding-v6'::text, 'forge-coding-v7'::text, 'forge-coding-v8'::text, 'forge-coding-v9'::text, 'forge-coding-v10'::text])) AND ((pinned_recipe ->> 'mode'::text) = ANY (ARRAY['plan'::text, 'review'::text, 'implement'::text])) AND (jsonb_typeof((pinned_recipe -> 'generation_token_limit'::text)) = 'null'::text) AND ((pinned_recipe ->> 'policy_digest'::text) ~ '^[0-9a-f]{64}$'::text)) IS TRUE))$def$),
            ('coding_attempts', 'coding_attempts_recipe_digest_check', 'c', $def$CHECK ((recipe_digest ~ '^[0-9a-f]{64}$'::text))$def$),
            ('coding_attempts', 'coding_attempts_pkey', 'p', $def$PRIMARY KEY (owner_id, project_alias, task_id, attempt_id)$def$),
            ('coding_attempts', 'coding_attempts_task_fk', 'f', $def$FOREIGN KEY (owner_id, project_alias, task_id) REFERENCES forge_agent.coding_tasks(owner_id, project_alias, id)$def$),
            ('coding_attempts', 'coding_attempts_id_unique', 'u', $def$UNIQUE (attempt_id)$def$),
            ('coding_attempts', 'coding_attempts_no_unique', 'u', $def$UNIQUE (owner_id, project_alias, task_id, attempt_no)$def$),
            ('coding_attempts', 'coding_attempts_unit_name_unique', 'u', $def$UNIQUE (unit_name)$def$),
            ('coding_attempts', 'coding_attempts_unit_name_check', 'c', $def$CHECK ((unit_name ~ '^forge-coding-[0-9a-f]{32}$'::text))$def$),
            ('coding_attempts', 'coding_attempts_phase_check', 'c', $def$CHECK ((phase = ANY (ARRAY['starting'::text, 'running'::text, 'stopping'::text, 'stopped'::text, 'unknown'::text])))$def$),
            ('coding_attempts', 'coding_attempts_outcome_check', 'c', $def$CHECK (((outcome IS NULL) OR (outcome = ANY (ARRAY['awaiting_review'::text, 'failed'::text, 'quota_blocked'::text, 'blocked_auth'::text, 'blocked_permission'::text, 'cancelled'::text, 'needs_reconcile'::text]))))$def$),
            ('coding_attempts', 'coding_attempts_result_byte_count_check', 'c', $def$CHECK (((result_byte_count IS NULL) OR ((result_byte_count >= 1) AND (result_byte_count <= 16777216))))$def$),
            ('coding_attempts', 'coding_attempts_result_chunk_count_check', 'c', $def$CHECK (((result_chunk_count IS NULL) OR ((result_chunk_count >= 1) AND (result_chunk_count <= 1024))))$def$),
            ('coding_attempts', 'coding_attempts_summary_check', 'c', $def$CHECK ((octet_length(summary) <= 4096))$def$),
            ('coding_attempts', 'coding_attempts_stop_proof_check', 'c', $def$CHECK (((stop_proof IS NULL) OR ((jsonb_typeof(stop_proof) = 'object'::text) AND ((stop_proof - ARRAY['unit_name'::text, 'scope_stopped'::text, 'source'::text, 'checked_at'::text]) = '{}'::jsonb) AND ((stop_proof ->> 'unit_name'::text) = unit_name) AND ((stop_proof ->> 'source'::text) = 'systemd'::text) AND (jsonb_typeof((stop_proof -> 'scope_stopped'::text)) = 'boolean'::text))))$def$),
            ('coding_attempts', 'coding_attempts_publish_state_check', 'c', $def$CHECK (((((outcome IS NULL) AND (outcome_digest IS NULL) AND (publish_receipt IS NULL) AND (published_at IS NULL)) OR ((outcome IS NOT NULL) AND (outcome_digest IS NOT NULL) AND (publish_receipt IS NOT NULL) AND (published_at IS NOT NULL))) IS TRUE))$def$),
            ('coding_attempts', 'coding_attempts_result_manifest_check', 'c', $def$CHECK (((((result_sha256 IS NULL) AND (result_byte_count IS NULL) AND (result_chunk_count IS NULL)) OR ((result_sha256 IS NOT NULL) AND (result_byte_count IS NOT NULL) AND (result_chunk_count IS NOT NULL))) IS TRUE))$def$),
            ('coding_attempts', 'coding_attempts_awaiting_review_manifest_check', 'c', $def$CHECK (((outcome IS NULL) OR (outcome <> 'awaiting_review'::text) OR (result_sha256 IS NOT NULL)))$def$),
            ('coding_result_chunks', 'coding_result_chunks_pkey', 'p', $def$PRIMARY KEY (owner_id, project_alias, task_id, attempt_id, chunk_index)$def$),
            ('coding_result_chunks', 'coding_result_chunks_attempt_fk', 'f', $def$FOREIGN KEY (owner_id, project_alias, task_id, attempt_id) REFERENCES forge_agent.coding_attempts(owner_id, project_alias, task_id, attempt_id)$def$),
            ('coding_result_chunks', 'coding_result_chunks_index_check', 'c', $def$CHECK (((chunk_index >= 0) AND (chunk_index <= 1023)))$def$),
            ('coding_result_chunks', 'coding_result_chunks_content_check', 'c', $def$CHECK (((octet_length(content) >= 1) AND (octet_length(content) <= 16384)))$def$),
            ('coding_result_chunks', 'coding_result_chunks_length_match_check', 'c', $def$CHECK ((byte_length = octet_length(content)))$def$),
            ('coding_reconciliation_receipts', 'coding_reconciliation_receipts_pkey', 'p', $def$PRIMARY KEY (owner_id, project_alias, task_id, receipt_id)$def$),
            ('coding_reconciliation_receipts', 'coding_reconciliation_receipts_id_unique', 'u', $def$UNIQUE (receipt_id)$def$),
            ('coding_reconciliation_receipts', 'coding_reconciliation_receipts_attempt_fk', 'f', $def$FOREIGN KEY (owner_id, project_alias, task_id, attempt_id) REFERENCES forge_agent.coding_attempts(owner_id, project_alias, task_id, attempt_id)$def$),
            ('coding_reconciliation_receipts', 'coding_reconciliation_receipts_stop_proof_check', 'c', $def$CHECK (((jsonb_typeof(stop_proof) = 'object'::text) AND ((stop_proof - ARRAY['unit_name'::text, 'scope_stopped'::text, 'source'::text, 'checked_at'::text]) = '{}'::jsonb) AND ((stop_proof ->> 'source'::text) = 'systemd'::text) AND ((stop_proof -> 'scope_stopped'::text) = 'true'::jsonb)))$def$),
            ('coding_provider_usage', 'coding_provider_usage_pkey', 'p', $def$PRIMARY KEY (owner_id, profile_alias)$def$),
            ('coding_provider_usage', 'coding_provider_usage_generation_order_check', 'c', $def$CHECK ((recorded_generation <= current_generation))$def$),
            ('coding_provider_usage', 'coding_provider_usage_epoch_order_check', 'c', $def$CHECK ((recorded_epoch <= configuration_epoch))$def$),
            ('coding_provider_usage_identities', 'coding_provider_usage_identities_pkey', 'p', $def$PRIMARY KEY (owner_id, profile_alias)$def$),
            ('coding_provider_usage_identities', 'coding_provider_usage_identities_usage_fk', 'f', $def$FOREIGN KEY (owner_id, profile_alias) REFERENCES forge_agent.coding_provider_usage(owner_id, profile_alias)$def$),
            ('coding_usage_refresh_requests', 'coding_usage_refresh_requests_pkey', 'p', $def$PRIMARY KEY (owner_id, profile_alias, refresh_id)$def$),
            ('coding_usage_refresh_requests', 'coding_usage_refresh_requests_status_check', 'c', $def$CHECK ((status = ANY (ARRAY['pending'::text, 'in_progress'::text, 'completed'::text, 'failed'::text])))$def$),
            ('coding_attempts', 'coding_attempts_plan_decision_fk', 'f', $def$FOREIGN KEY (owner_id, project_alias, task_id, plan_decision_id) REFERENCES forge_agent.coding_plan_decisions(owner_id, project_alias, task_id, decision_id)$def$),
            ('coding_plan_decisions', 'coding_plan_decisions_agent_profile_check', 'c', $def$CHECK ((agent_profile ~ '^[A-Za-z0-9_.-]{1,64}$'::text))$def$),
            ('coding_plan_decisions', 'coding_plan_decisions_attempt_fk', 'f', $def$FOREIGN KEY (owner_id, project_alias, task_id, plan_attempt_id) REFERENCES forge_agent.coding_attempts(owner_id, project_alias, task_id, attempt_id)$def$),
            ('coding_plan_decisions', 'coding_plan_decisions_base_revision_check', 'c', $def$CHECK ((base_revision ~ '^[0-9a-f]{40}$'::text))$def$),
            ('coding_plan_decisions', 'coding_plan_decisions_check', 'c', $def$CHECK (((octet_length(note) <= 2048) AND ((decision <> 'revise'::text) OR (btrim(note) <> ''::text))))$def$),
            ('coding_plan_decisions', 'coding_plan_decisions_decision_check', 'c', $def$CHECK ((decision = ANY (ARRAY['resume'::text, 'revise'::text, 'cancel'::text])))$def$),
            ('coding_plan_decisions', 'coding_plan_decisions_idempotency_key_check', 'c', $def$CHECK ((idempotency_key ~ '^[A-Za-z0-9_.-]{1,128}$'::text))$def$),
            ('coding_plan_decisions', 'coding_plan_decisions_intent_unique', 'u', $def$UNIQUE (owner_id, project_alias, task_id, idempotency_key)$def$),
            ('coding_plan_decisions', 'coding_plan_decisions_pkey', 'p', $def$PRIMARY KEY (decision_id)$def$),
            ('coding_plan_decisions', 'coding_plan_decisions_plan_sha256_check', 'c', $def$CHECK ((plan_sha256 ~ '^[0-9a-f]{64}$'::text))$def$),
            ('coding_plan_decisions', 'coding_plan_decisions_receipt_json_check', 'c', $def$CHECK ((jsonb_typeof(receipt_json) = 'object'::text))$def$),
            ('coding_plan_decisions', 'coding_plan_decisions_scope_unique', 'u', $def$UNIQUE (owner_id, project_alias, task_id, decision_id)$def$),
            ('coding_plan_decisions', 'coding_plan_decisions_source_revision_check', 'c', $def$CHECK (((source_revision >= 1) AND (source_revision <= '9007199254740990'::bigint)))$def$),
            ('coding_publications', 'coding_publications_attempt_fk', 'f', $def$FOREIGN KEY (owner_id, project_alias, task_id, attempt_id) REFERENCES forge_agent.coding_attempts(owner_id, project_alias, task_id, attempt_id)$def$),
            ('coding_publications', 'coding_publications_attempt_unique', 'u', $def$UNIQUE NULLS NOT DISTINCT (owner_id, project_alias, task_id, attempt_id, review_id)$def$),
            ('coding_publications', 'coding_publications_base_revision_check', 'c', $def$CHECK ((base_revision ~ '^[0-9a-f]{40}$'::text))$def$),
            ('coding_publications', 'coding_publications_branch_check', 'c', $def$CHECK (((branch IS NULL) OR ((octet_length(branch) <= 128) AND (branch ~ '^forge/[A-Za-z0-9][A-Za-z0-9._/-]*$'::text) AND (branch !~ '(\.\.|//|/$|\.lock$)'::text))))$def$),
            ('coding_publications', 'coding_publications_commit_sha_check', 'c', $def$CHECK (((commit_sha IS NULL) OR (commit_sha ~ '^[0-9a-f]{40}$'::text)))$def$),
            ('coding_publications', 'coding_publications_decision_fk', 'f', $def$FOREIGN KEY (owner_id, project_alias, task_id, plan_decision_id) REFERENCES forge_agent.coding_plan_decisions(owner_id, project_alias, task_id, decision_id)$def$),
            ('coding_publications', 'coding_publications_lease_check', 'c', $def$CHECK (((((status = ANY (ARRAY['checking'::text, 'publishing'::text])) AND (worker_id IS NOT NULL) AND (lease_token IS NOT NULL) AND (lease_expires_at IS NOT NULL)) OR ((status <> ALL (ARRAY['checking'::text, 'publishing'::text])) AND (lease_token IS NULL) AND (lease_expires_at IS NULL))) IS TRUE))$def$),
            ('coding_publications', 'coding_publications_pkey', 'p', $def$PRIMARY KEY (publication_id)$def$),
            ('coding_publications', 'coding_publications_pr_url_check', 'c', $def$CHECK (((pr_url IS NULL) OR ((octet_length(pr_url) <= 512) AND (pr_url ~ '^https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/pull/[1-9][0-9]*$'::text))))$def$),
            ('coding_publications', 'coding_publications_published_check', 'c', $def$CHECK ((((status <> 'published'::text) OR ((branch IS NOT NULL) AND (commit_sha IS NOT NULL) AND (pr_url IS NOT NULL))) IS TRUE))$def$),
            ('coding_publications', 'coding_publications_reconciliation_receipt_id_fkey', 'f', $def$FOREIGN KEY (reconciliation_receipt_id) REFERENCES forge_agent.coding_reconciliation_receipts(receipt_id)$def$),
            ('coding_publications', 'coding_publications_status_check', 'c', $def$CHECK ((status = ANY (ARRAY['pending'::text, 'checking'::text, 'publishing'::text, 'published'::text, 'failed'::text])))$def$),
            ('coding_publications', 'coding_publications_summary_check', 'c', $def$CHECK ((octet_length(summary) <= 4096))$def$),
            ('coding_publications', 'coding_publications_workspace_digest_check', 'c', $def$CHECK ((workspace_digest ~ '^[0-9a-f]{64}$'::text))$def$),
            ('coding_tasks', 'coding_tasks_plan_decision_fk', 'f', $def$FOREIGN KEY (owner_id, project_alias, id, plan_decision_id) REFERENCES forge_agent.coding_plan_decisions(owner_id, project_alias, task_id, decision_id)$def$)
        ) AS req(relname, conname, contype, definition)
    LOOP
        SELECT pg_catalog.pg_get_constraintdef(c.oid, false), c.conindid
        INTO actual_expression, actual_index
        FROM pg_catalog.pg_constraint c
        WHERE c.conrelid = ('forge_agent.' || expected.relname)::regclass
          AND c.conname = expected.conname
          AND c.contype::text = expected.contype
          -- M36's progress check is intentionally NOT VALID: legacy progress
          -- rows predate the actor field and are grandfathered untouched.
          AND (c.convalidated OR (expected.relname = 'coding_events'
              AND expected.conname = 'coding_events_progress_check'));
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
        -- M23's exact native recipe constraint is checked above. Remove only
        -- its new allowlist token for the frozen predecessor token comparison.
        IF expected.relname='coding_attempts' AND expected.conname='coding_attempts_pinned_recipe_check'
           AND EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=23) THEN
            actual_expression:=replace(actual_expression, ', ''forge-coding-agy-unattended-v1''::text', '');
        END IF;
        -- M36's progress check reports its definition with a trailing
        -- ' NOT VALID'; strip it so the token comparison sees the pinned
        -- expression (the exemption above already gates its validity).
        IF expected.relname='coding_events' AND expected.conname='coding_events_progress_check' THEN
            actual_expression:=regexp_replace(actual_expression, ' NOT VALID$', '');
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
            'coding_tasks', 'coding_events', 'coding_control_receipts',
            'coding_worker_grants', 'coding_worker_readiness', 'coding_attempts',
            'coding_result_chunks', 'coding_reconciliation_receipts',
            'coding_provider_usage', 'coding_provider_usage_identities',
            'coding_usage_refresh_requests', 'coding_plan_decisions', 'coding_publications'
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
    ) OR has_schema_privilege('public', 'forge_coding_worker_api', 'USAGE, CREATE')
      OR has_schema_privilege('public', 'forge_coding_reconciler_api', 'USAGE, CREATE')
      OR EXISTS (
          SELECT 1
          FROM pg_catalog.pg_proc p
          WHERE p.pronamespace IN (
              'forge_coding_worker_api'::regnamespace,
              'forge_coding_reconciler_api'::regnamespace
          )
            AND has_function_privilege('public', p.oid, 'EXECUTE')
      ) THEN
        RAISE EXCEPTION 'Unexpected PUBLIC privileges on Forge coding admission tables';
    END IF;

    IF has_schema_privilege('forge_gpu_worker', 'forge_agent', 'USAGE, CREATE')
       OR has_schema_privilege('forge_gpu_worker', 'forge_agent_worker_api', 'CREATE')
       OR has_schema_privilege('forge_gpu_worker', 'forge_coding_worker_api', 'USAGE, CREATE')
       OR has_schema_privilege('forge_gpu_worker', 'forge_coding_reconciler_api', 'USAGE, CREATE')
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
       OR EXISTS (
           SELECT 1
           FROM pg_catalog.pg_proc p
           WHERE p.pronamespace IN (
               'forge_coding_worker_api'::regnamespace,
               'forge_coding_reconciler_api'::regnamespace
           )
             AND has_function_privilege('forge_gpu_worker', p.oid, 'EXECUTE')
       )
    THEN
        RAISE EXCEPTION 'Unexpected Forge GPU worker privileges';
    END IF;

    IF has_schema_privilege('forge_coding_worker', 'forge_agent', 'USAGE, CREATE')
       OR has_schema_privilege('forge_coding_worker', 'forge_agent_worker_api', 'USAGE, CREATE')
       OR has_schema_privilege('forge_coding_worker', 'forge_coding_reconciler_api', 'USAGE, CREATE')
       OR has_schema_privilege('forge_coding_worker', 'forge_coding_worker_api', 'CREATE')
       OR NOT has_schema_privilege('forge_coding_worker', 'forge_coding_worker_api', 'USAGE')
       OR has_database_privilege('forge_coding_worker', current_database(), 'CREATE, TEMP')
       OR has_function_privilege('forge_coding_worker', 'forge_coding_worker_api._validate_stop_proof(jsonb,text)', 'EXECUTE')
       OR has_function_privilege('forge_coding_worker', 'forge_coding_worker_api._validate_usage_json(jsonb)', 'EXECUTE')
       OR has_function_privilege('forge_coding_worker', 'forge_coding_worker_api._validate_provider_snapshot(jsonb)', 'EXECUTE')
       OR has_function_privilege('forge_coding_worker', 'forge_coding_worker_api._lock_task_and_attempt(uuid,uuid)', 'EXECUTE')
       OR NOT (
           has_function_privilege('forge_coding_worker', 'forge_coding_worker_api.advertise(text,uuid,jsonb,jsonb,text)', 'EXECUTE')
           AND has_function_privilege('forge_coding_worker', 'forge_coding_worker_api.claim(text,uuid,text[])', 'EXECUTE')
           AND has_function_privilege('forge_coding_worker', 'forge_coding_worker_api.renew(uuid,uuid)', 'EXECUTE')
           AND has_function_privilege('forge_coding_worker', 'forge_coding_worker_api.checkpoint(uuid,uuid,text)', 'EXECUTE')
           AND has_function_privilege('forge_coding_worker', 'forge_coding_worker_api.report_progress(uuid,uuid,integer,text,text)', 'EXECUTE')
           AND has_function_privilege('forge_coding_worker', 'forge_coding_worker_api.read_plan(uuid,uuid)', 'EXECUTE')
           AND has_function_privilege('forge_coding_worker', 'forge_coding_worker_api.queue_publication(uuid,uuid,uuid)', 'EXECUTE')
           AND has_function_privilege('forge_coding_worker', 'forge_coding_worker_api.append_result(uuid,uuid,integer,text)', 'EXECUTE')
           AND has_function_privilege('forge_coding_worker', 'forge_coding_worker_api.publish(uuid,uuid,text,jsonb,jsonb,text,jsonb)', 'EXECUTE')
           AND has_function_privilege('forge_coding_worker', 'forge_coding_worker_api.record_stop(uuid,uuid,jsonb,text,text)', 'EXECUTE')
           AND has_function_privilege('forge_coding_worker', 'forge_coding_worker_api.cancellation_requested(uuid,uuid)', 'EXECUTE')
           AND has_function_privilege('forge_coding_worker', 'forge_coding_worker_api.begin_usage_poll(text)', 'EXECUTE')
           AND has_function_privilege('forge_coding_worker', 'forge_coding_worker_api.next_refresh()', 'EXECUTE')
           AND has_function_privilege('forge_coding_worker', 'forge_coding_worker_api.record_usage(text,bigint,bigint,jsonb)', 'EXECUTE')
           AND has_function_privilege('forge_coding_worker', 'forge_coding_worker_api.fail_usage_poll(text,bigint,bigint,text)', 'EXECUTE')
       )
       OR EXISTS (
           SELECT FROM pg_catalog.pg_class rel
           WHERE rel.relnamespace = 'forge_agent'::regnamespace AND rel.relkind IN ('r', 'p')
             AND (has_table_privilege('forge_coding_worker', rel.oid, 'SELECT, INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER')
                  OR has_any_column_privilege('forge_coding_worker', rel.oid, 'SELECT, INSERT, UPDATE, REFERENCES'))
       )
       OR EXISTS (
           SELECT 1
           FROM pg_catalog.pg_proc p
           WHERE p.pronamespace IN (
               'forge_agent_worker_api'::regnamespace,
               'forge_coding_reconciler_api'::regnamespace
           )
             AND has_function_privilege('forge_coding_worker', p.oid, 'EXECUTE')
       )
    THEN
        RAISE EXCEPTION 'Unexpected Forge coding worker privileges';
    END IF;

    IF has_schema_privilege('forge_coding_reconciler', 'forge_agent', 'USAGE, CREATE')
       OR has_schema_privilege('forge_coding_reconciler', 'forge_agent_worker_api', 'USAGE, CREATE')
       OR has_schema_privilege('forge_coding_reconciler', 'forge_coding_worker_api', 'USAGE, CREATE')
       OR has_schema_privilege('forge_coding_reconciler', 'forge_coding_reconciler_api', 'CREATE')
       OR NOT has_schema_privilege('forge_coding_reconciler', 'forge_coding_reconciler_api', 'USAGE')
       OR has_database_privilege('forge_coding_reconciler', current_database(), 'CREATE, TEMP')
       OR NOT (
           has_function_privilege('forge_coding_reconciler', 'forge_coding_reconciler_api.expire_leases()', 'EXECUTE')
           AND has_function_privilege('forge_coding_reconciler', 'forge_coding_reconciler_api.pending_reconciliations(integer)', 'EXECUTE')
           AND has_function_privilege('forge_coding_reconciler', 'forge_coding_reconciler_api.reconcile(uuid,uuid,text,jsonb)', 'EXECUTE')
           AND has_function_privilege('forge_coding_reconciler', 'forge_coding_reconciler_api.publication_claim(text)', 'EXECUTE')
           AND has_function_privilege('forge_coding_reconciler', 'forge_coding_reconciler_api.publication_renew(uuid,uuid)', 'EXECUTE')
           AND has_function_privilege('forge_coding_reconciler', 'forge_coding_reconciler_api.publication_finish(uuid,uuid,text,text,text,text,text)', 'EXECUTE')
       )
       OR EXISTS (
           SELECT FROM pg_catalog.pg_class rel
           WHERE rel.relnamespace = 'forge_agent'::regnamespace AND rel.relkind IN ('r', 'p')
             AND (has_table_privilege('forge_coding_reconciler', rel.oid, 'SELECT, INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER')
                  OR has_any_column_privilege('forge_coding_reconciler', rel.oid, 'SELECT, INSERT, UPDATE, REFERENCES'))
       )
       OR EXISTS (
           SELECT 1
           FROM pg_catalog.pg_proc p
           WHERE p.pronamespace IN (
               'forge_agent_worker_api'::regnamespace,
               'forge_coding_worker_api'::regnamespace
           )
             AND has_function_privilege('forge_coding_reconciler', p.oid, 'EXECUTE')
       )
    THEN
        RAISE EXCEPTION 'Unexpected Forge coding reconciler privileges';
    END IF;
END;
$verify$;

SELECT version, name, applied_at
FROM forge_agent.schema_migrations
ORDER BY version;

-- Validate the optional reader before adjusting only its reviewed ACL entry.
\ir verify-external-agent.sql

\ir verify-muse-read-access.sql

\ir verify-task-events.sql

\ir verify-external-workflows.sql

\ir verify-feedback.sql

\ir verify-autonomy.sql

COMMIT;
