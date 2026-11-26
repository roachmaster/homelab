-- M23 exact AGY-only recipe guards and additive native-policy capabilities.
-- Run inside the normal read-only verifier transaction before historical snapshots.
DO $agy_unattended_verify$
DECLARE expected record;actual text;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=23 AND name='coding_agy_unattended') THEN
  RAISE EXCEPTION 'Missing unattended Antigravity migration23';
 END IF;
 FOR expected IN SELECT * FROM(VALUES
  ('forge_agent.coding_attempts','coding_attempts_agy_unattended_recipe_check','8ffbc65b39fb97945cb4da7b33837575ac3c3d64f8929c716efde4011c2bfcab'),
  ('forge_agent.coding_attempts','coding_attempts_pinned_recipe_check','983db564b1f5f6bdbc0a0d085d3b478f16413952632a56a1bc996b787572065d'),
  ('forge_agent.coding_tasks','coding_tasks_agy_unattended_recipe_check','8ffbc65b39fb97945cb4da7b33837575ac3c3d64f8929c716efde4011c2bfcab')
 )x(relation,name,sha256) LOOP
  SELECT encode(sha256(convert_to(jsonb_build_array(pg_get_constraintdef(c.oid),c.contype,c.convalidated,c.condeferrable,c.condeferred)::text,'UTF8')),'hex')
  INTO actual FROM pg_constraint c JOIN pg_class r ON r.oid=c.conrelid
  WHERE c.conrelid=expected.relation::regclass AND c.conname=expected.name AND r.relowner='forge_agent_owner'::regrole;
  IF actual IS DISTINCT FROM expected.sha256 THEN RAISE EXCEPTION 'Unattended AGY constraint drift: %.%',expected.relation,expected.name;END IF;
 END LOOP;
 FOR expected IN SELECT * FROM(VALUES
  ('forge_agent.coding_make_feedback(uuid,uuid,text,text,jsonb)','31fe8ac18e8e5b8a38b8bd7534e7d783327e5d4b785702098c9f89f45248cca3'),
  ('forge_coding_reconciler_api.publication_claim(text)','0636bf07b9355e7019de93164c2e913a341690cc80db37357d0e44e3cec9c8ac'),
  ('forge_coding_reconciler_api.review_claim(text)','bea9f804d670dc5c905b85dc60d660853d811083851b7fd7a1adae0348d9415e'),
  ('forge_coding_worker_api.claim(text,uuid,text[])','25030544388d1d4fe200b3d68542375073c2f960279a759fcaf24bf5fd0ac5af')
 )x(signature,sha256) LOOP
  IF EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=40) THEN
  SELECT encode(sha256(convert_to(jsonb_build_array(replace(pg_get_functiondef(p.oid),p.prosrc,forge_agent._retry_legacy_source(p.oid,forge_agent._coding_author_handoff_legacy_source(p.oid,p.prosrc))),pg_get_userbyid(p.proowner),p.proacl)::text,'UTF8')),'hex')
  INTO actual FROM pg_proc p WHERE p.oid=expected.signature::regprocedure;
  ELSIF EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=31) THEN
  SELECT encode(sha256(convert_to(jsonb_build_array(replace(pg_get_functiondef(p.oid),p.prosrc,forge_agent._retry_legacy_source(p.oid,p.prosrc)),pg_get_userbyid(p.proowner),p.proacl)::text,'UTF8')),'hex')
  INTO actual FROM pg_proc p WHERE p.oid=expected.signature::regprocedure;
  ELSE
  SELECT encode(sha256(convert_to(jsonb_build_array(pg_get_functiondef(p.oid),pg_get_userbyid(p.proowner),p.proacl)::text,'UTF8')),'hex')
  INTO actual FROM pg_proc p WHERE p.oid=expected.signature::regprocedure;
  END IF;
  IF actual IS DISTINCT FROM expected.sha256 THEN RAISE EXCEPTION 'Unattended AGY function drift: %',expected.signature;END IF;
 END LOOP;
END;
$agy_unattended_verify$;
