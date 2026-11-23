-- M28 exact additions are checked before historical catalog normalization.
DO $post_merge_verify$
DECLARE expected record;actual text;relation oid;role_name text;privilege_name text;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=28 AND name='post_merge_verification') THEN RAISE EXCEPTION 'Missing post merge verification migration28';END IF;
 FOR expected IN SELECT * FROM(VALUES
  ('post_merge_verifications','146cebfbdfaff0a8cce5e55650229beabf74f05fb43905de7583e5a3e24a549e'),
  ('post_merge_verification_grants','a6aa1431e2a3f9795770347f1bf2f4445ee48bfa58f8a5def44ba2a4fec95352'),
  ('post_merge_remediations','a32a3f6275bb23e36a1c31400876477bad87c898c57549b7797154400056fc95'))x(name,sha256) LOOP
  relation:=to_regclass('forge_agent.'||expected.name);
  SELECT encode(sha256(convert_to(jsonb_build_object(
 'table',jsonb_build_array(c.relkind,c.relpersistence,pg_get_userbyid(c.relowner),c.relrowsecurity,c.relforcerowsecurity,c.relreplident,c.reloptions),
 'acl',(SELECT jsonb_agg(item::text ORDER BY item::text) FROM unnest(coalesce(c.relacl,acldefault('r',c.relowner))) item WHERE item::text<>'muse_events=r/forge_agent_owner'),
 'columns',(SELECT jsonb_agg(jsonb_build_array(a.attnum,a.attname,a.atttypid::regtype::text,a.atttypmod,a.attnotnull,a.attidentity,a.attgenerated,a.attcollation::regcollation::text,pg_get_expr(d.adbin,d.adrelid),a.attacl) ORDER BY a.attnum) FROM pg_attribute a LEFT JOIN pg_attrdef d ON(d.adrelid,d.adnum)=(a.attrelid,a.attnum) WHERE a.attrelid=c.oid AND a.attnum>0 AND NOT a.attisdropped),
 'constraints',(SELECT jsonb_agg(jsonb_build_array(k.conname,k.contype,k.convalidated,k.condeferrable,k.condeferred,pg_get_constraintdef(k.oid,false)) ORDER BY k.conname) FROM pg_constraint k WHERE k.conrelid=c.oid),
 'indexes',(SELECT jsonb_agg(jsonb_build_array(ic.relname,i.indisvalid,i.indisready,pg_get_indexdef(i.indexrelid)) ORDER BY ic.relname) FROM pg_index i JOIN pg_class ic ON ic.oid=i.indexrelid WHERE i.indrelid=c.oid),
 'triggers',(SELECT jsonb_agg(jsonb_build_array(t.tgname,t.tgenabled,pg_get_triggerdef(t.oid,false)) ORDER BY t.tgname) FROM pg_trigger t WHERE t.tgrelid=c.oid AND NOT t.tgisinternal)
 )::text,'UTF8')),'hex') INTO actual FROM pg_class c WHERE c.oid=relation;
  IF actual IS DISTINCT FROM expected.sha256 THEN RAISE EXCEPTION 'Post merge relation drift: %',expected.name;END IF;
  FOREACH role_name IN ARRAY ARRAY['forge_agent','forge_coding_worker','forge_coding_reconciler','forge_gpu_worker','public'] LOOP
   FOREACH privilege_name IN ARRAY ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER'] LOOP
    IF has_table_privilege(role_name,relation,privilege_name) OR(privilege_name IN('SELECT','INSERT','UPDATE','REFERENCES') AND has_any_column_privilege(role_name,relation,privilege_name)) THEN RAISE EXCEPTION 'Unexpected post merge table privilege: % % %',role_name,expected.name,privilege_name;END IF;
   END LOOP;
  END LOOP;
 END LOOP;
 FOR expected IN SELECT * FROM(VALUES
  ('forge_agent.post_merge_external_signal()','2a4f7c0252d0fd91cecbf279791890a268d8cefd83eaff58f50dca0b02845bd4'),
  ('forge_agent.post_merge_lock(uuid)','083c1ed69ebbf831ce6f427fa2e313bd5b9121281f7f9b453498c225f5e6065c'),
  ('forge_agent.post_merge_native_signal()','0ba470a79a987c62a6582160566829f12a944b656da0064b52be4abd67a96308'),
  ('forge_coding_reconciler_api.post_merge_available(text)','15ecc28d8062fe532636470b9cc0e68367cf3a51fa72f7271fd41b7f20963e80'),
  ('forge_coding_reconciler_api.post_merge_checkpoint(uuid,integer,uuid,jsonb)','d323b491d94fe4ce25aac9700d5f540f3ffa5f6a40770b09935b0e8a4cd0924f'),
  ('forge_coding_reconciler_api.post_merge_claim(text)','795e96ef8be131dc41363a9391547d2845d29491cfbd0f343a447544b13f0ea8'),
  ('forge_coding_reconciler_api.post_merge_finish(uuid,integer,uuid,text,jsonb,text)','5b87ba11f334bb1e278b2827f67af96cd527ae55f250b7eea030b0a06d8fd80f'),
  ('forge_coding_reconciler_api.post_merge_renew(uuid,integer,uuid)','eb761b65856b19f1864a6711f8b8274aa4ca7ac7f44d3616cbfc2508d0182038'),
  ('forge_coding_reconciler_api.post_merge_start_allowed(uuid,integer,uuid)','d120bd66e4037547a3a4c774e3498d81957601d445d168df650561913a4fea10'))x(signature,sha256) LOOP
  SELECT encode(sha256(convert_to(jsonb_build_array(pg_get_functiondef(p.oid),pg_get_userbyid(p.proowner),p.proacl)::text,'UTF8')),'hex') INTO actual FROM pg_proc p WHERE p.oid=to_regprocedure(expected.signature);
  IF actual IS DISTINCT FROM expected.sha256 THEN RAISE EXCEPTION 'Post merge function drift: %',expected.signature;END IF;
 END LOOP;
 FOR expected IN SELECT * FROM(VALUES
  ('post_merge_external_confirmed','b806b253f78aac7ab5c3ec89e9d3dc0ef82ebde21f5d1d0b80b2158d35cd15e2'),
  ('post_merge_native_confirmed','086c768d08add0fa676d146b1796bac8a3f330460d2e7e47c98d825aeae9fedf'))x(name,sha256) LOOP
  SELECT encode(sha256(convert_to(pg_get_triggerdef(t.oid,false),'UTF8')),'hex') INTO actual FROM pg_trigger t
  WHERE t.tgname=expected.name AND t.tgrelid=CASE WHEN expected.name='post_merge_native_confirmed' THEN 'forge_agent.coding_workflow_actions'::regclass ELSE 'forge_external_workflow.workflows'::regclass END AND t.tgenabled='O' AND NOT t.tgisinternal;
  IF actual IS DISTINCT FROM expected.sha256 THEN RAISE EXCEPTION 'Post merge trigger drift: %',expected.name;END IF;
 END LOOP;
END;
$post_merge_verify$;
