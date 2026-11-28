-- M56 pins complete changed/new bodies; predecessor management pins still apply.
DO $native_v4_verify$
DECLARE item record;actual text;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=56) THEN RETURN;END IF;
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=56 AND name='native_ci_verifier') THEN RAISE EXCEPTION 'Native v4 migration identity drift';END IF;
 FOR item IN SELECT * FROM(VALUES
 ('forge_agent._native_ci_v4_main_receipt_valid(jsonb,jsonb,text)','5e40ead06147309b464a61c6747afe7878e6e586c35a3cd8b0732e78c25a50b2','boolean',false,'i','sql'),
 ('forge_agent._native_ci_v4_legacy_source(oid,text)','0dc4866dba292c6191ede569d80d0a1a5051d33bb2e923257e05c15b5a3c5017','text',false,'s','plpgsql'),
 ('forge_agent._retry_legacy_source(oid,text)','e3cbd7bcff5fc3cf89f8f0c357e71e5793e33f0303f009b3ebe929a664f62b38','text',false,'s','plpgsql'),
 ('forge_agent.delivery_main_receipt_valid(jsonb,jsonb,text)','78921d8ee59c4d30f59918d56020a1b32b594c2c043987f47c64f0e0cce57172','boolean',false,'i','sql'),
 ('forge_agent.delivery_own_cleanup(uuid,jsonb,jsonb,text)','d725763b958b57dad66ba4ca2eb98fb4f4299ff0a995225a9870653294148339','boolean',false,'i','sql'),
 ('forge_agent.delivery_request_valid(jsonb,uuid,uuid,text,text,text,uuid)','f19cbcdf4d32135a2cdb76778f54192b42c4c68df71a6b06351a0bec8419599d','boolean',false,'i','sql'),
 ('forge_agent.delivery_retry_observe(uuid)','29606c22bd6bff37f9d95dcabfec123246f5bb502cd5fe63a76136199b0645c1','jsonb',true,'v','plpgsql'),
 ('forge_coding_reconciler_api.native_merge_failure(uuid,uuid,jsonb)','75b08b501ed50970f19863f2c5dad7b88385d3bf53ba15216c559bf6b4a096b8','jsonb',true,'v','plpgsql'),
 ('forge_coding_reconciler_api.post_merge_checkpoint(uuid,integer,uuid,jsonb)','dd89d682947fd608f6b2c66dd2a34d73eabc59ba8629d37f1ed913cc2defdf11','boolean',true,'v','plpgsql'),
 ('forge_coding_reconciler_api.post_merge_finish(uuid,integer,uuid,text,jsonb,text)','73183476c87e1ed8ad496edf333f7ab3a346f2cd1ca8f1290dba063223cfae5d','boolean',true,'v','plpgsql'),
 ('forge_coding_reconciler_api.review_finish(uuid,uuid,jsonb)','fef5b40c0c1e67495754ac9196b9a450c2a40c89f9fa788741df6534eb8946fb','jsonb',true,'v','plpgsql'))expected(signature,source_sha256,result_type,security_definer,volatility,language) LOOP
 SELECT encode(sha256(convert_to(p.prosrc,'UTF8')),'hex') INTO actual FROM pg_proc p
 WHERE p.oid=to_regprocedure(item.signature) AND p.proowner='forge_agent_owner'::regrole
 AND p.proconfig=ARRAY['search_path=pg_catalog, pg_temp'] AND p.prokind='f'
 AND p.prorettype=to_regtype(item.result_type) AND p.prosecdef=item.security_definer
 AND p.provolatile=item.volatility::"char" AND p.prolang=(SELECT oid FROM pg_language WHERE lanname=item.language);
 IF item.signature LIKE 'forge_agent._native_ci_v4_%' AND (SELECT ARRAY(SELECT unnest(p.proacl::text[]) ORDER BY 1) FROM pg_proc p WHERE p.oid=to_regprocedure(item.signature)) IS DISTINCT FROM ARRAY['forge_agent_owner=X/forge_agent_owner'] THEN RAISE EXCEPTION 'Native v4 helper ACL drift: %',item.signature;END IF;
 IF actual IS DISTINCT FROM item.source_sha256 THEN RAISE EXCEPTION 'Native v4 source/security drift: %',item.signature;END IF;
 END LOOP;
 IF (SELECT count(*) FROM pg_constraint WHERE conrelid IN('forge_agent.coding_reviews'::regclass,'forge_agent.coding_workflow_reviews'::regclass)
 AND conname IN('coding_reviews_gate_manifest_sha256_check','coding_workflow_reviews_gate_manifest_sha256_check') AND contype='c' AND convalidated
 AND position('forge-sdlc-jenkins-v[1-4]:(main|detached|branch/' IN pg_get_constraintdef(oid))>0)<>2 THEN RAISE EXCEPTION 'Native v4 gate constraint drift';END IF;
END;$native_v4_verify$;
