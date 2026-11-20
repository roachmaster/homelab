-- Admit the explicitly versioned task-scoped conversation policy without
-- rewriting historical v1/v2 recipes, attempts, results, receipts or privileges.
-- Session reuse remains a parent-owned execution decision, not SQL authority.
-- Only workers advertising the exact pinned policy digest may claim a task.
ALTER TABLE forge_agent.coding_attempts
    DROP CONSTRAINT coding_attempts_pinned_recipe_check,
    ADD CONSTRAINT coding_attempts_pinned_recipe_check CHECK (
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
             AND pinned_recipe->>'resource_profile' IN ('forge-coding-v1', 'forge-coding-v2', 'forge-coding-v3')
             AND pinned_recipe->>'mode' IN ('plan', 'review', 'implement')
             AND jsonb_typeof(pinned_recipe->'generation_token_limit') = 'null'
             AND (pinned_recipe->>'policy_digest') ~ '^[0-9a-f]{64}$') IS TRUE
        );

CREATE OR REPLACE FUNCTION forge_coding_worker_api.claim(
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
          AND (t.pinned_recipe->>'resource_profile') IN ('forge-coding-v1', 'forge-coding-v2', 'forge-coding-v3')
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
            'resume_workspace_digest', v_resume_digest,
            'resume_attempt_id', CASE WHEN v_resume_digest IS NOT NULL THEN v_task.latest_attempt_id ELSE NULL END
        );
    END LOOP;

    RETURN NULL;
END;
$fn$;

-- CREATE OR REPLACE preserves the existing scoped EXECUTE grants.
REVOKE ALL ON FUNCTION forge_coding_worker_api.claim(text, uuid, text[]) FROM PUBLIC;
