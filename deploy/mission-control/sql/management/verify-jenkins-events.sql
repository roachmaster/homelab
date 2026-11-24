-- Strict M27 catalog and effective privilege checks; no repairs or authority grants.
DO $jenkins_verify$
DECLARE expected record;actual text;relation oid;runtime_privilege text;runtime_allowed text[];worker text;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=27 AND name='jenkins_events') THEN
  RAISE EXCEPTION 'Missing Jenkins events migration27';END IF;
 FOR expected IN SELECT * FROM(VALUES
  ('jenkins_events','d0838fc8f42db1a372e6f4b78d89ab2e8f7a5f5010a57c083afde39be10cc26d'),('jenkins_event_deliveries','29d75bf9cc955afc14dd7f7f74fe5bbf8a4fa680eb415b3e0ab1fe71b3bdc17d'),('jenkins_observations','0e9f4900e964abbe1661720dba214fae0a1af4969df36d72dd795b11aa1e217f')
 )x(name,sha256) LOOP
  relation:=to_regclass('forge_agent.'||expected.name);
  IF relation IS NULL THEN RAISE EXCEPTION 'Missing Jenkins relation %',expected.name;END IF;
  SELECT encode(sha256(convert_to(jsonb_build_object(
   'table',jsonb_build_array(c.relkind,c.relpersistence,pg_get_userbyid(c.relowner),c.relrowsecurity,c.relforcerowsecurity,c.relreplident,c.reloptions),
   'acl',(SELECT jsonb_agg(item::text ORDER BY item::text) FROM unnest(coalesce(c.relacl,acldefault('r',c.relowner))) item
    WHERE NOT EXISTS(SELECT FROM aclexplode(ARRAY[item]) a WHERE a.grantee=to_regrole('muse_events')
     AND a.grantor='forge_agent_owner'::regrole AND a.privilege_type='SELECT' AND NOT a.is_grantable)),
   'columns',(SELECT jsonb_agg(jsonb_build_array(a.attnum,a.attname,a.atttypid::regtype::text,a.atttypmod,a.attnotnull,
      a.attidentity,a.attgenerated,a.attcollation::regcollation::text,pg_get_expr(d.adbin,d.adrelid),a.attacl) ORDER BY a.attnum)
     FROM pg_attribute a LEFT JOIN pg_attrdef d ON(d.adrelid,d.adnum)=(a.attrelid,a.attnum) WHERE a.attrelid=c.oid AND a.attnum>0 AND NOT a.attisdropped),
   'constraints',(SELECT jsonb_agg(jsonb_build_array(k.conname,k.contype,k.convalidated,k.condeferrable,k.condeferred,
      pg_get_constraintdef(k.oid,false)) ORDER BY k.conname) FROM pg_constraint k WHERE k.conrelid=c.oid),
   'indexes',(SELECT jsonb_agg(jsonb_build_array(ic.relname,i.indisvalid,i.indisready,pg_get_indexdef(i.indexrelid)) ORDER BY ic.relname)
     FROM pg_index i JOIN pg_class ic ON ic.oid=i.indexrelid WHERE i.indrelid=c.oid),
   'triggers',(SELECT jsonb_agg(jsonb_build_array(t.tgname,t.tgenabled,pg_get_triggerdef(t.oid,false)) ORDER BY t.tgname)
     FROM pg_trigger t WHERE t.tgrelid=c.oid AND NOT t.tgisinternal)
  )::text,'UTF8')),'hex') INTO actual FROM pg_class c WHERE c.oid=relation;
  IF actual IS DISTINCT FROM expected.sha256 THEN
   RAISE EXCEPTION 'Jenkins relation catalog drift: %',expected.name;END IF;
  runtime_allowed:=CASE WHEN expected.name='jenkins_events' THEN ARRAY['SELECT','INSERT'] ELSE ARRAY['SELECT','INSERT','UPDATE'] END;
  FOREACH runtime_privilege IN ARRAY ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER'] LOOP
   IF has_table_privilege('forge_agent',relation,runtime_privilege) IS DISTINCT FROM(runtime_privilege=ANY(runtime_allowed))
    OR(runtime_privilege IN('SELECT','INSERT','UPDATE','REFERENCES') AND NOT runtime_privilege=ANY(runtime_allowed)
     AND has_any_column_privilege('forge_agent',relation,runtime_privilege))
    OR has_table_privilege('public',relation,runtime_privilege)
    OR(runtime_privilege IN('SELECT','INSERT','UPDATE','REFERENCES') AND has_any_column_privilege('public',relation,runtime_privilege)) THEN
    RAISE EXCEPTION 'Unexpected Jenkins runtime/PUBLIC privilege on %',expected.name;END IF;
   FOREACH worker IN ARRAY ARRAY['forge_gpu_worker','forge_coding_worker','forge_coding_reconciler'] LOOP
    IF has_table_privilege(worker,relation,runtime_privilege)
     OR(runtime_privilege IN('SELECT','INSERT','UPDATE','REFERENCES') AND has_any_column_privilege(worker,relation,runtime_privilege)) THEN
     RAISE EXCEPTION 'Native worker has Jenkins observation table access: %',worker;END IF;
   END LOOP;
  END LOOP;
 END LOOP;
 FOR expected IN SELECT * FROM(VALUES
  ('forge_agent.jenkins_events_immutable()','1e338dc1d03aa75034f345ad6aebed6d64dc8ba66e12eab22b299d812bd536fb'),
  ('forge_agent.jenkins_event_deliveries_guard()','8c48e963fc0004016f5c1e2efa0d0dcff9fcdcd7a6e4f8d1cfcdd1038313f70f'),
  ('forge_agent.jenkins_observations_guard()','3dd7cff5b9eaa6078cefbf2d0167dbaaf0bf901f8fa032d5d5826fd059fa945d')
 )x(signature,sha256) LOOP
  SELECT encode(sha256(convert_to(jsonb_build_array(pg_get_functiondef(p.oid),pg_get_userbyid(p.proowner),p.proacl)::text,'UTF8')),'hex')
  INTO actual FROM pg_proc p WHERE p.oid=expected.signature::regprocedure;
  IF actual IS DISTINCT FROM expected.sha256 THEN
   RAISE EXCEPTION 'Jenkins trigger function drift: %',expected.signature;END IF;
 END LOOP;
END;
$jenkins_verify$;
