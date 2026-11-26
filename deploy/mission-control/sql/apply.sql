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

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version=20
) AS apply_0020
\gset
\if :apply_0020
\ir migrations/0020_external_agent_transport.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(20,'external_agent_transport');
\endif

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version=21
) AS apply_0021
\gset
\if :apply_0021
\ir migrations/0021_coding_event_trace.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(21,'coding_event_trace');
\endif
SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version=22
) AS apply_0022
\gset
\if :apply_0022
\ir migrations/0022_external_workflows.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(22,'external_workflows');
\endif

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version=23
) AS apply_0023
\gset
\if :apply_0023
\ir migrations/0023_coding_agy_unattended.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(23,'coding_agy_unattended');
\endif

SELECT NOT EXISTS (
    SELECT 1 FROM forge_agent.schema_migrations WHERE version=24
) AS apply_0024
\gset
\if :apply_0024
\ir migrations/0024_native_jenkins_context.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(24,'native_jenkins_context');
\endif

SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=25) AS apply_0025
\gset
\if :apply_0025
\ir migrations/0025_native_delivery_repair.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(25,'native_delivery_repair');
\endif
SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=26) AS apply_0026
\gset
\if :apply_0026
\ir migrations/0026_coding_owner_continuation.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(26,'coding_owner_continuation');
\endif

SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=27) AS apply_0027
\gset
\if :apply_0027
\ir migrations/0027_jenkins_events.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(27,'jenkins_events');
\endif
SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=28) AS apply_0028
\gset
\if :apply_0028
\ir migrations/0028_post_merge_verification.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(28,'post_merge_verification');
\endif
SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=29) AS apply_0029
\gset
\if :apply_0029
\ir migrations/0029_delivery_automation.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(29,'delivery_automation');
\endif
SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=30) AS apply_0030
\gset
\if :apply_0030
\ir migrations/0030_agent_wide_retry_policy.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(30,'agent_wide_retry_policy');
\endif
SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=31) AS apply_0031
\gset
\if :apply_0031
\ir migrations/0031_retry_events.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(31,'retry_events');
\endif
SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=32) AS apply_0032
\gset
\if :apply_0032
\ir migrations/0032_observer_retry_lineage.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(32,'observer_retry_lineage');
\endif
SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=33) AS apply_0033
\gset
\if :apply_0033
\ir migrations/0033_superseding_reviews.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(33,'superseding_reviews');
\endif
SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=34) AS apply_0034
\gset
\if :apply_0034
\ir migrations/0034_jenkins_stage_started.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(34,'jenkins_stage_started');
\endif
SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=35) AS apply_0035
\gset
\if :apply_0035
\ir migrations/0035_event_stream_peer_review_diagnostics.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(35,'event_stream_peer_review_diagnostics');
\endif
SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=36) AS apply_0036
\gset
\if :apply_0036
\ir migrations/0036_event_stream_actor_cause.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(36,'event_stream_actor_cause');
\endif
SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=37) AS apply_0037
\gset
\if :apply_0037
\ir migrations/0037_event_stream_jenkins_producer.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(37,'event_stream_jenkins_producer');
\endif
SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=38) AS apply_0038
\gset
\if :apply_0038
\ir migrations/0038_external_plan_approve_override.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(38,'external_plan_approve_override');
\endif
SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=39) AS apply_0039
\gset
\if :apply_0039
\ir migrations/0039_terminal_retry_settlement.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(39,'terminal_retry_settlement');
\endif
SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=40) AS apply_0040
\gset
\if :apply_0040
\ir migrations/0040_provider_author_handoff.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(40,'provider_author_handoff');
\endif
SELECT NOT EXISTS (SELECT 1 FROM forge_agent.schema_migrations WHERE version=42) AS apply_0042
\gset
\if :apply_0042
\ir migrations/0042_pg_proxy_preparation.sql
INSERT INTO forge_agent.schema_migrations(version,name) VALUES(42,'pg_proxy_preparation');
\endif
-- Migration 43 (pg_proxy_cutover) is NOT applied here. Cutover is an
-- explicitly gated operator step (rollout step 4): it revokes direct
-- writes and rewrites admission, so an ordinary upgrade must never
-- perform it. Apply it only via sql/apply-cutover.sql from
-- deploy/mission-control/proxy-cutover.sh, after quiescence
-- verification, credential rotation, and session termination.
COMMIT;
