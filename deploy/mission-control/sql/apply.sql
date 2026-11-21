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

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version = 2
) AS apply_0002
\gset

\if :apply_0002
\ir migrations/0002_ide_workspace.sql
INSERT INTO forge_agent.schema_migrations (version, name)
VALUES (2, 'ide_workspace');
\endif

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version = 3
) AS apply_0003
\gset

\if :apply_0003
\ir migrations/0003_ide_chat_history.sql
INSERT INTO forge_agent.schema_migrations (version, name)
VALUES (3, 'ide_chat_history');
\endif

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version = 4
) AS apply_0004
\gset

\if :apply_0004
\ir migrations/0004_ide_chat_default_expiration.sql
INSERT INTO forge_agent.schema_migrations (version, name)
VALUES (4, 'ide_chat_default_expiration');
\endif

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version = 5
) AS apply_0005
\gset

\if :apply_0005
\ir migrations/0005_inference_dispatch.sql
INSERT INTO forge_agent.schema_migrations (version, name)
VALUES (5, 'inference_dispatch');
\endif

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version = 6
) AS apply_0006
\gset

\if :apply_0006
\ir migrations/0006_inference_model_selection.sql
INSERT INTO forge_agent.schema_migrations (version, name)
VALUES (6, 'inference_model_selection');
\endif

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version = 7
) AS apply_0007
\gset

\if :apply_0007
\ir migrations/0007_ide_mail.sql
INSERT INTO forge_agent.schema_migrations (version, name)
VALUES (7, 'ide_mail');
\endif

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version = 8
) AS apply_0008
\gset

\if :apply_0008
\ir migrations/0008_mail_archive.sql
INSERT INTO forge_agent.schema_migrations (version, name)
VALUES (8, 'mail_archive');
\endif

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version = 9
) AS apply_0009
\gset

\if :apply_0009
\ir migrations/0009_coding_admission.sql
INSERT INTO forge_agent.schema_migrations (version, name)
VALUES (9, 'coding_admission');
\endif

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version = 10
) AS apply_0010
\gset

\if :apply_0010
\ir migrations/0010_coding_execution_and_usage.sql
INSERT INTO forge_agent.schema_migrations (version, name)
VALUES (10, 'coding_execution_and_usage');
\endif

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version = 11
) AS apply_0011
\gset

\if :apply_0011
\ir migrations/0011_coding_elevated_tool_policy.sql
INSERT INTO forge_agent.schema_migrations (version, name)
VALUES (11, 'coding_elevated_tool_policy');
\endif

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version = 12
) AS apply_0012
\gset

\if :apply_0012
\ir migrations/0012_coding_progress.sql
INSERT INTO forge_agent.schema_migrations (version, name)
VALUES (12, 'coding_progress');
\endif

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version = 13
) AS apply_0013
\gset

\if :apply_0013
\ir migrations/0013_coding_task_session_policy.sql
INSERT INTO forge_agent.schema_migrations (version, name)
VALUES (13, 'coding_task_session_policy');
\endif

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version = 14
) AS apply_0014
\gset

\if :apply_0014
\ir migrations/0014_coding_plan_workflow.sql
INSERT INTO forge_agent.schema_migrations (version,name) VALUES (14,'coding_plan_workflow');
\endif

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version = 15
) AS apply_0015
\gset
\if :apply_0015
\ir migrations/0015_coding_feedback_loop.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(15,'coding_feedback_loop');
\endif

SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=16) AS apply_0016
\gset
\if :apply_0016
\ir migrations/0016_coding_autonomous_workflows.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(16,'coding_autonomous_workflows');
\endif

SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=17) AS apply_0017
\gset
\if :apply_0017
\ir migrations/0017_coding_codex_warm_sessions.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(17,'coding_codex_warm_sessions');
\endif

SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=18) AS apply_0018
\gset
\if :apply_0018
\ir migrations/0018_coding_grok_provider.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(18,'coding_grok_provider');
\endif

SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=19) AS apply_0019
\gset
\if :apply_0019
\ir migrations/0019_coding_muse_provider.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(19,'coding_muse_provider');
\endif

COMMIT;
