ALTER TABLE forge_agent.runs
    DROP CONSTRAINT runs_status_check,
    ADD COLUMN distributed boolean NOT NULL DEFAULT false,
    ADD COLUMN run_deadline timestamptz,
    ADD COLUMN next_attempt_at timestamptz,
    ADD COLUMN attempt_no integer NOT NULL DEFAULT 0 CHECK (attempt_no BETWEEN 0 AND 3),
    ADD COLUMN attempt_token uuid,
    ADD COLUMN lease_owner name,
    ADD COLUMN lease_until timestamptz,
    ADD COLUMN accepted_result_hash bytea,
    ADD CONSTRAINT runs_status_check CHECK (status IN (
        'queued', 'running', 'waiting_for_tool', 'result_ready', 'completed',
        'failed', 'cancelled', 'interrupted'
    )),
    ADD CONSTRAINT runs_dispatch_deadline_check CHECK (
        (distributed AND run_deadline IS NOT NULL AND next_attempt_at IS NOT NULL)
        OR (NOT distributed AND run_deadline IS NULL AND next_attempt_at IS NULL)
    );

CREATE INDEX distributed_jobs
    ON forge_agent.runs (model_id, created_at, id) WHERE distributed AND status = 'queued';

CREATE TABLE forge_agent.inference_results (
    run_id uuid PRIMARY KEY REFERENCES forge_agent.runs(id) ON DELETE CASCADE,
    attempt_token uuid NOT NULL,
    outcome jsonb NOT NULL CHECK (
        jsonb_typeof(outcome) = 'object' AND octet_length(outcome::text) <= 131072
    ),
    accepted_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE forge_agent.gpu_worker_readiness (
    worker_name name PRIMARY KEY,
    model_id text NOT NULL,
    ready boolean NOT NULL,
    seen_at timestamptz NOT NULL
);

CREATE SCHEMA forge_agent_worker_api;
REVOKE ALL ON SCHEMA forge_agent_worker_api FROM PUBLIC;

CREATE FUNCTION forge_agent_worker_api.advertise(p_model text, p_ready boolean)
RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
BEGIN
    IF p_model IS NULL OR p_model !~ '^[a-z0-9][a-z0-9._-]{0,79}$' OR p_ready IS NULL THEN
        RAISE EXCEPTION 'Invalid worker readiness' USING ERRCODE = '22023';
    END IF;
    INSERT INTO forge_agent.gpu_worker_readiness (worker_name, model_id, ready, seen_at)
    VALUES (session_user, p_model, p_ready, clock_timestamp())
    ON CONFLICT (worker_name) DO UPDATE SET model_id = excluded.model_id,
        ready = excluded.ready, seen_at = excluded.seen_at;
END;
$fn$;

CREATE FUNCTION forge_agent_worker_api.claim(p_model text)
RETURNS jsonb
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    candidate record;
    job forge_agent.runs%ROWTYPE;
    settings jsonb;
    messages jsonb;
BEGIN
    IF NOT EXISTS (
        SELECT FROM forge_agent.gpu_worker_readiness
        WHERE worker_name = session_user AND model_id = p_model AND ready
          AND seen_at > clock_timestamp() - interval '30 seconds'
    ) THEN
        RETURN NULL;
    END IF;

    FOR candidate IN
        SELECT r.id, r.ide_chat_owner_id, r.ide_chat_id
        FROM forge_agent.runs r
        JOIN forge_agent.conversations c ON c.id = r.conversation_id
        WHERE c.owner_id = 'mission-control' AND r.distributed AND r.model_id = p_model
          AND r.status = 'queued' AND r.attempt_no < 3
          AND r.next_attempt_at <= clock_timestamp() AND r.run_deadline > clock_timestamp()
        ORDER BY r.created_at, r.id
    LOOP
        -- Chat deletion, result publication, and finalization all use this lock order.
        IF candidate.ide_chat_id IS NOT NULL THEN
            PERFORM 1 FROM forge_agent.ide_chats h
            WHERE (h.owner_id, h.id) = (candidate.ide_chat_owner_id, candidate.ide_chat_id)
              AND h.removed_at IS NULL AND (h.expires_at IS NULL OR h.expires_at > clock_timestamp())
            FOR UPDATE SKIP LOCKED;
            IF NOT FOUND THEN CONTINUE; END IF;
        END IF;

        SELECT r.* INTO job FROM forge_agent.runs r
        WHERE r.id = candidate.id AND r.distributed AND r.status = 'queued'
          AND r.attempt_no < 3 AND r.next_attempt_at <= clock_timestamp()
          AND r.run_deadline > clock_timestamp()
        FOR UPDATE SKIP LOCKED;
        IF NOT FOUND THEN CONTINUE; END IF;

        UPDATE forge_agent.runs
        SET status = 'running', attempt_no = attempt_no + 1, attempt_token = gen_random_uuid(),
            lease_owner = session_user, lease_until = least(run_deadline, clock_timestamp() + interval '60 seconds')
        WHERE id = job.id
        RETURNING * INTO job;

        SELECT e.data INTO STRICT settings FROM forge_agent.run_events e
        WHERE e.run_id = job.id AND e.sequence = 1 AND e.event_type = 'queued';
        SELECT jsonb_agg(jsonb_build_object('role', m.role, 'content', m.content) ORDER BY m.position)
        INTO messages FROM forge_agent.messages m
        WHERE m.conversation_id = job.conversation_id AND m.role IN ('system', 'user', 'assistant');

        INSERT INTO forge_agent.run_events (run_id, sequence, event_type, data)
        SELECT job.id, coalesce(max(e.sequence), 0) + 1, 'model_started',
               jsonb_build_object('model', job.model_id, 'attempt', job.attempt_no)
        FROM forge_agent.run_events e WHERE e.run_id = job.id;
        PERFORM pg_notify('forge_agent_events', '');

        RETURN jsonb_build_object('run_id', job.id, 'attempt_token', job.attempt_token,
            'model', job.model_id, 'messages', messages, 'max_tokens', settings->'max_tokens',
            'temperature', settings->'temperature', 'allow_partial', settings->'allow_partial',
            'lease_until', job.lease_until, 'deadline', job.run_deadline);
    END LOOP;
    RETURN NULL;
END;
$fn$;

CREATE FUNCTION forge_agent_worker_api.lock_attempt(p_run uuid, p_token uuid)
RETURNS forge_agent.runs
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    candidate record;
    job forge_agent.runs%ROWTYPE;
BEGIN
    SELECT r.ide_chat_owner_id, r.ide_chat_id INTO candidate
    FROM forge_agent.runs r
    WHERE r.id = p_run AND r.distributed AND r.attempt_token = p_token AND r.lease_owner = session_user;
    IF NOT FOUND THEN RETURN NULL; END IF;
    IF candidate.ide_chat_id IS NOT NULL THEN
        PERFORM 1 FROM forge_agent.ide_chats h
        WHERE (h.owner_id, h.id) = (candidate.ide_chat_owner_id, candidate.ide_chat_id)
          AND h.removed_at IS NULL AND (h.expires_at IS NULL OR h.expires_at > clock_timestamp())
        FOR UPDATE;
        IF NOT FOUND THEN RETURN NULL; END IF;
    END IF;
    SELECT r.* INTO job FROM forge_agent.runs r
    WHERE r.id = p_run AND r.distributed AND r.attempt_token = p_token AND r.lease_owner = session_user
    FOR UPDATE;
    RETURN job;
END;
$fn$;

CREATE FUNCTION forge_agent_worker_api.renew(p_run uuid, p_token uuid)
RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    job forge_agent.runs%ROWTYPE;
BEGIN
    job := forge_agent_worker_api.lock_attempt(p_run, p_token);
    IF job.id IS NULL OR job.status <> 'running' THEN RETURN false; END IF;
    UPDATE forge_agent.runs SET lease_until = least(run_deadline, clock_timestamp() + interval '60 seconds')
    WHERE id = job.id AND lease_until > clock_timestamp() AND run_deadline > clock_timestamp();
    RETURN FOUND;
END;
$fn$;

CREATE FUNCTION forge_agent_worker_api.publish(p_run uuid, p_token uuid, p_result jsonb)
RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    job forge_agent.runs%ROWTYPE;
    result_hash bytea;
BEGIN
    IF jsonb_typeof(p_result) IS DISTINCT FROM 'object' OR octet_length(p_result::text) > 131072 THEN
        RETURN false;
    END IF;
    IF p_result ? 'error' THEN
        IF p_result - 'error' <> '{}'::jsonb
           OR jsonb_typeof(p_result->'error') IS DISTINCT FROM 'string'
           OR (p_result->>'error') !~ '^[a-z0-9_]{1,80}$' THEN RETURN false; END IF;
    ELSIF p_result - ARRAY['answer', 'finish_reason', 'usage'] <> '{}'::jsonb
       OR jsonb_typeof(p_result->'answer') IS DISTINCT FROM 'string'
       OR length(btrim(p_result->>'answer')) = 0
       OR octet_length(p_result->>'answer') > 65536
       OR coalesce(p_result->>'finish_reason', '') NOT IN ('stop', 'length')
       OR jsonb_typeof(p_result->'usage') IS DISTINCT FROM 'object'
       OR octet_length((p_result->'usage')::text) > 8192 THEN RETURN false;
    END IF;

    job := forge_agent_worker_api.lock_attempt(p_run, p_token);
    IF job.id IS NULL THEN RETURN false; END IF;
    result_hash := sha256(convert_to(p_result::text, 'UTF8'));
    IF job.status IN ('result_ready', 'completed', 'failed') THEN
        RETURN coalesce(job.accepted_result_hash = result_hash, false);
    END IF;
    IF job.status <> 'running' THEN RETURN false; END IF;
    UPDATE forge_agent.runs SET status = 'result_ready', accepted_result_hash = result_hash, lease_until = NULL
    WHERE id = job.id AND lease_until > clock_timestamp() AND run_deadline > clock_timestamp();
    IF NOT FOUND THEN RETURN false; END IF;
    INSERT INTO forge_agent.inference_results (run_id, attempt_token, outcome)
    VALUES (job.id, p_token, p_result);
    PERFORM pg_notify('forge_agent_results', '');
    RETURN true;
END;
$fn$;

REVOKE ALL ON ALL FUNCTIONS IN SCHEMA forge_agent_worker_api FROM PUBLIC;
