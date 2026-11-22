-- M24 exact narrow source/activity APIs; no source or workflow authority changes.
DO $native_jenkins_verify$
DECLARE expected record;actual text;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=24 AND name='native_jenkins_context') THEN
  RAISE EXCEPTION 'Missing native Jenkins context migration24';
 END IF;
 FOR expected IN SELECT * FROM(VALUES
  ('forge_coding_reconciler_api.native_ci_context(uuid,uuid,text,uuid,uuid)','1b429704dea198a337af14c77c262120404f5e5ed82e6c7945b08fa9bfe87a73'),
  ('forge_coding_reconciler_api.native_ci_status(uuid,text,uuid,uuid)','dbe10655ab46388a8d0d138093f31b509b82db67e20c0e1c11f297314ce5c2f9')
 )x(signature,sha256) LOOP
  SELECT encode(sha256(convert_to(jsonb_build_array(pg_get_functiondef(p.oid),pg_get_userbyid(p.proowner),p.proacl)::text,'UTF8')),'hex')
  INTO actual FROM pg_proc p WHERE p.oid=expected.signature::regprocedure;
  IF actual IS DISTINCT FROM expected.sha256 THEN RAISE EXCEPTION 'Native Jenkins context function drift: %',expected.signature;END IF;
 END LOOP;
END;
$native_jenkins_verify$;
