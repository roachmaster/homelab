-- M27: untrusted Jenkins event log, fenced consumer deliveries and observations.
-- Selective port of never-applied legacy0018; shipped migrations1--26 stay frozen.
-- These observations confer no coding review, CI, source or merge authority.
SET LOCAL lock_timeout='5s';
SET LOCAL statement_timeout='60s';

CREATE TABLE forge_agent.jenkins_events (
    event_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_id text NOT NULL
        CONSTRAINT jenkins_events_owner_id_check CHECK (
            octet_length(owner_id) BETWEEN 1 AND 128
            AND btrim(owner_id) <> ''
            AND owner_id ~ '^[ -~]+$'
        ),
    project_alias text NOT NULL
        CONSTRAINT jenkins_events_project_alias_check CHECK (
            project_alias ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    sequence bigint NOT NULL
        CONSTRAINT jenkins_events_sequence_check CHECK (
            sequence BETWEEN 1 AND 9007199254740991
        ),
    jenkins_instance text NOT NULL
        CONSTRAINT jenkins_events_instance_check CHECK (
            jenkins_instance ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    job_name text NOT NULL
        CONSTRAINT jenkins_events_job_name_check CHECK (
            job_name ~ '^[A-Za-z0-9_./-]{1,128}$'
            AND left(job_name, 1) <> '/'
            AND right(job_name, 1) <> '/'
            AND position('//' in job_name) = 0
            AND job_name !~ '(^|/)\.\.?($|/)'
        ),
    delivery_id text NOT NULL
        CONSTRAINT jenkins_events_delivery_id_check CHECK (
            delivery_id ~ '^[A-Za-z0-9_.:-]{1,128}$'
        ),
    event_kind text NOT NULL
        CONSTRAINT jenkins_events_kind_check CHECK (
            event_kind IN ('build_queued', 'build_started', 'build_completed', 'stage_completed')
        ),
    build_number integer
        CONSTRAINT jenkins_events_build_number_check CHECK (
            build_number IS NULL OR build_number BETWEEN 1 AND 2147483647
        ),
    queue_id bigint
        CONSTRAINT jenkins_events_queue_id_check CHECK (
            queue_id IS NULL OR queue_id BETWEEN 1 AND 9223372036854775807
        ),
    commit_sha text
        CONSTRAINT jenkins_events_commit_sha_check CHECK (
            commit_sha IS NULL OR commit_sha ~ '^[0-9a-f]{40}$'
        ),
    branch text
        CONSTRAINT jenkins_events_branch_check CHECK (
            branch IS NULL OR (
                octet_length(branch) BETWEEN 1 AND 256
                AND btrim(branch) <> ''
                AND branch !~ '[[:cntrl:]]'
            )
        ),
    build_result text
        CONSTRAINT jenkins_events_build_result_check CHECK (
            build_result IS NULL OR build_result IN ('success', 'failure', 'unstable', 'aborted', 'not_built')
        ),
    stage_name text
        CONSTRAINT jenkins_events_stage_name_check CHECK (
            stage_name IS NULL OR (
                octet_length(stage_name) BETWEEN 1 AND 128
                AND btrim(stage_name) <> ''
                AND stage_name !~ '[[:cntrl:]]'
            )
        ),
    stage_status text
        CONSTRAINT jenkins_events_stage_status_check CHECK (
            stage_status IS NULL OR stage_status IN ('success', 'failure', 'unstable', 'aborted', 'skipped')
        ),
    duration_ms integer
        CONSTRAINT jenkins_events_duration_ms_check CHECK (
            duration_ms IS NULL OR duration_ms BETWEEN 0 AND 86400000
        ),
    build_url text
        CONSTRAINT jenkins_events_build_url_check CHECK (
            build_url IS NULL OR (
                octet_length(build_url) BETWEEN 9 AND 512
            AND (build_url ~ '^https://[^/?#@[:space:][:cntrl:]]+([/?#][^[:space:][:cntrl:]]*)?$'
              OR (build_url ~ '^http://' AND build_url ~* '^http://(localhost|127\.0\.0\.1|\[::1\])(:[0-9]{1,5})?(/[^[:space:][:cntrl:]]*)?$'))
            )
        ),
    summary text NOT NULL
        CONSTRAINT jenkins_events_summary_check CHECK (
            octet_length(summary) BETWEEN 1 AND 1024
            AND btrim(summary) <> ''
            AND summary !~ '[[:cntrl:]]'
        ),
    correlation_id text
        CONSTRAINT jenkins_events_correlation_id_check CHECK (
            correlation_id IS NULL OR correlation_id ~ '^[A-Za-z0-9_.:-]{1,128}$'
        ),
    occurred_at timestamptz NOT NULL,
    received_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    payload_sha256 text NOT NULL
        CONSTRAINT jenkins_events_payload_sha256_check CHECK (
            payload_sha256 ~ '^[0-9a-f]{64}$'
        ),
    CONSTRAINT jenkins_events_project_sequence_unique UNIQUE (owner_id, project_alias, sequence),
    CONSTRAINT jenkins_events_source_identity_unique UNIQUE (event_id, owner_id, project_alias, sequence),
    CONSTRAINT jenkins_events_delivery_unique UNIQUE (owner_id, project_alias, jenkins_instance, delivery_id),
    CONSTRAINT jenkins_events_kind_fields_check CHECK (
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
            OR (event_kind = 'stage_completed'
                AND build_number IS NOT NULL
                AND build_result IS NULL
                AND stage_name IS NOT NULL
                AND stage_status IS NOT NULL)
        ) IS TRUE
    )
);

CREATE INDEX jenkins_events_by_job_build
    ON forge_agent.jenkins_events (owner_id, project_alias, jenkins_instance, job_name, build_number, sequence);

CREATE INDEX jenkins_events_by_commit
    ON forge_agent.jenkins_events (owner_id, project_alias, commit_sha, sequence)
    WHERE commit_sha IS NOT NULL;

CREATE FUNCTION forge_agent.jenkins_events_immutable()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $$
BEGIN
    RAISE EXCEPTION 'Jenkins events are append-only and immutable'
        USING ERRCODE = 'P0009';
END;
$$;

CREATE TRIGGER jenkins_events_immutable
    BEFORE UPDATE OR DELETE ON forge_agent.jenkins_events
    FOR EACH ROW
    EXECUTE FUNCTION forge_agent.jenkins_events_immutable();

CREATE TABLE forge_agent.jenkins_event_deliveries (
    delivery_row_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    event_id uuid NOT NULL
        CONSTRAINT jenkins_event_deliveries_event_fk
        REFERENCES forge_agent.jenkins_events (event_id),
    owner_id text NOT NULL
        CONSTRAINT jenkins_event_deliveries_owner_id_check CHECK (
            octet_length(owner_id) BETWEEN 1 AND 128
            AND btrim(owner_id) <> ''
            AND owner_id ~ '^[ -~]+$'
        ),
    project_alias text NOT NULL
        CONSTRAINT jenkins_event_deliveries_project_alias_check CHECK (
            project_alias ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    event_sequence bigint NOT NULL
        CONSTRAINT jenkins_event_deliveries_sequence_check CHECK (
            event_sequence BETWEEN 1 AND 9007199254740991
        ),
    consumer_name text NOT NULL
        CONSTRAINT jenkins_event_deliveries_consumer_name_check CHECK (
            consumer_name ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    status text NOT NULL DEFAULT 'pending'
        CONSTRAINT jenkins_event_deliveries_status_check CHECK (
            status IN ('pending', 'leased', 'consumed', 'failed', 'dead_lettered')
        ),
    attempt_count integer NOT NULL DEFAULT 0
        CONSTRAINT jenkins_event_deliveries_attempt_count_check CHECK (
            attempt_count BETWEEN 0 AND 100
        ),
    max_attempts integer NOT NULL DEFAULT 5
        CONSTRAINT jenkins_event_deliveries_max_attempts_check CHECK (
            max_attempts BETWEEN 1 AND 20
        ),
    worker_id text
        CONSTRAINT jenkins_event_deliveries_worker_id_check CHECK (
            worker_id IS NULL OR worker_id ~ '^[A-Za-z0-9_.:-]{1,128}$'
        ),
    lease_token uuid,
    lease_expires_at timestamptz,
    next_attempt_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    last_error text
        CONSTRAINT jenkins_event_deliveries_last_error_check CHECK (
            last_error IS NULL OR (
                octet_length(last_error) BETWEEN 1 AND 1024
                AND last_error !~ '[[:cntrl:]]'
            )
        ),
    receipt_digest text
        CONSTRAINT jenkins_event_deliveries_receipt_digest_check CHECK (
            receipt_digest IS NULL OR receipt_digest ~ '^[0-9a-f]{64}$'
        ),
    consumed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT jenkins_event_deliveries_event_seq_fk
        FOREIGN KEY (event_id, owner_id, project_alias, event_sequence)
        REFERENCES forge_agent.jenkins_events (event_id, owner_id, project_alias, sequence),
    CONSTRAINT jenkins_event_deliveries_event_consumer_unique
        UNIQUE (event_id, consumer_name),
    CONSTRAINT jenkins_event_deliveries_status_fields_check CHECK (
        (
            (status = 'pending'
                AND worker_id IS NULL
                AND lease_token IS NULL
                AND lease_expires_at IS NULL
                AND receipt_digest IS NULL
                AND consumed_at IS NULL)
            OR (status = 'leased'
                AND attempt_count >= 1
                AND worker_id IS NOT NULL
                AND lease_token IS NOT NULL
                AND lease_expires_at IS NOT NULL
                AND receipt_digest IS NULL
                AND consumed_at IS NULL)
            OR (status = 'consumed'
                AND attempt_count >= 1
                AND worker_id IS NULL
                AND lease_token IS NULL
                AND lease_expires_at IS NULL
                AND receipt_digest IS NOT NULL
                AND consumed_at IS NOT NULL)
            OR (status = 'failed'
                AND attempt_count >= 1
                AND attempt_count < max_attempts
                AND worker_id IS NULL
                AND lease_token IS NULL
                AND lease_expires_at IS NULL
                AND last_error IS NOT NULL
                AND receipt_digest IS NULL
                AND consumed_at IS NULL)
            OR (status = 'dead_lettered'
                AND worker_id IS NULL
                AND lease_token IS NULL
                AND lease_expires_at IS NULL
                AND last_error IS NOT NULL
                AND receipt_digest IS NULL
                AND consumed_at IS NULL)
        ) IS TRUE
    )
);

CREATE INDEX jenkins_event_deliveries_claimable
    ON forge_agent.jenkins_event_deliveries (owner_id, consumer_name, next_attempt_at, event_sequence)
    WHERE status IN ('pending', 'leased', 'failed');

CREATE INDEX jenkins_event_deliveries_by_project_sequence
    ON forge_agent.jenkins_event_deliveries (owner_id, project_alias, event_sequence, consumer_name);

CREATE FUNCTION forge_agent.jenkins_event_deliveries_guard()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog
AS $$
BEGIN
    IF OLD.status IN ('consumed', 'dead_lettered') THEN
        RAISE EXCEPTION 'Terminal Jenkins event delivery is immutable'
            USING ERRCODE = 'P0009';
    END IF;
    IF TG_OP='DELETE' THEN RETURN OLD;END IF;
    IF NEW.delivery_row_id IS DISTINCT FROM OLD.delivery_row_id
       OR NEW.event_id IS DISTINCT FROM OLD.event_id
       OR NEW.owner_id IS DISTINCT FROM OLD.owner_id
       OR NEW.project_alias IS DISTINCT FROM OLD.project_alias
       OR NEW.event_sequence IS DISTINCT FROM OLD.event_sequence
       OR NEW.consumer_name IS DISTINCT FROM OLD.consumer_name
       OR NEW.max_attempts IS DISTINCT FROM OLD.max_attempts
       OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'Jenkins event delivery identity columns are immutable'
            USING ERRCODE = 'P0009';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER jenkins_event_deliveries_guard
    BEFORE UPDATE OR DELETE ON forge_agent.jenkins_event_deliveries
    FOR EACH ROW
    EXECUTE FUNCTION forge_agent.jenkins_event_deliveries_guard();

CREATE TABLE forge_agent.jenkins_observations (
    owner_id text NOT NULL
        CONSTRAINT jenkins_observations_owner_id_check CHECK (
            octet_length(owner_id) BETWEEN 1 AND 128
            AND btrim(owner_id) <> ''
            AND owner_id ~ '^[ -~]+$'
        ),
    project_alias text NOT NULL
        CONSTRAINT jenkins_observations_project_alias_check CHECK (
            project_alias ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    jenkins_instance text NOT NULL
        CONSTRAINT jenkins_observations_instance_check CHECK (
            jenkins_instance ~ '^[A-Za-z0-9_.-]{1,64}$'
        ),
    job_name text NOT NULL
        CONSTRAINT jenkins_observations_job_name_check CHECK (
            job_name ~ '^[A-Za-z0-9_./-]{1,128}$'
            AND left(job_name, 1) <> '/'
            AND right(job_name, 1) <> '/'
            AND position('//' in job_name) = 0
            AND job_name !~ '(^|/)\.\.?($|/)'
        ),
    observation_key text NOT NULL
        CONSTRAINT jenkins_observations_key_check CHECK (
            observation_key ~ '^(build:[1-9][0-9]{0,9}|queue:[1-9][0-9]{0,18})$'
        ),
    build_number integer
        CONSTRAINT jenkins_observations_build_number_check CHECK (
            build_number IS NULL OR build_number BETWEEN 1 AND 2147483647
        ),
    queue_id bigint
        CONSTRAINT jenkins_observations_queue_id_check CHECK (
            queue_id IS NULL OR queue_id BETWEEN 1 AND 9223372036854775807
        ),
    last_event_id uuid NOT NULL
        CONSTRAINT jenkins_observations_event_fk
        REFERENCES forge_agent.jenkins_events (event_id),
    last_event_sequence bigint NOT NULL
        CONSTRAINT jenkins_observations_sequence_check CHECK (
            last_event_sequence BETWEEN 1 AND 9007199254740991
        ),
    last_event_kind text NOT NULL
        CONSTRAINT jenkins_observations_kind_check CHECK (
            last_event_kind IN ('build_queued', 'build_started', 'build_completed', 'stage_completed')
        ),
    commit_sha text
        CONSTRAINT jenkins_observations_commit_sha_check CHECK (
            commit_sha IS NULL OR commit_sha ~ '^[0-9a-f]{40}$'
        ),
    branch text
        CONSTRAINT jenkins_observations_branch_check CHECK (
            branch IS NULL OR (
                octet_length(branch) BETWEEN 1 AND 256
                AND btrim(branch) <> ''
                AND branch !~ '[[:cntrl:]]'
            )
        ),
    build_result text
        CONSTRAINT jenkins_observations_build_result_check CHECK (
            build_result IS NULL OR build_result IN ('success', 'failure', 'unstable', 'aborted', 'not_built')
        ),
    stage_name text
        CONSTRAINT jenkins_observations_stage_name_check CHECK (
            stage_name IS NULL OR (
                octet_length(stage_name) BETWEEN 1 AND 128
                AND btrim(stage_name) <> ''
                AND stage_name !~ '[[:cntrl:]]'
            )
        ),
    stage_status text
        CONSTRAINT jenkins_observations_stage_status_check CHECK (
            stage_status IS NULL OR stage_status IN ('success', 'failure', 'unstable', 'aborted', 'skipped')
        ),
    duration_ms integer
        CONSTRAINT jenkins_observations_duration_ms_check CHECK (
            duration_ms IS NULL OR duration_ms BETWEEN 0 AND 86400000
        ),
    build_url text
        CONSTRAINT jenkins_observations_build_url_check CHECK (
            build_url IS NULL OR (
                octet_length(build_url) BETWEEN 9 AND 512
                AND (build_url ~ '^https://[^/?#@[:space:][:cntrl:]]+([/?#][^[:space:][:cntrl:]]*)?$'
                  OR (build_url ~ '^http://' AND build_url ~* '^http://(localhost|127\.0\.0\.1|\[::1\])(:[0-9]{1,5})?(/[^[:space:][:cntrl:]]*)?$'))
            )
        ),
    summary text NOT NULL
        CONSTRAINT jenkins_observations_summary_check CHECK (
            octet_length(summary) BETWEEN 1 AND 1024
            AND btrim(summary) <> ''
            AND summary !~ '[[:cntrl:]]'
        ),
    occurred_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT jenkins_observations_pkey
        PRIMARY KEY (owner_id, project_alias, jenkins_instance, job_name, observation_key),
    CONSTRAINT jenkins_observations_event_seq_fk
        FOREIGN KEY (last_event_id, owner_id, project_alias, last_event_sequence)
        REFERENCES forge_agent.jenkins_events (event_id, owner_id, project_alias, sequence)
);

CREATE FUNCTION forge_agent.jenkins_observations_guard()
RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog AS $guard$
DECLARE source forge_agent.jenkins_events%ROWTYPE;
BEGIN
 IF TG_OP='UPDATE' THEN
  IF(NEW.owner_id,NEW.project_alias,NEW.jenkins_instance,NEW.job_name,NEW.observation_key)
   IS DISTINCT FROM(OLD.owner_id,OLD.project_alias,OLD.jenkins_instance,OLD.job_name,OLD.observation_key)
   OR NEW.last_event_sequence<=OLD.last_event_sequence THEN
   RAISE EXCEPTION 'Jenkins observation scope and sequence must be monotonic' USING ERRCODE='P0009';
  END IF;
 END IF;
 SELECT * INTO source FROM forge_agent.jenkins_events WHERE event_id=NEW.last_event_id;
 IF NOT FOUND OR(NEW.owner_id,NEW.project_alias,NEW.last_event_sequence,NEW.jenkins_instance,NEW.job_name,NEW.last_event_kind)
  IS DISTINCT FROM(source.owner_id,source.project_alias,source.sequence,source.jenkins_instance,source.job_name,source.event_kind)
  OR(NEW.build_number,NEW.stage_name,NEW.stage_status,NEW.summary,NEW.occurred_at)
   IS DISTINCT FROM(source.build_number,source.stage_name,source.stage_status,source.summary,source.occurred_at)
  OR(source.queue_id IS NOT NULL AND NEW.queue_id IS DISTINCT FROM source.queue_id)
  OR(source.commit_sha IS NOT NULL AND NEW.commit_sha IS DISTINCT FROM source.commit_sha)
  OR(source.branch IS NOT NULL AND NEW.branch IS DISTINCT FROM source.branch)
  OR(source.build_result IS NOT NULL AND NEW.build_result IS DISTINCT FROM source.build_result)
  OR(source.duration_ms IS NOT NULL AND NEW.duration_ms IS DISTINCT FROM source.duration_ms)
  OR(source.build_url IS NOT NULL AND NEW.build_url IS DISTINCT FROM source.build_url)
  OR NEW.observation_key IS DISTINCT FROM(CASE WHEN source.build_number IS NOT NULL THEN 'build:'||source.build_number::text ELSE 'queue:'||source.queue_id::text END) THEN
  RAISE EXCEPTION 'Jenkins observation must match its source event' USING ERRCODE='P0009';
 END IF;
 RETURN NEW;
END;
$guard$;
CREATE TRIGGER jenkins_observations_guard BEFORE INSERT OR UPDATE ON forge_agent.jenkins_observations
 FOR EACH ROW EXECUTE FUNCTION forge_agent.jenkins_observations_guard();

REVOKE ALL ON TABLE
    forge_agent.jenkins_events,
    forge_agent.jenkins_event_deliveries,
    forge_agent.jenkins_observations
FROM PUBLIC;

REVOKE ALL ON FUNCTION
    forge_agent.jenkins_events_immutable(),
    forge_agent.jenkins_event_deliveries_guard(),
    forge_agent.jenkins_observations_guard()
FROM PUBLIC;
