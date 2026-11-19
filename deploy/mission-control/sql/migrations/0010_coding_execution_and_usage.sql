-- Migration 0010: Durable coding execution, chunked results, reconciliation,
-- trusted worker grants, and provider usage snapshots/outbox.

ALTER TABLE forge_agent.coding_tasks
    ADD COLUMN title text,
    ADD COLUMN latest_attempt_id uuid,
    ADD COLUMN resume_workspace_digest text,
    ADD COLUMN accepted_at timestamptz,
    ADD COLUMN review_note text;

UPDATE forge_agent.coding_tasks
SET title = pg_catalog.left(
    pg_catalog.regexp_replace(
        pg_catalog.btrim(
            (
                SELECT line
                FROM pg_catalog.unnest(pg_catalog.regexp_split_to_array(prompt, E'\r?\n'))
                    WITH ORDINALITY AS lines(line, pos)
                WHERE pg_catalog.btrim(line, E' \t\r\n') <> ''
                ORDER BY pos
                LIMIT 1
            ),
            E' \t\r\n'
        ),
        '[[:space:]]+',
        ' ',
        'g'
    ),
    128
)
WHERE title IS NULL;

ALTER TABLE forge_agent.coding_tasks
    DROP CONSTRAINT coding_tasks_pinned_recipe_check,
    DROP CONSTRAINT coding_tasks_status_check,
    DROP CONSTRAINT coding_tasks_status_revision_check,
    ADD CONSTRAINT coding_tasks_title_check CHECK (
        title IS NULL
        OR (
            char_length(title) >= 1
            AND char_length(title) <= 128
            AND octet_length(title) >= 1
            AND octet_length(title) <= 512
            AND btrim(title) <> ''
        )
    ),
    ADD CONSTRAINT coding_tasks_resume_workspace_digest_check CHECK (
        resume_workspace_digest IS NULL
        OR resume_workspace_digest ~ '^[0-9a-f]{64}$'
    ),
    ADD CONSTRAINT coding_tasks_review_note_check CHECK (
        review_note IS NULL
        OR octet_length(review_note) <= 2048
    ),
    ADD CONSTRAINT coding_tasks_pinned_recipe_check CHECK (
        (
            jsonb_typeof(pinned_recipe) = 'object'
            AND pinned_recipe ? 'version'
            AND pinned_recipe ? 'backend'
            AND jsonb_typeof(pinned_recipe->'backend') = 'string'
            AND pinned_recipe->>'backend' IN ('antigravity', 'codex')
            AND pinned_recipe ? 'model'
            AND jsonb_typeof(pinned_recipe->'model') = 'string'
            AND octet_length(pinned_recipe->>'model') >= 1
            AND octet_length(pinned_recipe->>'model') <= 128
            AND btrim(pinned_recipe->>'model') <> ''
            AND (pinned_recipe->>'model') ~ '^[ -~]+$'
            AND pinned_recipe ? 'resource_profile'
            AND jsonb_typeof(pinned_recipe->'resource_profile') = 'string'
            AND (pinned_recipe->>'resource_profile') ~ '^[A-Za-z0-9_.-]{1,64}$'
            AND pinned_recipe ? 'mode'
            AND jsonb_typeof(pinned_recipe->'mode') = 'string'
            AND pinned_recipe->>'mode' IN ('plan', 'review', 'implement')
            AND pinned_recipe ? 'generation_token_limit'
            AND jsonb_typeof(pinned_recipe->'generation_token_limit') = 'null'
            AND (
                (
                    pinned_recipe->'version' = '1'::jsonb
                    AND pinned_recipe - ARRAY[
                        'version', 'backend', 'model', 'resource_profile', 'mode', 'generation_token_limit'
                    ] = '{}'::jsonb
                )
                OR (
                    pinned_recipe->'version' = '2'::jsonb
                    AND pinned_recipe - ARRAY[
                        'version', 'backend', 'model', 'resource_profile', 'mode',
                        'generation_token_limit', 'policy_digest'
                    ] = '{}'::jsonb
                    AND pinned_recipe ? 'policy_digest'
                    AND jsonb_typeof(pinned_recipe->'policy_digest') = 'string'
                    AND (pinned_recipe->>'policy_digest') ~ '^[0-9a-f]{64}$'
                )
            )
        ) IS TRUE
    ),
    ADD CONSTRAINT coding_tasks_status_check CHECK (
        status IN (
            'queued', 'running', 'cancel_requested', 'awaiting_review', 'completed',
            'cancelled', 'failed', 'quota_blocked', 'blocked_auth', 'blocked_permission',
            'needs_reconcile'
        )
    ),
    ADD CONSTRAINT coding_tasks_status_revision_check CHECK (
        (
            (status = 'queued' AND cancelled_at IS NULL AND accepted_at IS NULL)
            OR (status = 'cancelled' AND cancelled_at IS NOT NULL AND accepted_at IS NULL AND revision > 1)
            OR (status = 'completed' AND accepted_at IS NOT NULL AND cancelled_at IS NULL AND revision > 1)
            OR (
                status IN (
                    'running', 'cancel_requested', 'awaiting_review', 'failed',
                    'quota_blocked', 'blocked_auth', 'blocked_permission', 'needs_reconcile'
                )
                AND cancelled_at IS NULL
                AND accepted_at IS NULL
                AND revision > 1
            )
        ) IS TRUE
    );

ALTER TABLE forge_agent.coding_events
    DROP CONSTRAINT coding_events_kind_check,
    DROP CONSTRAINT coding_events_sequence_kind_check,
    ADD CONSTRAINT coding_events_kind_check CHECK (
        event_kind IN (
            'queued', 'started', 'checkpoint', 'cancel_requested', 'cancelled',
            'awaiting_review', 'completed', 'failed', 'quota_blocked',
            'blocked_auth', 'blocked_permission', 'needs_reconcile', 'resumed'
        )
    ),
    ADD CONSTRAINT coding_events_sequence_kind_check CHECK (
        (
            (sequence = 1 AND event_kind = 'queued')
            OR (sequence > 1 AND event_kind <> 'queued')
        ) IS TRUE
    );

ALTER TABLE forge_agent.coding_control_receipts
    DROP CONSTRAINT coding_control_receipts_action_check,
    ADD CONSTRAINT coding_control_receipts_action_check CHECK (
        action IN ('cancel', 'accept', 'resume')
    ),
    ADD CONSTRAINT coding_control_receipts_task_key_unique UNIQUE (
        owner_id, project_alias, task_id, idempotency_key
    );

