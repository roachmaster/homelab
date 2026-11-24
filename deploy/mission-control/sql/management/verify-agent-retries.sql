-- M30 validates only the explicit allowance expansion; predecessor authority remains exact.
DO $verify$
DECLARE expected record;actual text;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=30) THEN RETURN;END IF;
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=30 AND name='agent_wide_retry_policy') THEN RAISE EXCEPTION 'Shared retry migration identity mismatch';END IF;
 FOR expected IN SELECT * FROM(VALUES
 ('forge_agent.coding_workflows','coding_workflows_plan_revisions_check','CHECK (((plan_revisions >= 0) AND (plan_revisions <= 20)))'),
 ('forge_external_workflow.workflows','workflows_plan_revisions_check','CHECK (((plan_revisions >= 0) AND (plan_revisions <= 20)))'),
 ('forge_external_workflow.workflows','workflows_code_revisions_check','CHECK (((code_revisions >= 0) AND (code_revisions <= 20)))'),
 ('forge_agent.coding_review_policy','coding_review_policy_max_rounds_check','CHECK (((max_rounds >= 0) AND (max_rounds <= 20)))'),
 ('forge_agent.coding_review_policy','coding_review_policy_max_stagnant_rounds_check','CHECK (((max_stagnant_rounds >= 1) AND (max_stagnant_rounds <= 20)))'),
 ('forge_agent.coding_review_policy','coding_review_policy_max_infrastructure_retries_check','CHECK (((max_infrastructure_retries >= 0) AND (max_infrastructure_retries <= 20)))'),
 ('forge_agent.coding_review_budget','coding_review_budget_repair_rounds_check','CHECK (((repair_rounds >= 0) AND (repair_rounds <= 20)))'),
 ('forge_agent.coding_review_budget','coding_review_budget_stagnant_rounds_check','CHECK (((stagnant_rounds >= 0) AND (stagnant_rounds <= 20)))')
 ) checks(relation,name,definition) LOOP
 IF EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=31) THEN
  SELECT forge_agent._retry_legacy_constraint(conrelid,conname,pg_get_constraintdef(oid)) INTO actual FROM pg_constraint WHERE conrelid=expected.relation::regclass AND conname=expected.name;
 ELSE
  SELECT pg_get_constraintdef(oid) INTO actual FROM pg_constraint WHERE conrelid=expected.relation::regclass AND conname=expected.name;
 END IF;
  IF actual IS DISTINCT FROM expected.definition THEN RAISE EXCEPTION 'Shared retry constraint drift: %.%',expected.relation,expected.name;END IF;
 END LOOP;
 FOR expected IN SELECT * FROM(VALUES
 ('forge_agent.coding_review_policy','max_rounds','20'),('forge_agent.coding_review_policy','max_stagnant_rounds','20'),
 ('forge_agent.coding_review_policy','max_infrastructure_retries','20'),
 ('forge_agent.coding_review_budget','policy',format('%L::jsonb','{"version":"forge-repair-v1","max_rounds":20,"max_stagnant_rounds":20,"max_infrastructure_retries":20,"required_hosted_checks":["check","pr-fast-gate","real-browser"]}'::jsonb::text))
 ) defaults(relation,name,definition) LOOP
  SELECT pg_get_expr(d.adbin,d.adrelid) INTO actual FROM pg_attribute a JOIN pg_attrdef d ON(d.adrelid,d.adnum)=(a.attrelid,a.attnum)
  WHERE a.attrelid=expected.relation::regclass AND a.attname=expected.name;
  IF actual IS DISTINCT FROM expected.definition THEN RAISE EXCEPTION 'Shared retry default drift: %.%',expected.relation,expected.name;END IF;
 END LOOP;
 IF NOT EXISTS(SELECT FROM pg_proc WHERE oid='forge_agent.amend_agent_retry_policy(text,uuid,jsonb)'::regprocedure
  AND proowner='forge_agent_owner'::regrole AND prosecdef AND provolatile='v'
  AND proconfig=ARRAY['search_path=pg_catalog, pg_temp'] AND prorettype='jsonb'::regtype
  AND proacl=ARRAY['forge_agent_owner=X/forge_agent_owner','forge_agent=X/forge_agent_owner']::aclitem[]
  AND encode(sha256(convert_to(prosrc,'UTF8')),'hex')='1f066aa435653adda101786580e078739652e8c9fc5e87addb0ef32ed4582f3b')
 THEN RAISE EXCEPTION 'Shared retry amendment function drift';END IF;
END;$verify$;
