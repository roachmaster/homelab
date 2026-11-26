-- M41: Persistent per-agent cached branches with exclusive active-workflow
-- ownership, correlation-scoped Jenkins event indexing, and immutable stage
-- results + bounded redacted console log chunks.
-- Preserves all M01..M38 tables, triggers, constraints and pinned function
-- fingerprints intact.
SET LOCAL lock_timeout='5s';
SET LOCAL statement_timeout='60s';

-- 1. Persistent per-agent branch state and exclusive active-workflow ownership.
CREATE TABLE forge_agent.agent_branch_state (
    owner_id text NOT NULL
        CONSTRAINT agent_branch_state_owner_id_check CHECK (
            octet_length(owner_id) BETWEEN 1 AND 128
            AND btrim(owner_id) <> ''
            AND owner_id ~ '^[ -~]+$'
        ),
    project_alias text NOT NULL
        CONSTRAINT agent_branch_state_project_alias_check CHECK (
            project_alias ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    agent_profile text NOT NULL
        CONSTRAINT agent_branch_state_agent_profile_check CHECK (
            agent_profile ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    branch_name text NOT NULL
        CONSTRAINT agent_branch_state_branch_name_check CHECK (
            branch_name <> 'main'
            AND octet_length(branch_name) BETWEEN 1 AND 128
            AND branch_name ~ '^[A-Za-z0-9][A-Za-z0-9._/-]{0,127}$'
            AND position('..' in branch_name) = 0
            AND position('//' in branch_name) = 0
            AND position('@{' in branch_name) = 0
            AND right(branch_name, 1) NOT IN ('/', '.')
            AND branch_name !~ '(^|/)\.'
            AND branch_name !~ '\.lock($|/)'
        ),
    branch_scope text NOT NULL
        CONSTRAINT agent_branch_state_branch_scope_check CHECK (
            position('..' in branch_scope) = 0
            AND branch_scope ~ '^branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?$'
        ),
    head_commit_sha text
        CONSTRAINT agent_branch_state_head_commit_check CHECK (
            head_commit_sha IS NULL OR head_commit_sha ~ '^[0-9a-f]{40}$'
        ),
    fetched_main_sha text
        CONSTRAINT agent_branch_state_fetched_main_sha_check CHECK (
            fetched_main_sha IS NULL OR fetched_main_sha ~ '^[0-9a-f]{40}$'
        ),
    fetched_main_at timestamptz,
    sync_status text NOT NULL DEFAULT 'unknown'
        CONSTRAINT agent_branch_state_sync_status_check CHECK (
            sync_status IN ('synced_with_main', 'behind_main', 'rebasing', 'conflict', 'unknown')
        ),
    conflict_paths jsonb NOT NULL DEFAULT '[]'::jsonb
        CONSTRAINT agent_branch_state_conflict_paths_check CHECK (
            jsonb_typeof(conflict_paths) = 'array'
            AND jsonb_array_length(conflict_paths) <= 32
        ),
    cache_status text NOT NULL DEFAULT 'unknown'
        CONSTRAINT agent_branch_state_cache_status_check CHECK (
            cache_status IN ('hit_branch', 'hit_main', 'miss', 'disabled', 'unknown')
        ),
    cache_updated_at timestamptz,
    last_task_id uuid,
    last_correlation_id uuid,
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    -- Exclusive active-workflow ownership (reservation) state.
    reservation_status text NOT NULL DEFAULT 'idle'
        CONSTRAINT agent_branch_state_reservation_status_check CHECK (
            reservation_status IN ('idle', 'active')
        ),
    active_scope_kind text
        CONSTRAINT agent_branch_state_active_scope_kind_check CHECK (
            active_scope_kind IS NULL
            OR active_scope_kind IN ('native_workflow', 'standalone_task', 'external_workflow')
        ),
    active_scope_id uuid,
    active_root_task_id uuid,
    active_task_id uuid,
    active_review_id uuid,
    active_publication_id uuid,
    active_candidate_commit text
        CONSTRAINT agent_branch_state_active_candidate_commit_check CHECK (
            active_candidate_commit IS NULL OR active_candidate_commit ~ '^[0-9a-f]{40}$'
        ),
    active_pr_url text
        CONSTRAINT agent_branch_state_active_pr_url_check CHECK (
            active_pr_url IS NULL OR (
                octet_length(active_pr_url) BETWEEN 9 AND 512
                AND active_pr_url ~ '^https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/pull/[1-9][0-9]*$'
            )
        ),
    reservation_epoch bigint NOT NULL DEFAULT 0
        CONSTRAINT agent_branch_state_reservation_epoch_check CHECK (
            reservation_epoch >= 0
        ),
    reserved_at timestamptz,
    released_at timestamptz,
    release_reason text
        CONSTRAINT agent_branch_state_release_reason_check CHECK (
            release_reason IS NULL
            OR release_reason IN ('merged', 'accepted', 'cancelled', 'failed_terminal', 'reconciled_terminal')
        ),
    reservation_blocker text
        CONSTRAINT agent_branch_state_reservation_blocker_check CHECK (
            reservation_blocker IS NULL
            OR reservation_blocker IN ('open_pr_awaiting_closure')
        ),
    CONSTRAINT agent_branch_state_pkey PRIMARY KEY (owner_id, project_alias, agent_profile),
    CONSTRAINT agent_branch_state_unique_branch UNIQUE (owner_id, project_alias, branch_name),
    CONSTRAINT agent_branch_state_reservation_fields_check CHECK (
        (
            (reservation_status = 'idle'
                AND active_scope_kind IS NULL
                AND active_scope_id IS NULL
                AND active_root_task_id IS NULL
                AND active_task_id IS NULL
                AND active_review_id IS NULL
                AND active_publication_id IS NULL
                AND active_candidate_commit IS NULL
                AND active_pr_url IS NULL
                AND reserved_at IS NULL
                AND reservation_blocker IS NULL)
            OR (reservation_status = 'active'
                AND active_scope_kind IS NOT NULL
                AND active_scope_id IS NOT NULL
                AND active_root_task_id IS NOT NULL
                AND active_task_id IS NOT NULL
                AND reserved_at IS NOT NULL
                AND reservation_epoch >= 1)
        ) IS TRUE
    )
);

CREATE INDEX agent_branch_state_by_active_scope
    ON forge_agent.agent_branch_state (owner_id, project_alias, active_scope_kind, active_scope_id)
    WHERE reservation_status = 'active';

REVOKE ALL ON TABLE forge_agent.agent_branch_state FROM PUBLIC;
GRANT SELECT ON TABLE forge_agent.agent_branch_state TO forge_agent;

-- 2. Additive columns and correlation/action indexes on jenkins_events.
-- correlation_id (text) already exists from M27; add action_id and branch_scope.
ALTER TABLE forge_agent.jenkins_events
    ADD COLUMN action_id uuid,
    ADD COLUMN branch_scope text
        CONSTRAINT jenkins_events_branch_scope_check CHECK (
            branch_scope IS NULL
            OR branch_scope IN ('main', 'detached')
            OR (
                position('..' in branch_scope) = 0
                AND branch_scope ~ '^branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?$'
            )
        );

CREATE INDEX jenkins_events_by_correlation
    ON forge_agent.jenkins_events (owner_id, project_alias, correlation_id, sequence)
    WHERE correlation_id IS NOT NULL;

CREATE INDEX jenkins_events_by_action
    ON forge_agent.jenkins_events (owner_id, project_alias, action_id, sequence)
    WHERE action_id IS NOT NULL;

-- 3. Immutable per-stage CI/CD evidence and bounded redacted console log chunks.
CREATE TABLE forge_agent.jenkins_stage_results (
    owner_id text NOT NULL
        CONSTRAINT jenkins_stage_results_owner_id_check CHECK (
            octet_length(owner_id) BETWEEN 1 AND 128
            AND btrim(owner_id) <> ''
            AND owner_id ~ '^[ -~]+$'
        ),
    project_alias text NOT NULL
        CONSTRAINT jenkins_stage_results_project_alias_check CHECK (
            project_alias ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    action_id uuid NOT NULL,
    stage_index integer NOT NULL
        CONSTRAINT jenkins_stage_results_stage_index_check CHECK (
            stage_index BETWEEN 0 AND 127
        ),
    correlation_id uuid NOT NULL,
    task_id uuid,
    job_name text NOT NULL
        CONSTRAINT jenkins_stage_results_job_name_check CHECK (
            job_name ~ '^[A-Za-z0-9_./-]{1,128}$'
            AND left(job_name, 1) <> '/'
            AND right(job_name, 1) <> '/'
            AND position('//' in job_name) = 0
            AND job_name !~ '(^|/)\.\.?($|/)'
        ),
    build_number integer
        CONSTRAINT jenkins_stage_results_build_number_check CHECK (
            build_number IS NULL OR build_number BETWEEN 1 AND 2147483647
        ),
    commit_sha text NOT NULL
        CONSTRAINT jenkins_stage_results_commit_sha_check CHECK (
            commit_sha ~ '^[0-9a-f]{40}$'
        ),
    branch_name text
        CONSTRAINT jenkins_stage_results_branch_name_check CHECK (
            branch_name IS NULL OR (
                octet_length(branch_name) BETWEEN 1 AND 256
                AND btrim(branch_name) <> ''
                AND branch_name !~ '[[:cntrl:]]'
            )
        ),
    branch_scope text NOT NULL
        CONSTRAINT jenkins_stage_results_branch_scope_check CHECK (
            branch_scope IN ('main', 'detached')
            OR (
                position('..' in branch_scope) = 0
                AND branch_scope ~ '^branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?$'
            )
        ),
    pipeline_scope text NOT NULL
        CONSTRAINT jenkins_stage_results_pipeline_scope_check CHECK (
            pipeline_scope IN ('branch', 'main')
        ),
    logical_stage_id text NOT NULL
        CONSTRAINT jenkins_stage_results_logical_stage_id_check CHECK (
            logical_stage_id ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    phase_id text NOT NULL
        CONSTRAINT jenkins_stage_results_phase_id_check CHECK (
            phase_id ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    phase_kind text NOT NULL
        CONSTRAINT jenkins_stage_results_phase_kind_check CHECK (
            phase_kind ~ '^[a-z_]{1,32}$'
        ),
    label text NOT NULL
        CONSTRAINT jenkins_stage_results_label_check CHECK (
            octet_length(label) BETWEEN 1 AND 128
            AND btrim(label) <> ''
            AND label !~ '[[:cntrl:]]'
        ),
    status text NOT NULL
        CONSTRAINT jenkins_stage_results_status_check CHECK (
            status IN ('waiting', 'running', 'passed', 'failed', 'skipped', 'not_run', 'unknown')
        ),
    returncode integer
        CONSTRAINT jenkins_stage_results_returncode_check CHECK (
            returncode IS NULL OR returncode BETWEEN -255 AND 255
        ),
    duration_ms integer
        CONSTRAINT jenkins_stage_results_duration_ms_check CHECK (
            duration_ms IS NULL OR duration_ms BETWEEN 0 AND 86400000
        ),
    tests_passed integer
        CONSTRAINT jenkins_stage_results_tests_passed_check CHECK (
            tests_passed IS NULL OR tests_passed >= 0
        ),
    tests_failed integer
        CONSTRAINT jenkins_stage_results_tests_failed_check CHECK (
            tests_failed IS NULL OR tests_failed >= 0
        ),
    tests_skipped integer
        CONSTRAINT jenkins_stage_results_tests_skipped_check CHECK (
            tests_skipped IS NULL OR tests_skipped >= 0
        ),
    test_summary jsonb NOT NULL DEFAULT '{}'::jsonb
        CONSTRAINT jenkins_stage_results_test_summary_check CHECK (
            jsonb_typeof(test_summary) = 'object'
            AND octet_length(test_summary::text) <= 32768
        ),
    error_excerpt text
        CONSTRAINT jenkins_stage_results_error_excerpt_check CHECK (
            error_excerpt IS NULL OR octet_length(error_excerpt) <= 4096
        ),
    log_sha256 text
        CONSTRAINT jenkins_stage_results_log_sha256_check CHECK (
            log_sha256 IS NULL OR log_sha256 ~ '^[0-9a-f]{64}$'
        ),
    log_byte_count integer NOT NULL DEFAULT 0
        CONSTRAINT jenkins_stage_results_log_byte_count_check CHECK (
            log_byte_count BETWEEN 0 AND 262144
        ),
    log_chunk_count integer NOT NULL DEFAULT 0
        CONSTRAINT jenkins_stage_results_log_chunk_count_check CHECK (
            log_chunk_count BETWEEN 0 AND 16
        ),
    receipt_sha256 text NOT NULL
        CONSTRAINT jenkins_stage_results_receipt_sha256_check CHECK (
            receipt_sha256 ~ '^[0-9a-f]{64}$'
        ),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT jenkins_stage_results_pkey PRIMARY KEY (owner_id, project_alias, action_id, stage_index)
);

CREATE INDEX jenkins_stage_results_by_correlation
    ON forge_agent.jenkins_stage_results (owner_id, project_alias, correlation_id, created_at, action_id, stage_index);

CREATE FUNCTION forge_agent.jenkins_stage_results_immutable()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $$
BEGIN
    RAISE EXCEPTION 'Jenkins stage results are append-only and immutable'
        USING ERRCODE = 'P0009';
END;
$$;

CREATE TRIGGER jenkins_stage_results_immutable
    BEFORE UPDATE OR DELETE ON forge_agent.jenkins_stage_results
    FOR EACH ROW
    EXECUTE FUNCTION forge_agent.jenkins_stage_results_immutable();

REVOKE ALL ON TABLE forge_agent.jenkins_stage_results FROM PUBLIC;
REVOKE ALL ON FUNCTION forge_agent.jenkins_stage_results_immutable() FROM PUBLIC;
GRANT SELECT ON TABLE forge_agent.jenkins_stage_results TO forge_agent;

CREATE TABLE forge_agent.jenkins_stage_log_chunks (
    owner_id text NOT NULL,
    project_alias text NOT NULL,
    action_id uuid NOT NULL,
    stage_index integer NOT NULL
        CONSTRAINT jenkins_stage_log_chunks_stage_index_check CHECK (
            stage_index BETWEEN 0 AND 127
        ),
    chunk_index integer NOT NULL
        CONSTRAINT jenkins_stage_log_chunks_chunk_index_check CHECK (
            chunk_index BETWEEN 0 AND 15
        ),
    chunk_bytes bytea NOT NULL
        CONSTRAINT jenkins_stage_log_chunks_bytes_check CHECK (
            octet_length(chunk_bytes) BETWEEN 1 AND 16384
        ),
    byte_offset integer NOT NULL
        CONSTRAINT jenkins_stage_log_chunks_offset_check CHECK (
            byte_offset BETWEEN 0 AND 262143
        ),
    byte_len integer NOT NULL
        CONSTRAINT jenkins_stage_log_chunks_len_check CHECK (
            byte_len = octet_length(chunk_bytes)
            AND byte_offset + byte_len <= 262144
        ),
    chunk_sha256 text NOT NULL
        CONSTRAINT jenkins_stage_log_chunks_sha256_check CHECK (
            chunk_sha256 ~ '^[0-9a-f]{64}$'
        ),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT jenkins_stage_log_chunks_pkey
        PRIMARY KEY (owner_id, project_alias, action_id, stage_index, chunk_index),
    CONSTRAINT jenkins_stage_log_chunks_stage_fk
        FOREIGN KEY (owner_id, project_alias, action_id, stage_index)
        REFERENCES forge_agent.jenkins_stage_results (owner_id, project_alias, action_id, stage_index)
);

CREATE FUNCTION forge_agent.jenkins_stage_log_chunks_immutable()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $$
BEGIN
    RAISE EXCEPTION 'Jenkins stage log chunks are append-only and immutable'
        USING ERRCODE = 'P0009';
END;
$$;

CREATE TRIGGER jenkins_stage_log_chunks_immutable
    BEFORE UPDATE OR DELETE ON forge_agent.jenkins_stage_log_chunks
    FOR EACH ROW
    EXECUTE FUNCTION forge_agent.jenkins_stage_log_chunks_immutable();

REVOKE ALL ON TABLE forge_agent.jenkins_stage_log_chunks FROM PUBLIC;
REVOKE ALL ON FUNCTION forge_agent.jenkins_stage_log_chunks_immutable() FROM PUBLIC;
GRANT SELECT ON TABLE forge_agent.jenkins_stage_log_chunks TO forge_agent;

-- 4. Scoped reconciler API functions (preserving all M01..M38 pinned functions).
CREATE FUNCTION forge_coding_reconciler_api.task_branch_scope(
    p_project_alias text,
    p_task_id uuid
) RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_owner text := current_setting('forge.coding_owner', true);
    t forge_agent.coding_tasks%ROWTYPE;
    v_scope_kind text;
    v_scope_id uuid;
    v_root_task_id uuid;
BEGIN
    IF v_owner IS NULL OR btrim(v_owner) = '' OR p_project_alias IS NULL
       OR p_project_alias !~ '^[A-Za-z0-9_.-]{1,64}$' OR p_task_id IS NULL THEN
        RAISE EXCEPTION 'Invalid task branch scope request' USING ERRCODE = '22023';
    END IF;
    SELECT * INTO t FROM forge_agent.coding_tasks
    WHERE owner_id = v_owner AND project_alias = p_project_alias AND id = p_task_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'Task branch scope denied' USING ERRCODE = '42501';
    END IF;
    IF t.workflow_id IS NOT NULL THEN
        v_scope_kind := 'native_workflow';
        v_scope_id := t.workflow_id;
        SELECT w.root_task_id INTO v_root_task_id
        FROM forge_agent.coding_workflows w
        WHERE w.owner_id = t.owner_id AND w.project_alias = t.project_alias AND w.workflow_id = t.workflow_id;
        v_root_task_id := coalesce(v_root_task_id, t.repair_root_task_id, t.id);
    ELSE
        v_scope_kind := 'standalone_task';
        v_root_task_id := coalesce(t.repair_root_task_id, t.id);
        v_scope_id := v_root_task_id;
    END IF;
    RETURN jsonb_build_object(
        'owner_id', t.owner_id,
        'project_alias', t.project_alias,
        'task_id', t.id,
        'agent_profile', t.agent_profile,
        'scope_kind', v_scope_kind,
        'scope_id', v_scope_id,
        'root_task_id', v_root_task_id,
        'workflow_id', t.workflow_id,
        'correlation_id', t.correlation_id
    );
END;
$fn$;

CREATE FUNCTION forge_coding_reconciler_api._branch_row_json(r forge_agent.agent_branch_state)
RETURNS jsonb
LANGUAGE sql
IMMUTABLE
SET search_path = pg_catalog, pg_temp
AS $fn$
    SELECT jsonb_build_object(
        'owner_id', r.owner_id,
        'project_alias', r.project_alias,
        'agent_profile', r.agent_profile,
        'branch_name', r.branch_name,
        'branch_scope', r.branch_scope,
        'head_commit_sha', r.head_commit_sha,
        'fetched_main_sha', r.fetched_main_sha,
        'fetched_main_at', CASE WHEN r.fetched_main_at IS NOT NULL THEN to_char(r.fetched_main_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') ELSE NULL END,
        'sync_status', r.sync_status,
        'conflict_paths', r.conflict_paths,
        'cache_status', r.cache_status,
        'cache_updated_at', CASE WHEN r.cache_updated_at IS NOT NULL THEN to_char(r.cache_updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') ELSE NULL END,
        'last_task_id', r.last_task_id,
        'last_correlation_id', r.last_correlation_id,
        'updated_at', to_char(r.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
        'reservation_status', r.reservation_status,
        'active_scope_kind', r.active_scope_kind,
        'active_scope_id', r.active_scope_id,
        'active_root_task_id', r.active_root_task_id,
        'active_task_id', r.active_task_id,
        'active_review_id', r.active_review_id,
        'active_publication_id', r.active_publication_id,
        'active_candidate_commit', r.active_candidate_commit,
        'active_pr_url', r.active_pr_url,
        'reservation_epoch', r.reservation_epoch,
        'reserved_at', CASE WHEN r.reserved_at IS NOT NULL THEN to_char(r.reserved_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') ELSE NULL END,
        'released_at', CASE WHEN r.released_at IS NOT NULL THEN to_char(r.released_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') ELSE NULL END,
        'release_reason', r.release_reason,
        'reservation_blocker', r.reservation_blocker
    );
$fn$;

CREATE FUNCTION forge_coding_reconciler_api.agent_branch_get(
    p_project_alias text,
    p_agent_profile text
) RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_owner text := current_setting('forge.coding_owner', true);
    r forge_agent.agent_branch_state%ROWTYPE;
BEGIN
    IF v_owner IS NULL OR btrim(v_owner) = '' OR p_project_alias IS NULL OR p_agent_profile IS NULL THEN
        RAISE EXCEPTION 'Invalid agent branch lookup' USING ERRCODE = '22023';
    END IF;
    SELECT * INTO r FROM forge_agent.agent_branch_state
    WHERE owner_id = v_owner AND project_alias = p_project_alias AND agent_profile = p_agent_profile;
    IF NOT FOUND THEN RETURN NULL; END IF;
    RETURN forge_coding_reconciler_api._branch_row_json(r);
END;
$fn$;

CREATE FUNCTION forge_coding_reconciler_api.agent_branch_upsert(
    p_project_alias text,
    p_agent_profile text,
    p_branch_name text,
    p_branch_scope text,
    p_head_commit_sha text DEFAULT NULL,
    p_fetched_main_sha text DEFAULT NULL,
    p_fetched_main_at timestamptz DEFAULT NULL,
    p_sync_status text DEFAULT NULL,
    p_conflict_paths jsonb DEFAULT NULL,
    p_cache_status text DEFAULT NULL,
    p_last_task_id uuid DEFAULT NULL,
    p_last_correlation_id uuid DEFAULT NULL
) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_owner text := current_setting('forge.coding_owner', true);
    r forge_agent.agent_branch_state%ROWTYPE;
    v_now timestamptz := clock_timestamp();
BEGIN
    IF v_owner IS NULL OR btrim(v_owner) = '' OR p_project_alias IS NULL
       OR p_project_alias !~ '^[A-Za-z0-9_.-]{1,64}$'
       OR p_agent_profile IS NULL OR p_agent_profile !~ '^[A-Za-z0-9_.-]{1,64}$'
       OR p_branch_name IS NULL OR p_branch_name = 'main'
       OR p_branch_scope IS NULL THEN
        RAISE EXCEPTION 'Invalid agent branch state' USING ERRCODE = '22023';
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended(v_owner || E'\n' || p_project_alias, 2026111901));
    SELECT * INTO r FROM forge_agent.agent_branch_state
    WHERE owner_id = v_owner AND project_alias = p_project_alias AND agent_profile = p_agent_profile
    FOR UPDATE;
    IF NOT FOUND THEN
        INSERT INTO forge_agent.agent_branch_state(
            owner_id, project_alias, agent_profile, branch_name, branch_scope,
            head_commit_sha, fetched_main_sha, fetched_main_at,
            sync_status, conflict_paths, cache_status, cache_updated_at,
            last_task_id, last_correlation_id, updated_at
        ) VALUES (
            v_owner, p_project_alias, p_agent_profile, p_branch_name, p_branch_scope,
            p_head_commit_sha, p_fetched_main_sha, p_fetched_main_at,
            coalesce(p_sync_status, 'unknown'), coalesce(p_conflict_paths, '[]'::jsonb),
            coalesce(p_cache_status, 'unknown'), CASE WHEN p_cache_status IS NOT NULL THEN v_now ELSE NULL END,
            p_last_task_id, p_last_correlation_id, v_now
        ) RETURNING * INTO r;
    ELSE
        UPDATE forge_agent.agent_branch_state
        SET branch_name = p_branch_name,
            branch_scope = p_branch_scope,
            head_commit_sha = coalesce(p_head_commit_sha, head_commit_sha),
            fetched_main_sha = coalesce(p_fetched_main_sha, fetched_main_sha),
            fetched_main_at = coalesce(p_fetched_main_at, fetched_main_at),
            sync_status = coalesce(p_sync_status, sync_status),
            conflict_paths = coalesce(p_conflict_paths, conflict_paths),
            cache_status = coalesce(p_cache_status, cache_status),
            cache_updated_at = CASE WHEN p_cache_status IS NOT NULL THEN v_now ELSE cache_updated_at END,
            last_task_id = coalesce(p_last_task_id, last_task_id),
            last_correlation_id = coalesce(p_last_correlation_id, last_correlation_id),
            updated_at = v_now
        WHERE owner_id = v_owner AND project_alias = p_project_alias AND agent_profile = p_agent_profile
        RETURNING * INTO r;
    END IF;
    RETURN forge_coding_reconciler_api._branch_row_json(r);
END;
$fn$;

CREATE FUNCTION forge_coding_reconciler_api.agent_branch_acquire(
    p_project_alias text,
    p_agent_profile text,
    p_branch_name text,
    p_branch_scope text,
    p_scope_kind text,
    p_scope_id uuid,
    p_root_task_id uuid,
    p_task_id uuid,
    p_review_id uuid DEFAULT NULL,
    p_publication_id uuid DEFAULT NULL
) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_owner text := current_setting('forge.coding_owner', true);
    r forge_agent.agent_branch_state%ROWTYPE;
    v_now timestamptz := clock_timestamp();
BEGIN
    IF v_owner IS NULL OR btrim(v_owner) = '' OR p_project_alias IS NULL
       OR p_project_alias !~ '^[A-Za-z0-9_.-]{1,64}$'
       OR p_agent_profile IS NULL OR p_agent_profile !~ '^[A-Za-z0-9_.-]{1,64}$'
       OR p_branch_name IS NULL OR p_branch_name = 'main'
       OR p_branch_scope IS NULL
       OR p_scope_kind NOT IN ('native_workflow', 'standalone_task', 'external_workflow')
       OR p_scope_id IS NULL OR p_root_task_id IS NULL OR p_task_id IS NULL THEN
        RAISE EXCEPTION 'Invalid agent branch acquisition parameters' USING ERRCODE = '22023';
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended(v_owner || E'\n' || p_project_alias, 2026111901));
    SELECT * INTO r FROM forge_agent.agent_branch_state
    WHERE owner_id = v_owner AND project_alias = p_project_alias AND agent_profile = p_agent_profile
    FOR UPDATE;
    IF NOT FOUND THEN
        INSERT INTO forge_agent.agent_branch_state(
            owner_id, project_alias, agent_profile, branch_name, branch_scope,
            reservation_status, active_scope_kind, active_scope_id,
            active_root_task_id, active_task_id, active_review_id, active_publication_id,
            reservation_epoch, reserved_at, last_task_id, updated_at
        ) VALUES (
            v_owner, p_project_alias, p_agent_profile, p_branch_name, p_branch_scope,
            'active', p_scope_kind, p_scope_id,
            p_root_task_id, p_task_id, p_review_id, p_publication_id,
            1, v_now, p_task_id, v_now
        ) RETURNING * INTO r;
        RETURN jsonb_build_object('acquired', true, 'reentrant', false)
            || forge_coding_reconciler_api._branch_row_json(r);
    END IF;
    IF r.reservation_status = 'idle' THEN
        UPDATE forge_agent.agent_branch_state
        SET branch_name = p_branch_name,
            branch_scope = p_branch_scope,
            reservation_status = 'active',
            active_scope_kind = p_scope_kind,
            active_scope_id = p_scope_id,
            active_root_task_id = p_root_task_id,
            active_task_id = p_task_id,
            active_review_id = p_review_id,
            active_publication_id = p_publication_id,
            active_candidate_commit = NULL,
            active_pr_url = NULL,
            reservation_epoch = r.reservation_epoch + 1,
            reserved_at = v_now,
            released_at = NULL,
            release_reason = NULL,
            reservation_blocker = NULL,
            last_task_id = p_task_id,
            updated_at = v_now
        WHERE owner_id = v_owner AND project_alias = p_project_alias AND agent_profile = p_agent_profile
        RETURNING * INTO r;
        RETURN jsonb_build_object('acquired', true, 'reentrant', false)
            || forge_coding_reconciler_api._branch_row_json(r);
    END IF;
    IF r.active_scope_kind = p_scope_kind AND r.active_scope_id = p_scope_id THEN
        UPDATE forge_agent.agent_branch_state
        SET active_task_id = p_task_id,
            active_review_id = coalesce(p_review_id, active_review_id),
            active_publication_id = coalesce(p_publication_id, active_publication_id),
            last_task_id = p_task_id,
            updated_at = v_now
        WHERE owner_id = v_owner AND project_alias = p_project_alias AND agent_profile = p_agent_profile
        RETURNING * INTO r;
        RETURN jsonb_build_object('acquired', true, 'reentrant', true)
            || forge_coding_reconciler_api._branch_row_json(r);
    END IF;
    RETURN jsonb_build_object('acquired', false, 'conflict', true)
        || forge_coding_reconciler_api._branch_row_json(r);
END;
$fn$;

CREATE FUNCTION forge_coding_reconciler_api.agent_branch_verify_owner(
    p_project_alias text,
    p_agent_profile text,
    p_scope_kind text,
    p_scope_id uuid,
    p_expected_epoch bigint
) RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_owner text := current_setting('forge.coding_owner', true);
    r forge_agent.agent_branch_state%ROWTYPE;
    v_valid boolean;
BEGIN
    IF v_owner IS NULL OR btrim(v_owner) = '' OR p_project_alias IS NULL OR p_agent_profile IS NULL
       OR p_scope_kind IS NULL OR p_scope_id IS NULL OR p_expected_epoch IS NULL THEN
        RAISE EXCEPTION 'Invalid agent branch owner check' USING ERRCODE = '22023';
    END IF;
    SELECT * INTO r FROM forge_agent.agent_branch_state
    WHERE owner_id = v_owner AND project_alias = p_project_alias AND agent_profile = p_agent_profile;
    IF NOT FOUND THEN
        RETURN jsonb_build_object('valid', false, 'reason', 'missing');
    END IF;
    v_valid := (r.reservation_status = 'active'
        AND r.active_scope_kind = p_scope_kind
        AND r.active_scope_id = p_scope_id
        AND r.reservation_epoch = p_expected_epoch);
    RETURN jsonb_build_object('valid', v_valid) || forge_coding_reconciler_api._branch_row_json(r);
END;
$fn$;

CREATE FUNCTION forge_coding_reconciler_api.agent_branch_record_publication(
    p_project_alias text,
    p_agent_profile text,
    p_scope_kind text,
    p_scope_id uuid,
    p_expected_epoch bigint,
    p_candidate_commit text DEFAULT NULL,
    p_pr_url text DEFAULT NULL,
    p_publication_id uuid DEFAULT NULL
) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_owner text := current_setting('forge.coding_owner', true);
    r forge_agent.agent_branch_state%ROWTYPE;
BEGIN
    IF v_owner IS NULL OR btrim(v_owner) = '' OR p_project_alias IS NULL OR p_agent_profile IS NULL
       OR p_scope_kind IS NULL OR p_scope_id IS NULL OR p_expected_epoch IS NULL THEN
        RAISE EXCEPTION 'Invalid agent branch publication update' USING ERRCODE = '22023';
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended(v_owner || E'\n' || p_project_alias, 2026111901));
    SELECT * INTO r FROM forge_agent.agent_branch_state
    WHERE owner_id = v_owner AND project_alias = p_project_alias AND agent_profile = p_agent_profile
    FOR UPDATE;
    IF NOT FOUND OR r.reservation_status <> 'active'
       OR r.active_scope_kind IS DISTINCT FROM p_scope_kind
       OR r.active_scope_id IS DISTINCT FROM p_scope_id
       OR r.reservation_epoch IS DISTINCT FROM p_expected_epoch THEN
        RAISE EXCEPTION 'Agent branch ownership epoch mismatch' USING ERRCODE = 'P0006';
    END IF;
    UPDATE forge_agent.agent_branch_state
    SET head_commit_sha = coalesce(p_candidate_commit, head_commit_sha),
        active_candidate_commit = coalesce(p_candidate_commit, active_candidate_commit),
        active_pr_url = coalesce(p_pr_url, active_pr_url),
        active_publication_id = coalesce(p_publication_id, active_publication_id),
        updated_at = clock_timestamp()
    WHERE owner_id = v_owner AND project_alias = p_project_alias AND agent_profile = p_agent_profile
    RETURNING * INTO r;
    RETURN forge_coding_reconciler_api._branch_row_json(r);
END;
$fn$;

CREATE FUNCTION forge_coding_reconciler_api.agent_branch_set_blocker(
    p_project_alias text,
    p_agent_profile text,
    p_scope_kind text,
    p_scope_id uuid,
    p_expected_epoch bigint,
    p_blocker text
) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_owner text := current_setting('forge.coding_owner', true);
    r forge_agent.agent_branch_state%ROWTYPE;
BEGIN
    IF v_owner IS NULL OR btrim(v_owner) = '' OR p_project_alias IS NULL OR p_agent_profile IS NULL
       OR p_scope_kind IS NULL OR p_scope_id IS NULL OR p_expected_epoch IS NULL THEN
        RAISE EXCEPTION 'Invalid agent branch blocker update' USING ERRCODE = '22023';
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended(v_owner || E'\n' || p_project_alias, 2026111901));
    SELECT * INTO r FROM forge_agent.agent_branch_state
    WHERE owner_id = v_owner AND project_alias = p_project_alias AND agent_profile = p_agent_profile
    FOR UPDATE;
    IF NOT FOUND OR r.reservation_status <> 'active'
       OR r.active_scope_kind IS DISTINCT FROM p_scope_kind
       OR r.active_scope_id IS DISTINCT FROM p_scope_id
       OR r.reservation_epoch IS DISTINCT FROM p_expected_epoch THEN
        RAISE EXCEPTION 'Agent branch ownership epoch mismatch' USING ERRCODE = 'P0006';
    END IF;
    UPDATE forge_agent.agent_branch_state
    SET reservation_blocker = p_blocker,
        updated_at = clock_timestamp()
    WHERE owner_id = v_owner AND project_alias = p_project_alias AND agent_profile = p_agent_profile
    RETURNING * INTO r;
    RETURN forge_coding_reconciler_api._branch_row_json(r);
END;
$fn$;

CREATE FUNCTION forge_coding_reconciler_api.agent_branch_release(
    p_project_alias text,
    p_agent_profile text,
    p_scope_kind text,
    p_scope_id uuid,
    p_expected_epoch bigint,
    p_release_reason text,
    p_remote_open_pr_confirmed_none boolean,
    p_merge_commit text DEFAULT NULL
) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_owner text := current_setting('forge.coding_owner', true);
    r forge_agent.agent_branch_state%ROWTYPE;
    v_now timestamptz := clock_timestamp();
BEGIN
    IF v_owner IS NULL OR btrim(v_owner) = '' OR p_project_alias IS NULL OR p_agent_profile IS NULL
       OR p_scope_kind IS NULL OR p_scope_id IS NULL OR p_expected_epoch IS NULL
       OR p_release_reason NOT IN ('merged', 'accepted', 'cancelled', 'failed_terminal', 'reconciled_terminal') THEN
        RAISE EXCEPTION 'Invalid agent branch release parameters' USING ERRCODE = '22023';
    END IF;
    IF p_remote_open_pr_confirmed_none IS DISTINCT FROM true THEN
        RAISE EXCEPTION 'Remote zero open PR confirmation required before release' USING ERRCODE = 'P0009';
    END IF;
    IF p_merge_commit IS NOT NULL AND p_merge_commit !~ '^[0-9a-f]{40}$' THEN
        RAISE EXCEPTION 'Invalid merge commit SHA' USING ERRCODE = '22023';
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended(v_owner || E'\n' || p_project_alias, 2026111901));
    SELECT * INTO r FROM forge_agent.agent_branch_state
    WHERE owner_id = v_owner AND project_alias = p_project_alias AND agent_profile = p_agent_profile
    FOR UPDATE;
    IF NOT FOUND OR r.reservation_status <> 'active'
       OR r.active_scope_kind IS DISTINCT FROM p_scope_kind
       OR r.active_scope_id IS DISTINCT FROM p_scope_id
       OR r.reservation_epoch IS DISTINCT FROM p_expected_epoch THEN
        RAISE EXCEPTION 'Agent branch ownership epoch mismatch on release' USING ERRCODE = 'P0006';
    END IF;
    UPDATE forge_agent.agent_branch_state
    SET reservation_status = 'idle',
        active_scope_kind = NULL,
        active_scope_id = NULL,
        active_root_task_id = NULL,
        active_task_id = NULL,
        active_review_id = NULL,
        active_publication_id = NULL,
        active_candidate_commit = NULL,
        active_pr_url = NULL,
        reservation_epoch = r.reservation_epoch + 1,
        reserved_at = NULL,
        released_at = v_now,
        release_reason = p_release_reason,
        reservation_blocker = NULL,
        fetched_main_sha = coalesce(p_merge_commit, fetched_main_sha),
        fetched_main_at = CASE WHEN p_merge_commit IS NOT NULL THEN v_now ELSE fetched_main_at END,
        sync_status = CASE WHEN p_merge_commit IS NOT NULL THEN 'synced_with_main' ELSE sync_status END,
        conflict_paths = CASE WHEN p_merge_commit IS NOT NULL THEN '[]'::jsonb ELSE conflict_paths END,
        updated_at = v_now
    WHERE owner_id = v_owner AND project_alias = p_project_alias AND agent_profile = p_agent_profile
    RETURNING * INTO r;
    PERFORM pg_notify('forge_coding_jobs', '');
    RETURN forge_coding_reconciler_api._branch_row_json(r);
END;
$fn$;

CREATE FUNCTION forge_coding_reconciler_api.agent_branch_reconcilable_reservations(
    p_limit integer DEFAULT 64
) RETURNS jsonb
LANGUAGE plpgsql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_owner text := current_setting('forge.coding_owner', true);
    v_limit integer := greatest(1, least(coalesce(p_limit, 64), 128));
    v_rows jsonb;
BEGIN
    IF v_owner IS NULL OR btrim(v_owner) = '' THEN
        RAISE EXCEPTION 'Missing coding owner for branch reconciliation' USING ERRCODE = '42501';
    END IF;
    SELECT coalesce(jsonb_agg(item ORDER BY reserved_at, project_alias, agent_profile), '[]'::jsonb)
    INTO v_rows
    FROM (
        SELECT
            b.reserved_at,
            b.project_alias,
            b.agent_profile,
            forge_coding_reconciler_api._branch_row_json(b) || jsonb_build_object(
                'terminal_reason',
                CASE
                    WHEN b.active_scope_kind = 'native_workflow' THEN (
                        SELECT CASE WHEN w.phase = 'completed' THEN 'merged' ELSE 'cancelled' END
                        FROM forge_agent.coding_workflows w
                        WHERE w.owner_id = b.owner_id AND w.project_alias = b.project_alias AND w.workflow_id = b.active_scope_id
                    )
                    WHEN b.active_scope_kind = 'external_workflow' THEN (
                        SELECT CASE WHEN ew.phase = 'completed' THEN 'merged' ELSE 'cancelled' END
                        FROM forge_external_workflow.workflows ew
                        WHERE ew.owner_id = b.owner_id AND ew.project_alias = b.project_alias AND ew.workflow_id = b.active_scope_id
                    )
                    ELSE (
                        SELECT CASE
                            WHEN rt.status = 'completed' THEN 'accepted'
                            WHEN rt.status = 'cancelled' THEN 'cancelled'
                            ELSE 'failed_terminal'
                        END
                        FROM forge_agent.coding_tasks rt
                        WHERE rt.owner_id = b.owner_id AND rt.project_alias = b.project_alias AND rt.id = b.active_root_task_id
                    )
                END
            ) AS item
        FROM forge_agent.agent_branch_state b
        WHERE b.owner_id = v_owner
          AND b.reservation_status = 'active'
          AND (
              (b.active_scope_kind = 'native_workflow' AND EXISTS (
                  SELECT 1 FROM forge_agent.coding_workflows w
                  WHERE w.owner_id = b.owner_id AND w.project_alias = b.project_alias
                    AND w.workflow_id = b.active_scope_id
                    AND w.phase IN ('completed', 'cancelled')
              ) AND NOT EXISTS (
                  SELECT 1 FROM forge_agent.coding_tasks t
                  JOIN forge_agent.coding_attempts a ON (a.owner_id, a.project_alias, a.task_id) = (t.owner_id, t.project_alias, t.id)
                  WHERE t.owner_id = b.owner_id AND t.project_alias = b.project_alias
                    AND t.workflow_id = b.active_scope_id AND a.reservation_active
              ) AND NOT EXISTS (
                  SELECT 1 FROM forge_agent.coding_tasks t
                  JOIN forge_agent.coding_reviews q ON (q.owner_id, q.project_alias, q.task_id) = (t.owner_id, t.project_alias, t.id)
                  WHERE t.owner_id = b.owner_id AND t.project_alias = b.project_alias
                    AND t.workflow_id = b.active_scope_id AND q.status IN ('pending', 'checking')
              ) AND NOT EXISTS (
                  SELECT 1 FROM forge_agent.coding_tasks t
                  JOIN forge_agent.coding_publications p ON (p.owner_id, p.project_alias, p.task_id) = (t.owner_id, t.project_alias, t.id)
                  WHERE t.owner_id = b.owner_id AND t.project_alias = b.project_alias
                    AND t.workflow_id = b.active_scope_id AND p.status IN ('pending', 'checking', 'publishing')
              ) AND NOT EXISTS (
                  SELECT 1 FROM forge_agent.coding_workflow_actions wa
                  WHERE wa.owner_id = b.owner_id AND wa.project_alias = b.project_alias
                    AND wa.workflow_id = b.active_scope_id AND wa.kind = 'merge'
                    AND wa.status IN ('pending', 'leased')
              ))
              OR (b.active_scope_kind = 'standalone_task' AND EXISTS (
                  SELECT 1 FROM forge_agent.coding_tasks rt
                  WHERE rt.owner_id = b.owner_id AND rt.project_alias = b.project_alias
                    AND rt.id = b.active_root_task_id
                    AND rt.status IN ('completed', 'cancelled', 'failed')
              ) AND NOT EXISTS (
                  SELECT 1 FROM forge_agent.coding_tasks t
                  WHERE t.owner_id = b.owner_id AND t.project_alias = b.project_alias
                    AND (t.id = b.active_root_task_id OR t.repair_root_task_id = b.active_root_task_id)
                    AND t.status NOT IN ('completed', 'cancelled', 'failed')
              ) AND NOT EXISTS (
                  SELECT 1 FROM forge_agent.coding_tasks t
                  JOIN forge_agent.coding_attempts a ON (a.owner_id, a.project_alias, a.task_id) = (t.owner_id, t.project_alias, t.id)
                  WHERE t.owner_id = b.owner_id AND t.project_alias = b.project_alias
                    AND (t.id = b.active_root_task_id OR t.repair_root_task_id = b.active_root_task_id)
                    AND a.reservation_active
              ) AND NOT EXISTS (
                  SELECT 1 FROM forge_agent.coding_tasks t
                  JOIN forge_agent.coding_reviews q ON (q.owner_id, q.project_alias, q.task_id) = (t.owner_id, t.project_alias, t.id)
                  WHERE t.owner_id = b.owner_id AND t.project_alias = b.project_alias
                    AND (t.id = b.active_root_task_id OR t.repair_root_task_id = b.active_root_task_id)
                    AND q.status IN ('pending', 'checking')
              ) AND NOT EXISTS (
                  SELECT 1 FROM forge_agent.coding_tasks t
                  JOIN forge_agent.coding_publications p ON (p.owner_id, p.project_alias, p.task_id) = (t.owner_id, t.project_alias, t.id)
                  WHERE t.owner_id = b.owner_id AND t.project_alias = b.project_alias
                    AND (t.id = b.active_root_task_id OR t.repair_root_task_id = b.active_root_task_id)
                    AND p.status IN ('pending', 'checking', 'publishing')
              ))
              OR (b.active_scope_kind = 'external_workflow' AND EXISTS (
                  SELECT 1 FROM forge_external_workflow.workflows ew
                  WHERE ew.owner_id = b.owner_id AND ew.project_alias = b.project_alias
                    AND ew.workflow_id = b.active_scope_id
                    AND ew.phase IN ('completed', 'cancelled')
              ) AND NOT EXISTS (
                  SELECT 1 FROM forge_external_workflow.actions ea
                  WHERE ea.owner_id = b.owner_id AND ea.project_alias = b.project_alias
                    AND ea.workflow_id = b.active_scope_id
                    AND ea.status IN ('pending', 'leased')
              ))
          )
        LIMIT v_limit
    ) sub;
    RETURN v_rows;
END;
$fn$;

-- Branch-ownership-aware additive claim wrappers (leave pinned review_claim /
-- publication_claim completely unmodified).
CREATE FUNCTION forge_coding_reconciler_api.review_claim_for_branch_policy(p_worker_id text)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    j forge_agent.coding_reviews%ROWTYPE;
    p forge_agent.coding_review_policy%ROWTYPE;
    t_row forge_agent.coding_tasks%ROWTYPE;
    frozen_policy jsonb;
BEGIN
    IF p_worker_id IS NULL OR p_worker_id !~ '^[A-Za-z0-9_.-]{1,64}$' THEN
        RAISE EXCEPTION 'Invalid review worker' USING ERRCODE = '22023';
    END IF;
    INSERT INTO forge_agent.coding_reviews(owner_id,project_alias,task_id,attempt_id,plan_decision_id,reconciliation_receipt_id,
        base_revision,workspace_digest,parent_result_sha256,source_revision,review_no,round)
    SELECT a.owner_id,a.project_alias,a.task_id,a.attempt_id,a.plan_decision_id,r.receipt_id,t.base_revision,r.workspace_digest,
        a.result_sha256,t.revision,coalesce((SELECT max(q.review_no) FROM forge_agent.coding_reviews q WHERE q.owner_id=a.owner_id AND q.project_alias=a.project_alias AND q.task_id=a.task_id),0)+1,
        coalesce((SELECT b.repair_rounds FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),0)
    FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)=(a.owner_id,a.project_alias,a.task_id,a.attempt_id)
    JOIN LATERAL(SELECT x.* FROM forge_agent.coding_reconciliation_receipts x WHERE x.attempt_id=a.attempt_id ORDER BY x.created_at DESC LIMIT 1)r ON true
    WHERE a.owner_id=current_setting('forge.coding_owner',true) AND t.review_required AND t.mode='implement'
        AND t.status='awaiting_review' AND a.phase='stopped' AND NOT a.reservation_active AND a.lease_expires_at IS NULL
        AND a.published_at IS NOT NULL AND a.result_sha256 IS NOT NULL
        AND NOT EXISTS(SELECT 1 FROM forge_agent.coding_reviews q WHERE q.owner_id=a.owner_id AND q.project_alias=a.project_alias AND q.task_id=a.task_id AND q.attempt_id=a.attempt_id)
    ON CONFLICT DO NOTHING;
    UPDATE forge_agent.coding_reviews q SET status='cancelled',lease_token=NULL,lease_expires_at=NULL,reason='Source superseded or cancelled',updated_at=clock_timestamp()
    WHERE q.owner_id=current_setting('forge.coding_owner',true) AND q.status IN('pending','checking')
        AND NOT EXISTS(SELECT 1 FROM forge_agent.coding_tasks t WHERE(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)=(q.owner_id,q.project_alias,q.task_id,q.attempt_id)
            AND t.status IN('awaiting_review','completed') AND t.plan_decision_id IS NOT DISTINCT FROM q.plan_decision_id AND t.scope_plan_decision_id IS NOT DISTINCT FROM q.scope_plan_decision_id);
    SELECT q.* INTO j
    FROM forge_agent.coding_reviews q
    JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)=(q.owner_id,q.project_alias,q.task_id,q.attempt_id)
    WHERE q.owner_id=current_setting('forge.coding_owner',true)
      AND (q.status='pending' OR (q.status='checking' AND q.lease_expires_at<=clock_timestamp()))
      AND t.status IN('awaiting_review','completed')
      AND t.plan_decision_id IS NOT DISTINCT FROM q.plan_decision_id
      AND t.scope_plan_decision_id IS NOT DISTINCT FROM q.scope_plan_decision_id
      AND q.next_poll_at<=clock_timestamp()
      AND NOT EXISTS (
          SELECT 1 FROM forge_agent.agent_branch_state bs
          WHERE bs.owner_id = q.owner_id
            AND bs.project_alias = q.project_alias
            AND bs.agent_profile = t.agent_profile
            AND bs.reservation_status = 'active'
            AND (bs.active_scope_kind, bs.active_scope_id) IS DISTINCT FROM (
                CASE WHEN t.workflow_id IS NOT NULL THEN 'native_workflow' ELSE 'standalone_task' END,
                coalesce(t.workflow_id, t.repair_root_task_id, t.id)
            )
      )
    ORDER BY q.created_at, q.review_id
    FOR UPDATE OF q SKIP LOCKED
    LIMIT 1;
    IF NOT FOUND THEN RETURN NULL; END IF;
    UPDATE forge_agent.coding_reviews SET status='checking',worker_id=p_worker_id,lease_token=gen_random_uuid(),
        lease_expires_at=clock_timestamp()+interval '60 seconds',updated_at=clock_timestamp()
    WHERE review_id=j.review_id RETURNING * INTO j;
    SELECT * INTO t_row FROM forge_agent.coding_tasks WHERE owner_id=j.owner_id AND project_alias=j.project_alias AND id=j.task_id;
    INSERT INTO forge_agent.coding_review_policy(owner_id,project_alias) VALUES(j.owner_id,j.project_alias) ON CONFLICT DO NOTHING;
    SELECT * INTO p FROM forge_agent.coding_review_policy WHERE owner_id=j.owner_id AND project_alias=j.project_alias;
    SELECT b.policy INTO frozen_policy FROM forge_agent.coding_review_budget b
    WHERE b.owner_id=t_row.owner_id AND b.project_alias=t_row.project_alias AND b.root_task_id=coalesce(t_row.repair_root_task_id,t_row.id);
    RETURN forge_agent._retry_native_context(j.task_id) || jsonb_build_object(
        'review_id',j.review_id,
        'hosted_deadline_at',to_char((j.created_at+interval '24 hours') AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
        'phase',j.phase,'target_publication_id',j.target_publication_id,
        'source_publication_id',j.source_publication_id,'publication_id',j.source_publication_id,
        'pr_url',(SELECT pr_url FROM forge_agent.coding_publications WHERE publication_id=j.source_publication_id),
        'task_id',j.task_id,'attempt_id',j.attempt_id,'project_alias',j.project_alias,
        'agent_profile',t_row.agent_profile,'workflow_id',t_row.workflow_id,
        'repair_root_task_id',t_row.repair_root_task_id,'correlation_id',t_row.correlation_id,
        'base_revision',j.base_revision,'workspace_digest',j.workspace_digest,'plan_decision_id',j.plan_decision_id,'scope_plan_decision_id',j.scope_plan_decision_id,
        'owner_source_authorized',j.phase='source' AND j.owner_decision->>'decision'='repair_source','summary',j.summary,'findings',j.findings,
        'reconciliation_receipt_id',j.reconciliation_receipt_id,'parent_result_sha256',j.parent_result_sha256,'source_revision',j.source_revision,
        'lease_token',j.lease_token,'status',j.status,'round',j.round,'stagnant_rounds',j.stagnant_rounds,'infrastructure_retries',j.infrastructure_retries,
        'candidate_commit',j.candidate_commit,'gate_manifest_sha256',j.gate_manifest_sha256,'previous_review_id',j.previous_review_id,
        'source_review_id',CASE WHEN j.phase='hosted' THEN(SELECT review_id FROM forge_agent.coding_publications WHERE publication_id=j.source_publication_id) WHEN j.phase='local' AND j.infrastructure_retries>0 THEN j.previous_review_id ELSE NULL END,
        'direct_implementation',t_row.mode='implement' AND t_row.review_required AND t_row.pinned_recipe->>'resource_profile' IN('forge-coding-v5','forge-coding-v6','forge-coding-v7','forge-coding-v8','forge-coding-v9','forge-coding-v10','forge-coding-agy-unattended-v1') AND t_row.plan_decision_id IS NULL AND t_row.parent_task_id IS NULL,
        'parent_task_id',t_row.parent_task_id,'feedback_id',t_row.feedback_id,'auto_repair',t_row.auto_repair,
        'open_findings',coalesce((SELECT findings FROM forge_agent.coding_reviews WHERE review_id=j.previous_review_id),'[]'::jsonb),
        'previous_fingerprint',(SELECT fingerprint FROM forge_agent.coding_reviews WHERE review_id=j.previous_review_id),
        'policy',coalesce(frozen_policy,jsonb_build_object('version',p.version,'max_rounds',p.max_rounds,'max_stagnant_rounds',p.max_stagnant_rounds,'max_infrastructure_retries',p.max_infrastructure_retries))
    );
END;
$fn$;

CREATE FUNCTION forge_coding_reconciler_api.publication_claim_for_branch_policy(p_worker_id text)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    j forge_agent.coding_publications%ROWTYPE;
    t_row forge_agent.coding_tasks%ROWTYPE;
    approved_review record;
    now_at timestamptz := clock_timestamp();
BEGIN
    IF p_worker_id IS NULL OR p_worker_id !~ '^[A-Za-z0-9_.-]{1,64}$' THEN
        RAISE EXCEPTION 'Invalid publication worker' USING ERRCODE = '22023';
    END IF;
    INSERT INTO forge_agent.coding_publications(owner_id,project_alias,task_id,attempt_id,plan_decision_id,
        reconciliation_receipt_id,base_revision,workspace_digest)
    SELECT a.owner_id,a.project_alias,a.task_id,a.attempt_id,d.decision_id,r.receipt_id,t.base_revision,r.workspace_digest
    FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t
      ON (t.owner_id,t.project_alias,t.id,t.latest_attempt_id)=(a.owner_id,a.project_alias,a.task_id,a.attempt_id)
    JOIN forge_agent.coding_plan_decisions d ON (d.owner_id,d.project_alias,d.task_id,d.decision_id)=
      (a.owner_id,a.project_alias,a.task_id,a.plan_decision_id)
    JOIN LATERAL (SELECT x.* FROM forge_agent.coding_reconciliation_receipts x
       WHERE (x.owner_id,x.project_alias,x.task_id,x.attempt_id)=(a.owner_id,a.project_alias,a.task_id,a.attempt_id)
       ORDER BY x.created_at DESC LIMIT 1) r ON true
    WHERE a.owner_id=current_setting('forge.coding_owner',true) AND NOT t.review_required AND d.decision='resume'
      AND t.plan_decision_id=d.decision_id AND t.mode='implement' AND a.pinned_recipe->>'mode'='implement'
      AND t.status IN ('awaiting_review','completed') AND a.phase='stopped' AND NOT a.reservation_active
      AND a.lease_expires_at IS NULL AND a.published_at IS NOT NULL AND a.result_sha256 IS NOT NULL
    ON CONFLICT DO NOTHING;
    FOR approved_review IN SELECT review_id FROM forge_agent.coding_reviews WHERE owner_id=current_setting('forge.coding_owner',true) AND status='approved' LOOP
        PERFORM forge_agent.coding_queue_approved_review(approved_review.review_id);
    END LOOP;
    UPDATE forge_agent.coding_publications p SET status='failed',lease_token=NULL,lease_expires_at=NULL,
      summary='Publication source is no longer current; review retained remote metadata before retrying.',updated_at=now_at
    WHERE p.owner_id=current_setting('forge.coding_owner',true) AND p.status IN ('pending','checking','publishing')
      AND NOT EXISTS (SELECT 1 FROM forge_agent.coding_tasks t
        WHERE (t.owner_id,t.project_alias,t.id,t.latest_attempt_id,t.plan_decision_id) IS NOT DISTINCT FROM
              (p.owner_id,p.project_alias,p.task_id,p.attempt_id,p.plan_decision_id)
          AND t.mode='implement' AND t.status IN ('awaiting_review','completed') AND(NOT t.review_required OR forge_agent.coding_review_authorized(p.review_id)));
    UPDATE forge_agent.coding_publications SET status='failed',lease_token=NULL,lease_expires_at=NULL,
      summary='Publication outcome is uncertain after its lease expired; review remote state before retrying.',updated_at=now_at
    WHERE owner_id=current_setting('forge.coding_owner',true) AND status='publishing' AND lease_expires_at<=now_at;
    SELECT p.* INTO j
    FROM forge_agent.coding_publications p
    JOIN forge_agent.coding_tasks t
      ON (t.owner_id,t.project_alias,t.id,t.latest_attempt_id,t.plan_decision_id) IS NOT DISTINCT FROM
         (p.owner_id,p.project_alias,p.task_id,p.attempt_id,p.plan_decision_id)
    WHERE p.owner_id=current_setting('forge.coding_owner',true)
      AND (p.status='pending' OR (p.status='checking' AND p.lease_expires_at<=now_at))
      AND t.mode='implement' AND t.status IN ('awaiting_review','completed')
      AND (NOT t.review_required OR forge_agent.coding_review_authorized(p.review_id))
      AND NOT EXISTS (
          SELECT 1 FROM forge_agent.agent_branch_state bs
          WHERE bs.owner_id = p.owner_id
            AND bs.project_alias = p.project_alias
            AND bs.agent_profile = t.agent_profile
            AND bs.reservation_status = 'active'
            AND (bs.active_scope_kind, bs.active_scope_id) IS DISTINCT FROM (
                CASE WHEN t.workflow_id IS NOT NULL THEN 'native_workflow' ELSE 'standalone_task' END,
                coalesce(t.workflow_id, t.repair_root_task_id, t.id)
            )
      )
    ORDER BY p.created_at, p.publication_id
    FOR UPDATE OF p SKIP LOCKED
    LIMIT 1;
    IF NOT FOUND THEN RETURN NULL; END IF;
    UPDATE forge_agent.coding_publications SET status='checking',worker_id=p_worker_id,lease_token=gen_random_uuid(),
       lease_expires_at=clock_timestamp()+interval '60 seconds',updated_at=clock_timestamp()
    WHERE publication_id=j.publication_id RETURNING * INTO j;
    SELECT * INTO t_row FROM forge_agent.coding_tasks WHERE owner_id=j.owner_id AND project_alias=j.project_alias AND id=j.task_id;
    RETURN jsonb_build_object(
        'publication_id',j.publication_id,'task_id',j.task_id,'attempt_id',j.attempt_id,
        'project_alias',j.project_alias,'agent_profile',t_row.agent_profile,
        'workflow_id',t_row.workflow_id,'repair_root_task_id',t_row.repair_root_task_id,
        'correlation_id',t_row.correlation_id,
        'plan_decision_id',j.plan_decision_id,'base_revision',j.base_revision,
        'workspace_digest',j.workspace_digest,'worker_id',j.worker_id,'lease_token',j.lease_token,
        'lease_expires_at',to_char(j.lease_expires_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
        'status',j.status,'branch',j.branch,'commit_sha',j.commit_sha,'pr_url',j.pr_url,
        'review_id',j.review_id,'candidate_commit',(SELECT candidate_commit FROM forge_agent.coding_reviews WHERE review_id=j.review_id),
        'gate_manifest_sha256',(SELECT gate_manifest_sha256 FROM forge_agent.coding_reviews WHERE review_id=j.review_id),
        'target_publication_id',coalesce(j.parent_publication_id,j.publication_id),'expected_remote_head',j.expected_remote_head,
        'direct_implementation',t_row.mode='implement' AND t_row.review_required AND t_row.pinned_recipe->>'resource_profile' IN('forge-coding-v5','forge-coding-v6','forge-coding-v7','forge-coding-v8','forge-coding-v9','forge-coding-v10','forge-coding-agy-unattended-v1') AND t_row.plan_decision_id IS NULL AND t_row.parent_task_id IS NULL,
        'parent_task_id',t_row.parent_task_id,'feedback_id',t_row.feedback_id
    );
END;
$fn$;

CREATE FUNCTION forge_coding_reconciler_api.native_agent_branch_published(
    p_review uuid,
    p_token uuid,
    p_receipt jsonb
) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    context jsonb;
    scope jsonb;
    v_payload jsonb;
    event uuid;
    previous forge_agent.coding_events%ROWTYPE;
    t_row forge_agent.coding_tasks%ROWTYPE;
    b_row forge_agent.agent_branch_state%ROWTYPE;
    v_epoch bigint;
    v_branch text;
    v_profile text;
BEGIN
    IF (jsonb_typeof(p_receipt) <> 'object' OR octet_length(p_receipt::text) > 4096
        OR p_receipt - ARRAY['version','scope','candidate_commit','branch_publication_id','branch','agent_profile','reservation_epoch'] <> '{}'::jsonb
        OR NOT p_receipt ?& ARRAY['version','scope','candidate_commit','branch_publication_id','branch','agent_profile','reservation_epoch']
        OR p_receipt->>'version' <> 'forge-native-branch-v2'
        OR p_receipt->>'candidate_commit' !~ '^[0-9a-f]{40}$'
        OR p_receipt->>'branch_publication_id' IS DISTINCT FROM p_review::text
        OR p_receipt->>'agent_profile' !~ '^[A-Za-z0-9_.-]{1,64}$'
        OR jsonb_typeof(p_receipt->'reservation_epoch') <> 'number'
        OR p_receipt->>'branch' !~ '^forge/[A-Za-z0-9][A-Za-z0-9._/-]{0,121}$'
        OR position('..' in p_receipt->>'branch') > 0
        OR position('//' in p_receipt->>'branch') > 0
        OR jsonb_typeof(p_receipt->'scope') <> 'object'
        OR (p_receipt->'scope') - ARRAY['owner_id','project_alias','task_id','attempt_id','review_id','workflow_id','correlation_id'] <> '{}'::jsonb
        OR NOT (p_receipt->'scope') ?& ARRAY['owner_id','project_alias','task_id','attempt_id','review_id','workflow_id','correlation_id']
        OR p_receipt->'scope'->>'project_alias' !~ '^[A-Za-z0-9_.-]{1,64}$'
        OR p_receipt->'scope'->>'task_id' !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
        OR p_receipt->'scope'->>'attempt_id' !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
    ) IS DISTINCT FROM false THEN
        RAISE EXCEPTION 'Invalid native agent branch receipt' USING ERRCODE = '22023';
    END IF;
    v_epoch := (p_receipt->>'reservation_epoch')::bigint;
    v_branch := p_receipt->>'branch';
    v_profile := p_receipt->>'agent_profile';
    context := forge_coding_reconciler_api.native_ci_context(
        p_review, p_token, p_receipt->'scope'->>'project_alias',
        (p_receipt->'scope'->>'task_id')::uuid, (p_receipt->'scope'->>'attempt_id')::uuid
    );
    scope := context - ARRAY['version','candidate_commit','active','state'];
    IF p_receipt->'scope' IS DISTINCT FROM scope THEN
        RAISE EXCEPTION 'Native branch scope changed' USING ERRCODE = 'P0006';
    END IF;
    SELECT * INTO t_row FROM forge_agent.coding_tasks
    WHERE id = (scope->>'task_id')::uuid AND owner_id = scope->>'owner_id' FOR UPDATE;
    IF NOT FOUND OR t_row.agent_profile IS DISTINCT FROM v_profile THEN
        RAISE EXCEPTION 'Native agent branch profile mismatch' USING ERRCODE = 'P0006';
    END IF;
    SELECT * INTO b_row FROM forge_agent.agent_branch_state
    WHERE owner_id = scope->>'owner_id' AND project_alias = scope->>'project_alias' AND agent_profile = v_profile
    FOR UPDATE;
    IF NOT FOUND OR b_row.reservation_status <> 'active'
       OR b_row.branch_name IS DISTINCT FROM v_branch
       OR b_row.active_scope_kind IS DISTINCT FROM 'native_workflow'
       OR b_row.active_scope_id IS DISTINCT FROM (scope->>'workflow_id')::uuid
       OR b_row.reservation_epoch IS DISTINCT FROM v_epoch THEN
        RAISE EXCEPTION 'Native agent branch ownership mismatch' USING ERRCODE = 'P0006';
    END IF;
    event := md5('forge-native-branch-v2:' || p_review::text)::uuid;
    v_payload := p_receipt || jsonb_build_object(
        'stage', 'branch_pushed',
        'review_id', p_review,
        'actor', jsonb_build_object('kind', 'service', 'id', 'forge-coding-reconciler')
    );
    SELECT * INTO previous FROM forge_agent.coding_events WHERE event_id = event;
    IF FOUND THEN
        IF (previous.owner_id, previous.project_alias, previous.task_id, previous.payload)
           IS DISTINCT FROM (scope->>'owner_id', scope->>'project_alias', (scope->>'task_id')::uuid, v_payload) THEN
            RAISE EXCEPTION 'Native branch receipt conflict' USING ERRCODE = 'P0006';
        END IF;
        RETURN jsonb_build_object('event_id', previous.event_id, 'correlation_id', previous.correlation_id);
    END IF;
    UPDATE forge_agent.agent_branch_state
    SET head_commit_sha = p_receipt->>'candidate_commit',
        active_candidate_commit = p_receipt->>'candidate_commit',
        active_review_id = p_review,
        last_task_id = (scope->>'task_id')::uuid,
        last_correlation_id = (scope->>'correlation_id')::uuid,
        updated_at = clock_timestamp()
    WHERE owner_id = scope->>'owner_id' AND project_alias = scope->>'project_alias' AND agent_profile = v_profile;
    INSERT INTO forge_agent.coding_events(owner_id, project_alias, task_id, sequence, event_kind, payload, event_id)
    SELECT scope->>'owner_id', scope->>'project_alias', (scope->>'task_id')::uuid,
           coalesce(max(sequence), 0) + 1, 'checkpoint', v_payload, event
    FROM forge_agent.coding_events
    WHERE owner_id = scope->>'owner_id' AND project_alias = scope->>'project_alias' AND task_id = (scope->>'task_id')::uuid;
    PERFORM pg_notify('forge_coding_jobs', '');
    RETURN jsonb_build_object('event_id', event, 'correlation_id', scope->>'correlation_id');
END;
$fn$;

CREATE FUNCTION forge_coding_reconciler_api.record_jenkins_stage_evidence(
    p_evidence jsonb,
    p_chunks jsonb DEFAULT '[]'::jsonb
) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_owner text := current_setting('forge.coding_owner', true);
    v_project text;
    v_action uuid;
    v_stage_index integer;
    v_receipt text;
    existing forge_agent.jenkins_stage_results%ROWTYPE;
    chunk_item jsonb;
    v_chunk_index integer;
    v_chunk_bytes bytea;
    v_chunk_sha text;
    v_chunk_offset integer;
    v_chunk_len integer;
    existing_chunk forge_agent.jenkins_stage_log_chunks%ROWTYPE;
    inserted_chunks integer := 0;
BEGIN
    IF v_owner IS NULL OR btrim(v_owner) = '' OR jsonb_typeof(p_evidence) <> 'object'
       OR jsonb_typeof(p_chunks) <> 'array' OR jsonb_array_length(p_chunks) > 16 THEN
        RAISE EXCEPTION 'Invalid stage evidence payload' USING ERRCODE = '22023';
    END IF;
    v_project := p_evidence->>'project_alias';
    v_action := (p_evidence->>'action_id')::uuid;
    v_stage_index := (p_evidence->>'stage_index')::integer;
    v_receipt := p_evidence->>'receipt_sha256';
    IF v_project IS NULL OR v_action IS NULL OR v_stage_index IS NULL OR v_receipt IS NULL THEN
        RAISE EXCEPTION 'Missing stage evidence identity' USING ERRCODE = '22023';
    END IF;
    SELECT * INTO existing FROM forge_agent.jenkins_stage_results
    WHERE owner_id = v_owner AND project_alias = v_project AND action_id = v_action AND stage_index = v_stage_index;
    IF FOUND THEN
        IF existing.receipt_sha256 IS DISTINCT FROM v_receipt
           OR existing.log_sha256 IS DISTINCT FROM (p_evidence->>'log_sha256') THEN
            RAISE EXCEPTION 'Jenkins stage evidence conflict' USING ERRCODE = 'P0006';
        END IF;
    ELSE
        INSERT INTO forge_agent.jenkins_stage_results(
            owner_id, project_alias, action_id, stage_index, correlation_id, task_id,
            job_name, build_number, commit_sha, branch_name, branch_scope, pipeline_scope,
            logical_stage_id, phase_id, phase_kind, label, status, returncode, duration_ms,
            tests_passed, tests_failed, tests_skipped, test_summary, error_excerpt,
            log_sha256, log_byte_count, log_chunk_count, receipt_sha256
        ) VALUES (
            v_owner, v_project, v_action, v_stage_index,
            (p_evidence->>'correlation_id')::uuid,
            CASE WHEN p_evidence->>'task_id' IS NOT NULL THEN (p_evidence->>'task_id')::uuid ELSE NULL END,
            p_evidence->>'job_name',
            CASE WHEN p_evidence->'build_number' IS NOT NULL AND jsonb_typeof(p_evidence->'build_number') = 'number' THEN (p_evidence->>'build_number')::integer ELSE NULL END,
            p_evidence->>'commit_sha',
            p_evidence->>'branch_name',
            p_evidence->>'branch_scope',
            p_evidence->>'pipeline_scope',
            p_evidence->>'logical_stage_id',
            p_evidence->>'phase_id',
            p_evidence->>'phase_kind',
            p_evidence->>'label',
            p_evidence->>'status',
            CASE WHEN p_evidence->'returncode' IS NOT NULL AND jsonb_typeof(p_evidence->'returncode') = 'number' THEN (p_evidence->>'returncode')::integer ELSE NULL END,
            CASE WHEN p_evidence->'duration_ms' IS NOT NULL AND jsonb_typeof(p_evidence->'duration_ms') = 'number' THEN (p_evidence->>'duration_ms')::integer ELSE NULL END,
            CASE WHEN p_evidence->'tests_passed' IS NOT NULL AND jsonb_typeof(p_evidence->'tests_passed') = 'number' THEN (p_evidence->>'tests_passed')::integer ELSE NULL END,
            CASE WHEN p_evidence->'tests_failed' IS NOT NULL AND jsonb_typeof(p_evidence->'tests_failed') = 'number' THEN (p_evidence->>'tests_failed')::integer ELSE NULL END,
            CASE WHEN p_evidence->'tests_skipped' IS NOT NULL AND jsonb_typeof(p_evidence->'tests_skipped') = 'number' THEN (p_evidence->>'tests_skipped')::integer ELSE NULL END,
            coalesce(p_evidence->'test_summary', '{}'::jsonb),
            p_evidence->>'error_excerpt',
            p_evidence->>'log_sha256',
            coalesce((p_evidence->>'log_byte_count')::integer, 0),
            coalesce((p_evidence->>'log_chunk_count')::integer, 0),
            v_receipt
        );
    END IF;
    FOR chunk_item IN SELECT value FROM jsonb_array_elements(p_chunks) LOOP
        v_chunk_index := (chunk_item->>'chunk_index')::integer;
        v_chunk_bytes := decode(chunk_item->>'chunk_b64', 'base64');
        v_chunk_offset := (chunk_item->>'byte_offset')::integer;
        v_chunk_len := (chunk_item->>'byte_len')::integer;
        v_chunk_sha := chunk_item->>'chunk_sha256';
        IF encode(sha256(v_chunk_bytes), 'hex') IS DISTINCT FROM v_chunk_sha THEN
            RAISE EXCEPTION 'Stage log chunk digest mismatch' USING ERRCODE = '22023';
        END IF;
        SELECT * INTO existing_chunk FROM forge_agent.jenkins_stage_log_chunks
        WHERE owner_id = v_owner AND project_alias = v_project
          AND action_id = v_action AND stage_index = v_stage_index AND chunk_index = v_chunk_index;
        IF FOUND THEN
            IF existing_chunk.chunk_sha256 IS DISTINCT FROM v_chunk_sha THEN
                RAISE EXCEPTION 'Stage log chunk conflict' USING ERRCODE = 'P0006';
            END IF;
        ELSE
            INSERT INTO forge_agent.jenkins_stage_log_chunks(
                owner_id, project_alias, action_id, stage_index, chunk_index,
                chunk_bytes, byte_offset, byte_len, chunk_sha256
            ) VALUES (
                v_owner, v_project, v_action, v_stage_index, v_chunk_index,
                v_chunk_bytes, v_chunk_offset, v_chunk_len, v_chunk_sha
            );
            inserted_chunks := inserted_chunks + 1;
        END IF;
    END LOOP;
    RETURN jsonb_build_object(
        'action_id', v_action,
        'stage_index', v_stage_index,
        'inserted_chunks', inserted_chunks
    );
END;
$fn$;

REVOKE ALL ON FUNCTION
    forge_coding_reconciler_api._branch_row_json(forge_agent.agent_branch_state),
    forge_coding_reconciler_api.task_branch_scope(text, uuid),
    forge_coding_reconciler_api.agent_branch_get(text, text),
    forge_coding_reconciler_api.agent_branch_upsert(text, text, text, text, text, text, timestamptz, text, jsonb, text, uuid, uuid),
    forge_coding_reconciler_api.agent_branch_acquire(text, text, text, text, text, uuid, uuid, uuid, uuid, uuid),
    forge_coding_reconciler_api.agent_branch_verify_owner(text, text, text, uuid, bigint),
    forge_coding_reconciler_api.agent_branch_record_publication(text, text, text, uuid, bigint, text, text, uuid),
    forge_coding_reconciler_api.agent_branch_set_blocker(text, text, text, uuid, bigint, text),
    forge_coding_reconciler_api.agent_branch_release(text, text, text, uuid, bigint, text, boolean, text),
    forge_coding_reconciler_api.agent_branch_reconcilable_reservations(integer),
    forge_coding_reconciler_api.review_claim_for_branch_policy(text),
    forge_coding_reconciler_api.publication_claim_for_branch_policy(text),
    forge_coding_reconciler_api.native_agent_branch_published(uuid, uuid, jsonb),
    forge_coding_reconciler_api.record_jenkins_stage_evidence(jsonb, jsonb)
FROM PUBLIC;

GRANT EXECUTE ON FUNCTION
    forge_coding_reconciler_api.task_branch_scope(text, uuid),
    forge_coding_reconciler_api.agent_branch_get(text, text),
    forge_coding_reconciler_api.agent_branch_upsert(text, text, text, text, text, text, timestamptz, text, jsonb, text, uuid, uuid),
    forge_coding_reconciler_api.agent_branch_acquire(text, text, text, text, text, uuid, uuid, uuid, uuid, uuid),
    forge_coding_reconciler_api.agent_branch_verify_owner(text, text, text, uuid, bigint),
    forge_coding_reconciler_api.agent_branch_record_publication(text, text, text, uuid, bigint, text, text, uuid),
    forge_coding_reconciler_api.agent_branch_set_blocker(text, text, text, uuid, bigint, text),
    forge_coding_reconciler_api.agent_branch_release(text, text, text, uuid, bigint, text, boolean, text),
    forge_coding_reconciler_api.agent_branch_reconcilable_reservations(integer),
    forge_coding_reconciler_api.review_claim_for_branch_policy(text),
    forge_coding_reconciler_api.publication_claim_for_branch_policy(text),
    forge_coding_reconciler_api.native_agent_branch_published(uuid, uuid, jsonb),
    forge_coding_reconciler_api.record_jenkins_stage_evidence(jsonb, jsonb)
TO forge_coding_reconciler;

-- 5. Dedicated external delivery branch ownership API schema (preserves M22
-- forge_external_workflow_delivery_api catalog hash intact).
CREATE SCHEMA IF NOT EXISTS forge_external_delivery_api;
REVOKE ALL ON SCHEMA forge_external_delivery_api FROM PUBLIC;
GRANT USAGE ON SCHEMA forge_external_delivery_api TO forge_coding_reconciler;

CREATE FUNCTION forge_external_delivery_api.agent_branch_acquire(
    p_owner_id text,
    p_project_alias text,
    p_agent_profile text,
    p_branch_name text,
    p_branch_scope text,
    p_workflow_id uuid,
    p_root_task_id uuid,
    p_task_id uuid
) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $fn$
BEGIN
    PERFORM set_config('forge.coding_owner', p_owner_id, true);
    RETURN forge_coding_reconciler_api.agent_branch_acquire(
        p_project_alias, p_agent_profile, p_branch_name, p_branch_scope,
        'external_workflow', p_workflow_id, p_root_task_id, p_task_id, NULL, NULL
    );
END;
$fn$;

CREATE FUNCTION forge_external_delivery_api.agent_branch_verify_owner(
    p_owner_id text,
    p_project_alias text,
    p_agent_profile text,
    p_workflow_id uuid,
    p_expected_epoch bigint
) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $fn$
BEGIN
    PERFORM set_config('forge.coding_owner', p_owner_id, true);
    RETURN forge_coding_reconciler_api.agent_branch_verify_owner(
        p_project_alias, p_agent_profile, 'external_workflow', p_workflow_id, p_expected_epoch
    );
END;
$fn$;

CREATE FUNCTION forge_external_delivery_api.agent_branch_upsert(
    p_owner_id text,
    p_project_alias text,
    p_agent_profile text,
    p_branch_name text,
    p_branch_scope text,
    p_head_commit_sha text DEFAULT NULL,
    p_fetched_main_sha text DEFAULT NULL,
    p_fetched_main_at timestamptz DEFAULT NULL,
    p_sync_status text DEFAULT NULL,
    p_conflict_paths jsonb DEFAULT NULL,
    p_cache_status text DEFAULT NULL,
    p_last_task_id uuid DEFAULT NULL,
    p_last_correlation_id uuid DEFAULT NULL
) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $fn$
BEGIN
    PERFORM set_config('forge.coding_owner', p_owner_id, true);
    RETURN forge_coding_reconciler_api.agent_branch_upsert(
        p_project_alias, p_agent_profile, p_branch_name, p_branch_scope,
        p_head_commit_sha, p_fetched_main_sha, p_fetched_main_at,
        p_sync_status, p_conflict_paths, p_cache_status,
        p_last_task_id, p_last_correlation_id
    );
END;
$fn$;

CREATE FUNCTION forge_external_delivery_api.agent_branch_release(
    p_owner_id text,
    p_project_alias text,
    p_agent_profile text,
    p_workflow_id uuid,
    p_expected_epoch bigint,
    p_release_reason text,
    p_remote_open_pr_confirmed_none boolean,
    p_merge_commit text DEFAULT NULL
) RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $fn$
BEGIN
    PERFORM set_config('forge.coding_owner', p_owner_id, true);
    RETURN forge_coding_reconciler_api.agent_branch_release(
        p_project_alias, p_agent_profile, 'external_workflow', p_workflow_id,
        p_expected_epoch, p_release_reason, p_remote_open_pr_confirmed_none, p_merge_commit
    );
END;
$fn$;

REVOKE ALL ON FUNCTION
    forge_external_delivery_api.agent_branch_acquire(text, text, text, text, text, uuid, uuid, uuid),
    forge_external_delivery_api.agent_branch_verify_owner(text, text, text, uuid, bigint),
    forge_external_delivery_api.agent_branch_upsert(text, text, text, text, text, text, text, timestamptz, text, jsonb, text, uuid, uuid),
    forge_external_delivery_api.agent_branch_release(text, text, text, uuid, bigint, text, boolean, text)
FROM PUBLIC;

GRANT EXECUTE ON FUNCTION
    forge_external_delivery_api.agent_branch_acquire(text, text, text, text, text, uuid, uuid, uuid),
    forge_external_delivery_api.agent_branch_verify_owner(text, text, text, uuid, bigint),
    forge_external_delivery_api.agent_branch_upsert(text, text, text, text, text, text, text, timestamptz, text, jsonb, text, uuid, uuid),
    forge_external_delivery_api.agent_branch_release(text, text, text, uuid, bigint, text, boolean, text)
TO forge_coding_reconciler;
