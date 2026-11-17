CREATE TABLE forge_agent.mail_threads (
    owner_id text NOT NULL,
    id uuid NOT NULL,
    subject text NOT NULL CHECK (octet_length(subject) BETWEEN 1 AND 120),
    revision bigint NOT NULL DEFAULT 0 CHECK (revision BETWEEN 0 AND 9007199254740991),
    profile_id text NOT NULL CHECK (btrim(profile_id) <> ''),
    context_remaining_tokens integer NOT NULL CHECK (context_remaining_tokens >= 0),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    archived_at timestamptz,
    latest_run_id uuid,
    PRIMARY KEY (owner_id, id)
);

CREATE INDEX mail_threads_by_folder
    ON forge_agent.mail_threads (owner_id, (archived_at IS NOT NULL), updated_at DESC, id DESC);

ALTER TABLE forge_agent.runs
    ADD COLUMN mail_owner_id text,
    ADD COLUMN mail_thread_id uuid,
    ADD COLUMN mail_source_run_id uuid,
    ADD CONSTRAINT runs_mail_thread_owner_fk FOREIGN KEY (mail_owner_id, mail_thread_id)
        REFERENCES forge_agent.mail_threads (owner_id, id) MATCH FULL,
    ADD CONSTRAINT runs_mail_model_check CHECK (
        (mail_thread_id IS NULL AND mail_source_run_id IS NULL AND model_id <> 'gpt-oss-120b')
        OR (mail_thread_id IS NOT NULL AND distributed AND ide_chat_id IS NULL
            AND model_id = 'gpt-oss-120b')
    ),
    ADD CONSTRAINT runs_mail_thread_run_unique UNIQUE (mail_owner_id, mail_thread_id, id);

ALTER TABLE forge_agent.runs
    ADD CONSTRAINT runs_mail_source_fk FOREIGN KEY (mail_owner_id, mail_thread_id, mail_source_run_id)
        REFERENCES forge_agent.runs (mail_owner_id, mail_thread_id, id);

CREATE UNIQUE INDEX mail_runs_by_request
    ON forge_agent.runs (mail_owner_id, mail_thread_id, request_id)
    WHERE mail_thread_id IS NOT NULL;

CREATE UNIQUE INDEX one_active_run_per_mail_thread
    ON forge_agent.runs (mail_owner_id, mail_thread_id)
    WHERE mail_thread_id IS NOT NULL AND status IN ('queued', 'running', 'waiting_for_tool', 'result_ready');

ALTER TABLE forge_agent.mail_threads
    ADD CONSTRAINT mail_threads_latest_run_fk FOREIGN KEY (owner_id, id, latest_run_id)
        REFERENCES forge_agent.runs (mail_owner_id, mail_thread_id, id);

CREATE TABLE forge_agent.mail_messages (
    owner_id text NOT NULL,
    thread_id uuid NOT NULL,
    sequence bigint NOT NULL CHECK (sequence > 0),
    run_id uuid NOT NULL,
    role text NOT NULL CHECK (role IN ('user', 'assistant')),
    content text NOT NULL CHECK (octet_length(content) BETWEEN 1 AND 32768),
    PRIMARY KEY (owner_id, thread_id, sequence),
    FOREIGN KEY (owner_id, thread_id, run_id)
        REFERENCES forge_agent.runs (mail_owner_id, mail_thread_id, id),
    UNIQUE (run_id, role)
);

CREATE OR REPLACE FUNCTION forge_agent_worker_api.next_model(p_models text[])
RETURNS text
LANGUAGE sql SECURITY DEFINER STRICT SET search_path = pg_catalog, pg_temp
AS $fn$
    SELECT r.model_id
    FROM forge_agent.runs r
    JOIN forge_agent.conversations c ON c.id = r.conversation_id
    LEFT JOIN forge_agent.ide_chats h
        ON (h.owner_id, h.id) = (r.ide_chat_owner_id, r.ide_chat_id)
    WHERE c.owner_id = 'mission-control' AND r.distributed AND r.status = 'queued'
      AND (r.model_id IN ('gpt-oss-20b', 'qwen2.5-coder-7b')
           OR (r.model_id = 'gpt-oss-120b' AND r.mail_thread_id IS NOT NULL))
      AND r.model_id = ANY(p_models)
      AND r.attempt_no < 3 AND r.next_attempt_at <= clock_timestamp()
      AND r.run_deadline > clock_timestamp()
      AND (r.ide_chat_id IS NULL OR (h.id IS NOT NULL AND h.removed_at IS NULL
          AND (h.expires_at IS NULL OR h.expires_at > clock_timestamp())))
    ORDER BY r.created_at, r.id
    LIMIT 1;
$fn$;

REVOKE ALL ON FUNCTION forge_agent_worker_api.next_model(text[]) FROM PUBLIC;

CREATE OR REPLACE FUNCTION forge_agent_worker_api.claim(p_model text)
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
            'lease_until', job.lease_until, 'deadline', job.run_deadline) ||
            CASE WHEN job.mail_thread_id IS NOT NULL
                THEN jsonb_build_object('mail_profile', settings->'mail_profile') ELSE '{}'::jsonb END;
    END LOOP;
    RETURN NULL;
END;
$fn$;
