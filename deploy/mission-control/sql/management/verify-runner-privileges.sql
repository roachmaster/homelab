\set ON_ERROR_STOP on

BEGIN READ ONLY;
SET LOCAL search_path = pg_catalog;

DO $verify$
DECLARE
  ok boolean;
BEGIN
  IF current_user <> 'forge_agent' THEN
    RAISE EXCEPTION 'Expected current_user forge_agent, got %', current_user;
  END IF;

  SELECT has_schema_privilege(current_user, 'forge_agent', 'USAGE')
    AND NOT has_schema_privilege(current_user, 'forge_agent', 'CREATE')
    AND NOT has_table_privilege(current_user, 'forge_agent.schema_migrations', 'INSERT')
    AND (
      SELECT bool_and(has_table_privilege(current_user, 'forge_agent.' || relation, privilege))
      FROM unnest(ARRAY[
        'conversations', 'runs', 'messages', 'tool_calls',
        'tool_results', 'run_events', 'ide_workspace_tabs'
      ]) AS relation
      CROSS JOIN unnest(ARRAY['SELECT', 'INSERT', 'UPDATE', 'DELETE']) AS privilege
    )
    AND (
      SELECT bool_and(has_table_privilege(current_user, 'forge_agent.' || relation, privilege))
      FROM unnest(ARRAY['ide_workspace_history', 'ide_chats']) AS relation
      CROSS JOIN unnest(ARRAY['SELECT', 'INSERT', 'UPDATE']) AS privilege
    )
    AND (
      SELECT bool_and(
        has_table_privilege(current_user, 'forge_agent.' || spec.relation, privilege)
        = (privilege = ANY(spec.allowed))
        AND CASE
          -- Table-level checks alone miss column-only grants, including grants
          -- inherited through another role or PUBLIC.
          WHEN privilege IN ('SELECT', 'INSERT', 'UPDATE', 'REFERENCES')
               AND NOT privilege = ANY(spec.allowed)
          THEN NOT has_any_column_privilege(
            current_user, 'forge_agent.' || spec.relation, privilege
          )
          ELSE true
        END
      )
      FROM (
        VALUES
          ('mail_threads',            ARRAY['SELECT', 'INSERT', 'UPDATE']),
          ('mail_messages',           ARRAY['SELECT', 'INSERT']),
          ('inference_results',       ARRAY['SELECT', 'DELETE']),
          ('gpu_worker_readiness',    ARRAY['SELECT']),
          ('coding_tasks',                    ARRAY['SELECT', 'INSERT', 'UPDATE']),
          ('coding_events',                   ARRAY['SELECT', 'INSERT']),
          ('coding_control_receipts',         ARRAY['SELECT', 'INSERT']),
          ('coding_plan_decisions',           ARRAY['SELECT', 'INSERT']),
          ('coding_publications',             ARRAY['SELECT']),
          ('coding_worker_grants',            ARRAY[]::text[]),
          ('coding_worker_readiness',         ARRAY['SELECT']),
          ('coding_attempts',                 ARRAY['SELECT']),
          ('coding_result_chunks',            ARRAY['SELECT']),
          ('coding_reconciliation_receipts',  ARRAY['SELECT']),
          ('coding_provider_usage',           ARRAY['SELECT']),
          ('coding_provider_usage_identities', ARRAY[]::text[]),
          ('coding_usage_refresh_requests',   ARRAY['SELECT', 'INSERT'])
      ) AS spec(relation, allowed)
      CROSS JOIN unnest(ARRAY[
        'SELECT', 'INSERT', 'UPDATE', 'DELETE', 'TRUNCATE', 'REFERENCES', 'TRIGGER'
      ]) AS privilege
    )
    AND NOT has_schema_privilege(current_user, 'forge_agent_worker_api', 'USAGE, CREATE')
    AND NOT has_schema_privilege(current_user, 'forge_coding_worker_api', 'USAGE, CREATE')
    AND NOT has_schema_privilege(current_user, 'forge_coding_reconciler_api', 'USAGE, CREATE')
  INTO ok;

  IF ok IS DISTINCT FROM true THEN
    RAISE EXCEPTION 'Unexpected Forge database privileges for role %', current_user;
  END IF;
END;
$verify$;

COMMIT;
