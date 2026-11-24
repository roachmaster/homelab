-- Exact additive M33 authority is verified before predecessor normalization.
-- M35 (event-stream peer-review diagnostics) legitimately changes
-- coding_workflow_reviews (diagnostics + supersedes columns, partial
-- source-binding unique index, 'superseded' status value), so the table
-- fingerprint is gated on its presence; the M33-only hash stays valid
-- for a schema without M35.
DO $superseding_review_verify$
DECLARE actual text;expected_sha text;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=33) THEN RETURN;END IF;
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=33 AND name='superseding_reviews') THEN RAISE EXCEPTION 'Superseding review migration identity drift';END IF;
 IF EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=35 AND name='event_stream_peer_review_diagnostics') THEN expected_sha:='9589f619da6ee0b4837f98b797f25a16cc27caf11c71bf59be0905fadc810e12';ELSE expected_sha:='148ba97312b2a5feb7de6afd14aca19fa6a72fa65c58af429c6845df2198f80f';END IF;
 SELECT encode(sha256(convert_to(jsonb_build_object('table',jsonb_build_array(c.relkind,c.relpersistence,pg_get_userbyid(c.relowner),c.relrowsecurity,c.relforcerowsecurity,c.relreplident,c.reloptions),
 'acl',(SELECT jsonb_agg(a::text ORDER BY n) FROM unnest(c.relacl) WITH ORDINALITY entries(a,n) WHERE a::text<>'muse_events=r/forge_agent_owner'),
 'columns',(SELECT jsonb_agg(jsonb_build_array(a.attnum,a.attname,format_type(a.atttypid,a.atttypmod),a.attnotnull,a.attidentity,a.attgenerated,a.attcollation::regcollation::text,pg_get_expr(d.adbin,d.adrelid),a.attacl) ORDER BY a.attnum) FROM pg_attribute a LEFT JOIN pg_attrdef d ON(d.adrelid,d.adnum)=(a.attrelid,a.attnum) WHERE a.attrelid=c.oid AND a.attnum>0 AND NOT a.attisdropped),
 'constraints',(SELECT jsonb_agg(jsonb_build_array(k.conname,k.contype,k.convalidated,k.condeferrable,k.condeferred,pg_get_constraintdef(k.oid,false)) ORDER BY k.conname) FROM pg_constraint k WHERE k.conrelid=c.oid),
 'indexes',(SELECT jsonb_agg(jsonb_build_array(ic.relname,i.indisvalid,i.indisready,pg_get_indexdef(i.indexrelid)) ORDER BY ic.relname) FROM pg_index i JOIN pg_class ic ON ic.oid=i.indexrelid WHERE i.indrelid=c.oid),
 'triggers',(SELECT jsonb_agg(jsonb_build_array(t.tgname,t.tgenabled,pg_get_triggerdef(t.oid,false)) ORDER BY t.tgname) FROM pg_trigger t WHERE t.tgrelid=c.oid AND NOT t.tgisinternal))::text,'UTF8')),'hex') INTO actual FROM pg_class c WHERE c.oid='forge_agent.coding_workflow_reviews'::regclass;
 IF actual IS DISTINCT FROM expected_sha THEN RAISE EXCEPTION 'Superseding review table or guard drift';END IF;
 SELECT encode(sha256(convert_to(jsonb_build_array(pg_get_functiondef(p.oid),pg_get_userbyid(p.proowner),p.proacl)::text,'UTF8')),'hex') INTO actual FROM pg_proc p WHERE p.oid=to_regprocedure('forge_agent.coding_review_supersession_guard()');
 IF actual IS DISTINCT FROM 'e8c86156f6736ef97b926bd72d356d844ae2d901bf16652a99a60be0408d4bc7' THEN RAISE EXCEPTION 'Superseding review guard authority drift';END IF;
END;$superseding_review_verify$;
