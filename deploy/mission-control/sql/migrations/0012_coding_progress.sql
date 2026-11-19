-- Optional observational progress on existing checkpoint events. Older writers
-- and readers keep their existing columns/behavior; no task state is changed.
ALTER TABLE forge_agent.coding_events
    ADD COLUMN progress_attempt_id uuid,
    ADD COLUMN progress_sequence integer,
    ADD CONSTRAINT coding_events_progress_attempt_fk FOREIGN KEY (
        owner_id, project_alias, task_id, progress_attempt_id
    ) REFERENCES forge_agent.coding_attempts (
        owner_id, project_alias, task_id, attempt_id
    ),
    ADD CONSTRAINT coding_events_progress_check CHECK (
        ((progress_attempt_id IS NULL AND progress_sequence IS NULL)
         OR (progress_attempt_id IS NOT NULL
             AND progress_sequence BETWEEN 1 AND 128
             AND event_kind = 'checkpoint'
             AND payload - ARRAY['attempt_id', 'progress_sequence', 'stage', 'summary'] = '{}'::jsonb
             AND payload ?& ARRAY['attempt_id', 'progress_sequence', 'stage', 'summary']
             AND jsonb_typeof(payload->'attempt_id') = 'string'
             AND payload->>'attempt_id' = progress_attempt_id::text
             AND jsonb_typeof(payload->'progress_sequence') = 'number'
             AND payload->>'progress_sequence' = progress_sequence::text
             AND jsonb_typeof(payload->'stage') = 'string'
             AND ((progress_sequence = 1 AND payload->>'stage' = 'plan')
                  OR (progress_sequence > 1 AND payload->>'stage' = 'progress'))
             AND jsonb_typeof(payload->'summary') = 'string'
             AND octet_length(payload->>'summary') BETWEEN 1 AND 4096
             AND btrim(payload->>'summary', E' \t\r\n') <> ''
             AND (payload->>'summary') !~ U&'[\0001-\0008\000B-\001F\007F-\009F]')) IS TRUE
    );

CREATE UNIQUE INDEX coding_events_progress_identity
    ON forge_agent.coding_events (
        owner_id, project_alias, task_id, progress_attempt_id, progress_sequence
    ) WHERE progress_attempt_id IS NOT NULL;

CREATE FUNCTION forge_coding_worker_api.report_progress(
    p_attempt_id uuid,
    p_attempt_token uuid,
    p_sequence integer,
    p_kind text,
    p_summary text
)
RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_attempt forge_agent.coding_attempts%ROWTYPE;
    v_task forge_agent.coding_tasks%ROWTYPE;
    v_existing jsonb;
    v_next_sequence integer;
    v_event_sequence bigint;
    v_now timestamptz;
BEGIN
    IF p_sequence IS NULL OR p_sequence NOT BETWEEN 1 AND 128
       OR p_kind IS NULL OR p_kind NOT IN ('plan', 'progress')
       OR p_summary IS NULL OR octet_length(p_summary) NOT BETWEEN 1 AND 4096
       OR btrim(p_summary, E' \t\r\n') = ''
       OR p_summary ~ U&'[\0001-\0008\000B-\001F\007F-\009F]' THEN
        RAISE EXCEPTION 'Invalid coding progress arguments' USING ERRCODE = '22023';
    END IF;

    -- This helper validates session_user, the configured owner and attempt token,
    -- then locks task before attempt. It never grants a new execution lease.
    v_attempt := forge_coding_worker_api._lock_task_and_attempt(p_attempt_id, p_attempt_token);
    IF v_attempt.attempt_id IS NULL THEN
        RAISE EXCEPTION 'Invalid attempt token or ownership' USING ERRCODE = '42501';
    END IF;

    SELECT e.payload INTO v_existing
    FROM forge_agent.coding_events e
    WHERE e.owner_id = v_attempt.owner_id AND e.project_alias = v_attempt.project_alias
      AND e.task_id = v_attempt.task_id AND e.progress_attempt_id = v_attempt.attempt_id
      AND e.progress_sequence = p_sequence;
    IF FOUND THEN
        IF v_existing->>'stage' = p_kind AND v_existing->>'summary' = p_summary THEN
            RETURN; -- An exact lost-ack replay never mutates even terminal state.
        END IF;
        RAISE EXCEPTION 'Progress sequence content conflict' USING ERRCODE = 'P0009';
    END IF;

    SELECT coalesce(max(e.progress_sequence), 0) + 1 INTO v_next_sequence
    FROM forge_agent.coding_events e
    WHERE e.owner_id = v_attempt.owner_id AND e.project_alias = v_attempt.project_alias
      AND e.task_id = v_attempt.task_id AND e.progress_attempt_id = v_attempt.attempt_id;
    IF p_sequence <> v_next_sequence
       OR (p_sequence = 1 AND p_kind <> 'plan')
       OR (p_sequence > 1 AND p_kind <> 'progress') THEN
        RAISE EXCEPTION 'Progress requires plan first and consecutive sequence' USING ERRCODE = 'P0009';
    END IF;

    SELECT t.* INTO v_task FROM forge_agent.coding_tasks t
    WHERE t.owner_id = v_attempt.owner_id AND t.project_alias = v_attempt.project_alias
      AND t.id = v_attempt.task_id;
    -- Capture DB time after every potentially blocking lock and before mutation.
    v_now := clock_timestamp();
    IF v_task.latest_attempt_id IS DISTINCT FROM v_attempt.attempt_id
       OR v_task.status NOT IN ('running', 'cancel_requested')
       OR v_attempt.published_at IS NOT NULL OR v_attempt.lease_expires_at IS NULL
       OR v_attempt.lease_expires_at <= v_now OR v_attempt.deadline_at <= v_now THEN
        RAISE EXCEPTION 'Attempt lease is not active' USING ERRCODE = 'P0008';
    END IF;

    SELECT coalesce(max(e.sequence), 0) + 1 INTO v_event_sequence
    FROM forge_agent.coding_events e
    WHERE e.owner_id = v_attempt.owner_id AND e.project_alias = v_attempt.project_alias
      AND e.task_id = v_attempt.task_id;
    IF v_event_sequence > 9007199254740991 THEN
        RAISE EXCEPTION 'Task event sequence limit reached' USING ERRCODE = 'P0009';
    END IF;

    UPDATE forge_agent.coding_attempts SET summary = p_summary, updated_at = v_now
    WHERE owner_id = v_attempt.owner_id AND project_alias = v_attempt.project_alias
      AND task_id = v_attempt.task_id AND attempt_id = v_attempt.attempt_id;
    INSERT INTO forge_agent.coding_events (
        owner_id, project_alias, task_id, sequence, event_kind, payload,
        progress_attempt_id, progress_sequence, created_at
    ) VALUES (
        v_attempt.owner_id, v_attempt.project_alias, v_attempt.task_id,
        v_event_sequence, 'checkpoint', pg_catalog.jsonb_build_object(
            'attempt_id', v_attempt.attempt_id, 'progress_sequence', p_sequence,
            'stage', p_kind, 'summary', p_summary
        ), v_attempt.attempt_id, p_sequence, v_now
    );
    PERFORM pg_catalog.pg_notify('forge_coding_jobs', '');
END;
$fn$;

REVOKE ALL ON FUNCTION forge_coding_worker_api.report_progress(uuid, uuid, integer, text, text) FROM PUBLIC;
