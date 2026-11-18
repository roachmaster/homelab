\set ON_ERROR_STOP on
BEGIN;

CREATE SCHEMA IF NOT EXISTS forge_game;
REVOKE ALL ON SCHEMA forge_game FROM PUBLIC;

CREATE TABLE IF NOT EXISTS forge_game.builds (
    build_id uuid PRIMARY KEY,
    node_alias text NOT NULL CHECK (node_alias ~ '^[A-Za-z0-9_.-]{1,64}$'),
    project_alias text NOT NULL CHECK (project_alias ~ '^[A-Za-z0-9_.-]{1,64}$'),
    revision text NOT NULL CHECK (revision ~ '^[0-9a-f]{40}$'),
    profile text NOT NULL CHECK (profile = 'static-web'),
    source_recipe text NOT NULL CHECK (source_recipe ~ '^[0-9a-f]{64}$'),
    intent_key text NOT NULL CHECK (intent_key ~ '^[A-Za-z0-9_.-]{1,128}$'),
    state text NOT NULL CHECK (state IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 3),
    attempt_token uuid,
    lease_until timestamptz,
    artifact_digest text CHECK (artifact_digest ~ '^[0-9a-f]{64}$'),
    artifact_bytes integer CHECK (artifact_bytes BETWEEN 1 AND 16777216),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (node_alias, project_alias, intent_key),
    CHECK ((state = 'running') = (attempt_token IS NOT NULL AND lease_until IS NOT NULL)),
    CHECK ((attempt_token IS NULL) = (lease_until IS NULL)),
    CHECK ((state = 'succeeded') = (artifact_digest IS NOT NULL AND artifact_bytes IS NOT NULL)),
    CHECK ((artifact_digest IS NULL) = (artifact_bytes IS NULL))
);

CREATE INDEX IF NOT EXISTS forge_game_queued
ON forge_game.builds (created_at, build_id) WHERE state = 'queued';

CREATE INDEX IF NOT EXISTS forge_game_leases
ON forge_game.builds (lease_until, build_id) WHERE state = 'running';

CREATE TABLE IF NOT EXISTS forge_game.build_events (
    build_id uuid NOT NULL REFERENCES forge_game.builds (build_id),
    sequence integer NOT NULL CHECK (sequence > 0),
    state text NOT NULL CHECK (state IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    PRIMARY KEY (build_id, sequence)
);

COMMIT;
