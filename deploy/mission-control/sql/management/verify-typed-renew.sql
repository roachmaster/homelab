-- M45 typed lease renewal and progress-aware deadlines: exact structure, owner and privileges. Read only.
DO $renew_verify$
DECLARE actual jsonb;privilege_name text;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=45) THEN RETURN;END IF;
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=45 AND name='typed_renew_progress_deadline') THEN RAISE EXCEPTION 'Typed renew migration identity drift';END IF;
 SELECT jsonb_build_object('kind',c.relkind,'persistence',c.relpersistence,'row_security',c.relrowsecurity,'force_row_security',c.relforcerowsecurity,'owner',pg_get_userbyid(c.relowner),
  'columns',(SELECT jsonb_agg(jsonb_build_array(a.attname,format_type(a.atttypid,a.atttypmod),a.attnotnull,pg_get_expr(d.adbin,d.adrelid)) ORDER BY a.attnum) FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum WHERE a.attrelid=c.oid AND a.attnum>0 AND NOT a.attisdropped),
  'constraints',(SELECT coalesce(jsonb_agg(jsonb_build_array(x.conname,x.contype,pg_get_constraintdef(x.oid),x.convalidated,x.condeferrable,x.condeferred) ORDER BY x.conname),'[]') FROM pg_constraint x WHERE x.conrelid=c.oid),
  'indexes',(SELECT coalesce(jsonb_agg(jsonb_build_array(i.relname,pg_get_indexdef(i.oid),x.indisvalid,x.indisready) ORDER BY i.relname),'[]') FROM pg_index x JOIN pg_class i ON i.oid=x.indexrelid WHERE x.indrelid=c.oid),
  'triggers',(SELECT coalesce(jsonb_agg(jsonb_build_array(t.tgname,pg_get_triggerdef(t.oid),t.tgenabled) ORDER BY t.tgname),'[]') FROM pg_trigger t WHERE t.tgrelid=c.oid AND NOT t.tgisinternal))
 INTO actual FROM pg_class c WHERE c.relnamespace='forge_agent'::regnamespace AND c.relname='coding_attempt_deadline_extensions';
 IF actual IS DISTINCT FROM $catalog${"kind": "r", "owner": "forge_agent_owner", "columns": [["owner_id", "text", true, null], ["project_alias", "text", true, null], ["task_id", "uuid", true, null], ["attempt_id", "uuid", true, null], ["extension_no", "integer", true, null], ["previous_deadline", "timestamp with time zone", true, null], ["new_deadline", "timestamp with time zone", true, null], ["signal", "text", true, null], ["extended_at", "timestamp with time zone", true, "clock_timestamp()"]], "indexes": [["coding_attempt_deadline_extensions_pkey", "CREATE UNIQUE INDEX coding_attempt_deadline_extensions_pkey ON forge_agent.coding_attempt_deadline_extensions USING btree (attempt_id, extension_no)", true, true]], "triggers": [], "constraints": [["coding_attempt_deadline_exten_owner_id_project_alias_task__fkey", "f", "FOREIGN KEY (owner_id, project_alias, task_id) REFERENCES forge_agent.coding_tasks(owner_id, project_alias, id)", true, false, false], ["coding_attempt_deadline_extensions_check", "c", "CHECK ((new_deadline > previous_deadline))", true, false, false], ["coding_attempt_deadline_extensions_extension_no_check", "c", "CHECK (((extension_no >= 1) AND (extension_no <= 64)))", true, false, false], ["coding_attempt_deadline_extensions_pkey", "p", "PRIMARY KEY (attempt_id, extension_no)", true, false, false], ["coding_attempt_deadline_extensions_signal_check", "c", "CHECK ((signal = ANY (ARRAY['progress_report'::text, 'checkpoint'::text, 'output'::text])))", true, false, false]], "persistence": "p", "row_security": false, "force_row_security": false}$catalog$::jsonb THEN RAISE EXCEPTION 'Deadline extension table drift';END IF;
 FOREACH privilege_name IN ARRAY ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER'] LOOP
  IF has_table_privilege('forge_agent','forge_agent.coding_attempt_deadline_extensions',privilege_name) IS DISTINCT FROM (privilege_name='SELECT') THEN RAISE EXCEPTION 'Deadline extension runner privilege drift';END IF;
  IF has_table_privilege('forge_coding_worker','forge_agent.coding_attempt_deadline_extensions',privilege_name) THEN RAISE EXCEPTION 'Deadline extension worker privilege drift';END IF;
 END LOOP;
 IF EXISTS(SELECT FROM pg_class c,LATERAL aclexplode(c.relacl) a WHERE c.relnamespace='forge_agent'::regnamespace AND c.relname='coding_attempt_deadline_extensions'
  AND (a.grantee=0 OR a.is_grantable OR a.grantor<>'forge_agent_owner'::regrole
   OR (a.grantee NOT IN('forge_agent_owner'::regrole,'forge_agent'::regrole) AND NOT(pg_get_userbyid(a.grantee)='muse_events' AND a.privilege_type='SELECT')))) THEN
  RAISE EXCEPTION 'Deadline extension ACL drift';END IF;
 SELECT to_jsonb(jsonb_build_array(pg_get_userbyid(p.proowner),p.prosecdef,p.proconfig,ARRAY(SELECT unnest(p.proacl::text[]) ORDER BY 1))) INTO actual FROM pg_proc p WHERE p.oid=to_regprocedure('forge_coding_worker_api.renew_status(uuid,uuid,text)');
 IF actual IS DISTINCT FROM '["forge_agent_owner", true, ["search_path=pg_catalog, pg_temp"], ["forge_agent_owner=X/forge_agent_owner", "forge_coding_worker=X/forge_agent_owner"]]'::jsonb THEN RAISE EXCEPTION 'Typed renew function drift';END IF;
END;
$renew_verify$;
