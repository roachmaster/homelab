-- M29 complete new catalog and narrow entrypoints; checked before M28 normalization.
DO $delivery_verify$
DECLARE expected record;actual text;relation oid;role_name text;privilege_name text;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=29 AND name='delivery_automation') THEN RAISE EXCEPTION 'Missing delivery automation migration29';END IF;
 FOR expected IN SELECT * FROM(VALUES
 ('delivery_projects','7145b29d3d8b6b2305093c32f14720cb6e54ec147f526ace5880fa8bd8ec0058'),
 ('delivery_capability_grants','afa9159f7499c95be6acca2fdb85d62c480ceb5b1b4b6c01f889df530df5e030'),
 ('delivery_lineages','e5969fab6a4d580b7e22fa75b61829482ab6e8c29225dcac5f16b7388a4b4450'),
 ('delivery_ledger','b7160a62c9067de24e5f64456677a4772825304ec3da4eeb8709b0eb5f602a79'),
 ('delivery_post_merge_links','d10df6d24970620161497b49ff60e487bb2175032a8847e7127829ab4e4440b5'),
 ('delivery_task_lineages','12c51a70990faaee793a73f34ceeb45c9d657f3fa3f2fcd31c5193762a2b002c'),
 ('delivery_project_main_heads','3b9b5f81d27ef242cdeefcba4314b2f1daaa7ea123f14a6c3b808a8696428c08'),
 ('delivery_operations','50a6af275fb86c76b25d0a2339775c98c30e9647e4d9d245898c391c36ef7ab5'))x(name,sha256) LOOP
 relation:=to_regclass('forge_agent.'||expected.name);
 SELECT encode(sha256(convert_to(jsonb_build_object(
 'table',jsonb_build_array(c.relkind,c.relpersistence,pg_get_userbyid(c.relowner),c.relrowsecurity,c.relforcerowsecurity,c.relreplident,c.reloptions),
 'acl',(SELECT jsonb_agg(item::text ORDER BY item::text) FROM unnest(coalesce(c.relacl,acldefault('r',c.relowner))) item WHERE item::text<>'muse_events=r/forge_agent_owner'),
 'columns',(SELECT jsonb_agg(jsonb_build_array(a.attnum,a.attname,a.atttypid::regtype::text,a.atttypmod,a.attnotnull,a.attidentity,a.attgenerated,a.attcollation::regcollation::text,pg_get_expr(d.adbin,d.adrelid),a.attacl) ORDER BY a.attnum) FROM pg_attribute a LEFT JOIN pg_attrdef d ON(d.adrelid,d.adnum)=(a.attrelid,a.attnum) WHERE a.attrelid=c.oid AND a.attnum>0 AND NOT a.attisdropped),
 'constraints',(SELECT jsonb_agg(jsonb_build_array(k.conname,k.contype,k.convalidated,k.condeferrable,k.condeferred,pg_get_constraintdef(k.oid,false)) ORDER BY k.conname) FROM pg_constraint k WHERE k.conrelid=c.oid),
 'indexes',(SELECT jsonb_agg(jsonb_build_array(ic.relname,i.indisvalid,i.indisready,pg_get_indexdef(i.indexrelid)) ORDER BY ic.relname) FROM pg_index i JOIN pg_class ic ON ic.oid=i.indexrelid WHERE i.indrelid=c.oid),
 'triggers',(SELECT jsonb_agg(jsonb_build_array(t.tgname,t.tgenabled,pg_get_triggerdef(t.oid,false)) ORDER BY t.tgname) FROM pg_trigger t WHERE t.tgrelid=c.oid AND NOT t.tgisinternal)
 )::text,'UTF8')),'hex') INTO actual FROM pg_class c WHERE c.oid=relation;
 IF actual IS DISTINCT FROM expected.sha256 THEN RAISE EXCEPTION 'Delivery relation drift: %',expected.name;END IF;
 FOREACH role_name IN ARRAY ARRAY['forge_agent','forge_coding_worker','forge_coding_reconciler','forge_gpu_worker','public'] LOOP
 FOREACH privilege_name IN ARRAY ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER'] LOOP
 IF has_table_privilege(role_name,relation,privilege_name) OR(privilege_name IN('SELECT','INSERT','UPDATE','REFERENCES') AND has_any_column_privilege(role_name,relation,privilege_name)) THEN RAISE EXCEPTION 'Unexpected delivery table privilege: % % %',role_name,expected.name,privilege_name;END IF;
 END LOOP;END LOOP;END LOOP;
 FOR expected IN SELECT * FROM(VALUES
 ('forge_agent.delivery_actor_authorize(uuid,integer,uuid,boolean)','5d0b214d398ec896d523e483f76be1cb18ca27fab247519b206e926c6a0e4a9c'),
 ('forge_agent.delivery_canonical(jsonb)','a7dd3282f71921384ec8b1fe12da24cc3aad5c8ad8d4117d0ea98ee4cd15b75c'),
 ('forge_agent.delivery_capable(text,text)','1fa7d011b3831c4daea5b2d13a31838f9f874ded4afd30bf9b0f5fed776a896e'),
 ('forge_agent.delivery_enqueue_successors(uuid)','95f75e3edb49cf4a95f9b46ec63935be4ab41f294401495f2273a2b2b3bf13d9'),
 ('forge_agent.delivery_main_receipt_valid(jsonb,jsonb,text)','5dbff0a4fb98d3ebba780ffdf417a78ae40fcc9928af2c5131d0a2cea3009f95'),
 ('forge_agent.delivery_operation_claim(text,text)','6b19b9250bdcbeff7efe4f4f2dc5647ee7b3a245ee3c18a13b50c6abf7882a20'),
 ('forge_agent.delivery_operation_finish(uuid,integer,uuid,text,jsonb,text)','1bbba07284ea007efd99c5108247cdd48593a0aeadd4bbe885ac56d989539d37'),
 ('forge_agent.delivery_operation_lock(uuid)','a46a7ede63cb2568a36dd40367c5d713a32f58865184f16b760a2343e2a57c89'),
 ('forge_agent.delivery_operation_renew(uuid,integer,uuid)','d1b43be6fe08e70cb89d5d67c1871d65634a71220b3efb3a3fa67f3c6c17231e'),
 ('forge_agent.delivery_operation_replayed(uuid,integer,uuid,jsonb)','8332b5ea0e19f7c1bd82e86c02e380a2148e21ee56f18d4d5b19d5d744ce7c36'),
 ('forge_agent.delivery_own_cleanup(uuid,jsonb,jsonb,text)','324ea6520fae217330434c3bb933d80c1a860aec6f69bb8c12e066512144eb3d'),
 ('forge_agent.delivery_project_head(text)','7f471e7116ce8163692b4756de668015a7b7f26c3704ad760ad603e0628384a9'),
 ('forge_agent.delivery_projection(text,uuid)','537c592c65f48d665dbe096c0970ec71fe3f9b2fa47b2402d8fe28dccdc1a7f9'),
 ('forge_agent.delivery_register_post_merge(uuid)','33f8daed2d89b4aed4e9dcc0d9286da77309ef1dbe443f823d61130384473a06'),
 ('forge_agent.delivery_repair_admit(uuid,integer,uuid,text)','7387358a2471cd0a2c2398b5953b7d2857ea9ef466327bc98d823bb27adf4492'),
 ('forge_agent.delivery_repair_bind(uuid,integer,uuid,uuid,jsonb)','114ccac21314b3289fc30af5f508eb4813a4f0e0c4abad84c25cb91c8d07ac12'),
 ('forge_agent.delivery_repair_checkpoint(uuid,integer,uuid,jsonb)','b6a3b222e24d0c7a91a2648885c90028421bb61130048030ea191a8a02109c46'),
 ('forge_agent.delivery_repair_claim(text)','c6b036b4f2c140b52863db887be11a5e01a3b7b3a28f71bbe38dd1ac8ca6864f'),
 ('forge_agent.delivery_repair_finish(uuid,integer,uuid,text,jsonb,text)','c748112144fa532aff625d031917ec295312a237c2611b65cee4f685c149ab28'),
 ('forge_agent.delivery_repair_renew(uuid,integer,uuid)','07f09d1eb7ede0d61600c009c4770aeb72351dc7cf60abf2801a9ed73f672a23'),
 ('forge_agent.delivery_request_valid(jsonb,uuid,uuid,text,text,text,uuid)','2fd460345d4cd5c07d94f6bd9189fed7e1b5c1477433e3aa40a7e238fbe3cb4f'),
 ('forge_agent.delivery_scope(jsonb)','50e8abdf4ed34103b560285af4bfd5499a8a5d046bc7ff9f91f47263b1164c29'),
 ('forge_agent.delivery_sync_post_merge()','9e0bda0cd3c8c0c7d4f170a8cc88e9a17acc6444f13153ad5645219bf75875de'),
 ('forge_agent.delivery_task_scope(text,uuid)','50287b211eb767165745a559c70f63e11b37390e61b8eee631347b31c9425ed5'),
 ('forge_agent.delivery_timestamp(timestamp with time zone)','cc302af183fdb0edf29151d8e77dbe0ffb77fa9640ae8325d62811e019a6d4dd'),
 ('forge_agent.delivery_verify_granted(text)','9f8aa6c5819cfa4f12d7801dff28b80b6808669599657102ee40e596af17a7d4'),
 ('forge_coding_reconciler_api.delivery_available(text)','ca35cb336cf93cbf33b9e8fe7eceacf39247d1410063b63b2cc298cca7be1cf9'),
 ('forge_coding_reconciler_api.delivery_checkpoint(uuid,integer,uuid,jsonb)','9c23dced4fa1a310ef90a255e2ed2a656d3f278af3af53d60f746c45cdc72ce7'),
 ('forge_coding_reconciler_api.delivery_claim(text)','f32584033410fb18d2b77a9f37e987b346632361b0b34698d5c183c05d1a0f0e'),
 ('forge_coding_reconciler_api.delivery_finish(uuid,integer,uuid,text,jsonb,text)','c9261f328e407930ed7fc4aae3b24622ebb19f6cd0aa78ba300a39e4ce47db1e'),
 ('forge_coding_reconciler_api.delivery_observe(text,text,text,text,text)','53d1dc39d4431b3e7375398a81581b2a8f33636ea2a4552332d35074c98bf6d3'),
 ('forge_coding_reconciler_api.delivery_project_head(text)','a8cc601ef3ef0d4cedf44ff90457b2b3278bddd6a8a427a472605e3f33d18119'),
 ('forge_coding_reconciler_api.delivery_release_checkpoint(uuid,integer,uuid,jsonb)','8d67ad42c0643d9cb0baaedc0b359e45d5bc21b0e4d5afc1c052fc729a448d8c'),
 ('forge_coding_reconciler_api.delivery_release_claim(text)','73eb4408353cd387a57e39460a41dec606b08e5603d8ff1299244703e75779e7'),
 ('forge_coding_reconciler_api.delivery_release_finish(uuid,integer,uuid,text,jsonb,text)','5f453a6c728c8f15e19dfc95a136707ba34d30d338735606363b1a4c585e74d1'),
 ('forge_coding_reconciler_api.delivery_release_renew(uuid,integer,uuid)','be4b29a5e14b3f8a4f9469557ec32507a39198e2a85da1765a634cc538463cd6'),
 ('forge_coding_reconciler_api.delivery_release_start_allowed(uuid,integer,uuid)','7670d9cb3e34179707edd9d39ce06bf12e0041fa9a327ca9312be0d3116baae4'),
 ('forge_coding_reconciler_api.delivery_renew(uuid,integer,uuid)','59d776933079e4a302c1c739a09931310e09b86d520ed2009c4feccdb5688e24'),
 ('forge_coding_reconciler_api.delivery_start_allowed(uuid,integer,uuid)','fbfb727c9ba01707095dcb2ff85f063b670b27b777b660e0fe651b24097759a1'))x(signature,sha256) LOOP
 SELECT encode(sha256(convert_to(jsonb_build_array(pg_get_functiondef(p.oid),pg_get_userbyid(p.proowner),p.proacl)::text,'UTF8')),'hex') INTO actual FROM pg_proc p WHERE p.oid=to_regprocedure(expected.signature);
 IF actual IS DISTINCT FROM expected.sha256 THEN RAISE EXCEPTION 'Delivery function drift: %',expected.signature;END IF;
 END LOOP;
 SELECT encode(sha256(convert_to(pg_get_triggerdef(t.oid,false),'UTF8')),'hex') INTO actual FROM pg_trigger t WHERE t.tgrelid='forge_agent.post_merge_verifications'::regclass AND t.tgname='delivery_post_merge_sync' AND t.tgenabled='O' AND NOT t.tgisinternal;
 IF actual IS DISTINCT FROM '90f0e66aece57e2496201bf7197a47e47a6602f38c97ab9bff75f633d2335afc' THEN RAISE EXCEPTION 'Delivery source signal drift';END IF;
END;
$delivery_verify$;
