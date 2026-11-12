\set ON_ERROR_STOP on

BEGIN ISOLATION LEVEL READ COMMITTED;

-- Keep this lock key stable across migration versions.
SELECT pg_catalog.pg_advisory_xact_lock(20261111, 1);

CREATE SCHEMA IF NOT EXISTS forge_agent;

CREATE TABLE IF NOT EXISTS forge_agent.schema_migrations (
    version integer PRIMARY KEY CHECK (version > 0),
    name text NOT NULL UNIQUE,
    applied_at timestamptz NOT NULL DEFAULT now()
);

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version = 1
) AS apply_0001
\gset

\if :apply_0001
\ir migrations/0001_agent.sql
INSERT INTO forge_agent.schema_migrations (version, name)
VALUES (1, 'agent');
\endif

COMMIT;
