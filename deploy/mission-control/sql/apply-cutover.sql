-- Apply ONLY the proxy cutover migration (41). This file is the
-- single entry point for cutover: apply.sql deliberately stops at
-- migration 40 (preparation), so an ordinary upgrade can never
-- revoke direct writes or rewrite admission on its own. Run this
-- only from deploy/mission-control/proxy-cutover.sh, after the
-- quiescence checks pass. It refuses to run unless the preparation
-- migration (40) is applied, and it is a no-op once 41 is recorded.
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
               WHERE version = 40 AND name = 'pg_proxy_preparation') THEN
  RAISE EXCEPTION 'cutover requires the preparation migration (40) first';
 END IF;
END
$apply_cutover_guard$;

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version = 41
) AS apply_0041
\gset

\if :apply_0041
\ir migrations/0041_pg_proxy_cutover.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(41,'pg_proxy_cutover');
\endif

COMMIT;
