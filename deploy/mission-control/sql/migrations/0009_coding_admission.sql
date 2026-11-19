CREATE TABLE forge_agent.coding_tasks (
    owner_id text NOT NULL
        CONSTRAINT coding_tasks_owner_id_check CHECK (
            octet_length(owner_id) BETWEEN 1 AND 128
            AND btrim(owner_id) <> ''
            AND owner_id ~ '^[ -~]+$'
        ),
    project_alias text NOT NULL
        CONSTRAINT coding_tasks_project_alias_check CHECK (
            project_alias ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    id uuid NOT NULL,
    idempotency_key text NOT NULL
        CONSTRAINT coding_tasks_idempotency_key_check CHECK (
            idempotency_key ~ '^[A-Za-z0-9_.-]{1,128}$'
        ),
    intent_digest text NOT NULL
        CONSTRAINT coding_tasks_intent_digest_check CHECK (
            intent_digest ~ '^[0-9a-f]{64}$'
        ),
    base_revision text NOT NULL
        CONSTRAINT coding_tasks_base_revision_check CHECK (
            base_revision ~ '^[0-9a-f]{40}$'
        ),
    mode text NOT NULL
        CONSTRAINT coding_tasks_mode_check CHECK (
            mode IN ('plan', 'review', 'implement')
        ),
    agent_profile text NOT NULL
        CONSTRAINT coding_tasks_agent_profile_check CHECK (
            agent_profile ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    prompt text NOT NULL
        CONSTRAINT coding_tasks_prompt_check CHECK (
            octet_length(prompt) BETWEEN 1 AND 8192
            AND btrim(prompt, E' \t\r\n') <> ''
        ),
    acceptance_criteria text NOT NULL
        CONSTRAINT coding_tasks_acceptance_criteria_check CHECK (
            octet_length(acceptance_criteria) BETWEEN 1 AND 2048
            AND btrim(acceptance_criteria, E' \t\r\n') <> ''
        ),
    pinned_recipe jsonb NOT NULL
        CONSTRAINT coding_tasks_pinned_recipe_check CHECK (
            (
                jsonb_typeof(pinned_recipe) = 'object'
                AND pinned_recipe - ARRAY[
                    'version', 'backend', 'model', 'resource_profile', 'mode', 'generation_token_limit'
                ] = '{}'::jsonb
                AND pinned_recipe ? 'version'
                AND pinned_recipe->'version' = '1'::jsonb
                AND pinned_recipe ? 'backend'
                AND jsonb_typeof(pinned_recipe->'backend') = 'string'
                AND pinned_recipe->>'backend' IN ('antigravity', 'codex')
                AND pinned_recipe ? 'model'
                AND jsonb_typeof(pinned_recipe->'model') = 'string'
                AND octet_length(pinned_recipe->>'model') BETWEEN 1 AND 128
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
            ) IS TRUE
        ),
    recipe_digest text NOT NULL
        CONSTRAINT coding_tasks_recipe_digest_check CHECK (
            recipe_digest ~ '^[0-9a-f]{64}$'
        ),
    status text NOT NULL DEFAULT 'queued'
        CONSTRAINT coding_tasks_status_check CHECK (
            status IN ('queued', 'cancelled')
        ),
    revision bigint NOT NULL DEFAULT 1
        CONSTRAINT coding_tasks_revision_check CHECK (
            revision BETWEEN 1 AND 9007199254740991
        ),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    cancelled_at timestamptz,
    CONSTRAINT coding_tasks_pkey PRIMARY KEY (owner_id, project_alias, id),
    CONSTRAINT coding_tasks_id_unique UNIQUE (id),
    CONSTRAINT coding_tasks_idempotency_unique UNIQUE (owner_id, project_alias, idempotency_key),
    CONSTRAINT coding_tasks_status_revision_check CHECK (
        (
            (status = 'queued' AND cancelled_at IS NULL AND revision = 1)
            OR (status = 'cancelled' AND cancelled_at IS NOT NULL AND revision > 1)
        ) IS TRUE
    ),
    CONSTRAINT coding_tasks_recipe_mode_check CHECK (
        (pinned_recipe->>'mode') = mode
    )
);

CREATE INDEX coding_tasks_by_project_created
    ON forge_agent.coding_tasks (owner_id, project_alias, created_at ASC, id ASC);

CREATE INDEX coding_tasks_by_project_status_created
    ON forge_agent.coding_tasks (owner_id, project_alias, status, created_at ASC, id ASC);

CREATE TABLE forge_agent.coding_events (
    owner_id text NOT NULL,
    project_alias text NOT NULL,
    task_id uuid NOT NULL,
    sequence bigint NOT NULL
        CONSTRAINT coding_events_sequence_check CHECK (
            sequence BETWEEN 1 AND 9007199254740991
        ),
    event_kind text NOT NULL
        CONSTRAINT coding_events_kind_check CHECK (
            event_kind IN ('queued', 'cancelled')
        ),
    payload jsonb NOT NULL DEFAULT '{}'::jsonb
        CONSTRAINT coding_events_payload_check CHECK (
            jsonb_typeof(payload) = 'object'
        ),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT coding_events_pkey PRIMARY KEY (owner_id, project_alias, task_id, sequence),
    CONSTRAINT coding_events_task_fk FOREIGN KEY (owner_id, project_alias, task_id)
        REFERENCES forge_agent.coding_tasks (owner_id, project_alias, id),
    CONSTRAINT coding_events_sequence_kind_check CHECK (
        (
            (sequence = 1 AND event_kind = 'queued')
            OR (sequence > 1 AND event_kind = 'cancelled')
        ) IS TRUE
    )
);

CREATE TABLE forge_agent.coding_control_receipts (
    owner_id text NOT NULL,
    project_alias text NOT NULL,
    task_id uuid NOT NULL,
    action text NOT NULL
        CONSTRAINT coding_control_receipts_action_check CHECK (
            action IN ('cancel')
        ),
    idempotency_key text NOT NULL
        CONSTRAINT coding_control_receipts_idempotency_key_check CHECK (
            idempotency_key ~ '^[A-Za-z0-9_.-]{1,128}$'
        ),
    intent_digest text NOT NULL
        CONSTRAINT coding_control_receipts_intent_digest_check CHECK (
            intent_digest ~ '^[0-9a-f]{64}$'
        ),
    base_revision bigint NOT NULL
        CONSTRAINT coding_control_receipts_base_revision_check CHECK (
            base_revision BETWEEN 1 AND 9007199254740991
        ),
    result_revision bigint NOT NULL
        CONSTRAINT coding_control_receipts_result_revision_check CHECK (
            result_revision BETWEEN 2 AND 9007199254740991
        ),
    receipt_json jsonb NOT NULL
        CONSTRAINT coding_control_receipts_receipt_json_check CHECK (
            jsonb_typeof(receipt_json) = 'object'
        ),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT coding_control_receipts_pkey
        PRIMARY KEY (owner_id, project_alias, task_id, action, idempotency_key),
    CONSTRAINT coding_control_receipts_task_fk FOREIGN KEY (owner_id, project_alias, task_id)
        REFERENCES forge_agent.coding_tasks (owner_id, project_alias, id),
    CONSTRAINT coding_control_receipts_revision_step_check CHECK (
        result_revision = base_revision + 1
    )
);

REVOKE ALL ON TABLE
    forge_agent.coding_tasks,
    forge_agent.coding_events,
    forge_agent.coding_control_receipts
FROM PUBLIC;
