\set ON_ERROR_STOP on

BEGIN READ ONLY;
SET LOCAL search_path = pg_catalog;

DO $verify$
DECLARE
  ok boolean;cutover boolean;
BEGIN
  -- M40 revokes every direct forge_agent table write; the proxy is the
  -- only writer, so forge_agent keeps SELECT only from here on.
  -- forge_agent cannot SELECT schema_migrations, so the stage signal
  -- is its own INSERT privilege on forge_external_workflow.workflows:
  -- granted pre-cutover, revoked at cutover, and outside this file's
  -- spec, so the probe cannot mask an assertion below. NULL (table
  -- absent pre-M22) degrades to the pre-cutover expectations.
  -- Likewise the M39 probe below is forge_agent's SELECT on
  -- coding_retry_events: granted by the M39 proxy read surface, kept
  -- at cutover, and outside this file's spec, so it cannot mask the
  -- coding_worker_grants assertion it gates.
  SELECT NOT has_table_privilege(current_user, 'forge_external_workflow.workflows', 'INSERT') INTO cutover;
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
      CROSS JOIN unnest(CASE WHEN cutover THEN ARRAY['SELECT'] ELSE ARRAY['SELECT', 'INSERT', 'UPDATE', 'DELETE'] END) AS privilege
    )
    AND (
      SELECT bool_and(has_table_privilege(current_user, 'forge_agent.' || relation, privilege))
      FROM unnest(ARRAY['ide_workspace_history', 'ide_chats']) AS relation
      CROSS JOIN unnest(CASE WHEN cutover THEN ARRAY['SELECT'] ELSE ARRAY['SELECT', 'INSERT', 'UPDATE'] END) AS privilege
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
      FROM (SELECT v.relation,
              CASE WHEN v.relation='coding_worker_grants' AND has_table_privilege(current_user, 'forge_agent.coding_retry_events', 'SELECT') THEN ARRAY['SELECT']
                   WHEN cutover AND v.allowed <> ARRAY[]::text[] THEN ARRAY['SELECT'] ELSE v.allowed END AS allowed
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
          ('coding_review_policy',ARRAY['SELECT','INSERT','UPDATE']),
          ('coding_reviews',ARRAY['SELECT','INSERT','UPDATE']),
          ('coding_feedback',ARRAY['SELECT','INSERT']),
          ('coding_review_budget',ARRAY['SELECT','INSERT']),
          ('coding_hosted_checks',ARRAY['SELECT']),
          ('coding_workflows',ARRAY['SELECT','INSERT','UPDATE']),
          ('coding_workflow_reviews',ARRAY['SELECT','INSERT','UPDATE']),
          ('coding_workflow_actions',ARRAY['SELECT','INSERT','UPDATE']),
          ('coding_worker_grants',            ARRAY[]::text[]),
          ('coding_worker_readiness',         ARRAY['SELECT']),
          ('coding_attempts',                 ARRAY['SELECT']),
          ('coding_result_chunks',            ARRAY['SELECT']),
          ('coding_reconciliation_receipts',  ARRAY['SELECT']),
          ('coding_provider_usage',           ARRAY['SELECT']),
          ('coding_provider_usage_identities', ARRAY[]::text[]),
          ('coding_usage_refresh_requests',   ARRAY['SELECT', 'INSERT'])
      ) AS v(relation, allowed)) AS spec(relation, allowed)
      CROSS JOIN unnest(ARRAY[
        'SELECT', 'INSERT', 'UPDATE', 'DELETE', 'TRUNCATE', 'REFERENCES', 'TRIGGER'
      ]) AS privilege
    )
    AND has_function_privilege(current_user, 'forge_agent.owner_continuation_capability()', 'EXECUTE')
    AND NOT has_schema_privilege(current_user, 'forge_agent_worker_api', 'USAGE, CREATE')
    AND NOT has_schema_privilege(current_user, 'forge_coding_worker_api', 'USAGE, CREATE')
    AND NOT has_schema_privilege(current_user, 'forge_coding_reconciler_api', 'USAGE, CREATE')
  INTO ok;

  IF ok IS DISTINCT FROM true THEN
    RAISE EXCEPTION 'Unexpected Forge database privileges for role %', current_user;
  END IF;
END;
$verify$;

-- M27 is optional for a pre-upgrade backup check; exact privileges after admission.
DO $jenkins_runtime$
DECLARE spec record;privilege_name text;cutover boolean;
BEGIN
 -- M40 revokes every direct forge_agent table write; the proxy is the
 -- only writer, so forge_agent keeps SELECT only from here on.
 -- Same canary as above: forge_agent cannot SELECT schema_migrations,
 -- so the stage signal is its own INSERT privilege on
 -- forge_external_workflow.workflows (outside this file's spec).
 SELECT NOT has_table_privilege(current_user, 'forge_external_workflow.workflows', 'INSERT') INTO cutover;
 IF to_regclass('forge_agent.jenkins_events') IS NULL THEN RETURN;END IF;
 FOR spec IN SELECT * FROM(VALUES
  ('jenkins_events',ARRAY['SELECT','INSERT']),
  ('jenkins_event_deliveries',ARRAY['SELECT','INSERT','UPDATE']),
  ('jenkins_observations',ARRAY['SELECT','INSERT','UPDATE'])
 )x(name,allowed) LOOP
  IF cutover THEN spec.allowed:=ARRAY['SELECT']; END IF;
  FOREACH privilege_name IN ARRAY ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER'] LOOP
   IF has_table_privilege(current_user,'forge_agent.'||spec.name,privilege_name) IS DISTINCT FROM(privilege_name=ANY(spec.allowed))
    OR(privilege_name IN('SELECT','INSERT','UPDATE','REFERENCES') AND NOT privilege_name=ANY(spec.allowed)
     AND has_any_column_privilege(current_user,'forge_agent.'||spec.name,privilege_name)) THEN
    RAISE EXCEPTION 'Unexpected Jenkins runtime privilege: %',spec.name;END IF;
  END LOOP;
 END LOOP;
END;
$jenkins_runtime$;

COMMIT;
