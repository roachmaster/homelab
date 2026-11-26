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
DO $post_merge_reads$
BEGIN
    IF EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=28 AND name='post_merge_verification') THEN
        GRANT SELECT ON forge_agent.post_merge_verification_grants,forge_agent.post_merge_verifications,forge_agent.post_merge_remediations TO muse_events;
    END IF;
END;
$post_merge_reads$;
DO $delivery_reads$
BEGIN
 IF EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=29 AND name='delivery_automation') THEN
 GRANT SELECT ON forge_agent.delivery_projects,forge_agent.delivery_capability_grants,forge_agent.delivery_lineages,forge_agent.delivery_ledger,forge_agent.delivery_post_merge_links,forge_agent.delivery_task_lineages,forge_agent.delivery_project_main_heads,forge_agent.delivery_operations TO muse_events;
 END IF;
END;
$delivery_reads$;
DO $cicd_branch_reads$
BEGIN
 IF EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=41 AND name='agent_branch_and_cicd_stages') THEN
  GRANT SELECT ON forge_agent.agent_branch_state,forge_agent.jenkins_stage_results,forge_agent.jenkins_stage_log_chunks TO muse_events;
 END IF;
END;
$cicd_branch_reads$;
-- No read grants for the migration 0044 identity catalog tables or the
-- migration 0045 deadline-extension table: those migrations deliberately
-- revoke muse_events access, and the reader must stay excluded.
-- Match the pre-existing permission for future tables from this owner/schema.
ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent_owner IN SCHEMA forge_agent
    GRANT SELECT ON TABLES TO muse_events;
\ir verify-muse-read-access.sql
COMMIT;
