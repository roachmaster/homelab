-- Apply ONLY the proxy cutover migration (52). This file is the
-- single entry point for cutover: apply.sql deliberately stops at
-- migration 52 (preparation), so an ordinary upgrade can never
-- revoke direct writes or rewrite admission on its own. Run this
-- only from deploy/mission-control/proxy-cutover.sh, after the
-- quiescence checks pass. It refuses to run unless the preparation
-- migration (51) is applied, and it is a no-op once 52 is recorded.
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

DO $apply_cutover_guard$
BEGIN
 IF NOT EXISTS(SELECT 1 FROM forge_agent.schema_migrations
               WHERE version = 52 AND name = 'pg_proxy_preparation') THEN
  RAISE EXCEPTION 'cutover requires the preparation migration (51) first';
 END IF;
END
$apply_cutover_guard$;

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version = 53
) AS apply_0052
\gset

\if :apply_0052
\ir migrations/0053_pg_proxy_cutover.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(53,'pg_proxy_cutover');
\endif

COMMIT;
