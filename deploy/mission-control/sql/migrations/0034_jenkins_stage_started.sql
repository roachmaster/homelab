-- M34: admit the stage_started Jenkins event kind for the pipeline stage
-- diagram. The stage producer emits it at each fixed-check start; the
-- lifecycle producer and the stage-completion path are unchanged. M1--33
-- and their authority stay intact.
ALTER TABLE forge_agent.jenkins_events DROP CONSTRAINT jenkins_events_kind_check;
ALTER TABLE forge_agent.jenkins_events ADD CONSTRAINT jenkins_events_kind_check CHECK (
    event_kind IN ('build_queued', 'build_started', 'build_completed', 'stage_started', 'stage_completed')
);
ALTER TABLE forge_agent.jenkins_events DROP CONSTRAINT jenkins_events_kind_fields_check;
ALTER TABLE forge_agent.jenkins_events ADD CONSTRAINT jenkins_events_kind_fields_check CHECK (
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
ALTER TABLE forge_agent.jenkins_observations DROP CONSTRAINT jenkins_observations_kind_check;
ALTER TABLE forge_agent.jenkins_observations ADD CONSTRAINT jenkins_observations_kind_check CHECK (
    last_event_kind IN ('build_queued', 'build_started', 'build_completed', 'stage_started', 'stage_completed')
);
