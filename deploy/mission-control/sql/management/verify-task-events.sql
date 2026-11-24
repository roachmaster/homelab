-- M21 trace additions are exact; historical catalogs are checked separately.
DO $trace_verify$
DECLARE e record;p record;index_record record;actual jsonb;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=21 AND name='coding_event_trace') THEN RAISE EXCEPTION 'Missing task event trace migration21';END IF;
 FOR e IN SELECT * FROM(VALUES
 ('coding_tasks','correlation_id','gen_random_uuid()'::text),
 ('coding_events','event_id','gen_random_uuid()'::text),
 ('coding_events','correlation_id',NULL::text)) AS fields(relation,name,expression) LOOP
 IF NOT EXISTS(SELECT FROM pg_attribute a LEFT JOIN pg_attrdef d ON(d.adrelid,d.adnum)=(a.attrelid,a.attnum)
 WHERE a.attrelid=('forge_agent.'||e.relation)::regclass AND a.attname=e.name AND a.atttypid='uuid'::regtype
 AND a.attnotnull AND NOT a.attisdropped AND a.attidentity='' AND a.attgenerated='' AND a.attcollation=0
 AND pg_get_expr(d.adbin,d.adrelid) IS NOT DISTINCT FROM e.expression) THEN RAISE EXCEPTION 'Trace column drift: %.%',e.relation,e.name;END IF;
 END LOOP;
 FOR e IN SELECT * FROM(VALUES
 ('coding_task_trace_immutable','e24ca24598746e114d0cf9e679ea155c2cf2042ba7ca04ccd8f34ce90df7f4d6'),('coding_event_trace_identity','f6d8b251b18f9c45f1be37ab2982a79f5606a5ec224c429eb10d9f6baa26f733')) AS functions(name,digest) LOOP
 SELECT p0.*,l.lanname INTO p FROM pg_proc p0 JOIN pg_language l ON l.oid=p0.prolang WHERE p0.oid=('forge_agent.'||e.name||'()')::regprocedure;
 IF NOT FOUND OR p.proowner<>'forge_agent_owner'::regrole OR p.lanname<>'plpgsql' OR p.prokind<>'f'
 OR NOT p.prosecdef OR p.proisstrict OR p.proleakproof OR p.proretset OR p.prorettype<>'trigger'::regtype
 OR p.pronargs<>0 OR p.pronargdefaults<>0 OR p.proargnames IS NOT NULL OR p.proallargtypes IS NOT NULL
 OR p.provolatile<>'v' OR p.proparallel<>'u' OR p.proconfig IS DISTINCT FROM ARRAY['search_path=pg_catalog, pg_temp']
 OR p.proacl IS DISTINCT FROM ARRAY['forge_agent_owner=X/forge_agent_owner']::aclitem[]
 OR encode(sha256(convert_to(p.prosrc,'UTF8')),'hex')<>e.digest THEN RAISE EXCEPTION 'Trace function drift: %',e.name;END IF;
 END LOOP;
 FOR e IN SELECT * FROM(VALUES
 ('coding_tasks','coding_tasks_correlation_id_key','CREATE UNIQUE INDEX coding_tasks_correlation_id_key ON forge_agent.coding_tasks USING btree (correlation_id)',true),
 ('coding_events','coding_events_event_id_key','CREATE UNIQUE INDEX coding_events_event_id_key ON forge_agent.coding_events USING btree (event_id)',true),
 ('coding_events','coding_events_by_correlation_sequence','CREATE INDEX coding_events_by_correlation_sequence ON forge_agent.coding_events USING btree (correlation_id, sequence)',false)) AS indexes(relation,name,definition,is_unique) LOOP
 SELECT x.*,c.relowner INTO index_record FROM pg_index x JOIN pg_class c ON c.oid=x.indexrelid WHERE c.oid=to_regclass('forge_agent.'||e.name);
 IF NOT FOUND OR index_record.indrelid<>('forge_agent.'||e.relation)::regclass OR index_record.relowner<>'forge_agent_owner'::regrole
 OR NOT index_record.indisvalid OR NOT index_record.indisready OR index_record.indisunique IS DISTINCT FROM e.is_unique OR index_record.indisprimary OR index_record.indisexclusion
 OR index_record.indimmediate IS DISTINCT FROM true OR pg_get_indexdef(index_record.indexrelid)<>e.definition THEN RAISE EXCEPTION 'Trace index drift: %',e.name;END IF;
 END LOOP;
 FOR e IN SELECT * FROM(VALUES
 ('coding_tasks','coding_tasks_correlation_id_key','UNIQUE (correlation_id)'),
 ('coding_events','coding_events_event_id_key','UNIQUE (event_id)')) AS constraints(relation,name,definition) LOOP
 IF NOT EXISTS(SELECT FROM pg_constraint c WHERE c.conrelid=('forge_agent.'||e.relation)::regclass AND c.conname=e.name
 AND c.contype='u' AND c.convalidated AND NOT c.condeferrable AND NOT c.condeferred AND pg_get_constraintdef(c.oid)=e.definition) THEN RAISE EXCEPTION 'Trace constraint drift: %',e.name;END IF;
 END LOOP;
 FOR e IN SELECT * FROM(VALUES
 ('coding_tasks','coding_tasks_trace_immutable','CREATE TRIGGER coding_tasks_trace_immutable BEFORE UPDATE ON forge_agent.coding_tasks FOR EACH ROW EXECUTE FUNCTION forge_agent.coding_task_trace_immutable()'),
 ('coding_events','coding_events_trace_identity','CREATE TRIGGER coding_events_trace_identity BEFORE INSERT OR UPDATE ON forge_agent.coding_events FOR EACH ROW EXECUTE FUNCTION forge_agent.coding_event_trace_identity()')) AS triggers(relation,name,definition) LOOP
 IF NOT EXISTS(SELECT FROM pg_trigger t WHERE t.tgrelid=('forge_agent.'||e.relation)::regclass AND t.tgname=e.name
 AND NOT t.tgisinternal AND t.tgenabled='O' AND pg_get_triggerdef(t.oid)=e.definition) THEN RAISE EXCEPTION 'Trace trigger drift: %',e.name;END IF;
 END LOOP;