-- Trusted admin-owned worker grants keyed by PostgreSQL session_user.
CREATE TABLE forge_agent.coding_worker_grants (
    worker_role name NOT NULL,
    owner_id text NOT NULL
        CONSTRAINT coding_worker_grants_owner_id_check CHECK (
            octet_length(owner_id) >= 1
            AND octet_length(owner_id) <= 128
            AND btrim(owner_id) <> ''
            AND owner_id ~ '^[ -~]+$'
        ),
    project_alias text NOT NULL
        CONSTRAINT coding_worker_grants_project_alias_check CHECK (
            project_alias ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    agent_profile text NOT NULL
        CONSTRAINT coding_worker_grants_agent_profile_check CHECK (
            agent_profile ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    mode text NOT NULL
        CONSTRAINT coding_worker_grants_mode_check CHECK (
            mode IN ('plan', 'review', 'implement')
        ),
    granted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT coding_worker_grants_pkey PRIMARY KEY (
        worker_role, owner_id, project_alias, agent_profile, mode
    )
);

-- Worker heartbeat advertisements (already intersected with trusted worker grants).
CREATE TABLE forge_agent.coding_worker_readiness (
    worker_role name NOT NULL,
    owner_id text NOT NULL
        CONSTRAINT coding_worker_readiness_owner_id_check CHECK (
            octet_length(owner_id) >= 1
            AND octet_length(owner_id) <= 128
            AND btrim(owner_id) <> ''
            AND owner_id ~ '^[ -~]+$'
        ),
    worker_id text NOT NULL
        CONSTRAINT coding_worker_readiness_worker_id_check CHECK (
            worker_id ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    incarnation uuid NOT NULL,
    policy_digest text NOT NULL
        CONSTRAINT coding_worker_readiness_policy_digest_check CHECK (
            policy_digest ~ '^[0-9a-f]{64}$'
        ),
    projects jsonb NOT NULL
        CONSTRAINT coding_worker_readiness_projects_check CHECK (
            jsonb_typeof(projects) = 'array'
        ),
    profiles jsonb NOT NULL
        CONSTRAINT coding_worker_readiness_profiles_check CHECK (
            jsonb_typeof(profiles) = 'array'
        ),
    observed_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT coding_worker_readiness_pkey PRIMARY KEY (worker_role, owner_id, worker_id)
);

CREATE INDEX coding_worker_readiness_by_owner_observed
    ON forge_agent.coding_worker_readiness (owner_id, observed_at DESC);

-- Durable fenced execution attempts.
CREATE TABLE forge_agent.coding_attempts (
    owner_id text NOT NULL,
    project_alias text NOT NULL,
    task_id uuid NOT NULL,
    attempt_id uuid NOT NULL,
    attempt_no integer NOT NULL
        CONSTRAINT coding_attempts_no_check CHECK (attempt_no >= 1),
    attempt_token uuid NOT NULL,
    agent_profile text NOT NULL
        CONSTRAINT coding_attempts_agent_profile_check CHECK (
            agent_profile ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    pinned_recipe jsonb NOT NULL
        CONSTRAINT coding_attempts_pinned_recipe_check CHECK (
            (jsonb_typeof(pinned_recipe) = 'object'
             AND pinned_recipe - ARRAY[
                 'version', 'backend', 'model', 'resource_profile', 'mode',
                 'generation_token_limit', 'policy_digest'
             ] = '{}'::jsonb
             AND pinned_recipe ?& ARRAY[
                 'version', 'backend', 'model', 'resource_profile', 'mode',
                 'generation_token_limit', 'policy_digest'
             ]
             AND pinned_recipe->'version' = '2'::jsonb
             AND pinned_recipe->>'backend' IN ('antigravity', 'codex')
             AND jsonb_typeof(pinned_recipe->'model') = 'string'
             AND octet_length(pinned_recipe->>'model') BETWEEN 1 AND 128
             AND pinned_recipe->>'resource_profile' = 'forge-coding-v1'
             AND pinned_recipe->>'mode' IN ('plan', 'review', 'implement')
             AND jsonb_typeof(pinned_recipe->'generation_token_limit') = 'null'
             AND (pinned_recipe->>'policy_digest') ~ '^[0-9a-f]{64}$') IS TRUE
        ),
    recipe_digest text NOT NULL
        CONSTRAINT coding_attempts_recipe_digest_check CHECK (
            recipe_digest ~ '^[0-9a-f]{64}$'
        ),
    worker_role name NOT NULL,
    worker_id text NOT NULL
        CONSTRAINT coding_attempts_worker_id_check CHECK (
            worker_id ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    worker_incarnation uuid NOT NULL,
    unit_name text NOT NULL
        CONSTRAINT coding_attempts_unit_name_check CHECK (
            unit_name ~ '^forge-coding-[0-9a-f]{32}$'
        ),
    phase text NOT NULL
        CONSTRAINT coding_attempts_phase_check CHECK (
            phase IN ('starting', 'running', 'stopping', 'stopped', 'unknown')
        ),
    reservation_active boolean NOT NULL DEFAULT true,
    outcome text
        CONSTRAINT coding_attempts_outcome_check CHECK (
            outcome IS NULL
            OR outcome IN (
                'awaiting_review', 'failed', 'quota_blocked', 'blocked_auth',
                'blocked_permission', 'cancelled', 'needs_reconcile'
            )
        ),
    outcome_digest text
        CONSTRAINT coding_attempts_outcome_digest_check CHECK (
            outcome_digest IS NULL
            OR outcome_digest ~ '^[0-9a-f]{64}$'
        ),
    publish_receipt jsonb
        CONSTRAINT coding_attempts_publish_receipt_check CHECK (
            publish_receipt IS NULL
            OR jsonb_typeof(publish_receipt) = 'object'
        ),
    result_sha256 text
        CONSTRAINT coding_attempts_result_sha256_check CHECK (
            result_sha256 IS NULL
            OR result_sha256 ~ '^[0-9a-f]{64}$'
        ),
    result_byte_count bigint
        CONSTRAINT coding_attempts_result_byte_count_check CHECK (
            result_byte_count IS NULL
            OR (result_byte_count >= 1 AND result_byte_count <= 16777216)
        ),
    result_chunk_count integer
        CONSTRAINT coding_attempts_result_chunk_count_check CHECK (
            result_chunk_count IS NULL
            OR (result_chunk_count >= 1 AND result_chunk_count <= 1024)
        ),
    summary text NOT NULL DEFAULT ''
        CONSTRAINT coding_attempts_summary_check CHECK (
            octet_length(summary) <= 4096
        ),
    usage_json jsonb
        CONSTRAINT coding_attempts_usage_json_check CHECK (
            usage_json IS NULL
            OR jsonb_typeof(usage_json) = 'object'
        ),
    stop_proof jsonb
        CONSTRAINT coding_attempts_stop_proof_check CHECK (
            stop_proof IS NULL
            OR (
                jsonb_typeof(stop_proof) = 'object'
                AND stop_proof - ARRAY['unit_name', 'scope_stopped', 'source', 'checked_at'] = '{}'::jsonb
                AND (stop_proof->>'unit_name') = unit_name
                AND (stop_proof->>'source') = 'systemd'
                AND jsonb_typeof(stop_proof->'scope_stopped') = 'boolean'
            )
        ),
    resume_workspace_digest text
        CONSTRAINT coding_attempts_resume_workspace_digest_check CHECK (
            resume_workspace_digest IS NULL
            OR resume_workspace_digest ~ '^[0-9a-f]{64}$'
        ),
    lease_expires_at timestamptz,
    deadline_at timestamptz NOT NULL,
    started_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    finished_at timestamptz,
    published_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT coding_attempts_pkey PRIMARY KEY (owner_id, project_alias, task_id, attempt_id),
    CONSTRAINT coding_attempts_task_fk FOREIGN KEY (owner_id, project_alias, task_id)
        REFERENCES forge_agent.coding_tasks (owner_id, project_alias, id),
    CONSTRAINT coding_attempts_id_unique UNIQUE (attempt_id),
    CONSTRAINT coding_attempts_no_unique UNIQUE (owner_id, project_alias, task_id, attempt_no),
    CONSTRAINT coding_attempts_unit_name_unique UNIQUE (unit_name),
    CONSTRAINT coding_attempts_publish_state_check CHECK (
        (
            (outcome IS NULL AND outcome_digest IS NULL AND publish_receipt IS NULL AND published_at IS NULL)
            OR (outcome IS NOT NULL AND outcome_digest IS NOT NULL AND publish_receipt IS NOT NULL AND published_at IS NOT NULL)
        ) IS TRUE
    ),
    CONSTRAINT coding_attempts_result_manifest_check CHECK (
        (
            (result_sha256 IS NULL AND result_byte_count IS NULL AND result_chunk_count IS NULL)
            OR (result_sha256 IS NOT NULL AND result_byte_count IS NOT NULL AND result_chunk_count IS NOT NULL)
        ) IS TRUE
    ),
    CONSTRAINT coding_attempts_awaiting_review_manifest_check CHECK (
        outcome IS NULL OR outcome <> 'awaiting_review' OR result_sha256 IS NOT NULL
    )
);

CREATE UNIQUE INDEX coding_attempts_one_open_per_task
    ON forge_agent.coding_attempts (owner_id, project_alias, task_id)
    WHERE lease_expires_at IS NOT NULL;

CREATE UNIQUE INDEX coding_attempts_one_reserved_per_task
    ON forge_agent.coding_attempts (owner_id, project_alias, task_id)
    WHERE reservation_active;

CREATE INDEX coding_attempts_by_task_no
    ON forge_agent.coding_attempts (owner_id, project_alias, task_id, attempt_no DESC);

CREATE INDEX coding_attempts_by_lease_expiry
    ON forge_agent.coding_attempts (lease_expires_at)
    WHERE lease_expires_at IS NOT NULL;

CREATE INDEX coding_attempts_by_project_usage
    ON forge_agent.coding_attempts (owner_id, project_alias, published_at)
    WHERE published_at IS NOT NULL;

-- Staging and published UTF-8 result chunks (<= 16 KiB/chunk, <= 1024 chunks, <= 16 MiB).
CREATE TABLE forge_agent.coding_result_chunks (
    owner_id text NOT NULL,
    project_alias text NOT NULL,
    task_id uuid NOT NULL,
    attempt_id uuid NOT NULL,
    chunk_index integer NOT NULL
        CONSTRAINT coding_result_chunks_index_check CHECK (
            chunk_index >= 0 AND chunk_index <= 1023
        ),
    content text NOT NULL
        CONSTRAINT coding_result_chunks_content_check CHECK (
            octet_length(content) >= 1 AND octet_length(content) <= 16384
        ),
    byte_length integer NOT NULL
        CONSTRAINT coding_result_chunks_byte_length_check CHECK (
            byte_length >= 1 AND byte_length <= 16384
        ),
    content_sha256 text NOT NULL
        CONSTRAINT coding_result_chunks_sha256_check CHECK (
            content_sha256 ~ '^[0-9a-f]{64}$'
        ),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT coding_result_chunks_pkey PRIMARY KEY (
        owner_id, project_alias, task_id, attempt_id, chunk_index
    ),
    CONSTRAINT coding_result_chunks_attempt_fk FOREIGN KEY (
        owner_id, project_alias, task_id, attempt_id
    ) REFERENCES forge_agent.coding_attempts (
        owner_id, project_alias, task_id, attempt_id
    ),
    CONSTRAINT coding_result_chunks_length_match_check CHECK (
        byte_length = octet_length(content)
    )
);

-- Append-only trusted reconciler receipts proving old scope stopped + workspace digest locked.
CREATE TABLE forge_agent.coding_reconciliation_receipts (
    receipt_id uuid NOT NULL,
    owner_id text NOT NULL,
    project_alias text NOT NULL,
    task_id uuid NOT NULL,
    attempt_id uuid NOT NULL,
    task_revision bigint NOT NULL
        CONSTRAINT coding_reconciliation_receipts_revision_check CHECK (
            task_revision >= 1 AND task_revision <= 9007199254740991
        ),
    workspace_digest text NOT NULL
        CONSTRAINT coding_reconciliation_receipts_digest_check CHECK (
            workspace_digest ~ '^[0-9a-f]{64}$'
        ),
    stop_proof jsonb NOT NULL
        CONSTRAINT coding_reconciliation_receipts_stop_proof_check CHECK (
            jsonb_typeof(stop_proof) = 'object'
            AND stop_proof - ARRAY['unit_name', 'scope_stopped', 'source', 'checked_at'] = '{}'::jsonb
            AND (stop_proof->>'source') = 'systemd'
            AND stop_proof->'scope_stopped' = 'true'::jsonb
        ),
    reconciled_by name NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT coding_reconciliation_receipts_pkey PRIMARY KEY (
        owner_id, project_alias, task_id, receipt_id
    ),
    CONSTRAINT coding_reconciliation_receipts_id_unique UNIQUE (receipt_id),
    CONSTRAINT coding_reconciliation_receipts_attempt_fk FOREIGN KEY (
        owner_id, project_alias, task_id, attempt_id
    ) REFERENCES forge_agent.coding_attempts (
        owner_id, project_alias, task_id, attempt_id
    )
);

CREATE INDEX coding_reconciliation_receipts_by_task_rev
    ON forge_agent.coding_reconciliation_receipts (
        owner_id, project_alias, task_id, task_revision DESC, created_at DESC
    );

-- Provider usage snapshots (public-safe fields readable by runner UsageStore).
CREATE TABLE forge_agent.coding_provider_usage (
    owner_id text NOT NULL
        CONSTRAINT coding_provider_usage_owner_id_check CHECK (
            octet_length(owner_id) >= 1
            AND octet_length(owner_id) <= 128
            AND btrim(owner_id) <> ''
            AND owner_id ~ '^[ -~]+$'
        ),
    profile_alias text NOT NULL
        CONSTRAINT coding_provider_usage_profile_alias_check CHECK (
            profile_alias ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    current_generation bigint NOT NULL DEFAULT 0
        CONSTRAINT coding_provider_usage_current_generation_check CHECK (
            current_generation >= 0
        ),
    configuration_epoch bigint NOT NULL DEFAULT 1
        CONSTRAINT coding_provider_usage_configuration_epoch_check CHECK (
            configuration_epoch >= 1
        ),
    recorded_generation bigint NOT NULL DEFAULT 0
        CONSTRAINT coding_provider_usage_recorded_generation_check CHECK (
            recorded_generation >= 0
        ),
    recorded_epoch bigint NOT NULL DEFAULT 0
        CONSTRAINT coding_provider_usage_recorded_epoch_check CHECK (
            recorded_epoch >= 0
        ),
    snapshot_state text NOT NULL DEFAULT 'unknown'
        CONSTRAINT coding_provider_usage_snapshot_state_check CHECK (
            snapshot_state IN ('fresh', 'stale', 'unknown', 'unavailable', 'auth_required')
        ),
    observed_at timestamptz,
    source text NOT NULL DEFAULT 'unobserved'
        CONSTRAINT coding_provider_usage_source_check CHECK (
            octet_length(source) >= 1 AND octet_length(source) <= 128
        ),
    version text NOT NULL DEFAULT 'unknown'
        CONSTRAINT coding_provider_usage_version_check CHECK (
            octet_length(version) >= 1 AND octet_length(version) <= 64
        ),
    last_error text
        CONSTRAINT coding_provider_usage_last_error_check CHECK (
            last_error IS NULL OR last_error ~ '^[a-z0-9_.-]{1,64}$'
        ),
    allowance jsonb NOT NULL DEFAULT '{"state":"unknown","reason":null,"ordinary_usage_allowed":null,"buckets":[]}'::jsonb
        CONSTRAINT coding_provider_usage_allowance_check CHECK (
            jsonb_typeof(allowance) = 'object'
        ),
    account_activity jsonb NOT NULL DEFAULT '{"state":"unknown","lifetime_tokens":null,"daily":[]}'::jsonb
        CONSTRAINT coding_provider_usage_account_activity_check CHECK (
            jsonb_typeof(account_activity) = 'object'
        ),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT coding_provider_usage_pkey PRIMARY KEY (owner_id, profile_alias),
    CONSTRAINT coding_provider_usage_generation_order_check CHECK (
        recorded_generation <= current_generation
    ),
    CONSTRAINT coding_provider_usage_epoch_order_check CHECK (
        recorded_epoch <= configuration_epoch
    )
);

-- Internal HMAC account identities isolated from runner/browser (worker SECURITY DEFINER only).
CREATE TABLE forge_agent.coding_provider_usage_identities (
    owner_id text NOT NULL,
    profile_alias text NOT NULL,
    account_identity text NOT NULL
        CONSTRAINT coding_provider_usage_identities_hmac_check CHECK (
            account_identity ~ '^[0-9a-f]{64}$'
        ),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT coding_provider_usage_identities_pkey PRIMARY KEY (owner_id, profile_alias),
    CONSTRAINT coding_provider_usage_identities_usage_fk FOREIGN KEY (owner_id, profile_alias)
        REFERENCES forge_agent.coding_provider_usage (owner_id, profile_alias)
);

-- Coalesced bounded outbox for metadata-only provider usage refresh requests.
CREATE TABLE forge_agent.coding_usage_refresh_requests (
    owner_id text NOT NULL
        CONSTRAINT coding_usage_refresh_requests_owner_id_check CHECK (
            octet_length(owner_id) >= 1
            AND octet_length(owner_id) <= 128
            AND btrim(owner_id) <> ''
            AND owner_id ~ '^[ -~]+$'
        ),
    profile_alias text NOT NULL
        CONSTRAINT coding_usage_refresh_requests_profile_alias_check CHECK (
            profile_alias ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    refresh_id uuid NOT NULL,
    project_alias text NOT NULL
        CONSTRAINT coding_usage_refresh_requests_project_alias_check CHECK (
            project_alias ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    status text NOT NULL DEFAULT 'pending'
        CONSTRAINT coding_usage_refresh_requests_status_check CHECK (
            status IN ('pending', 'in_progress', 'completed', 'failed')
        ),
    generation bigint
        CONSTRAINT coding_usage_refresh_requests_generation_check CHECK (
            generation IS NULL OR generation >= 1
        ),
    configuration_epoch bigint
        CONSTRAINT coding_usage_refresh_requests_epoch_check CHECK (
            configuration_epoch IS NULL OR configuration_epoch >= 1
        ),
    error_code text
        CONSTRAINT coding_usage_refresh_requests_error_code_check CHECK (
            error_code IS NULL OR error_code ~ '^[a-z0-9_.-]{1,64}$'
        ),
    requested_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    claimed_at timestamptz,
    completed_at timestamptz,
    CONSTRAINT coding_usage_refresh_requests_pkey PRIMARY KEY (owner_id, profile_alias, refresh_id)
);

CREATE UNIQUE INDEX coding_usage_refresh_one_open_per_profile
    ON forge_agent.coding_usage_refresh_requests (owner_id, profile_alias)
    WHERE status IN ('pending', 'in_progress');

CREATE INDEX coding_usage_refresh_pending_order
    ON forge_agent.coding_usage_refresh_requests (owner_id, requested_at, profile_alias)
    WHERE status = 'pending';

REVOKE ALL ON TABLE
    forge_agent.coding_worker_grants,
    forge_agent.coding_worker_readiness,
    forge_agent.coding_attempts,
    forge_agent.coding_result_chunks,
    forge_agent.coding_reconciliation_receipts,
    forge_agent.coding_provider_usage,
    forge_agent.coding_provider_usage_identities,
    forge_agent.coding_usage_refresh_requests
FROM PUBLIC;

-- Scoped worker and reconciler API schemas.
CREATE SCHEMA forge_coding_worker_api;
REVOKE ALL ON SCHEMA forge_coding_worker_api FROM PUBLIC;

CREATE SCHEMA forge_coding_reconciler_api;
REVOKE ALL ON SCHEMA forge_coding_reconciler_api FROM PUBLIC;

-- Internal validator: exact 4-key systemd stop proof.
CREATE FUNCTION forge_coding_worker_api._validate_stop_proof(p_proof jsonb, p_unit_name text)
RETURNS boolean
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_ts timestamptz;
BEGIN
    IF p_proof IS NULL OR pg_catalog.jsonb_typeof(p_proof) IS DISTINCT FROM 'object' THEN
        RETURN false;
    END IF;
    IF p_proof - ARRAY['unit_name', 'scope_stopped', 'source', 'checked_at'] <> '{}'::jsonb
       OR NOT (p_proof ?& ARRAY['unit_name', 'scope_stopped', 'source', 'checked_at']) THEN
        RETURN false;
    END IF;
    IF pg_catalog.jsonb_typeof(p_proof->'unit_name') IS DISTINCT FROM 'string'
       OR (p_proof->>'unit_name') IS DISTINCT FROM p_unit_name
       OR pg_catalog.jsonb_typeof(p_proof->'scope_stopped') IS DISTINCT FROM 'boolean'
       OR pg_catalog.jsonb_typeof(p_proof->'source') IS DISTINCT FROM 'string'
       OR (p_proof->>'source') IS DISTINCT FROM 'systemd'
       OR pg_catalog.jsonb_typeof(p_proof->'checked_at') IS DISTINCT FROM 'string'
       OR (p_proof->>'checked_at') !~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{1,6})?Z$' THEN
        RETURN false;
    END IF;
    BEGIN
        v_ts := (p_proof->>'checked_at')::timestamptz;
    EXCEPTION WHEN OTHERS THEN
        RETURN false;
    END;
    RETURN v_ts IS NOT NULL;
END;
$fn$;

-- Internal validator: exact 7-key normalized attempt usage JSON.
CREATE FUNCTION forge_coding_worker_api._validate_usage_json(p_usage jsonb)
RETURNS boolean
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_key text;
BEGIN
    IF p_usage IS NULL OR pg_catalog.jsonb_typeof(p_usage) IS DISTINCT FROM 'object' THEN
        RETURN false;
    END IF;
    IF p_usage - ARRAY[
            'input_tokens', 'output_tokens', 'cached_input_tokens', 'total_tokens',
            'source', 'coverage', 'session_provenance'
        ] <> '{}'::jsonb
       OR NOT (p_usage ?& ARRAY[
            'input_tokens', 'output_tokens', 'cached_input_tokens', 'total_tokens',
            'source', 'coverage', 'session_provenance'
        ]) THEN
        RETURN false;
    END IF;
    FOREACH v_key IN ARRAY ARRAY['input_tokens', 'output_tokens', 'cached_input_tokens', 'total_tokens'] LOOP
        IF pg_catalog.jsonb_typeof(p_usage->v_key) = 'null' THEN
            CONTINUE;
        END IF;
        IF pg_catalog.jsonb_typeof(p_usage->v_key) IS DISTINCT FROM 'string'
           OR (p_usage->>v_key) !~ '^(0|[1-9][0-9]{0,39})$' THEN
            RETURN false;
        END IF;
    END LOOP;
    IF pg_catalog.jsonb_typeof(p_usage->'source') IS DISTINCT FROM 'string'
       OR (p_usage->>'source') NOT IN ('codex-exec-turn-v1', 'agy-result-v1')
       OR pg_catalog.jsonb_typeof(p_usage->'coverage') IS DISTINCT FROM 'string'
       OR (p_usage->>'coverage') NOT IN ('complete', 'partial', 'unknown')
       OR pg_catalog.jsonb_typeof(p_usage->'session_provenance') IS DISTINCT FROM 'string'
       OR (p_usage->>'session_provenance') NOT IN ('fresh_session', 'unverified_baseline') THEN
        RETURN false;
    END IF;
    RETURN true;
END;
$fn$;

-- Internal validator: exact 8-key provider usage snapshot from collector.
CREATE FUNCTION forge_coding_worker_api._validate_provider_snapshot(p_snapshot jsonb)
RETURNS boolean
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_allowance jsonb;
    v_activity jsonb;
    v_bucket jsonb;
    v_day jsonb;
    v_num numeric;
    v_ts timestamptz;
    v_date date;
BEGIN
    IF p_snapshot IS NULL OR pg_catalog.jsonb_typeof(p_snapshot) IS DISTINCT FROM 'object' THEN
        RETURN false;
    END IF;
    IF p_snapshot - ARRAY[
            'snapshot_state', 'observed_at', 'source', 'version',
            'last_error', 'allowance', 'account_activity', 'account_identity'
        ] <> '{}'::jsonb
       OR NOT (p_snapshot ?& ARRAY[
            'snapshot_state', 'observed_at', 'source', 'version',
            'last_error', 'allowance', 'account_activity', 'account_identity'
        ]) THEN
        RETURN false;
    END IF;
    IF pg_catalog.jsonb_typeof(p_snapshot->'snapshot_state') IS DISTINCT FROM 'string'
       OR (p_snapshot->>'snapshot_state') NOT IN ('fresh', 'stale', 'unknown', 'unavailable', 'auth_required') THEN
        RETURN false;
    END IF;
    IF pg_catalog.jsonb_typeof(p_snapshot->'observed_at') <> 'null' THEN
        IF pg_catalog.jsonb_typeof(p_snapshot->'observed_at') IS DISTINCT FROM 'string'
           OR (p_snapshot->>'observed_at') !~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{1,6})?Z$' THEN
            RETURN false;
        END IF;
        BEGIN
            v_ts := (p_snapshot->>'observed_at')::timestamptz;
        EXCEPTION WHEN OTHERS THEN
            RETURN false;
        END;
    END IF;
    IF pg_catalog.jsonb_typeof(p_snapshot->'source') IS DISTINCT FROM 'string'
       OR pg_catalog.octet_length(p_snapshot->>'source') NOT BETWEEN 1 AND 128
       OR pg_catalog.btrim(p_snapshot->>'source') = '' THEN
        RETURN false;
    END IF;
    IF pg_catalog.jsonb_typeof(p_snapshot->'version') IS DISTINCT FROM 'string'
       OR pg_catalog.octet_length(p_snapshot->>'version') NOT BETWEEN 1 AND 64
       OR pg_catalog.btrim(p_snapshot->>'version') = '' THEN
        RETURN false;
    END IF;
    IF pg_catalog.jsonb_typeof(p_snapshot->'last_error') <> 'null' THEN
        IF pg_catalog.jsonb_typeof(p_snapshot->'last_error') IS DISTINCT FROM 'string'
           OR (p_snapshot->>'last_error') !~ '^[a-z0-9_.-]{1,64}$' THEN
            RETURN false;
        END IF;
    END IF;
    IF pg_catalog.jsonb_typeof(p_snapshot->'account_identity') <> 'null' THEN
        IF pg_catalog.jsonb_typeof(p_snapshot->'account_identity') IS DISTINCT FROM 'string'
           OR (p_snapshot->>'account_identity') !~ '^[0-9a-f]{64}$' THEN
            RETURN false;
        END IF;
    END IF;

    v_allowance := p_snapshot->'allowance';
    IF pg_catalog.jsonb_typeof(v_allowance) IS DISTINCT FROM 'object'
       OR v_allowance - ARRAY['state', 'reason', 'ordinary_usage_allowed', 'buckets'] <> '{}'::jsonb
       OR NOT (v_allowance ?& ARRAY['state', 'reason', 'ordinary_usage_allowed', 'buckets'])
       OR pg_catalog.jsonb_typeof(v_allowance->'state') IS DISTINCT FROM 'string'
       OR (v_allowance->>'state') NOT IN ('available', 'unsupported', 'unknown', 'auth_required', 'unavailable')
       OR (pg_catalog.jsonb_typeof(v_allowance->'reason') <> 'null'
           AND (pg_catalog.jsonb_typeof(v_allowance->'reason') IS DISTINCT FROM 'string'
                OR pg_catalog.octet_length(v_allowance->>'reason') NOT BETWEEN 1 AND 128))
       OR pg_catalog.jsonb_typeof(v_allowance->'ordinary_usage_allowed') NOT IN ('null', 'boolean')
       OR pg_catalog.jsonb_typeof(v_allowance->'buckets') IS DISTINCT FROM 'array'
       OR pg_catalog.jsonb_array_length(v_allowance->'buckets') > 16 THEN
        RETURN false;
    END IF;

    FOR v_bucket IN SELECT value FROM pg_catalog.jsonb_array_elements(v_allowance->'buckets') LOOP
        IF pg_catalog.jsonb_typeof(v_bucket) IS DISTINCT FROM 'object'
           OR v_bucket - ARRAY['key', 'label', 'used_percent', 'window_minutes', 'resets_at'] <> '{}'::jsonb
           OR NOT (v_bucket ?& ARRAY['key', 'label', 'used_percent', 'window_minutes', 'resets_at'])
           OR pg_catalog.jsonb_typeof(v_bucket->'key') IS DISTINCT FROM 'string'
           OR (v_bucket->>'key') !~ '^[A-Za-z0-9_.-]{1,64}$'
           OR pg_catalog.jsonb_typeof(v_bucket->'label') IS DISTINCT FROM 'string'
           OR pg_catalog.octet_length(v_bucket->>'label') NOT BETWEEN 1 AND 128
           OR pg_catalog.btrim(v_bucket->>'label') = '' THEN
            RETURN false;
        END IF;
        IF pg_catalog.jsonb_typeof(v_bucket->'used_percent') <> 'null' THEN
            IF pg_catalog.jsonb_typeof(v_bucket->'used_percent') IS DISTINCT FROM 'number' THEN
                RETURN false;
            END IF;
            v_num := (v_bucket->>'used_percent')::numeric;
            IF v_num < 0 OR v_num > 100 THEN
                RETURN false;
            END IF;
        END IF;
        IF pg_catalog.jsonb_typeof(v_bucket->'window_minutes') <> 'null' THEN
            IF pg_catalog.jsonb_typeof(v_bucket->'window_minutes') IS DISTINCT FROM 'number'
               OR (v_bucket->>'window_minutes') !~ '^[1-9][0-9]{0,15}$' THEN
                RETURN false;
            END IF;
        END IF;
        IF pg_catalog.jsonb_typeof(v_bucket->'resets_at') <> 'null' THEN
            IF pg_catalog.jsonb_typeof(v_bucket->'resets_at') IS DISTINCT FROM 'string'
               OR (v_bucket->>'resets_at') !~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{1,6})?Z$' THEN
                RETURN false;
            END IF;
            BEGIN
                v_ts := (v_bucket->>'resets_at')::timestamptz;
            EXCEPTION WHEN OTHERS THEN
                RETURN false;
            END;
        END IF;
    END LOOP;

    v_activity := p_snapshot->'account_activity';
    IF pg_catalog.jsonb_typeof(v_activity) IS DISTINCT FROM 'object'
       OR v_activity - ARRAY['state', 'lifetime_tokens', 'daily'] <> '{}'::jsonb
       OR NOT (v_activity ?& ARRAY['state', 'lifetime_tokens', 'daily'])
       OR pg_catalog.jsonb_typeof(v_activity->'state') IS DISTINCT FROM 'string'
       OR (v_activity->>'state') NOT IN ('available', 'unknown', 'unavailable')
       OR (pg_catalog.jsonb_typeof(v_activity->'lifetime_tokens') <> 'null'
           AND (pg_catalog.jsonb_typeof(v_activity->'lifetime_tokens') IS DISTINCT FROM 'string'
                OR (v_activity->>'lifetime_tokens') !~ '^(0|[1-9][0-9]{0,39})$'))
       OR pg_catalog.jsonb_typeof(v_activity->'daily') IS DISTINCT FROM 'array'
       OR pg_catalog.jsonb_array_length(v_activity->'daily') > 31 THEN
        RETURN false;
    END IF;

    FOR v_day IN SELECT value FROM pg_catalog.jsonb_array_elements(v_activity->'daily') LOOP
        IF pg_catalog.jsonb_typeof(v_day) IS DISTINCT FROM 'object'
           OR v_day - ARRAY['day', 'tokens'] <> '{}'::jsonb
           OR NOT (v_day ?& ARRAY['day', 'tokens'])
           OR pg_catalog.jsonb_typeof(v_day->'day') IS DISTINCT FROM 'string'
           OR (v_day->>'day') !~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}$'
           OR pg_catalog.jsonb_typeof(v_day->'tokens') IS DISTINCT FROM 'string'
           OR (v_day->>'tokens') !~ '^(0|[1-9][0-9]{0,39})$' THEN
            RETURN false;
        END IF;
        BEGIN
            v_date := (v_day->>'day')::date;
        EXCEPTION WHEN OTHERS THEN
            RETURN false;
        END;
    END LOOP;

    RETURN true;
END;
$fn$;

-- Internal helper: lock coding_tasks THEN coding_attempts in strict lock order.
CREATE FUNCTION forge_coding_worker_api._lock_task_and_attempt(
    p_attempt_id uuid,
    p_attempt_token uuid
)
RETURNS forge_agent.coding_attempts
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_locator record;
    v_attempt forge_agent.coding_attempts%ROWTYPE;
BEGIN
    IF p_attempt_id IS NULL OR p_attempt_token IS NULL THEN
        RETURN NULL;
    END IF;
    SELECT a.owner_id, a.project_alias, a.task_id
    INTO v_locator
    FROM forge_agent.coding_attempts a
    WHERE a.attempt_id = p_attempt_id
      AND a.attempt_token = p_attempt_token
      AND a.worker_role = session_user
      AND a.owner_id = pg_catalog.current_setting('forge.coding_owner', true);
    IF NOT FOUND THEN
        RETURN NULL;
    END IF;

    PERFORM 1
    FROM forge_agent.coding_tasks t
    WHERE t.owner_id = v_locator.owner_id
      AND t.project_alias = v_locator.project_alias
      AND t.id = v_locator.task_id
    FOR UPDATE;
    IF NOT FOUND THEN
        RETURN NULL;
    END IF;

    SELECT a.*
    INTO v_attempt
    FROM forge_agent.coding_attempts a
    WHERE a.owner_id = v_locator.owner_id
      AND a.project_alias = v_locator.project_alias
      AND a.task_id = v_locator.task_id
      AND a.attempt_id = p_attempt_id
      AND a.attempt_token = p_attempt_token
      AND a.worker_role = session_user
      AND a.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
    FOR UPDATE;
    RETURN v_attempt;
END;
$fn$;

-- Public worker function 1: advertise configured projects/profiles intersected with trusted grants.
CREATE FUNCTION forge_coding_worker_api.advertise(
    p_worker_id text,
    p_incarnation uuid,
    p_projects jsonb,
    p_profiles jsonb,
    p_policy_digest text
)
RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_item jsonb;
    v_mode jsonb;
    v_owner text;
    v_owner_count integer := 0;
    v_filtered_projects jsonb;
    v_filtered_profiles jsonb;
    v_allowed_modes jsonb;
BEGIN
    IF p_worker_id IS NULL OR p_worker_id !~ '^[A-Za-z0-9_.-]{1,64}$'
       OR p_incarnation IS NULL
       OR p_policy_digest IS NULL OR p_policy_digest !~ '^[0-9a-f]{64}$'
       OR p_projects IS NULL OR pg_catalog.jsonb_typeof(p_projects) IS DISTINCT FROM 'array'
       OR pg_catalog.jsonb_array_length(p_projects) > 64
       OR p_profiles IS NULL OR pg_catalog.jsonb_typeof(p_profiles) IS DISTINCT FROM 'array'
       OR pg_catalog.jsonb_array_length(p_profiles) > 64 THEN
        RAISE EXCEPTION 'Invalid coding worker advertisement' USING ERRCODE = '22023';
    END IF;

    FOR v_item IN SELECT value FROM pg_catalog.jsonb_array_elements(p_projects) LOOP
        IF pg_catalog.jsonb_typeof(v_item) IS DISTINCT FROM 'object'
           OR v_item - ARRAY['alias', 'base_revision'] <> '{}'::jsonb
           OR NOT (v_item ?& ARRAY['alias', 'base_revision'])
           OR pg_catalog.jsonb_typeof(v_item->'alias') IS DISTINCT FROM 'string'
           OR (v_item->>'alias') !~ '^[A-Za-z0-9_.-]{1,64}$'
           OR (pg_catalog.jsonb_typeof(v_item->'base_revision') <> 'null'
               AND (pg_catalog.jsonb_typeof(v_item->'base_revision') IS DISTINCT FROM 'string'
                    OR (v_item->>'base_revision') !~ '^[0-9a-f]{40}$')) THEN
            RAISE EXCEPTION 'Invalid advertised project entry' USING ERRCODE = '22023';
        END IF;
    END LOOP;

    FOR v_item IN SELECT value FROM pg_catalog.jsonb_array_elements(p_profiles) LOOP
        IF pg_catalog.jsonb_typeof(v_item) IS DISTINCT FROM 'object'
           OR v_item - ARRAY['alias', 'backend', 'model', 'modes'] <> '{}'::jsonb
           OR NOT (v_item ?& ARRAY['alias', 'backend', 'model', 'modes'])
           OR pg_catalog.jsonb_typeof(v_item->'alias') IS DISTINCT FROM 'string'
           OR (v_item->>'alias') !~ '^[A-Za-z0-9_.-]{1,64}$'
           OR pg_catalog.jsonb_typeof(v_item->'backend') IS DISTINCT FROM 'string'
           OR (v_item->>'backend') NOT IN ('antigravity', 'codex')
           OR pg_catalog.jsonb_typeof(v_item->'model') IS DISTINCT FROM 'string'
           OR pg_catalog.octet_length(v_item->>'model') NOT BETWEEN 1 AND 128
           OR pg_catalog.btrim(v_item->>'model') = ''
           OR (v_item->>'model') !~ '^[ -~]+$'
           OR pg_catalog.jsonb_typeof(v_item->'modes') IS DISTINCT FROM 'array'
           OR pg_catalog.jsonb_array_length(v_item->'modes') > 3 THEN
            RAISE EXCEPTION 'Invalid advertised profile entry' USING ERRCODE = '22023';
        END IF;
        FOR v_mode IN SELECT value FROM pg_catalog.jsonb_array_elements(v_item->'modes') LOOP
            IF pg_catalog.jsonb_typeof(v_mode) IS DISTINCT FROM 'string'
               OR (v_mode #>> '{}') NOT IN ('plan', 'review', 'implement') THEN
                RAISE EXCEPTION 'Invalid advertised mode' USING ERRCODE = '22023';
            END IF;
        END LOOP;
    END LOOP;

    FOR v_owner IN
        SELECT DISTINCT g.owner_id
        FROM forge_agent.coding_worker_grants g
        WHERE g.worker_role = session_user
              AND g.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
        ORDER BY g.owner_id
    LOOP
        v_owner_count := v_owner_count + 1;

        SELECT coalesce(pg_catalog.jsonb_agg(item), '[]'::jsonb)
        INTO v_filtered_projects
        FROM pg_catalog.jsonb_array_elements(p_projects) AS item
        WHERE EXISTS (
            SELECT 1
            FROM forge_agent.coding_worker_grants g
            WHERE g.worker_role = session_user
              AND g.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
              AND g.owner_id = v_owner
              AND g.project_alias = item->>'alias'
        );

        v_filtered_profiles := '[]'::jsonb;
        FOR v_item IN SELECT value FROM pg_catalog.jsonb_array_elements(p_profiles) LOOP
            SELECT coalesce(pg_catalog.jsonb_agg(DISTINCT m.mode ORDER BY m.mode), '[]'::jsonb)
            INTO v_allowed_modes
            FROM pg_catalog.jsonb_array_elements_text(v_item->'modes') AS m(mode)
            WHERE EXISTS (
                SELECT 1
                FROM forge_agent.coding_worker_grants g
                WHERE g.worker_role = session_user
              AND g.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
                  AND g.owner_id = v_owner
                  AND g.agent_profile = v_item->>'alias'
                  AND g.mode = m.mode
            );
            IF pg_catalog.jsonb_array_length(v_allowed_modes) > 0 THEN
                v_filtered_profiles := v_filtered_profiles || pg_catalog.jsonb_build_array(
                    pg_catalog.jsonb_build_object(
                        'alias', v_item->>'alias',
                        'backend', v_item->>'backend',
                        'model', v_item->>'model',
                        'modes', v_allowed_modes
                    )
                );
            END IF;
        END LOOP;

        INSERT INTO forge_agent.coding_worker_readiness (
            worker_role, owner_id, worker_id, incarnation, policy_digest,
            projects, profiles, observed_at
        )
        VALUES (
            session_user, v_owner, p_worker_id, p_incarnation, p_policy_digest,
            v_filtered_projects, v_filtered_profiles, clock_timestamp()
        )
        ON CONFLICT (worker_role, owner_id, worker_id) DO UPDATE
        SET incarnation = excluded.incarnation,
            policy_digest = excluded.policy_digest,
            projects = excluded.projects,
            profiles = excluded.profiles,
            observed_at = excluded.observed_at;
    END LOOP;

    IF v_owner_count = 0 THEN
        RAISE EXCEPTION 'Coding worker role has no trusted grants' USING ERRCODE = '42501';
    END IF;

    DELETE FROM forge_agent.coding_worker_readiness r
    WHERE r.worker_role = session_user
      AND r.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
      AND r.worker_id = p_worker_id
      AND NOT EXISTS (
          SELECT 1
          FROM forge_agent.coding_worker_grants g
          WHERE g.worker_role = session_user
              AND g.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
            AND g.owner_id = r.owner_id
      );
END;
$fn$;

-- Public worker function 2: SKIP LOCKED claim with policy v2 + grant + reservation checks.
CREATE FUNCTION forge_coding_worker_api.claim(
    p_worker_id text,
    p_incarnation uuid,
    p_allowed_profiles text[]
)
RETURNS jsonb
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_candidate record;
    v_task forge_agent.coding_tasks%ROWTYPE;
    v_attempt_id uuid;
    v_attempt_token uuid;
    v_attempt_no integer;
    v_unit_name text;
    v_now timestamptz;
    v_lease_expires timestamptz;
    v_deadline timestamptz;
    v_resume_digest text;
    v_new_revision bigint;
BEGIN
    IF p_worker_id IS NULL OR p_worker_id !~ '^[A-Za-z0-9_.-]{1,64}$'
       OR p_incarnation IS NULL
       OR p_allowed_profiles IS NULL
       OR pg_catalog.array_length(p_allowed_profiles, 1) IS NULL
       OR pg_catalog.array_length(p_allowed_profiles, 1) > 64 THEN
        RAISE EXCEPTION 'Invalid coding worker claim parameters' USING ERRCODE = '22023';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM forge_agent.coding_worker_grants g
        WHERE g.worker_role = session_user
          AND g.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
    ) THEN
        RAISE EXCEPTION 'Coding worker role has no grant for configured owner' USING ERRCODE = '42501';
    END IF;

    FOR v_candidate IN
        SELECT t.owner_id, t.project_alias, t.id
        FROM forge_agent.coding_tasks t
        JOIN forge_agent.coding_worker_readiness r
          ON r.worker_role = session_user
         AND r.owner_id = t.owner_id
         AND r.worker_id = p_worker_id
         AND r.incarnation = p_incarnation
         AND r.observed_at > clock_timestamp() - interval '90 seconds'
        JOIN forge_agent.coding_worker_grants g
          ON g.worker_role = session_user
              AND g.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
         AND g.owner_id = t.owner_id
         AND g.project_alias = t.project_alias
         AND g.agent_profile = t.agent_profile
         AND g.mode = t.mode
        WHERE t.status = 'queued'
          AND t.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
          AND t.agent_profile = ANY(p_allowed_profiles)
          AND (t.pinned_recipe->'version') = '2'::jsonb
          AND (t.pinned_recipe->>'resource_profile') = 'forge-coding-v1'
          AND (t.pinned_recipe->>'policy_digest') = r.policy_digest
          AND NOT EXISTS (
              SELECT 1
              FROM forge_agent.coding_attempts a
              WHERE a.owner_id = t.owner_id
                AND a.project_alias = t.project_alias
                AND a.task_id = t.id
                AND (a.reservation_active OR a.lease_expires_at IS NOT NULL)
          )
        ORDER BY t.created_at ASC, t.id ASC
    LOOP
        SELECT t.*
        INTO v_task
        FROM forge_agent.coding_tasks t
        WHERE t.owner_id = v_candidate.owner_id
          AND t.project_alias = v_candidate.project_alias
          AND t.id = v_candidate.id
          AND t.status = 'queued'
        FOR UPDATE SKIP LOCKED;
        IF NOT FOUND THEN
            CONTINUE;
        END IF;

        IF EXISTS (
            SELECT 1
            FROM forge_agent.coding_attempts a
            WHERE a.owner_id = v_task.owner_id
              AND a.project_alias = v_task.project_alias
              AND a.task_id = v_task.id
              AND (a.reservation_active OR a.lease_expires_at IS NOT NULL)
        ) THEN
            CONTINUE;
        END IF;

        IF v_task.revision >= 9007199254740991 THEN
            CONTINUE;
        END IF;

        v_now := clock_timestamp();
        v_lease_expires := v_now + interval '60 seconds';
        v_deadline := v_now + interval '1800 seconds';
        v_attempt_id := pg_catalog.gen_random_uuid();
        v_attempt_token := pg_catalog.gen_random_uuid();
        v_unit_name := 'forge-coding-' || pg_catalog.replace(v_attempt_id::text, '-', '');
        v_resume_digest := v_task.resume_workspace_digest;

        SELECT coalesce(max(a.attempt_no), 0) + 1
        INTO v_attempt_no
        FROM forge_agent.coding_attempts a
        WHERE a.owner_id = v_task.owner_id
          AND a.project_alias = v_task.project_alias
          AND a.task_id = v_task.id;

        INSERT INTO forge_agent.coding_attempts (
            owner_id, project_alias, task_id, attempt_id, attempt_no,
            attempt_token, agent_profile, pinned_recipe, recipe_digest,
            worker_role, worker_id, worker_incarnation,
            unit_name, phase, reservation_active, resume_workspace_digest,
            lease_expires_at, deadline_at, started_at, updated_at
        )
        VALUES (
            v_task.owner_id, v_task.project_alias, v_task.id, v_attempt_id, v_attempt_no,
            v_attempt_token, v_task.agent_profile, v_task.pinned_recipe, v_task.recipe_digest,
            session_user, p_worker_id, p_incarnation,
            v_unit_name, 'starting', true, v_resume_digest,
            v_lease_expires, v_deadline, v_now, v_now
        );

        UPDATE forge_agent.coding_tasks
        SET status = 'running',
            revision = revision + 1,
            latest_attempt_id = v_attempt_id,
            resume_workspace_digest = NULL,
            updated_at = v_now
        WHERE owner_id = v_task.owner_id
          AND project_alias = v_task.project_alias
          AND id = v_task.id
        RETURNING revision INTO v_new_revision;

        INSERT INTO forge_agent.coding_events (
            owner_id, project_alias, task_id, sequence, event_kind, payload
        )
        SELECT v_task.owner_id, v_task.project_alias, v_task.id,
               coalesce(max(e.sequence), 0) + 1,
               'started',
               pg_catalog.jsonb_build_object(
                   'attempt_id', v_attempt_id,
                   'attempt_no', v_attempt_no,
                   'worker_id', p_worker_id,
                   'revision', v_new_revision
               )
        FROM forge_agent.coding_events e
        WHERE e.owner_id = v_task.owner_id
          AND e.project_alias = v_task.project_alias
          AND e.task_id = v_task.id;

        RETURN pg_catalog.jsonb_build_object(
            'task_id', v_task.id,
            'project_alias', v_task.project_alias,
            'base_revision', v_task.base_revision,
            'mode', v_task.mode,
            'agent_profile', v_task.agent_profile,
            'prompt', v_task.prompt,
            'acceptance_criteria', v_task.acceptance_criteria,
            'recipe', v_task.pinned_recipe,
            'attempt_id', v_attempt_id,
            'attempt_token', v_attempt_token,
            'worker_id', p_worker_id,
            'incarnation', p_incarnation,
            'lease_expires_at', pg_catalog.to_char(v_lease_expires AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
            'deadline_at', pg_catalog.to_char(v_deadline AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
            'unit_name', v_unit_name,
            'resume_workspace_digest', v_resume_digest
        );
    END LOOP;

    RETURN NULL;
END;
$fn$;

-- Public worker function 3: renew active lease by 60s capped by deadline_at.
CREATE FUNCTION forge_coding_worker_api.renew(
    p_attempt_id uuid,
    p_attempt_token uuid
)
RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_attempt forge_agent.coding_attempts%ROWTYPE;
    v_task_status text;
    v_now timestamptz;
BEGIN
    v_attempt := forge_coding_worker_api._lock_task_and_attempt(p_attempt_id, p_attempt_token);
    v_now := clock_timestamp();
    IF v_attempt.attempt_id IS NULL THEN
        RETURN false;
    END IF;

    SELECT t.status
    INTO v_task_status
    FROM forge_agent.coding_tasks t
    WHERE t.owner_id = v_attempt.owner_id
      AND t.project_alias = v_attempt.project_alias
      AND t.id = v_attempt.task_id;

    IF v_task_status NOT IN ('running', 'cancel_requested')
       OR v_attempt.published_at IS NOT NULL
       OR v_attempt.lease_expires_at IS NULL
       OR v_attempt.lease_expires_at <= v_now
       OR v_attempt.deadline_at <= v_now THEN
        RETURN false;
    END IF;

    UPDATE forge_agent.coding_attempts
    SET lease_expires_at = least(deadline_at, v_now + interval '60 seconds'),
        phase = CASE WHEN phase = 'starting' THEN 'running' ELSE phase END,
        updated_at = v_now
    WHERE owner_id = v_attempt.owner_id
      AND project_alias = v_attempt.project_alias
      AND task_id = v_attempt.task_id
      AND attempt_id = v_attempt.attempt_id;

    RETURN true;
END;
$fn$;

-- Public worker function 4: fenced checkpoint (appends event without bumping task revision).
CREATE FUNCTION forge_coding_worker_api.checkpoint(
    p_attempt_id uuid,
    p_attempt_token uuid,
    p_summary text
)
RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_attempt forge_agent.coding_attempts%ROWTYPE;
    v_task_status text;
    v_now timestamptz;
BEGIN
    IF p_summary IS NULL OR pg_catalog.octet_length(p_summary) > 4096 THEN
        RAISE EXCEPTION 'Invalid checkpoint summary' USING ERRCODE = '22023';
    END IF;

    v_attempt := forge_coding_worker_api._lock_task_and_attempt(p_attempt_id, p_attempt_token);
    v_now := clock_timestamp();
    IF v_attempt.attempt_id IS NULL THEN
        RAISE EXCEPTION 'Invalid attempt token or ownership' USING ERRCODE = '42501';
    END IF;

    SELECT t.status
    INTO v_task_status
    FROM forge_agent.coding_tasks t
    WHERE t.owner_id = v_attempt.owner_id
      AND t.project_alias = v_attempt.project_alias
      AND t.id = v_attempt.task_id;

    IF v_task_status NOT IN ('running', 'cancel_requested')
       OR v_attempt.published_at IS NOT NULL
       OR v_attempt.lease_expires_at IS NULL
       OR v_attempt.lease_expires_at <= v_now
       OR v_attempt.deadline_at <= v_now THEN
        RAISE EXCEPTION 'Attempt lease is not active' USING ERRCODE = 'P0008';
    END IF;

    UPDATE forge_agent.coding_attempts
    SET phase = CASE WHEN phase = 'starting' THEN 'running' ELSE phase END,
        summary = p_summary,
        updated_at = v_now
    WHERE owner_id = v_attempt.owner_id
      AND project_alias = v_attempt.project_alias
      AND task_id = v_attempt.task_id
      AND attempt_id = v_attempt.attempt_id;

    INSERT INTO forge_agent.coding_events (
        owner_id, project_alias, task_id, sequence, event_kind, payload
    )
    SELECT v_attempt.owner_id, v_attempt.project_alias, v_attempt.task_id,
           coalesce(max(e.sequence), 0) + 1,
           'checkpoint',
           pg_catalog.jsonb_build_object(
               'attempt_id', v_attempt.attempt_id,
               'summary', p_summary
           )
    FROM forge_agent.coding_events e
    WHERE e.owner_id = v_attempt.owner_id
      AND e.project_alias = v_attempt.project_alias
      AND e.task_id = v_attempt.task_id;
END;
$fn$;

-- Public worker function 5: idempotent UTF-8 staging chunk writer.
CREATE FUNCTION forge_coding_worker_api.append_result(
    p_attempt_id uuid,
    p_attempt_token uuid,
    p_chunk_index integer,
    p_content text
)
RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_attempt forge_agent.coding_attempts%ROWTYPE;
    v_task_status text;
    v_now timestamptz;
    v_byte_len integer;
    v_sha text;
    v_existing record;
    v_total_bytes bigint;
    v_chunk_count integer;
BEGIN
    IF p_chunk_index IS NULL OR p_chunk_index NOT BETWEEN 0 AND 1023
       OR p_content IS NULL THEN
        RAISE EXCEPTION 'Invalid result chunk arguments' USING ERRCODE = '22023';
    END IF;
    v_byte_len := pg_catalog.octet_length(p_content);
    IF v_byte_len < 1 OR v_byte_len > 16384 THEN
        RAISE EXCEPTION 'Result chunk exceeds 16 KiB bound' USING ERRCODE = '22023';
    END IF;

    v_attempt := forge_coding_worker_api._lock_task_and_attempt(p_attempt_id, p_attempt_token);
    v_now := clock_timestamp();
    IF v_attempt.attempt_id IS NULL THEN
        RAISE EXCEPTION 'Invalid attempt token or ownership' USING ERRCODE = '42501';
    END IF;

    v_sha := pg_catalog.encode(pg_catalog.sha256(pg_catalog.convert_to(p_content, 'UTF8')), 'hex');

    SELECT c.byte_length, c.content_sha256, c.content
    INTO v_existing
    FROM forge_agent.coding_result_chunks c
    WHERE c.owner_id = v_attempt.owner_id
      AND c.project_alias = v_attempt.project_alias
      AND c.task_id = v_attempt.task_id
      AND c.attempt_id = v_attempt.attempt_id
      AND c.chunk_index = p_chunk_index;

    IF FOUND THEN
        IF v_existing.content_sha256 = v_sha
           AND v_existing.byte_length = v_byte_len
           AND v_existing.content = p_content THEN
            RETURN;
        END IF;
        RAISE EXCEPTION 'Result chunk content conflict' USING ERRCODE = 'P0009';
    END IF;

    SELECT t.status
    INTO v_task_status
    FROM forge_agent.coding_tasks t
    WHERE t.owner_id = v_attempt.owner_id
      AND t.project_alias = v_attempt.project_alias
      AND t.id = v_attempt.task_id;

    IF v_task_status NOT IN ('running', 'cancel_requested')
       OR v_attempt.published_at IS NOT NULL
       OR v_attempt.lease_expires_at IS NULL
       OR v_attempt.lease_expires_at <= v_now
       OR v_attempt.deadline_at <= v_now THEN
        RAISE EXCEPTION 'Attempt lease is not active' USING ERRCODE = 'P0008';
    END IF;

    SELECT count(*)::integer, coalesce(sum(c.byte_length), 0)::bigint
    INTO v_chunk_count, v_total_bytes
    FROM forge_agent.coding_result_chunks c
    WHERE c.owner_id = v_attempt.owner_id
      AND c.project_alias = v_attempt.project_alias
      AND c.task_id = v_attempt.task_id
      AND c.attempt_id = v_attempt.attempt_id;

    IF v_chunk_count >= 1024 OR v_total_bytes + v_byte_len > 16777216 THEN
        RAISE EXCEPTION 'Result exceeds 16 MiB or 1024 chunk limit' USING ERRCODE = '22023';
    END IF;

    INSERT INTO forge_agent.coding_result_chunks (
        owner_id, project_alias, task_id, attempt_id,
        chunk_index, content, byte_length, content_sha256
    )
    VALUES (
        v_attempt.owner_id, v_attempt.project_alias, v_attempt.task_id, v_attempt.attempt_id,
        p_chunk_index, p_content, v_byte_len, v_sha
    );
END;
$fn$;

-- Public worker function 6: atomic immutable publish with exact replay BEFORE lease/state fences.
CREATE FUNCTION forge_coding_worker_api.publish(
    p_attempt_id uuid,
    p_attempt_token uuid,
    p_outcome text,
    p_manifest jsonb,
    p_usage jsonb,
    p_summary text,
    p_stop_proof jsonb
)
RETURNS jsonb
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_attempt forge_agent.coding_attempts%ROWTYPE;
    v_task forge_agent.coding_tasks%ROWTYPE;
    v_now timestamptz;
    v_scope_stopped boolean;
    v_canonical_outcome jsonb;
    v_outcome_digest text;
    v_expected_sha text := NULL;
    v_expected_bytes bigint := NULL;
    v_expected_chunks integer := NULL;
    v_actual_chunks integer;
    v_min_index integer;
    v_max_index integer;
    v_actual_bytes bigint;
    v_full_text text;
    v_actual_sha text;
    v_new_revision bigint;
    v_receipt jsonb;
BEGIN
    IF p_outcome IS NULL OR p_outcome NOT IN (
            'awaiting_review', 'failed', 'quota_blocked', 'blocked_auth',
            'blocked_permission', 'cancelled', 'needs_reconcile'
        )
       OR p_summary IS NULL OR pg_catalog.octet_length(p_summary) > 4096 THEN
        RAISE EXCEPTION 'Invalid publish arguments' USING ERRCODE = '22023';
    END IF;

    IF p_manifest IS NOT NULL THEN
        IF pg_catalog.jsonb_typeof(p_manifest) IS DISTINCT FROM 'object'
           OR p_manifest - ARRAY['sha256', 'byte_count', 'chunk_count'] <> '{}'::jsonb
           OR NOT (p_manifest ?& ARRAY['sha256', 'byte_count', 'chunk_count'])
           OR pg_catalog.jsonb_typeof(p_manifest->'sha256') IS DISTINCT FROM 'string'
           OR (p_manifest->>'sha256') !~ '^[0-9a-f]{64}$'
           OR pg_catalog.jsonb_typeof(p_manifest->'byte_count') IS DISTINCT FROM 'number'
           OR (p_manifest->>'byte_count') !~ '^[1-9][0-9]{0,7}$'
           OR pg_catalog.jsonb_typeof(p_manifest->'chunk_count') IS DISTINCT FROM 'number'
           OR (p_manifest->>'chunk_count') !~ '^[1-9][0-9]{0,3}$' THEN
            RAISE EXCEPTION 'Invalid result manifest' USING ERRCODE = '22023';
        END IF;
        v_expected_sha := p_manifest->>'sha256';
        v_expected_bytes := (p_manifest->>'byte_count')::bigint;
        v_expected_chunks := (p_manifest->>'chunk_count')::integer;
        IF v_expected_bytes NOT BETWEEN 1 AND 16777216
           OR v_expected_chunks NOT BETWEEN 1 AND 1024 THEN
            RAISE EXCEPTION 'Result manifest out of bounds' USING ERRCODE = '22023';
        END IF;
    ELSIF p_outcome = 'awaiting_review' THEN
        RAISE EXCEPTION 'Complete success requires a nonempty result manifest' USING ERRCODE = '22023';
    END IF;

    IF p_usage IS NOT NULL AND NOT forge_coding_worker_api._validate_usage_json(p_usage) THEN
        RAISE EXCEPTION 'Invalid normalized attempt usage JSON' USING ERRCODE = '22023';
    END IF;

    v_attempt := forge_coding_worker_api._lock_task_and_attempt(p_attempt_id, p_attempt_token);
    v_now := clock_timestamp();
    IF v_attempt.attempt_id IS NULL THEN
        RAISE EXCEPTION 'Invalid attempt token or ownership' USING ERRCODE = '42501';
    END IF;

    IF NOT forge_coding_worker_api._validate_stop_proof(p_stop_proof, v_attempt.unit_name) THEN
        RAISE EXCEPTION 'Invalid stop proof for attempt' USING ERRCODE = '22023';
    END IF;

    v_scope_stopped := (p_stop_proof->>'scope_stopped')::boolean;
    IF p_outcome <> 'needs_reconcile' AND NOT v_scope_stopped THEN
        RAISE EXCEPTION 'Confirmed stopped scope required for outcome %', p_outcome USING ERRCODE = '22023';
    END IF;

    v_canonical_outcome := pg_catalog.jsonb_build_object(
        'attempt_id', v_attempt.attempt_id,
        'manifest', p_manifest,
        'outcome', p_outcome,
        'scope_stopped', v_scope_stopped,
        'summary', p_summary,
        'usage', p_usage
    );
    v_outcome_digest := pg_catalog.encode(
        pg_catalog.sha256(pg_catalog.convert_to(v_canonical_outcome::text, 'UTF8')),
        'hex'
    );

    -- Exact publish replay MUST precede live lease, expiry, and current task state checks.
    IF v_attempt.published_at IS NOT NULL THEN
        IF v_attempt.outcome_digest = v_outcome_digest THEN
            RETURN v_attempt.publish_receipt;
        END IF;
        RAISE EXCEPTION 'Outcome conflict for already published attempt' USING ERRCODE = 'P0009';
    END IF;

    SELECT t.*
    INTO v_task
    FROM forge_agent.coding_tasks t
    WHERE t.owner_id = v_attempt.owner_id
      AND t.project_alias = v_attempt.project_alias
      AND t.id = v_attempt.task_id;

    IF (v_task.status <> 'running'
        AND NOT (v_task.status = 'cancel_requested' AND p_outcome IN ('cancelled', 'needs_reconcile')))
       OR v_attempt.lease_expires_at IS NULL
       OR v_attempt.lease_expires_at <= v_now
       OR v_attempt.deadline_at <= v_now THEN
        RAISE EXCEPTION 'Attempt lease is not active' USING ERRCODE = 'P0008';
    END IF;

    IF v_task.revision >= 9007199254740991 THEN
        RAISE EXCEPTION 'Task revision limit reached' USING ERRCODE = 'P0009';
    END IF;

    IF p_manifest IS NOT NULL THEN
        SELECT count(*)::integer,
               coalesce(min(c.chunk_index), -1),
               coalesce(max(c.chunk_index), -1),
               coalesce(sum(c.byte_length), 0)::bigint,
               coalesce(pg_catalog.string_agg(c.content, '' ORDER BY c.chunk_index), '')
        INTO v_actual_chunks, v_min_index, v_max_index, v_actual_bytes, v_full_text
        FROM forge_agent.coding_result_chunks c
        WHERE c.owner_id = v_attempt.owner_id
          AND c.project_alias = v_attempt.project_alias
          AND c.task_id = v_attempt.task_id
          AND c.attempt_id = v_attempt.attempt_id;

        v_actual_sha := pg_catalog.encode(
            pg_catalog.sha256(pg_catalog.convert_to(v_full_text, 'UTF8')),
            'hex'
        );

        IF v_actual_chunks <> v_expected_chunks
           OR v_min_index <> 0
           OR v_max_index <> v_expected_chunks - 1
           OR v_actual_bytes <> v_expected_bytes
           OR v_actual_sha <> v_expected_sha THEN
            RAISE EXCEPTION 'Staged result chunks do not match manifest' USING ERRCODE = 'P0007';
        END IF;
    END IF;

    v_now := clock_timestamp();
    IF v_attempt.lease_expires_at <= v_now OR v_attempt.deadline_at <= v_now THEN
        RAISE EXCEPTION 'Attempt lease expired before publication' USING ERRCODE = 'P0008';
    END IF;

    v_new_revision := v_task.revision + 1;
    v_receipt := pg_catalog.jsonb_build_object(
        'task_id', v_task.id,
        'project_alias', v_task.project_alias,
        'attempt_id', v_attempt.attempt_id,
        'outcome', p_outcome,
        'outcome_digest', v_outcome_digest,
        'revision', v_new_revision,
        'status', p_outcome,
        'result', CASE WHEN p_manifest IS NULL THEN NULL ELSE pg_catalog.jsonb_build_object(
            'attempt_id', v_attempt.attempt_id,
            'sha256', v_expected_sha,
            'byte_count', v_expected_bytes,
            'chunk_count', v_expected_chunks
        ) END
    );

    UPDATE forge_agent.coding_attempts
    SET outcome = p_outcome,
        outcome_digest = v_outcome_digest,
        publish_receipt = v_receipt,
        result_sha256 = v_expected_sha,
        result_byte_count = v_expected_bytes,
        result_chunk_count = v_expected_chunks,
        summary = p_summary,
        usage_json = p_usage,
        stop_proof = p_stop_proof,
        phase = CASE WHEN v_scope_stopped THEN 'stopped' ELSE 'unknown' END,
        reservation_active = NOT v_scope_stopped,
        lease_expires_at = NULL,
        finished_at = CASE WHEN v_scope_stopped THEN coalesce(finished_at, v_now) ELSE finished_at END,
        published_at = v_now,
        updated_at = v_now
    WHERE owner_id = v_attempt.owner_id
      AND project_alias = v_attempt.project_alias
      AND task_id = v_attempt.task_id
      AND attempt_id = v_attempt.attempt_id;

    UPDATE forge_agent.coding_tasks
    SET status = p_outcome,
        revision = v_new_revision,
        cancelled_at = CASE WHEN p_outcome = 'cancelled' THEN v_now ELSE NULL END,
        updated_at = v_now
    WHERE owner_id = v_task.owner_id
      AND project_alias = v_task.project_alias
      AND id = v_task.id;

    INSERT INTO forge_agent.coding_events (
        owner_id, project_alias, task_id, sequence, event_kind, payload
    )
    SELECT v_task.owner_id, v_task.project_alias, v_task.id,
           coalesce(max(e.sequence), 0) + 1,
           p_outcome,
           pg_catalog.jsonb_build_object(
               'attempt_id', v_attempt.attempt_id,
               'revision', v_new_revision
           )
    FROM forge_agent.coding_events e
    WHERE e.owner_id = v_task.owner_id
      AND e.project_alias = v_task.project_alias
      AND e.task_id = v_task.id;

    RETURN v_receipt;
END;
$fn$;

-- Public worker function 7: post-expiry safe stop recording (never reopens task).
CREATE FUNCTION forge_coding_worker_api.record_stop(
    p_attempt_id uuid,
    p_attempt_token uuid,
    p_stop_proof jsonb,
    p_summary text
)
RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_attempt forge_agent.coding_attempts%ROWTYPE;
    v_now timestamptz;
    v_scope_stopped boolean;
    v_task forge_agent.coding_tasks%ROWTYPE;
    v_target_status text;
    v_new_revision bigint;
BEGIN
    IF p_summary IS NULL OR pg_catalog.octet_length(p_summary) > 4096 THEN
        RAISE EXCEPTION 'Invalid stop summary' USING ERRCODE = '22023';
    END IF;

    v_attempt := forge_coding_worker_api._lock_task_and_attempt(p_attempt_id, p_attempt_token);
    v_now := clock_timestamp();
    IF v_attempt.attempt_id IS NULL THEN
        RAISE EXCEPTION 'Invalid attempt token or ownership' USING ERRCODE = '42501';
    END IF;

    IF NOT forge_coding_worker_api._validate_stop_proof(p_stop_proof, v_attempt.unit_name) THEN
        RAISE EXCEPTION 'Invalid stop proof for attempt' USING ERRCODE = '22023';
    END IF;

    v_scope_stopped := (p_stop_proof->>'scope_stopped')::boolean;

    IF v_scope_stopped THEN
        UPDATE forge_agent.coding_attempts
        SET phase = 'stopped',
            stop_proof = p_stop_proof,
            reservation_active = false,
            lease_expires_at = NULL,
            finished_at = coalesce(finished_at, v_now),
            summary = CASE WHEN published_at IS NULL THEN p_summary ELSE summary END,
            updated_at = v_now
        WHERE owner_id = v_attempt.owner_id
          AND project_alias = v_attempt.project_alias
          AND task_id = v_attempt.task_id
          AND attempt_id = v_attempt.attempt_id;
        -- A stopped unpublished current writer must leave a recoverable task.
        -- A late stop for a published/superseded attempt never changes task state.
        IF v_attempt.published_at IS NULL THEN
            SELECT t.* INTO v_task FROM forge_agent.coding_tasks t
            WHERE t.owner_id = v_attempt.owner_id
              AND t.project_alias = v_attempt.project_alias
              AND t.id = v_attempt.task_id
              AND t.latest_attempt_id = v_attempt.attempt_id;
            IF FOUND AND v_task.status IN ('running', 'cancel_requested') THEN
                IF v_task.revision >= 9007199254740991 THEN
                    RAISE EXCEPTION 'Task revision limit reached' USING ERRCODE = 'P0009';
                END IF;
                v_target_status := CASE WHEN v_task.status = 'cancel_requested'
                    THEN 'cancelled' ELSE 'needs_reconcile' END;
                v_new_revision := v_task.revision + 1;
                UPDATE forge_agent.coding_tasks
                SET status = v_target_status, revision = v_new_revision,
                    cancelled_at = CASE WHEN v_target_status = 'cancelled' THEN v_now ELSE NULL END,
                    updated_at = v_now
                WHERE owner_id = v_task.owner_id AND project_alias = v_task.project_alias
                  AND id = v_task.id;
                INSERT INTO forge_agent.coding_events (
                    owner_id, project_alias, task_id, sequence, event_kind, payload
                )
                SELECT v_task.owner_id, v_task.project_alias, v_task.id,
                       coalesce(max(e.sequence), 0) + 1, v_target_status,
                       pg_catalog.jsonb_build_object(
                           'attempt_id', v_attempt.attempt_id,
                           'reason', 'stopped_without_publication', 'revision', v_new_revision
                       )
                FROM forge_agent.coding_events e
                WHERE e.owner_id = v_task.owner_id AND e.project_alias = v_task.project_alias
                  AND e.task_id = v_task.id;
            END IF;
        END IF;
    ELSIF v_attempt.phase <> 'stopped' THEN
        UPDATE forge_agent.coding_attempts
        SET phase = CASE WHEN phase = 'unknown' THEN 'unknown' ELSE 'stopping' END,
            stop_proof = p_stop_proof,
            summary = CASE WHEN published_at IS NULL THEN p_summary ELSE summary END,
            updated_at = v_now
        WHERE owner_id = v_attempt.owner_id
          AND project_alias = v_attempt.project_alias
          AND task_id = v_attempt.task_id
          AND attempt_id = v_attempt.attempt_id;
    END IF;
END;
$fn$;

-- Public worker function 8: cancellation / stop fence check.
CREATE FUNCTION forge_coding_worker_api.cancellation_requested(
    p_attempt_id uuid,
    p_attempt_token uuid
)
RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_state record;
    v_now timestamptz := clock_timestamp();
BEGIN
    IF p_attempt_id IS NULL OR p_attempt_token IS NULL THEN
        RETURN true;
    END IF;

    SELECT t.status, a.lease_expires_at, a.deadline_at, a.published_at
    INTO v_state
    FROM forge_agent.coding_attempts a
    JOIN forge_agent.coding_tasks t
      ON t.owner_id = a.owner_id
     AND t.project_alias = a.project_alias
     AND t.id = a.task_id
    WHERE a.attempt_id = p_attempt_id
      AND a.attempt_token = p_attempt_token
      AND a.worker_role = session_user
      AND a.owner_id = pg_catalog.current_setting('forge.coding_owner', true);

    IF NOT FOUND THEN
        RETURN true;
    END IF;

    IF v_state.status = 'cancel_requested'
       OR v_state.status <> 'running'
       OR v_state.published_at IS NOT NULL
       OR v_state.lease_expires_at IS NULL
       OR v_state.lease_expires_at <= v_now
       OR v_state.deadline_at <= v_now THEN
        RETURN true;
    END IF;

    RETURN false;
END;
$fn$;

-- Public worker function 9: allocate poll generation + epoch BEFORE metadata I/O.
CREATE FUNCTION forge_coding_worker_api.begin_usage_poll(p_profile_alias text)
RETURNS jsonb
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_owners text[];
    v_owner text;
    v_gen bigint;
    v_epoch bigint;
BEGIN
    IF p_profile_alias IS NULL OR p_profile_alias !~ '^[A-Za-z0-9_.-]{1,64}$' THEN
        RAISE EXCEPTION 'Invalid usage profile alias' USING ERRCODE = '22023';
    END IF;

    SELECT array_agg(DISTINCT g.owner_id ORDER BY g.owner_id)
    INTO v_owners
    FROM forge_agent.coding_worker_grants g
    WHERE g.worker_role = session_user
              AND g.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
      AND g.agent_profile = p_profile_alias;

    IF v_owners IS NULL OR pg_catalog.array_length(v_owners, 1) <> 1 THEN
        RAISE EXCEPTION 'Worker role lacks unambiguous grant for profile %', p_profile_alias
            USING ERRCODE = '42501';
    END IF;
    v_owner := v_owners[1];

    INSERT INTO forge_agent.coding_provider_usage (
        owner_id, profile_alias, current_generation, configuration_epoch, updated_at
    )
    VALUES (v_owner, p_profile_alias, 1, 1, clock_timestamp())
    ON CONFLICT (owner_id, profile_alias) DO UPDATE
    SET current_generation = forge_agent.coding_provider_usage.current_generation + 1,
        updated_at = clock_timestamp()
    RETURNING current_generation, configuration_epoch
    INTO v_gen, v_epoch;

    RETURN pg_catalog.jsonb_build_object(
        'generation', v_gen,
        'configuration_epoch', v_epoch
    );
END;
$fn$;

-- Public worker function 10: claim oldest pending usage refresh and allocate generation.
CREATE FUNCTION forge_coding_worker_api.next_refresh()
RETURNS jsonb
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_req forge_agent.coding_usage_refresh_requests%ROWTYPE;
    v_gen bigint;
    v_epoch bigint;
    v_now timestamptz := clock_timestamp();
BEGIN
    -- Release stale abandoned in-progress refresh requests so refresh_pending cannot wedge.
    UPDATE forge_agent.coding_usage_refresh_requests r
    SET status = 'failed',
        error_code = 'poll_timeout',
        completed_at = v_now
    WHERE r.status = 'in_progress'
      AND r.claimed_at < v_now - interval '120 seconds'
      AND EXISTS (
          SELECT 1
          FROM forge_agent.coding_worker_grants g
          WHERE g.worker_role = session_user
              AND g.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
            AND g.owner_id = r.owner_id
            AND g.agent_profile = r.profile_alias
      );

    SELECT r.*
    INTO v_req
    FROM forge_agent.coding_usage_refresh_requests r
    WHERE r.status = 'pending'
      AND EXISTS (
          SELECT 1
          FROM forge_agent.coding_worker_grants g
          WHERE g.worker_role = session_user
              AND g.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
            AND g.owner_id = r.owner_id
            AND g.agent_profile = r.profile_alias
      )
    ORDER BY r.requested_at ASC, r.profile_alias ASC
    FOR UPDATE SKIP LOCKED
    LIMIT 1;

    IF NOT FOUND THEN
        RETURN NULL;
    END IF;

    INSERT INTO forge_agent.coding_provider_usage (
        owner_id, profile_alias, current_generation, configuration_epoch, updated_at
    )
    VALUES (v_req.owner_id, v_req.profile_alias, 1, 1, v_now)
    ON CONFLICT (owner_id, profile_alias) DO UPDATE
    SET current_generation = forge_agent.coding_provider_usage.current_generation + 1,
        updated_at = v_now
    RETURNING current_generation, configuration_epoch
    INTO v_gen, v_epoch;

    UPDATE forge_agent.coding_usage_refresh_requests
    SET status = 'in_progress',
        generation = v_gen,
        configuration_epoch = v_epoch,
        claimed_at = v_now
    WHERE owner_id = v_req.owner_id
      AND profile_alias = v_req.profile_alias
      AND refresh_id = v_req.refresh_id;

    RETURN pg_catalog.jsonb_build_object(
        'refresh_id', v_req.refresh_id,
        'profile_alias', v_req.profile_alias,
        'generation', v_gen,
        'configuration_epoch', v_epoch
    );
END;
$fn$;

-- Public worker function 11: record newest provider snapshot with epoch/generation/account-switch fences.
CREATE FUNCTION forge_coding_worker_api.record_usage(
    p_profile_alias text,
    p_generation bigint,
    p_configuration_epoch bigint,
    p_snapshot jsonb
)
RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_owners text[];
    v_owner text;
    v_row forge_agent.coding_provider_usage%ROWTYPE;
    v_prev_identity text;
    v_new_identity text;
    v_next_epoch bigint;
    v_now timestamptz;
BEGIN
    IF p_profile_alias IS NULL OR p_profile_alias !~ '^[A-Za-z0-9_.-]{1,64}$'
       OR p_generation IS NULL OR p_generation < 1
       OR p_configuration_epoch IS NULL OR p_configuration_epoch < 1
       OR NOT forge_coding_worker_api._validate_provider_snapshot(p_snapshot) THEN
        RAISE EXCEPTION 'Invalid usage snapshot arguments' USING ERRCODE = '22023';
    END IF;

    SELECT array_agg(DISTINCT g.owner_id ORDER BY g.owner_id)
    INTO v_owners
    FROM forge_agent.coding_worker_grants g
    WHERE g.worker_role = session_user
              AND g.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
      AND g.agent_profile = p_profile_alias;

    IF v_owners IS NULL OR pg_catalog.array_length(v_owners, 1) <> 1 THEN
        RAISE EXCEPTION 'Worker role lacks unambiguous grant for profile %', p_profile_alias
            USING ERRCODE = '42501';
    END IF;
    v_owner := v_owners[1];

    SELECT u.*
    INTO v_row
    FROM forge_agent.coding_provider_usage u
    WHERE u.owner_id = v_owner
      AND u.profile_alias = p_profile_alias
    FOR UPDATE;

    v_now := clock_timestamp();

    IF NOT FOUND THEN
        RETURN false;
    END IF;

    IF p_configuration_epoch <> v_row.configuration_epoch
       OR p_generation <= v_row.recorded_generation
       OR p_generation <> v_row.current_generation THEN
        RETURN false;
    END IF;

    SELECT i.account_identity
    INTO v_prev_identity
    FROM forge_agent.coding_provider_usage_identities i
    WHERE i.owner_id = v_owner
      AND i.profile_alias = p_profile_alias
    FOR UPDATE;

    v_new_identity := p_snapshot->>'account_identity';
    v_next_epoch := v_row.configuration_epoch;

    IF v_new_identity IS NOT NULL THEN
        IF v_prev_identity IS NOT NULL AND v_prev_identity <> v_new_identity THEN
            v_next_epoch := v_row.configuration_epoch + 1;
        END IF;
        INSERT INTO forge_agent.coding_provider_usage_identities (
            owner_id, profile_alias, account_identity, updated_at
        )
        VALUES (v_owner, p_profile_alias, v_new_identity, v_now)
        ON CONFLICT (owner_id, profile_alias) DO UPDATE
        SET account_identity = excluded.account_identity,
            updated_at = excluded.updated_at;
    END IF;

    UPDATE forge_agent.coding_provider_usage
    SET configuration_epoch = v_next_epoch,
        recorded_generation = p_generation,
        recorded_epoch = v_next_epoch,
        snapshot_state = p_snapshot->>'snapshot_state',
        observed_at = CASE
            WHEN pg_catalog.jsonb_typeof(p_snapshot->'observed_at') = 'null' THEN NULL
            ELSE (p_snapshot->>'observed_at')::timestamptz
        END,
        source = p_snapshot->>'source',
        version = p_snapshot->>'version',
        last_error = CASE
            WHEN pg_catalog.jsonb_typeof(p_snapshot->'last_error') = 'null' THEN NULL
            ELSE p_snapshot->>'last_error'
        END,
        allowance = p_snapshot->'allowance',
        account_activity = p_snapshot->'account_activity',
        updated_at = v_now
    WHERE owner_id = v_owner
      AND profile_alias = p_profile_alias;

    UPDATE forge_agent.coding_usage_refresh_requests
    SET status = 'completed',
        completed_at = v_now
    WHERE owner_id = v_owner
      AND profile_alias = p_profile_alias
      AND status IN ('pending', 'in_progress')
      AND (generation IS NULL OR generation <= p_generation);

    RETURN true;
END;
$fn$;

-- Public worker function 12: record poll failure while keeping prior good snapshot intact.
CREATE FUNCTION forge_coding_worker_api.fail_usage_poll(
    p_profile_alias text,
    p_generation bigint,
    p_configuration_epoch bigint,
    p_error_code text
)
RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_owners text[];
    v_owner text;
    v_row forge_agent.coding_provider_usage%ROWTYPE;
    v_now timestamptz;
BEGIN
    IF p_profile_alias IS NULL OR p_profile_alias !~ '^[A-Za-z0-9_.-]{1,64}$'
       OR p_generation IS NULL OR p_generation < 1
       OR p_configuration_epoch IS NULL OR p_configuration_epoch < 1
       OR p_error_code IS NULL OR p_error_code !~ '^[a-z0-9_.-]{1,64}$' THEN
        RAISE EXCEPTION 'Invalid usage poll failure arguments' USING ERRCODE = '22023';
    END IF;

    SELECT array_agg(DISTINCT g.owner_id ORDER BY g.owner_id)
    INTO v_owners
    FROM forge_agent.coding_worker_grants g
    WHERE g.worker_role = session_user
              AND g.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
      AND g.agent_profile = p_profile_alias;

    IF v_owners IS NULL OR pg_catalog.array_length(v_owners, 1) <> 1 THEN
        RAISE EXCEPTION 'Worker role lacks unambiguous grant for profile %', p_profile_alias
            USING ERRCODE = '42501';
    END IF;
    v_owner := v_owners[1];

    SELECT u.*
    INTO v_row
    FROM forge_agent.coding_provider_usage u
    WHERE u.owner_id = v_owner
      AND u.profile_alias = p_profile_alias
    FOR UPDATE;

    v_now := clock_timestamp();

    IF NOT FOUND THEN
        RETURN;
    END IF;

    IF p_configuration_epoch = v_row.configuration_epoch
       AND p_generation > v_row.recorded_generation
       AND p_generation = v_row.current_generation THEN
        UPDATE forge_agent.coding_provider_usage
        SET last_error = p_error_code,
            recorded_generation = p_generation,
            updated_at = clock_timestamp()
        WHERE owner_id = v_owner
          AND profile_alias = p_profile_alias;
    ELSE
        RETURN;
    END IF;

    UPDATE forge_agent.coding_usage_refresh_requests
    SET status = 'failed',
        error_code = p_error_code,
        completed_at = v_now
    WHERE owner_id = v_owner
      AND profile_alias = p_profile_alias
      AND status = 'in_progress'
      AND generation = p_generation;
END;
$fn$;

-- Trusted reconciler function 1: expire overdue leases without requeueing; retain reservations.
CREATE FUNCTION forge_coding_reconciler_api.expire_leases()
RETURNS integer
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_candidate record;
    v_task forge_agent.coding_tasks%ROWTYPE;
    v_attempt forge_agent.coding_attempts%ROWTYPE;
    v_now timestamptz := clock_timestamp();
    v_expired integer := 0;
    v_new_revision bigint;
BEGIN
    FOR v_candidate IN
        SELECT a.owner_id, a.project_alias, a.task_id, a.attempt_id
        FROM forge_agent.coding_attempts a
        WHERE a.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
          AND a.lease_expires_at IS NOT NULL
          AND (a.lease_expires_at <= v_now OR a.deadline_at <= v_now)
        ORDER BY a.task_id ASC, a.attempt_id ASC
    LOOP
        SELECT t.*
        INTO v_task
        FROM forge_agent.coding_tasks t
        WHERE t.owner_id = v_candidate.owner_id
          AND t.project_alias = v_candidate.project_alias
          AND t.id = v_candidate.task_id
        FOR UPDATE SKIP LOCKED;
        IF NOT FOUND THEN
            CONTINUE;
        END IF;

        SELECT a.*
        INTO v_attempt
        FROM forge_agent.coding_attempts a
        WHERE a.owner_id = v_candidate.owner_id
          AND a.project_alias = v_candidate.project_alias
          AND a.task_id = v_candidate.task_id
          AND a.attempt_id = v_candidate.attempt_id
          AND a.lease_expires_at IS NOT NULL
          AND (a.lease_expires_at <= v_now OR a.deadline_at <= v_now)
        FOR UPDATE;
        IF NOT FOUND THEN
            CONTINUE;
        END IF;

        UPDATE forge_agent.coding_attempts
        SET lease_expires_at = NULL,
            phase = 'unknown',
            updated_at = v_now
        WHERE owner_id = v_attempt.owner_id
          AND project_alias = v_attempt.project_alias
          AND task_id = v_attempt.task_id
          AND attempt_id = v_attempt.attempt_id;

        IF v_task.status IN ('running', 'cancel_requested') AND v_task.revision < 9007199254740991 THEN
            v_new_revision := v_task.revision + 1;
            UPDATE forge_agent.coding_tasks
            SET status = 'needs_reconcile',
                revision = v_new_revision,
                updated_at = v_now
            WHERE owner_id = v_task.owner_id
              AND project_alias = v_task.project_alias
              AND id = v_task.id;

            INSERT INTO forge_agent.coding_events (
                owner_id, project_alias, task_id, sequence, event_kind, payload
            )
            SELECT v_task.owner_id, v_task.project_alias, v_task.id,
                   coalesce(max(e.sequence), 0) + 1,
                   'needs_reconcile',
                   pg_catalog.jsonb_build_object(
                       'attempt_id', v_attempt.attempt_id,
                       'reason', CASE WHEN v_attempt.deadline_at <= v_now THEN 'deadline_exceeded' ELSE 'lease_expired' END,
                       'revision', v_new_revision
                   )
            FROM forge_agent.coding_events e
            WHERE e.owner_id = v_task.owner_id
              AND e.project_alias = v_task.project_alias
              AND e.task_id = v_task.id;
        END IF;

        v_expired := v_expired + 1;
    END LOOP;

    RETURN v_expired;
END;
$fn$;

-- Trusted reconciler function 2: bounded list of attempts needing reconciliation (no prompt/token).
CREATE FUNCTION forge_coding_reconciler_api.pending_reconciliations(p_limit integer DEFAULT 64)
RETURNS jsonb
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_items jsonb;
BEGIN
    IF p_limit IS NULL OR p_limit NOT BETWEEN 1 AND 64 THEN
        RAISE EXCEPTION 'Invalid pending reconciliation limit' USING ERRCODE = '22023';
    END IF;

    SELECT coalesce(
        pg_catalog.jsonb_agg(
            pg_catalog.jsonb_build_object(
                'owner_id', row_data.owner_id,
                'task_id', row_data.task_id,
                'project_alias', row_data.project_alias,
                'base_revision', row_data.base_revision,
                'mode', row_data.mode,
                'agent_profile', row_data.agent_profile,
                'attempt_id', row_data.attempt_id,
                'worker_id', row_data.worker_id,
                'incarnation', row_data.worker_incarnation,
                'unit_name', row_data.unit_name,
                'phase', row_data.phase,
                'task_status', row_data.task_status,
                'task_revision', row_data.task_revision,
                'started_at', pg_catalog.to_char(row_data.started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
                'deadline_at', pg_catalog.to_char(row_data.deadline_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
                'resume_workspace_digest', row_data.resume_workspace_digest
            )
        ),
        '[]'::jsonb
    )
    INTO v_items
    FROM (
        SELECT a.owner_id, a.task_id, a.project_alias, t.base_revision, t.mode, t.agent_profile,
               a.attempt_id, a.worker_id, a.worker_incarnation, a.unit_name, a.phase,
               t.status AS task_status, t.revision AS task_revision, a.started_at, a.deadline_at,
               a.resume_workspace_digest
        FROM forge_agent.coding_attempts a
        JOIN forge_agent.coding_tasks t
          ON t.owner_id = a.owner_id
         AND t.project_alias = a.project_alias
         AND t.id = a.task_id
        WHERE a.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
          AND t.latest_attempt_id = a.attempt_id
          AND a.lease_expires_at IS NULL
          AND t.status IN (
              'needs_reconcile', 'awaiting_review', 'failed', 'quota_blocked',
              'blocked_auth', 'blocked_permission', 'cancelled'
          )
          AND NOT EXISTS (
              SELECT 1 FROM forge_agent.coding_reconciliation_receipts r
              WHERE r.owner_id = t.owner_id
                AND r.project_alias = t.project_alias
                AND r.task_id = t.id
                AND r.attempt_id = a.attempt_id
                AND r.task_revision = t.revision
          )
        ORDER BY a.started_at ASC, a.attempt_id ASC
        LIMIT p_limit
    ) AS row_data;

    RETURN v_items;
END;
$fn$;

-- Trusted reconciler function 3: verify stopped scope, release reservation, record append-only receipt.
CREATE FUNCTION forge_coding_reconciler_api.reconcile(
    p_task_id uuid,
    p_attempt_id uuid,
    p_workspace_digest text,
    p_stop_proof jsonb
)
RETURNS uuid
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_locator record;
    v_task forge_agent.coding_tasks%ROWTYPE;
    v_attempt forge_agent.coding_attempts%ROWTYPE;
    v_receipt_id uuid;
    v_now timestamptz;
BEGIN
    IF p_task_id IS NULL OR p_attempt_id IS NULL
       OR p_workspace_digest IS NULL OR p_workspace_digest !~ '^[0-9a-f]{64}$' THEN
        RAISE EXCEPTION 'Invalid reconciliation parameters' USING ERRCODE = '22023';
    END IF;

    SELECT a.owner_id, a.project_alias
    INTO v_locator
    FROM forge_agent.coding_attempts a
    WHERE a.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
      AND a.task_id = p_task_id AND a.attempt_id = p_attempt_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'Attempt not found for reconciliation' USING ERRCODE = 'P0002';
    END IF;

    SELECT t.*
    INTO v_task
    FROM forge_agent.coding_tasks t
    WHERE t.owner_id = v_locator.owner_id
      AND t.project_alias = v_locator.project_alias
      AND t.id = p_task_id
    FOR UPDATE;

    SELECT a.*
    INTO v_attempt
    FROM forge_agent.coding_attempts a
    WHERE a.owner_id = v_locator.owner_id
      AND a.project_alias = v_locator.project_alias
      AND a.task_id = p_task_id
      AND a.attempt_id = p_attempt_id
    FOR UPDATE;

    v_now := clock_timestamp();

    IF v_task.latest_attempt_id IS DISTINCT FROM v_attempt.attempt_id THEN
        RAISE EXCEPTION 'Attempt is not the latest attempt for this task' USING ERRCODE = 'P0009';
    END IF;

    IF v_task.status IN ('queued', 'running', 'cancel_requested', 'completed')
       OR v_attempt.lease_expires_at IS NOT NULL THEN
        RAISE EXCEPTION 'Task or attempt is not in a reconcilable state' USING ERRCODE = 'P0009';
    END IF;

    IF NOT forge_coding_worker_api._validate_stop_proof(p_stop_proof, v_attempt.unit_name)
       OR (p_stop_proof->>'scope_stopped')::boolean IS DISTINCT FROM true THEN
        RAISE EXCEPTION 'Reconciliation requires confirmed stopped systemd proof' USING ERRCODE = '22023';
    END IF;

    UPDATE forge_agent.coding_attempts
    SET phase = 'stopped',
        stop_proof = p_stop_proof,
        reservation_active = false,
        finished_at = coalesce(finished_at, v_now),
        updated_at = v_now
    WHERE owner_id = v_attempt.owner_id
      AND project_alias = v_attempt.project_alias
      AND task_id = v_attempt.task_id
      AND attempt_id = v_attempt.attempt_id;

    v_receipt_id := pg_catalog.gen_random_uuid();
    INSERT INTO forge_agent.coding_reconciliation_receipts (
        receipt_id, owner_id, project_alias, task_id, attempt_id,
        task_revision, workspace_digest, stop_proof, reconciled_by, created_at
    )
    VALUES (
        v_receipt_id, v_task.owner_id, v_task.project_alias, v_task.id, v_attempt.attempt_id,
        v_task.revision, p_workspace_digest, p_stop_proof, session_user, v_now
    );

    RETURN v_receipt_id;
END;
$fn$;

REVOKE ALL ON ALL FUNCTIONS IN SCHEMA forge_coding_worker_api FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA forge_coding_reconciler_api FROM PUBLIC;
