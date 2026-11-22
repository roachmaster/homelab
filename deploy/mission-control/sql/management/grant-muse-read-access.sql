\set ON_ERROR_STOP on
-- Explicit opt-in for an already provisioned account. Never creates a role,
-- changes its password/attributes, rewrites HBA, or revokes existing access.
BEGIN;
DO $required$
BEGIN
    IF current_database()<>'forge_agent' OR NOT EXISTS(SELECT FROM pg_roles WHERE rolname='muse_events') THEN
        RAISE EXCEPTION 'Existing muse_events account and forge_agent database required';
    END IF;
END;
$required$;
GRANT CONNECT ON DATABASE forge_agent TO muse_events;
GRANT USAGE ON SCHEMA forge_agent TO muse_events;
GRANT SELECT ON
    forge_agent.coding_events,
    forge_agent.coding_plan_decisions,
    forge_agent.coding_result_chunks,
    forge_agent.coding_reconciliation_receipts,
    forge_agent.coding_publications,
    forge_agent.coding_hosted_checks,
    forge_agent.coding_feedback,
    forge_agent.coding_reviews,
    forge_agent.coding_tasks,
    forge_agent.coding_workflows,
    forge_agent.coding_workflow_reviews,
    forge_agent.coding_workflow_actions,
    forge_agent.coding_attempts
TO muse_events;
DO $jenkins_reads$
BEGIN
    IF EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=27 AND name='jenkins_events') THEN
        GRANT SELECT ON forge_agent.jenkins_events,forge_agent.jenkins_event_deliveries,forge_agent.jenkins_observations TO muse_events;
    END IF;
END;
$jenkins_reads$;
-- Match the pre-existing permission for future tables from this owner/schema.
ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent_owner IN SCHEMA forge_agent
    GRANT SELECT ON TABLES TO muse_events;
\ir verify-muse-read-access.sql
COMMIT;
