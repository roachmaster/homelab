-- M37: event-stream fix 3 (plan section 5.3): the publication worker is
-- the lifecycle/stage producer. jenkins_events gains the stage_started
-- kind, the producer actor, and the four provenance columns that qualify
-- reconstructed or reconciled events. Existing rows and webhook emits
-- carry no actor and all provenance flags false; only the producer sets
-- them, so the actor column is nullable and the flags default false.
SET LOCAL lock_timeout='5s';
SET LOCAL statement_timeout='60s';

ALTER TABLE forge_agent.jenkins_events
    ADD COLUMN actor text
        CONSTRAINT jenkins_events_actor_check CHECK (
            actor IS NULL OR (
                octet_length(actor) BETWEEN 1 AND 128
                AND actor ~ '^[a-z]+:[A-Za-z0-9_.-]{1,128}$'
            )
        ),
    ADD COLUMN backfilled boolean NOT NULL DEFAULT FALSE,
    ADD COLUMN reconciled boolean NOT NULL DEFAULT FALSE,
    ADD COLUMN completion_time_estimated boolean NOT NULL DEFAULT FALSE,
    ADD COLUMN started_at timestamptz;

-- stage_started joins the kind vocabulary; its field rule mirrors
-- stage_completed except stage_status is forbidden (a start has no
-- outcome yet).
ALTER TABLE forge_agent.jenkins_events
    DROP CONSTRAINT jenkins_events_kind_check,
    ADD CONSTRAINT jenkins_events_kind_check CHECK (
        event_kind IN ('build_queued', 'build_started', 'build_completed', 'stage_started', 'stage_completed')
    ),
    DROP CONSTRAINT jenkins_events_kind_fields_check,
    ADD CONSTRAINT jenkins_events_kind_fields_check CHECK (
        (
            (event_kind = 'build_queued'
                AND queue_id IS NOT NULL
                AND build_number IS NULL
                AND build_result IS NULL
                AND stage_name IS NULL
                AND stage_status IS NULL)
            OR (event_kind = 'build_started'
                AND build_number IS NOT NULL
                AND build_result IS NULL
                AND stage_name IS NULL
                AND stage_status IS NULL)
            OR (event_kind = 'build_completed'
                AND build_number IS NOT NULL
                AND build_result IS NOT NULL
                AND stage_name IS NULL
                AND stage_status IS NULL)
            OR (event_kind = 'stage_started'
                AND build_number IS NOT NULL
                AND build_result IS NULL
                AND stage_name IS NOT NULL
                AND stage_status IS NULL)
            OR (event_kind = 'stage_completed'
                AND build_number IS NOT NULL
                AND build_result IS NULL
                AND stage_name IS NOT NULL
                AND stage_status IS NOT NULL)
        ) IS TRUE
    );
