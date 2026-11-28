-- Pin the new metadata contract independently of historical migration checks.
DO $m53_verify$
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=53) THEN RETURN;END IF;
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=53 AND name='defect_tasks') THEN
 RAISE EXCEPTION 'M53 defect migration missing';END IF;
 IF NOT EXISTS(SELECT FROM pg_attribute WHERE attrelid='forge_agent.coding_tasks'::regclass AND attname='task_kind' AND atttypid='text'::regtype AND attnotnull AND NOT attisdropped)
 OR NOT EXISTS(SELECT FROM pg_attribute WHERE attrelid='forge_agent.coding_tasks'::regclass AND attname='defect' AND atttypid='jsonb'::regtype AND NOT attnotnull AND NOT atthasdef AND NOT attisdropped)
 OR (SELECT pg_get_expr(d.adbin,d.adrelid) FROM pg_attrdef d JOIN pg_attribute a ON(a.attrelid,a.attnum)=(d.adrelid,d.adnum) WHERE a.attrelid='forge_agent.coding_tasks'::regclass AND a.attname='task_kind') IS DISTINCT FROM '''feature''::text'
 THEN RAISE EXCEPTION 'M53 column/default drift';END IF;
 IF NOT EXISTS(SELECT FROM pg_proc WHERE oid='forge_agent.valid_defect(jsonb)'::regprocedure AND prorettype='boolean'::regtype AND NOT proretset AND NOT proisstrict
 AND proowner='forge_agent_owner'::regrole AND prosecdef=false AND provolatile='i'
 AND proconfig=ARRAY['search_path=pg_catalog, pg_temp']
 AND prolang=(SELECT oid FROM pg_language WHERE lanname='plpgsql')
 AND encode(sha256(convert_to(prosrc,'UTF8')),'hex')='80d49be6ce55c0e60449c982c9af851432ff98e1d9591548fe29509e750aa68e'
 AND ARRAY(SELECT unnest(proacl::text[]) ORDER BY 1)=ARRAY['forge_agent=X/forge_agent_owner','forge_agent_owner=X/forge_agent_owner'])
 THEN RAISE EXCEPTION 'M53 function drift: forge_agent.valid_defect(jsonb)';END IF;
 IF NOT EXISTS(SELECT FROM pg_proc WHERE oid='forge_agent.inherit_task_defect()'::regprocedure AND prorettype='trigger'::regtype AND NOT proretset AND NOT proisstrict
 AND proowner='forge_agent_owner'::regrole AND prosecdef=false AND provolatile='v'
 AND proconfig=ARRAY['search_path=pg_catalog, pg_temp']
 AND prolang=(SELECT oid FROM pg_language WHERE lanname='plpgsql')
 AND encode(sha256(convert_to(prosrc,'UTF8')),'hex')='79e4bb9cb53ece6a587eeb3286ac37210d320befe25fa901caed6feaabf76c51'
 AND ARRAY(SELECT unnest(proacl::text[]) ORDER BY 1)=ARRAY['forge_agent_owner=X/forge_agent_owner'])
 THEN RAISE EXCEPTION 'M53 function drift: forge_agent.inherit_task_defect()';END IF;
 IF NOT EXISTS(SELECT FROM pg_proc WHERE oid='forge_coding_worker_api.read_defect(uuid,uuid)'::regprocedure AND prorettype='jsonb'::regtype AND NOT proretset AND NOT proisstrict
 AND proowner='forge_agent_owner'::regrole AND prosecdef=true AND provolatile='v'
 AND proconfig=ARRAY['search_path=pg_catalog, pg_temp']
 AND prolang=(SELECT oid FROM pg_language WHERE lanname='plpgsql')
 AND encode(sha256(convert_to(prosrc,'UTF8')),'hex')='5137a1329a620124c2a3ca3c01ab2fb4c211430843b8f1e2df47c332009bce8f'
 AND ARRAY(SELECT unnest(proacl::text[]) ORDER BY 1)=ARRAY['forge_agent_owner=X/forge_agent_owner','forge_coding_worker=X/forge_agent_owner'])
 THEN RAISE EXCEPTION 'M53 function drift: forge_coding_worker_api.read_defect(uuid,uuid)';END IF;
 IF NOT EXISTS(SELECT FROM pg_trigger WHERE tgrelid='forge_agent.coding_tasks'::regclass AND tgname='coding_tasks_inherit_defect' AND tgfoid='forge_agent.inherit_task_defect()'::regprocedure AND tgtype=7 AND tgenabled='O' AND NOT tgisinternal AND pg_get_triggerdef(oid)=$trigger$CREATE TRIGGER coding_tasks_inherit_defect BEFORE INSERT ON forge_agent.coding_tasks FOR EACH ROW EXECUTE FUNCTION forge_agent.inherit_task_defect()$trigger$)
 THEN RAISE EXCEPTION 'M53 inheritance trigger drift';END IF;
 IF NOT EXISTS(SELECT FROM pg_constraint WHERE conrelid='forge_agent.coding_tasks'::regclass AND conname='coding_tasks_defect_check' AND contype='c' AND convalidated AND pg_get_constraintdef(oid)=$contract$CHECK ((((task_kind = 'feature'::text) AND (defect IS NULL)) OR ((task_kind = 'defect'::text) AND (defect IS NOT NULL) AND forge_agent.valid_defect(defect))))$contract$)
 THEN RAISE EXCEPTION 'M53 defect constraint drift';END IF;
 IF NOT EXISTS(SELECT FROM pg_index WHERE indexrelid='forge_agent.coding_tasks_defect_filter'::regclass AND indrelid='forge_agent.coding_tasks'::regclass AND indisvalid AND indisready AND indislive AND pg_get_indexdef(indexrelid)=$index$CREATE INDEX coding_tasks_defect_filter ON forge_agent.coding_tasks USING btree (owner_id, project_alias, task_kind, created_at DESC)$index$)
 THEN RAISE EXCEPTION 'M53 defect filter index drift';END IF;
END
$m53_verify$;