SELECT jsonb_build_object('kind',c.relkind::text,'persistence',c.relpersistence::text,'row_security',c.relrowsecurity,'force_row_security',c.relforcerowsecurity,'partition',c.relispartition,'owner',pg_get_userbyid(c.relowner),'columns',(SELECT jsonb_agg(jsonb_build_array(a.attname,format_type(a.atttypid,a.atttypmod),a.attnotnull,pg_get_expr(d.adbin,d.adrelid),a.attidentity,a.attgenerated,a.attcollation) ORDER BY a.attnum) FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum WHERE a.attrelid=c.oid AND a.attnum>0 AND NOT a.attisdropped),'constraints',(SELECT coalesce(jsonb_agg(jsonb_build_array(x.conname,x.contype,pg_get_constraintdef(x.oid),x.convalidated,x.condeferrable,x.condeferred) ORDER BY x.conname),'[]') FROM pg_constraint x WHERE x.conrelid=c.oid),'indexes',(SELECT coalesce(jsonb_agg(jsonb_build_array(i.relname,pg_get_indexdef(i.oid),x.indisvalid,x.indisready) ORDER BY i.relname),'[]') FROM pg_index x JOIN pg_class i ON i.oid=x.indexrelid WHERE x.indrelid=c.oid),'acl',to_jsonb(c.relacl),'triggers',(SELECT coalesce(jsonb_agg(jsonb_build_array(t.tgname,pg_get_triggerdef(t.oid),t.tgenabled) ORDER BY t.tgname),'[]') FROM pg_trigger t WHERE t.tgrelid=c.oid AND NOT t.tgisinternal)) INTO actual FROM pg_class c WHERE c.oid='forge_agent.coding_events'::regclass;
 actual:=jsonb_set(actual,'{acl}',coalesce((SELECT jsonb_agg(entry ORDER BY ord) FROM jsonb_array_elements(actual->'acl') WITH ORDINALITY AS acl(entry,ord) WHERE entry IS DISTINCT FROM to_jsonb('muse_events=r/forge_agent_owner'::text)),'[]'::jsonb));
-- M36's progress check is intentionally NOT VALID (legacy progress rows
-- predate the actor field and are grandfathered untouched); the catalogue
-- fingerprint below reflects convalidated=false and the ' NOT VALID'
-- suffix on coding_events_progress_check.
 IF encode(sha256(convert_to(actual::text,'UTF8')),'hex')<>'cac034e808a03ffacc8e097e703e7731dcd5ae04e15d86a41d37d5b4c043ff81' THEN RAISE EXCEPTION 'Task event catalogue drift';END IF;
 SELECT p0.*,l.lanname INTO p FROM pg_proc p0 JOIN pg_language l ON l.oid=p0.prolang WHERE p0.oid='forge_external_agent_api.event_binding(text,text,uuid)'::regprocedure;
 IF NOT FOUND OR p.proowner<>'forge_agent_owner'::regrole OR p.lanname<>'plpgsql' OR p.prokind<>'f' OR NOT p.prosecdef OR p.proisstrict OR p.proleakproof OR p.proretset
 OR p.prorettype<>'jsonb'::regtype OR p.pronargs<>3 OR p.pronargdefaults<>0 OR p.proallargtypes IS NOT NULL
 OR p.proargnames IS DISTINCT FROM ARRAY['p_owner','p_project','p_task'] OR p.provolatile<>'v' OR p.proparallel<>'u'
 OR p.proconfig IS DISTINCT FROM ARRAY['search_path=pg_catalog, pg_temp'] OR p.proacl IS DISTINCT FROM ARRAY['forge_agent_owner=X/forge_agent_owner','forge_agent=X/forge_agent_owner']::aclitem[]
 OR encode(sha256(convert_to(p.prosrc,'UTF8')),'hex')<>'e2cc6b2afc78d432d199e0a04da8929de2133f4832b1e3dce4a908dc6b2af9af' THEN RAISE EXCEPTION 'External event binding function drift';END IF;
END;
$trace_verify$;
