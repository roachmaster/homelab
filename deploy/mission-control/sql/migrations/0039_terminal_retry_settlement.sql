-- M39: Transactional settlement of open retry events on terminal workflow/task scopes.
-- Preserves all M01-M38 tables, triggers, functions, exhausted retry evidence, and role boundaries.
CREATE FUNCTION forge_agent.retry_event_resolve_open_scope(p_project text, p_scope_kind text, p_scope_id uuid)
 RETURNS integer
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path = pg_catalog, pg_temp
AS $f$
DECLARE
 owner text := current_setting('forge.coding_owner', true);
 resolved_count integer := 0;
BEGIN
 PERFORM forge_agent._retry_scope(p_project, p_scope_kind, p_scope_id);
 PERFORM pg_advisory_xact_lock(hashtextextended(owner || E'\n' || p_project, 2026111901));
 IF NOT (CASE p_scope_kind
  WHEN 'native_workflow' THEN EXISTS(
   SELECT FROM forge_agent.coding_workflows
   WHERE owner_id = owner AND project_alias = p_project AND workflow_id = p_scope_id
     AND phase IN ('escalated', 'cancelled', 'completed'))
  WHEN 'external_workflow' THEN EXISTS(
   SELECT FROM forge_external_workflow.workflows
   WHERE owner_id = owner AND project_alias = p_project AND workflow_id = p_scope_id
     AND phase IN ('escalated', 'cancelled', 'completed'))
  WHEN 'manual_task' THEN EXISTS(
   SELECT FROM forge_agent.coding_tasks
   WHERE owner_id = owner AND project_alias = p_project AND id = p_scope_id
     AND workflow_id IS NULL AND status IN ('cancelled', 'completed'))
  ELSE false
 END) THEN
  RAISE EXCEPTION 'Terminal retry settlement requires a terminal scope' USING ERRCODE = 'P0009';
 END IF;
 UPDATE forge_agent.coding_retry_events
 SET status = 'resolved', resolved_at = clock_timestamp(), updated_at = clock_timestamp()
 WHERE owner_id = owner AND project_alias = p_project
   AND scope_kind = p_scope_kind AND scope_id = p_scope_id
   AND status = 'open';
 GET DIAGNOSTICS resolved_count = ROW_COUNT;
 RETURN resolved_count;
END;
$f$;
REVOKE ALL ON FUNCTION forge_agent.retry_event_resolve_open_scope(text, text, uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_agent.retry_event_resolve_open_scope(text, text, uuid) TO forge_agent, forge_coding_reconciler;

CREATE FUNCTION forge_coding_reconciler_api.retry_event_resolve_open_scope(p_project text, p_scope_kind text, p_scope_id uuid)
 RETURNS integer
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path = pg_catalog, pg_temp
AS $f$
BEGIN
 RETURN forge_agent.retry_event_resolve_open_scope(p_project, p_scope_kind, p_scope_id);
END;
$f$;
REVOKE ALL ON FUNCTION forge_coding_reconciler_api.retry_event_resolve_open_scope(text, text, uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_coding_reconciler_api.retry_event_resolve_open_scope(text, text, uuid) TO forge_coding_reconciler;
