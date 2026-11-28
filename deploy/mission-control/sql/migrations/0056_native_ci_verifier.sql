-- M56 additive native Rust Jenkins gate. M1-M54 and reserved M55 are unchanged.
-- No receipt/history rewrite: v2/v3 keep their original validation branch.
CREATE FUNCTION forge_agent._native_ci_v4_main_receipt_valid(p_request jsonb,p_receipt jsonb,p_commit text)
RETURNS boolean LANGUAGE sql IMMUTABLE SET search_path=pg_catalog,pg_temp AS $native_v4$
 SELECT (p_request->>'verification_profile'='forge-sdlc-jenkins-v4' AND ((p_request IS NOT NULL AND jsonb_typeof(p_receipt)='object'
 AND p_receipt@>((p_request-ARRAY['prepare_action_id','integration_base','source_branch'])||jsonb_build_object('version',CASE WHEN p_request->>'verification_profile'='forge-sdlc-jenkins-v4' THEN 4 ELSE 2 END,'expected_main',p_commit))
 AND p_receipt->>'provider'='jenkins' AND p_receipt->>'result'='SUCCESS' AND p_request->>'verification_profile' IN('forge-sdlc-jenkins-v2','forge-sdlc-jenkins-v4')
 AND p_receipt->'result_version'=CASE WHEN p_request->>'verification_profile'='forge-sdlc-jenkins-v4' THEN '4'::jsonb ELSE '2'::jsonb END AND p_receipt->'cleanup_confirmed'='true'::jsonb
 AND p_receipt->>'bundle_sha256'~'^[0-9a-f]{64}$' AND p_receipt->>'check_receipt_sha256'~'^[0-9a-f]{64}$' AND p_receipt->>'log_sha256'~'^[0-9a-f]{64}$'
 AND p_receipt->'coverage'='{"scope":"main","browser":"required","database":"required_zero_skips","backend_suites":"rust"}'::jsonb
 AND p_receipt->'checks'=CASE WHEN p_request->>'verification_profile'='forge-sdlc-jenkins-v4' THEN '[{"index":0,"argv":["/usr/bin/make","ci-preflight"],"returncode":0},{"index":1,"argv":["/usr/bin/python3","-B","-m","unittest","discover","-s","src/rust/scripts/tests","-v"],"returncode":0},{"index":2,"argv":["/usr/bin/make","fmt-check","PACKAGE=","CHANGED_ONLY=0"],"returncode":0},{"index":3,"argv":["/usr/bin/make","check-imports"],"returncode":0},{"index":4,"argv":["/usr/bin/make","check-deps"],"returncode":0},{"index":5,"argv":["/usr/bin/make","build","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":6,"argv":["/usr/bin/make","check","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":7,"argv":["/usr/bin/make","test-compile","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":8,"argv":["/usr/bin/make","test-release-compile","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":9,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-remote-https","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":10,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-ide","FEATURES=remote","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":11,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-ide","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":12,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-template","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":13,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-terminal","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":14,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-local","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":15,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-terminal","FEATURES=jobs","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":16,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-terminal","FEATURES=disk","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":17,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-system","FEATURES=node","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":18,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-ide-web","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":19,"argv":["/usr/bin/make","ci-ui-compile"],"returncode":0},{"index":20,"argv":["/usr/bin/make","lint","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":21,"argv":["/usr/bin/make","test","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":22,"argv":["/usr/bin/make","test-release","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":23,"argv":["/usr/bin/make","doc","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":24,"argv":["/usr/bin/make","test","PACKAGE=forge-remote-https","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":25,"argv":["/usr/bin/make","lint","PACKAGE=forge-remote-https","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":26,"argv":["/usr/bin/make","test","PACKAGE=forge-ide","FEATURES=remote","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":27,"argv":["/usr/bin/make","lint","PACKAGE=forge-ide","FEATURES=remote","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":28,"argv":["/usr/bin/make","test","PACKAGE=forge-ide","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":29,"argv":["/usr/bin/make","lint","PACKAGE=forge-ide","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":30,"argv":["/usr/bin/make","test","PACKAGE=forge-template","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":31,"argv":["/usr/bin/make","lint","PACKAGE=forge-template","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":32,"argv":["/usr/bin/make","test","PACKAGE=forge-terminal","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":33,"argv":["/usr/bin/make","lint","PACKAGE=forge-terminal","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":34,"argv":["/usr/bin/make","test","PACKAGE=forge-local","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":35,"argv":["/usr/bin/make","test","PACKAGE=forge-terminal","FEATURES=jobs","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":36,"argv":["/usr/bin/make","lint","PACKAGE=forge-terminal","FEATURES=jobs","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":37,"argv":["/usr/bin/make","test","PACKAGE=forge-terminal","FEATURES=disk","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":38,"argv":["/usr/bin/make","lint","PACKAGE=forge-terminal","FEATURES=disk","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":39,"argv":["/usr/bin/make","test","PACKAGE=forge-system","FEATURES=node","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":40,"argv":["/usr/bin/make","test","PACKAGE=forge-ide-web","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":41,"argv":["/usr/bin/make","lint","PACKAGE=forge-ide-web","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":42,"argv":["/usr/bin/make","ui-check"],"returncode":0},{"index":43,"argv":["/usr/bin/make","ui-qa-check"],"returncode":0},{"index":44,"argv":["/usr/bin/make","ui-qa"],"returncode":0},{"index":45,"argv":["/usr/bin/make","backend-suites","SCOPE=main"],"returncode":0}]'::jsonb ELSE '[{"index":0,"argv":["/usr/bin/make","ci-verified"],"returncode":0},{"index":1,"argv":["/usr/bin/make","ui-qa"],"returncode":0},{"index":2,"argv":["/usr/bin/make","backend-suites","SCOPE=main"],"returncode":0}]'::jsonb END
 AND(CASE WHEN p_request->>'verification_profile'='forge-sdlc-jenkins-v4' THEN p_receipt->'phase_results'='[{"index":0,"phase_id":"source-preflight","kind":"verification","argv":["/usr/bin/make","ci-preflight"],"returncode":0},{"index":1,"phase_id":"script-tests","kind":"tests","argv":["/usr/bin/python3","-B","-m","unittest","discover","-s","src/rust/scripts/tests","-v"],"returncode":0},{"index":2,"phase_id":"formatting","kind":"formatting","argv":["/usr/bin/make","fmt-check","PACKAGE=","CHANGED_ONLY=0"],"returncode":0},{"index":3,"phase_id":"imports","kind":"verification","argv":["/usr/bin/make","check-imports"],"returncode":0},{"index":4,"phase_id":"dependencies","kind":"verification","argv":["/usr/bin/make","check-deps"],"returncode":0},{"index":5,"phase_id":"all-features-build","kind":"compilation","argv":["/usr/bin/make","build","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":6,"phase_id":"all-features-check","kind":"compilation","argv":["/usr/bin/make","check","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":7,"phase_id":"all-features-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":8,"phase_id":"all-features-test-release-compile","kind":"compilation","argv":["/usr/bin/make","test-release-compile","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":9,"phase_id":"remote-https-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-remote-https","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":10,"phase_id":"ide-remote-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-ide","FEATURES=remote","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":11,"phase_id":"ide-minimal-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-ide","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":12,"phase_id":"template-minimal-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-template","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":13,"phase_id":"terminal-minimal-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-terminal","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":14,"phase_id":"local-minimal-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-local","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":15,"phase_id":"terminal-jobs-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-terminal","FEATURES=jobs","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":16,"phase_id":"terminal-disk-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-terminal","FEATURES=disk","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":17,"phase_id":"system-node-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-system","FEATURES=node","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":18,"phase_id":"web-minimal-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-ide-web","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":19,"phase_id":"ui-compilation","kind":"compilation","argv":["/usr/bin/make","ci-ui-compile"],"returncode":0},{"index":20,"phase_id":"all-features-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":21,"phase_id":"all-features-test","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":22,"phase_id":"all-features-test-release","kind":"tests","argv":["/usr/bin/make","test-release","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":23,"phase_id":"all-features-doc","kind":"verification","argv":["/usr/bin/make","doc","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":24,"phase_id":"remote-https-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-remote-https","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":25,"phase_id":"remote-https-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-remote-https","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":26,"phase_id":"ide-remote-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-ide","FEATURES=remote","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":27,"phase_id":"ide-remote-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-ide","FEATURES=remote","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":28,"phase_id":"ide-minimal-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-ide","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":29,"phase_id":"ide-minimal-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-ide","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":30,"phase_id":"template-minimal-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-template","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":31,"phase_id":"template-minimal-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-template","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":32,"phase_id":"terminal-minimal-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-terminal","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":33,"phase_id":"terminal-minimal-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-terminal","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":34,"phase_id":"local-minimal-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-local","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":35,"phase_id":"terminal-jobs-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-terminal","FEATURES=jobs","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":36,"phase_id":"terminal-jobs-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-terminal","FEATURES=jobs","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":37,"phase_id":"terminal-disk-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-terminal","FEATURES=disk","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":38,"phase_id":"terminal-disk-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-terminal","FEATURES=disk","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":39,"phase_id":"system-node-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-system","FEATURES=node","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":40,"phase_id":"web-minimal-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-ide-web","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":41,"phase_id":"web-minimal-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-ide-web","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":42,"phase_id":"ui-contracts","kind":"verification","argv":["/usr/bin/make","ui-check"],"returncode":0},{"index":43,"phase_id":"browser-driver-tests","kind":"tests","argv":["/usr/bin/make","ui-qa-check"],"returncode":0},{"index":44,"phase_id":"browser-journeys","kind":"browser","argv":["/usr/bin/make","ui-qa"],"returncode":0},{"index":45,"phase_id":"backend-main","kind":"tests","argv":["/usr/bin/make","backend-suites","SCOPE=main"],"returncode":0}]'::jsonb AND p_receipt->'failure_kind'='null'::jsonb AND p_receipt->'passed_kinds'='["browser","compilation","formatting","lint","tests","verification"]'::jsonb ELSE true END)) IS TRUE)) IS TRUE
$native_v4$;
CREATE FUNCTION forge_agent._native_ci_v4_legacy_source(p_oid oid,p_source text)
RETURNS text LANGUAGE plpgsql STABLE SET search_path=pg_catalog,pg_temp AS $native_v4$
BEGIN
 IF p_oid='forge_coding_reconciler_api.review_finish(uuid,uuid,jsonb)'::regprocedure THEN
  RETURN replace(p_source,'forge-sdlc-jenkins-v[1-4]:','forge-sdlc-jenkins-v[1-3]:');
 END IF;
 RETURN p_source;
END;
$native_v4$;
REVOKE ALL ON FUNCTION forge_agent._native_ci_v4_main_receipt_valid(jsonb,jsonb,text),
 forge_agent._native_ci_v4_legacy_source(oid,text) FROM PUBLIC;
DO $native_v4_patch$
DECLARE signature text:='forge_agent._retry_legacy_source(oid,text)';before text:='
DECLARE item jsonb;pair jsonb;source text:=p_source;
BEGIN
 IF p_oid=''forge_coding_reconciler_api.review_finish(uuid,uuid,jsonb)''::regprocedure THEN
  source:=replace(source,''v_gate!~''''^([0-9a-f]{64}|forge-sdlc-jenkins-v[1-3]:(main|detached|branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?))$'''''',''v_gate!~''''^[0-9a-f]{64}$'''''');
 END IF;
 FOR item IN SELECT jsonb_array_elements(forge_agent._retry_native_patches()) LOOP
  IF p_oid=(item->>''name'')::regprocedure THEN
   IF encode(sha256(convert_to(source,''UTF8'')),''hex'')<>item->>''new_sha'' THEN RAISE EXCEPTION ''Retry native authority source drift'';END IF;
   FOR pair IN SELECT value FROM jsonb_array_elements(item->''patches'') WITH ORDINALITY p(value,n) ORDER BY n DESC LOOP source:=replace(source,pair->>1,pair->>0);END LOOP;
   IF encode(sha256(convert_to(source,''UTF8'')),''hex'')<>item->>''old_sha'' THEN RAISE EXCEPTION ''Retry predecessor restoration drift'';END IF;
   RETURN source;
  END IF;
 END LOOP;
 RETURN source;
END;';after text:='
DECLARE item jsonb;pair jsonb;source text:=p_source;
BEGIN
 source:=forge_agent._native_ci_v4_legacy_source(p_oid,source);
 IF p_oid=''forge_coding_reconciler_api.review_finish(uuid,uuid,jsonb)''::regprocedure THEN
  source:=replace(source,''v_gate!~''''^([0-9a-f]{64}|forge-sdlc-jenkins-v[1-3]:(main|detached|branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?))$'''''',''v_gate!~''''^[0-9a-f]{64}$'''''');
 END IF;
 FOR item IN SELECT jsonb_array_elements(forge_agent._retry_native_patches()) LOOP
  IF p_oid=(item->>''name'')::regprocedure THEN
   IF encode(sha256(convert_to(source,''UTF8'')),''hex'')<>item->>''new_sha'' THEN RAISE EXCEPTION ''Retry native authority source drift'';END IF;
   FOR pair IN SELECT value FROM jsonb_array_elements(item->''patches'') WITH ORDINALITY p(value,n) ORDER BY n DESC LOOP source:=replace(source,pair->>1,pair->>0);END LOOP;
   IF encode(sha256(convert_to(source,''UTF8'')),''hex'')<>item->>''old_sha'' THEN RAISE EXCEPTION ''Retry predecessor restoration drift'';END IF;
   RETURN source;
  END IF;
 END LOOP;
 RETURN source;
END;';definition text;
BEGIN
 IF (SELECT encode(sha256(convert_to(prosrc,'UTF8')),'hex') FROM pg_proc WHERE oid=signature::regprocedure)<>'025faf263ceab3ac2b37898202e1de848b8fe8424a4bb058c9b902c38c7e28ae' THEN RAISE EXCEPTION 'M56 predecessor function drift: %',signature;END IF;
 definition:=pg_get_functiondef(signature::regprocedure);
 IF (length(definition)-length(replace(definition,before,'')))/length(before)<>1 THEN RAISE EXCEPTION 'M56 source target not unique: %',signature;END IF;
 EXECUTE replace(definition,before,after);
END;$native_v4_patch$;
DO $native_v4_patch$
DECLARE signature text:='forge_agent.delivery_main_receipt_valid(jsonb,jsonb,text)';before text:='
 SELECT(p_request IS NOT NULL AND jsonb_typeof(p_receipt)=''object''
 AND p_receipt@>((p_request-ARRAY[''prepare_action_id'',''integration_base'',''source_branch''])||jsonb_build_object(''version'',CASE WHEN p_request->>''verification_profile''=''forge-sdlc-jenkins-v3'' THEN 3 ELSE 2 END,''expected_main'',p_commit))
 AND p_receipt->>''provider''=''jenkins'' AND p_receipt->>''result''=''SUCCESS'' AND p_request->>''verification_profile'' IN(''forge-sdlc-jenkins-v2'',''forge-sdlc-jenkins-v3'')
 AND p_receipt->''result_version''=CASE WHEN p_request->>''verification_profile''=''forge-sdlc-jenkins-v3'' THEN ''3''::jsonb ELSE ''2''::jsonb END AND p_receipt->''cleanup_confirmed''=''true''::jsonb
 AND p_receipt->>''bundle_sha256''~''^[0-9a-f]{64}$'' AND p_receipt->>''check_receipt_sha256''~''^[0-9a-f]{64}$'' AND p_receipt->>''log_sha256''~''^[0-9a-f]{64}$''
 AND p_receipt->''coverage''=''{"scope":"main","browser":"required","database":"required_zero_skips","backend_suites":"all_four"}''::jsonb
 AND p_receipt->''checks''=CASE WHEN p_request->>''verification_profile''=''forge-sdlc-jenkins-v3'' THEN ''[{"index":0,"argv":["/usr/bin/make","ci-preflight"],"returncode":0},{"index":1,"argv":["/usr/bin/python3","-B","-m","unittest","discover","-s","src/rust/scripts/tests","-v"],"returncode":0},{"index":2,"argv":["/usr/bin/make","fmt-check","PACKAGE=","CHANGED_ONLY=0"],"returncode":0},{"index":3,"argv":["/usr/bin/make","check-imports"],"returncode":0},{"index":4,"argv":["/usr/bin/make","check-deps"],"returncode":0},{"index":5,"argv":["/usr/bin/make","build","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":6,"argv":["/usr/bin/make","check","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":7,"argv":["/usr/bin/make","test-compile","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":8,"argv":["/usr/bin/make","test-release-compile","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":9,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-remote-https","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":10,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-ide","FEATURES=remote","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":11,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-ide","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":12,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-template","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":13,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-terminal","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":14,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-local","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":15,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-terminal","FEATURES=jobs","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":16,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-terminal","FEATURES=disk","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":17,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-system","FEATURES=node","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":18,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-ide-web","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":19,"argv":["/usr/bin/make","ci-ui-compile"],"returncode":0},{"index":20,"argv":["/usr/bin/make","lint","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":21,"argv":["/usr/bin/make","test","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":22,"argv":["/usr/bin/make","test-release","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":23,"argv":["/usr/bin/make","doc","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":24,"argv":["/usr/bin/make","test","PACKAGE=forge-remote-https","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":25,"argv":["/usr/bin/make","lint","PACKAGE=forge-remote-https","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":26,"argv":["/usr/bin/make","test","PACKAGE=forge-ide","FEATURES=remote","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":27,"argv":["/usr/bin/make","lint","PACKAGE=forge-ide","FEATURES=remote","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":28,"argv":["/usr/bin/make","test","PACKAGE=forge-ide","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":29,"argv":["/usr/bin/make","lint","PACKAGE=forge-ide","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":30,"argv":["/usr/bin/make","test","PACKAGE=forge-template","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":31,"argv":["/usr/bin/make","lint","PACKAGE=forge-template","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":32,"argv":["/usr/bin/make","test","PACKAGE=forge-terminal","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":33,"argv":["/usr/bin/make","lint","PACKAGE=forge-terminal","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":34,"argv":["/usr/bin/make","test","PACKAGE=forge-local","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":35,"argv":["/usr/bin/make","test","PACKAGE=forge-terminal","FEATURES=jobs","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":36,"argv":["/usr/bin/make","lint","PACKAGE=forge-terminal","FEATURES=jobs","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":37,"argv":["/usr/bin/make","test","PACKAGE=forge-terminal","FEATURES=disk","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":38,"argv":["/usr/bin/make","lint","PACKAGE=forge-terminal","FEATURES=disk","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":39,"argv":["/usr/bin/make","test","PACKAGE=forge-system","FEATURES=node","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":40,"argv":["/usr/bin/make","test","PACKAGE=forge-ide-web","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":41,"argv":["/usr/bin/make","lint","PACKAGE=forge-ide-web","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":42,"argv":["/usr/bin/make","ui-check"],"returncode":0},{"index":43,"argv":["/usr/bin/make","ui-qa-check"],"returncode":0},{"index":44,"argv":["/usr/bin/make","ui-qa"],"returncode":0},{"index":45,"argv":["/usr/bin/python3","-B","deploy/mission-control/backend-suites.py","--scope","main"],"returncode":0}]''::jsonb ELSE ''[{"index":0,"argv":["/usr/bin/make","ci-verified"],"returncode":0},{"index":1,"argv":["/usr/bin/make","ui-qa"],"returncode":0},{"index":2,"argv":["/usr/bin/python3","-B","deploy/mission-control/backend-suites.py","--scope","main"],"returncode":0}]''::jsonb END
 AND(CASE WHEN p_request->>''verification_profile''=''forge-sdlc-jenkins-v3'' THEN p_receipt->''phase_results''=''[{"index":0,"phase_id":"source-preflight","kind":"verification","argv":["/usr/bin/make","ci-preflight"],"returncode":0},{"index":1,"phase_id":"script-tests","kind":"tests","argv":["/usr/bin/python3","-B","-m","unittest","discover","-s","src/rust/scripts/tests","-v"],"returncode":0},{"index":2,"phase_id":"formatting","kind":"formatting","argv":["/usr/bin/make","fmt-check","PACKAGE=","CHANGED_ONLY=0"],"returncode":0},{"index":3,"phase_id":"imports","kind":"verification","argv":["/usr/bin/make","check-imports"],"returncode":0},{"index":4,"phase_id":"dependencies","kind":"verification","argv":["/usr/bin/make","check-deps"],"returncode":0},{"index":5,"phase_id":"all-features-build","kind":"compilation","argv":["/usr/bin/make","build","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":6,"phase_id":"all-features-check","kind":"compilation","argv":["/usr/bin/make","check","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":7,"phase_id":"all-features-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":8,"phase_id":"all-features-test-release-compile","kind":"compilation","argv":["/usr/bin/make","test-release-compile","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":9,"phase_id":"remote-https-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-remote-https","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":10,"phase_id":"ide-remote-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-ide","FEATURES=remote","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":11,"phase_id":"ide-minimal-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-ide","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":12,"phase_id":"template-minimal-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-template","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":13,"phase_id":"terminal-minimal-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-terminal","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":14,"phase_id":"local-minimal-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-local","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":15,"phase_id":"terminal-jobs-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-terminal","FEATURES=jobs","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":16,"phase_id":"terminal-disk-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-terminal","FEATURES=disk","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":17,"phase_id":"system-node-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-system","FEATURES=node","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":18,"phase_id":"web-minimal-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-ide-web","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":19,"phase_id":"ui-compilation","kind":"compilation","argv":["/usr/bin/make","ci-ui-compile"],"returncode":0},{"index":20,"phase_id":"all-features-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":21,"phase_id":"all-features-test","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":22,"phase_id":"all-features-test-release","kind":"tests","argv":["/usr/bin/make","test-release","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":23,"phase_id":"all-features-doc","kind":"verification","argv":["/usr/bin/make","doc","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":24,"phase_id":"remote-https-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-remote-https","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":25,"phase_id":"remote-https-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-remote-https","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":26,"phase_id":"ide-remote-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-ide","FEATURES=remote","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":27,"phase_id":"ide-remote-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-ide","FEATURES=remote","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":28,"phase_id":"ide-minimal-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-ide","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":29,"phase_id":"ide-minimal-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-ide","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":30,"phase_id":"template-minimal-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-template","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":31,"phase_id":"template-minimal-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-template","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":32,"phase_id":"terminal-minimal-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-terminal","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":33,"phase_id":"terminal-minimal-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-terminal","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":34,"phase_id":"local-minimal-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-local","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":35,"phase_id":"terminal-jobs-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-terminal","FEATURES=jobs","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":36,"phase_id":"terminal-jobs-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-terminal","FEATURES=jobs","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":37,"phase_id":"terminal-disk-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-terminal","FEATURES=disk","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":38,"phase_id":"terminal-disk-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-terminal","FEATURES=disk","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":39,"phase_id":"system-node-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-system","FEATURES=node","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":40,"phase_id":"web-minimal-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-ide-web","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":41,"phase_id":"web-minimal-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-ide-web","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":42,"phase_id":"ui-contracts","kind":"verification","argv":["/usr/bin/make","ui-check"],"returncode":0},{"index":43,"phase_id":"browser-driver-tests","kind":"tests","argv":["/usr/bin/make","ui-qa-check"],"returncode":0},{"index":44,"phase_id":"browser-journeys","kind":"browser","argv":["/usr/bin/make","ui-qa"],"returncode":0},{"index":45,"phase_id":"backend-main","kind":"tests","argv":["/usr/bin/python3","-B","deploy/mission-control/backend-suites.py","--scope","main"],"returncode":0}]''::jsonb AND p_receipt->''failure_kind''=''null''::jsonb AND p_receipt->''passed_kinds''=''["browser","compilation","formatting","lint","tests","verification"]''::jsonb ELSE true END)) IS TRUE
';after text:='
 SELECT CASE WHEN p_request->>''verification_profile''=''forge-sdlc-jenkins-v4'' THEN forge_agent._native_ci_v4_main_receipt_valid(p_request,p_receipt,p_commit) ELSE ((p_request IS NOT NULL AND jsonb_typeof(p_receipt)=''object''
 AND p_receipt@>((p_request-ARRAY[''prepare_action_id'',''integration_base'',''source_branch''])||jsonb_build_object(''version'',CASE WHEN p_request->>''verification_profile''=''forge-sdlc-jenkins-v3'' THEN 3 ELSE 2 END,''expected_main'',p_commit))
 AND p_receipt->>''provider''=''jenkins'' AND p_receipt->>''result''=''SUCCESS'' AND p_request->>''verification_profile'' IN(''forge-sdlc-jenkins-v2'',''forge-sdlc-jenkins-v3'')
 AND p_receipt->''result_version''=CASE WHEN p_request->>''verification_profile''=''forge-sdlc-jenkins-v3'' THEN ''3''::jsonb ELSE ''2''::jsonb END AND p_receipt->''cleanup_confirmed''=''true''::jsonb
 AND p_receipt->>''bundle_sha256''~''^[0-9a-f]{64}$'' AND p_receipt->>''check_receipt_sha256''~''^[0-9a-f]{64}$'' AND p_receipt->>''log_sha256''~''^[0-9a-f]{64}$''
 AND p_receipt->''coverage''=''{"scope":"main","browser":"required","database":"required_zero_skips","backend_suites":"all_four"}''::jsonb
 AND p_receipt->''checks''=CASE WHEN p_request->>''verification_profile''=''forge-sdlc-jenkins-v3'' THEN ''[{"index":0,"argv":["/usr/bin/make","ci-preflight"],"returncode":0},{"index":1,"argv":["/usr/bin/python3","-B","-m","unittest","discover","-s","src/rust/scripts/tests","-v"],"returncode":0},{"index":2,"argv":["/usr/bin/make","fmt-check","PACKAGE=","CHANGED_ONLY=0"],"returncode":0},{"index":3,"argv":["/usr/bin/make","check-imports"],"returncode":0},{"index":4,"argv":["/usr/bin/make","check-deps"],"returncode":0},{"index":5,"argv":["/usr/bin/make","build","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":6,"argv":["/usr/bin/make","check","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":7,"argv":["/usr/bin/make","test-compile","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":8,"argv":["/usr/bin/make","test-release-compile","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":9,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-remote-https","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":10,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-ide","FEATURES=remote","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":11,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-ide","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":12,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-template","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":13,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-terminal","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":14,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-local","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":15,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-terminal","FEATURES=jobs","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":16,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-terminal","FEATURES=disk","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":17,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-system","FEATURES=node","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":18,"argv":["/usr/bin/make","test-compile","PACKAGE=forge-ide-web","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":19,"argv":["/usr/bin/make","ci-ui-compile"],"returncode":0},{"index":20,"argv":["/usr/bin/make","lint","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":21,"argv":["/usr/bin/make","test","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":22,"argv":["/usr/bin/make","test-release","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":23,"argv":["/usr/bin/make","doc","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":24,"argv":["/usr/bin/make","test","PACKAGE=forge-remote-https","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":25,"argv":["/usr/bin/make","lint","PACKAGE=forge-remote-https","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":26,"argv":["/usr/bin/make","test","PACKAGE=forge-ide","FEATURES=remote","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":27,"argv":["/usr/bin/make","lint","PACKAGE=forge-ide","FEATURES=remote","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":28,"argv":["/usr/bin/make","test","PACKAGE=forge-ide","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":29,"argv":["/usr/bin/make","lint","PACKAGE=forge-ide","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":30,"argv":["/usr/bin/make","test","PACKAGE=forge-template","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":31,"argv":["/usr/bin/make","lint","PACKAGE=forge-template","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":32,"argv":["/usr/bin/make","test","PACKAGE=forge-terminal","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":33,"argv":["/usr/bin/make","lint","PACKAGE=forge-terminal","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":34,"argv":["/usr/bin/make","test","PACKAGE=forge-local","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":35,"argv":["/usr/bin/make","test","PACKAGE=forge-terminal","FEATURES=jobs","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":36,"argv":["/usr/bin/make","lint","PACKAGE=forge-terminal","FEATURES=jobs","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":37,"argv":["/usr/bin/make","test","PACKAGE=forge-terminal","FEATURES=disk","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":38,"argv":["/usr/bin/make","lint","PACKAGE=forge-terminal","FEATURES=disk","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":39,"argv":["/usr/bin/make","test","PACKAGE=forge-system","FEATURES=node","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":40,"argv":["/usr/bin/make","test","PACKAGE=forge-ide-web","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":41,"argv":["/usr/bin/make","lint","PACKAGE=forge-ide-web","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":42,"argv":["/usr/bin/make","ui-check"],"returncode":0},{"index":43,"argv":["/usr/bin/make","ui-qa-check"],"returncode":0},{"index":44,"argv":["/usr/bin/make","ui-qa"],"returncode":0},{"index":45,"argv":["/usr/bin/python3","-B","deploy/mission-control/backend-suites.py","--scope","main"],"returncode":0}]''::jsonb ELSE ''[{"index":0,"argv":["/usr/bin/make","ci-verified"],"returncode":0},{"index":1,"argv":["/usr/bin/make","ui-qa"],"returncode":0},{"index":2,"argv":["/usr/bin/python3","-B","deploy/mission-control/backend-suites.py","--scope","main"],"returncode":0}]''::jsonb END
 AND(CASE WHEN p_request->>''verification_profile''=''forge-sdlc-jenkins-v3'' THEN p_receipt->''phase_results''=''[{"index":0,"phase_id":"source-preflight","kind":"verification","argv":["/usr/bin/make","ci-preflight"],"returncode":0},{"index":1,"phase_id":"script-tests","kind":"tests","argv":["/usr/bin/python3","-B","-m","unittest","discover","-s","src/rust/scripts/tests","-v"],"returncode":0},{"index":2,"phase_id":"formatting","kind":"formatting","argv":["/usr/bin/make","fmt-check","PACKAGE=","CHANGED_ONLY=0"],"returncode":0},{"index":3,"phase_id":"imports","kind":"verification","argv":["/usr/bin/make","check-imports"],"returncode":0},{"index":4,"phase_id":"dependencies","kind":"verification","argv":["/usr/bin/make","check-deps"],"returncode":0},{"index":5,"phase_id":"all-features-build","kind":"compilation","argv":["/usr/bin/make","build","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":6,"phase_id":"all-features-check","kind":"compilation","argv":["/usr/bin/make","check","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":7,"phase_id":"all-features-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":8,"phase_id":"all-features-test-release-compile","kind":"compilation","argv":["/usr/bin/make","test-release-compile","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":9,"phase_id":"remote-https-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-remote-https","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":10,"phase_id":"ide-remote-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-ide","FEATURES=remote","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":11,"phase_id":"ide-minimal-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-ide","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":12,"phase_id":"template-minimal-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-template","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":13,"phase_id":"terminal-minimal-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-terminal","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":14,"phase_id":"local-minimal-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-local","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":15,"phase_id":"terminal-jobs-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-terminal","FEATURES=jobs","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":16,"phase_id":"terminal-disk-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-terminal","FEATURES=disk","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":17,"phase_id":"system-node-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-system","FEATURES=node","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":18,"phase_id":"web-minimal-test-compile","kind":"compilation","argv":["/usr/bin/make","test-compile","PACKAGE=forge-ide-web","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":19,"phase_id":"ui-compilation","kind":"compilation","argv":["/usr/bin/make","ci-ui-compile"],"returncode":0},{"index":20,"phase_id":"all-features-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":21,"phase_id":"all-features-test","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":22,"phase_id":"all-features-test-release","kind":"tests","argv":["/usr/bin/make","test-release","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":23,"phase_id":"all-features-doc","kind":"verification","argv":["/usr/bin/make","doc","PACKAGE=","FEATURES=","ALL_FEATURES=1","NO_DEFAULT_FEATURES=0"],"returncode":0},{"index":24,"phase_id":"remote-https-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-remote-https","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":25,"phase_id":"remote-https-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-remote-https","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":26,"phase_id":"ide-remote-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-ide","FEATURES=remote","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":27,"phase_id":"ide-remote-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-ide","FEATURES=remote","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":28,"phase_id":"ide-minimal-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-ide","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":29,"phase_id":"ide-minimal-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-ide","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":30,"phase_id":"template-minimal-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-template","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":31,"phase_id":"template-minimal-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-template","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":32,"phase_id":"terminal-minimal-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-terminal","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":33,"phase_id":"terminal-minimal-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-terminal","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":34,"phase_id":"local-minimal-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-local","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":35,"phase_id":"terminal-jobs-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-terminal","FEATURES=jobs","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":36,"phase_id":"terminal-jobs-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-terminal","FEATURES=jobs","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":37,"phase_id":"terminal-disk-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-terminal","FEATURES=disk","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":38,"phase_id":"terminal-disk-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-terminal","FEATURES=disk","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":39,"phase_id":"system-node-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-system","FEATURES=node","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":40,"phase_id":"web-minimal-tests","kind":"tests","argv":["/usr/bin/make","test","PACKAGE=forge-ide-web","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":41,"phase_id":"web-minimal-lint","kind":"lint","argv":["/usr/bin/make","lint","PACKAGE=forge-ide-web","FEATURES=","ALL_FEATURES=0","NO_DEFAULT_FEATURES=1"],"returncode":0},{"index":42,"phase_id":"ui-contracts","kind":"verification","argv":["/usr/bin/make","ui-check"],"returncode":0},{"index":43,"phase_id":"browser-driver-tests","kind":"tests","argv":["/usr/bin/make","ui-qa-check"],"returncode":0},{"index":44,"phase_id":"browser-journeys","kind":"browser","argv":["/usr/bin/make","ui-qa"],"returncode":0},{"index":45,"phase_id":"backend-main","kind":"tests","argv":["/usr/bin/python3","-B","deploy/mission-control/backend-suites.py","--scope","main"],"returncode":0}]''::jsonb AND p_receipt->''failure_kind''=''null''::jsonb AND p_receipt->''passed_kinds''=''["browser","compilation","formatting","lint","tests","verification"]''::jsonb ELSE true END)) IS TRUE) END
';definition text;
BEGIN
 IF (SELECT encode(sha256(convert_to(prosrc,'UTF8')),'hex') FROM pg_proc WHERE oid=signature::regprocedure)<>'9e074e86b1fafc6e9a6d26e85a51a4bbeaae63fe47efffab1f4ee21d08ae3ad1' THEN RAISE EXCEPTION 'M56 predecessor function drift: %',signature;END IF;
 definition:=pg_get_functiondef(signature::regprocedure);
 IF (length(definition)-length(replace(definition,before,'')))/length(before)<>1 THEN RAISE EXCEPTION 'M56 source target not unique: %',signature;END IF;
 EXECUTE replace(definition,before,after);
END;$native_v4_patch$;
DO $native_v4_patch$
DECLARE signature text:='forge_agent.delivery_own_cleanup(uuid,jsonb,jsonb,text)';before text:='
 SELECT(CASE WHEN p_request IS NULL THEN p_receipt->''process_started''=''false''::jsonb ELSE
 (p_receipt->>''provider''=''jenkins'' AND p_receipt->>''target_action_id''=p_id::text AND(p_receipt->''process_started''=''false''::jsonb OR(p_receipt->''cleanup_confirmed''=''true''::jsonb AND p_receipt->>''check_receipt_sha256''~''^[0-9a-f]{64}$'')))
 OR(p_receipt->>''provider''=''jenkins'' AND p_receipt->''cleanup_confirmed''=''true''::jsonb AND p_receipt->>''check_receipt_sha256''~''^[0-9a-f]{64}$''
 AND p_receipt@>((p_request-ARRAY[''prepare_action_id'',''integration_base'',''source_branch''])||jsonb_build_object(''version'',CASE WHEN p_request->>''verification_profile''=''forge-sdlc-jenkins-v3'' THEN 3 ELSE 2 END,''expected_main'',p_commit))) END) IS TRUE
';after text:='
 SELECT(CASE WHEN p_request IS NULL THEN p_receipt->''process_started''=''false''::jsonb ELSE
 (p_receipt->>''provider''=''jenkins'' AND p_receipt->>''target_action_id''=p_id::text AND(p_receipt->''process_started''=''false''::jsonb OR(p_receipt->''cleanup_confirmed''=''true''::jsonb AND p_receipt->>''check_receipt_sha256''~''^[0-9a-f]{64}$'')))
 OR(p_receipt->>''provider''=''jenkins'' AND p_receipt->''cleanup_confirmed''=''true''::jsonb AND p_receipt->>''check_receipt_sha256''~''^[0-9a-f]{64}$''
 AND p_receipt@>((p_request-ARRAY[''prepare_action_id'',''integration_base'',''source_branch''])||jsonb_build_object(''version'',CASE WHEN p_request->>''verification_profile''=''forge-sdlc-jenkins-v4'' THEN 4 WHEN p_request->>''verification_profile''=''forge-sdlc-jenkins-v3'' THEN 3 ELSE 2 END,''expected_main'',p_commit))) END) IS TRUE
';definition text;
BEGIN
 IF (SELECT encode(sha256(convert_to(prosrc,'UTF8')),'hex') FROM pg_proc WHERE oid=signature::regprocedure)<>'681cb1c5f3e981894f3b75adffd6425f044e237de98ec624663f1f4c317c7368' THEN RAISE EXCEPTION 'M56 predecessor function drift: %',signature;END IF;
 definition:=pg_get_functiondef(signature::regprocedure);
 IF (length(definition)-length(replace(definition,before,'')))/length(before)<>1 THEN RAISE EXCEPTION 'M56 source target not unique: %',signature;END IF;
 EXECUTE replace(definition,before,after);
END;$native_v4_patch$;
DO $native_v4_patch$
DECLARE signature text:='forge_agent.delivery_request_valid(jsonb,uuid,uuid,text,text,text,uuid)';before text:='
 SELECT(jsonb_typeof(p_request)=''object'' AND p_request-ARRAY[''action_id'',''prepare_action_id'',''workflow_id'',''project_alias'',''candidate_commit'',''candidate_tree'',''integration_base'',''gate_manifest_sha256'',''branch_scope'',''source_branch'',''verification_profile'',''correlation_id'']=''{}''::jsonb
 AND p_request?&ARRAY[''action_id'',''prepare_action_id'',''workflow_id'',''project_alias'',''candidate_commit'',''candidate_tree'',''integration_base'',''gate_manifest_sha256'',''branch_scope'',''source_branch'',''verification_profile'',''correlation_id'']
 AND NOT EXISTS(SELECT FROM jsonb_each(p_request) x WHERE jsonb_typeof(x.value)<>''string'')
 AND p_request@>jsonb_build_object(''action_id'',p_id,''workflow_id'',p_scope,''project_alias'',p_project,''candidate_commit'',p_commit,''candidate_tree'',p_tree,''integration_base'',p_commit,''branch_scope'',''main'',''source_branch'',''main'',''verification_profile'',p_request->>''verification_profile'',''correlation_id'',p_correlation)
 AND p_request->>''verification_profile'' IN(''forge-sdlc-jenkins-v2'',''forge-sdlc-jenkins-v3'')
 AND p_request->>''gate_manifest_sha256''~''^([0-9a-f]{64}|forge-sdlc-jenkins-v[1-3]:(main|detached|branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?))$'' AND p_request->>''prepare_action_id''~''^[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'') IS TRUE
';after text:='
 SELECT(jsonb_typeof(p_request)=''object'' AND p_request-ARRAY[''action_id'',''prepare_action_id'',''workflow_id'',''project_alias'',''candidate_commit'',''candidate_tree'',''integration_base'',''gate_manifest_sha256'',''branch_scope'',''source_branch'',''verification_profile'',''correlation_id'']=''{}''::jsonb
 AND p_request?&ARRAY[''action_id'',''prepare_action_id'',''workflow_id'',''project_alias'',''candidate_commit'',''candidate_tree'',''integration_base'',''gate_manifest_sha256'',''branch_scope'',''source_branch'',''verification_profile'',''correlation_id'']
 AND NOT EXISTS(SELECT FROM jsonb_each(p_request) x WHERE jsonb_typeof(x.value)<>''string'')
 AND p_request@>jsonb_build_object(''action_id'',p_id,''workflow_id'',p_scope,''project_alias'',p_project,''candidate_commit'',p_commit,''candidate_tree'',p_tree,''integration_base'',p_commit,''branch_scope'',''main'',''source_branch'',''main'',''verification_profile'',p_request->>''verification_profile'',''correlation_id'',p_correlation)
 AND p_request->>''verification_profile'' IN(''forge-sdlc-jenkins-v2'',''forge-sdlc-jenkins-v3'',''forge-sdlc-jenkins-v4'')
 AND p_request->>''gate_manifest_sha256''~''^([0-9a-f]{64}|forge-sdlc-jenkins-v[1-4]:(main|detached|branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?))$'' AND p_request->>''prepare_action_id''~''^[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'') IS TRUE
';definition text;
BEGIN
 IF (SELECT encode(sha256(convert_to(prosrc,'UTF8')),'hex') FROM pg_proc WHERE oid=signature::regprocedure)<>'6d2b4970fe277c8ec082f1b916911caeb0fc3bf9af94787e1c4e1b24b2997267' THEN RAISE EXCEPTION 'M56 predecessor function drift: %',signature;END IF;
 definition:=pg_get_functiondef(signature::regprocedure);
 IF (length(definition)-length(replace(definition,before,'')))/length(before)<>1 THEN RAISE EXCEPTION 'M56 source target not unique: %',signature;END IF;
 EXECUTE replace(definition,before,after);
END;$native_v4_patch$;
DO $native_v4_patch$
DECLARE signature text:='forge_agent.delivery_retry_observe(uuid)';before text:='
DECLARE d forge_agent.delivery_ledger%ROWTYPE;kind text;passed text[]:=ARRAY[]::text[];
 phase text:='''';passed_phases text[]:=ARRAY[]::text[];
 evidence text;event jsonb;linked uuid;all_kinds text[]:=ARRAY[''compilation'',''tests'',''browser'',''lint'',''formatting'',''verification'',''infrastructure''];
BEGIN
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=p_verification;
 IF FOUND AND d.verification_status NOT IN(''passed'',''failed'') THEN RETURN NULL;END IF;
 IF NOT FOUND OR d.owner_id IS DISTINCT FROM current_setting(''forge.coding_owner'',true) THEN
  RAISE EXCEPTION ''Retry event delivery scope denied'' USING ERRCODE=''42501'';END IF;
 evidence:=encode(sha256(convert_to(forge_agent.delivery_canonical(d.verification_receipt),''UTF8'')),''hex'');
 IF d.verification_status=''passed'' THEN
  FOREACH kind IN ARRAY all_kinds LOOP
   PERFORM forge_agent.retry_event_resolve(d.project_alias,''delivery'',d.lineage_id,kind,d.verification_id::text,evidence);
  END LOOP;
  RETURN NULL;
 END IF;
 SELECT event_id INTO linked FROM forge_agent.delivery_retry_event_links WHERE verification_id=d.verification_id;
 IF FOUND THEN RETURN forge_agent.retry_event_get(linked);END IF;
 -- New receipts have fixed, directly executed phase witnesses. Legacy receipts
 -- retain an explicit verification event; diagnostics never mint a new budget.
 IF d.verification_receipt->>''verification_profile''=''forge-sdlc-jenkins-v3''
 AND d.verification_receipt->''result_version''=''3''::jsonb
 AND d.verification_receipt->>''failure_kind''=ANY(all_kinds)
 AND jsonb_typeof(d.verification_receipt->''passed_kinds'')=''array'' THEN
  kind:=d.verification_receipt->>''failure_kind'';
  SELECT coalesce(array_agg(value ORDER BY value),ARRAY[]::text[]) INTO passed
   FROM jsonb_array_elements_text(d.verification_receipt->''passed_kinds'') value;
  IF EXISTS(SELECT FROM unnest(passed) value WHERE NOT value=ANY(all_kinds))
  OR kind=ANY(passed) OR cardinality(passed)<>(SELECT count(DISTINCT value) FROM unnest(passed) value) THEN
   RAISE EXCEPTION ''Invalid trusted phase witness'' USING ERRCODE=''P0009'';END IF;
  SELECT item->>''phase_id'' INTO phase
   FROM jsonb_array_elements(d.verification_receipt->''phase_results'') WITH ORDINALITY phase_rows(item,ordinality)
   WHERE (item->>''returncode'')::integer<>0 ORDER BY ordinality LIMIT 1;
  IF d.verification_receipt->>''failure'' IS DISTINCT FROM ''source_changed_during_checks'' THEN
   SELECT coalesce(array_agg(item->>''phase_id'' ORDER BY ordinality),ARRAY[]::text[]) INTO passed_phases
    FROM jsonb_array_elements(d.verification_receipt->''phase_results'') WITH ORDINALITY phase_rows(item,ordinality)
    WHERE (item->>''returncode'')::integer=0;
  END IF;
 ELSE
  kind:=''verification'';
 END IF;
 event:=forge_agent.retry_event_observe(d.project_alias,''delivery'',d.lineage_id,kind,d.verification_id::text,evidence,passed,phase,passed_phases);
 INSERT INTO forge_agent.delivery_retry_event_links(verification_id,event_id) VALUES(d.verification_id,(event->>''event_id'')::uuid);
 RETURN event;
END;';after text:='
DECLARE d forge_agent.delivery_ledger%ROWTYPE;kind text;passed text[]:=ARRAY[]::text[];
 phase text:='''';passed_phases text[]:=ARRAY[]::text[];
 evidence text;event jsonb;linked uuid;all_kinds text[]:=ARRAY[''compilation'',''tests'',''browser'',''lint'',''formatting'',''verification'',''infrastructure''];
BEGIN
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=p_verification;
 IF FOUND AND d.verification_status NOT IN(''passed'',''failed'') THEN RETURN NULL;END IF;
 IF NOT FOUND OR d.owner_id IS DISTINCT FROM current_setting(''forge.coding_owner'',true) THEN
  RAISE EXCEPTION ''Retry event delivery scope denied'' USING ERRCODE=''42501'';END IF;
 evidence:=encode(sha256(convert_to(forge_agent.delivery_canonical(d.verification_receipt),''UTF8'')),''hex'');
 IF d.verification_status=''passed'' THEN
  FOREACH kind IN ARRAY all_kinds LOOP
   PERFORM forge_agent.retry_event_resolve(d.project_alias,''delivery'',d.lineage_id,kind,d.verification_id::text,evidence);
  END LOOP;
  RETURN NULL;
 END IF;
 SELECT event_id INTO linked FROM forge_agent.delivery_retry_event_links WHERE verification_id=d.verification_id;
 IF FOUND THEN RETURN forge_agent.retry_event_get(linked);END IF;
 -- New receipts have fixed, directly executed phase witnesses. Legacy receipts
 -- retain an explicit verification event; diagnostics never mint a new budget.
 IF (d.verification_receipt->>''verification_profile'',d.verification_receipt->>''result_version'') IN((''forge-sdlc-jenkins-v3'',''3''),(''forge-sdlc-jenkins-v4'',''4''))
 AND d.verification_receipt->>''failure_kind''=ANY(all_kinds)
 AND jsonb_typeof(d.verification_receipt->''passed_kinds'')=''array'' THEN
  kind:=d.verification_receipt->>''failure_kind'';
  SELECT coalesce(array_agg(value ORDER BY value),ARRAY[]::text[]) INTO passed
   FROM jsonb_array_elements_text(d.verification_receipt->''passed_kinds'') value;
  IF EXISTS(SELECT FROM unnest(passed) value WHERE NOT value=ANY(all_kinds))
  OR kind=ANY(passed) OR cardinality(passed)<>(SELECT count(DISTINCT value) FROM unnest(passed) value) THEN
   RAISE EXCEPTION ''Invalid trusted phase witness'' USING ERRCODE=''P0009'';END IF;
  SELECT item->>''phase_id'' INTO phase
   FROM jsonb_array_elements(d.verification_receipt->''phase_results'') WITH ORDINALITY phase_rows(item,ordinality)
   WHERE (item->>''returncode'')::integer<>0 ORDER BY ordinality LIMIT 1;
  IF d.verification_receipt->>''failure'' IS DISTINCT FROM ''source_changed_during_checks'' THEN
   SELECT coalesce(array_agg(item->>''phase_id'' ORDER BY ordinality),ARRAY[]::text[]) INTO passed_phases
    FROM jsonb_array_elements(d.verification_receipt->''phase_results'') WITH ORDINALITY phase_rows(item,ordinality)
    WHERE (item->>''returncode'')::integer=0;
  END IF;
 ELSE
  kind:=''verification'';
 END IF;
 event:=forge_agent.retry_event_observe(d.project_alias,''delivery'',d.lineage_id,kind,d.verification_id::text,evidence,passed,phase,passed_phases);
 INSERT INTO forge_agent.delivery_retry_event_links(verification_id,event_id) VALUES(d.verification_id,(event->>''event_id'')::uuid);
 RETURN event;
END;';definition text;
BEGIN
 IF (SELECT encode(sha256(convert_to(prosrc,'UTF8')),'hex') FROM pg_proc WHERE oid=signature::regprocedure)<>'b4b8717b1afe207a84979e0e5a216dbe1b4802afffd69ae65a0bbfd20d9bc9d1' THEN RAISE EXCEPTION 'M56 predecessor function drift: %',signature;END IF;
 definition:=pg_get_functiondef(signature::regprocedure);
 IF (length(definition)-length(replace(definition,before,'')))/length(before)<>1 THEN RAISE EXCEPTION 'M56 source target not unique: %',signature;END IF;
 EXECUTE replace(definition,before,after);
END;$native_v4_patch$;
DO $native_v4_patch$
DECLARE signature text:='forge_coding_reconciler_api.native_merge_failure(uuid,uuid,jsonb)';before text:='
DECLARE a forge_agent.coding_workflow_actions%ROWTYPE;w forge_agent.coding_workflows%ROWTYPE;
 p forge_agent.coding_publications%ROWTYPE;q forge_agent.coding_reviews%ROWTYPE;
 t forge_agent.coding_tasks%ROWTYPE;seed uuid;feedback uuid;child uuid;receipt jsonb;checks jsonb;owner_fenced boolean;
BEGIN
 SELECT * INTO a FROM forge_agent.coding_workflow_actions WHERE action_id=p_merge
  AND owner_id=current_setting(''forge.coding_owner'',true) AND kind=''merge'';
 IF NOT FOUND THEN RAISE EXCEPTION ''Native merge source denied'' USING ERRCODE=''42501'';END IF;
 PERFORM pg_advisory_xact_lock(hashtextextended(a.owner_id||E''\n''||a.project_alias,2026111901));
 SELECT * INTO w FROM forge_agent.coding_workflows WHERE workflow_id=a.workflow_id FOR UPDATE;
 PERFORM 1 FROM forge_agent.coding_tasks WHERE id=w.root_task_id AND owner_id=a.owner_id FOR UPDATE;
 SELECT x.* INTO t FROM forge_agent.coding_tasks x JOIN forge_agent.coding_publications y
  ON(y.owner_id,y.project_alias,y.task_id)=(x.owner_id,x.project_alias,x.id)
  WHERE y.publication_id=a.source_publication_id FOR UPDATE OF x;
 SELECT * INTO a FROM forge_agent.coding_workflow_actions WHERE action_id=p_merge FOR UPDATE;
 IF a.finish_token=p_token AND a.finish_receipt->>''policy''=''forge-native-delivery-repair-v1'' THEN
  IF a.finish_receipt->''failure'' IS DISTINCT FROM p_failure THEN
   RAISE EXCEPTION ''Native merge failure conflict'' USING ERRCODE=''P0006'';END IF;
  RETURN a.finish_receipt;
 END IF;
 IF p_token IS NULL OR a.status<>''leased'' OR a.lease_token IS DISTINCT FROM p_token
  OR a.lease_expires_at<=clock_timestamp() OR NOT forge_agent.coding_workflow_merge_authorized(p_merge)
  OR NOT w.enabled OR w.phase IN(''completed'',''cancelled'',''escalated'') THEN
  RAISE EXCEPTION ''Native merge lease denied'' USING ERRCODE=''P0008'';END IF;
 SELECT * INTO p FROM forge_agent.coding_publications WHERE publication_id=a.source_publication_id FOR UPDATE;
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p.review_id FOR UPDATE;
 IF (jsonb_typeof(p_failure)<>''object'' OR octet_length(p_failure::text)>32768
  OR p_failure-ARRAY[''version'',''reason'',''summary'',''source_review_id'',''candidate_commit'',''candidate_workspace_digest'',
   ''integration_base'',''gate_manifest_sha256'',''gate_source_sha256'',''tested_candidate_commit'',''evidence_digest'',''findings'',''build'',''no_merge_confirmed'',''delivery_failure'']<>''{}''::jsonb
  OR NOT p_failure?&ARRAY[''version'',''reason'',''summary'',''source_review_id'',''candidate_commit'',''candidate_workspace_digest'',
   ''integration_base'',''gate_manifest_sha256'',''gate_source_sha256'',''tested_candidate_commit'',''evidence_digest'',''findings'',''build'',''no_merge_confirmed'',''delivery_failure'']
  OR p_failure->''version''<>''1''::jsonb OR p_failure->''no_merge_confirmed''<>''true''::jsonb
  OR p_failure->>''reason'' NOT IN(''merge_main_requires_reverification'',''merge_source_repair_required'')
  OR jsonb_typeof(p_failure->''summary'')<>''string'' OR octet_length(p_failure->>''summary'') NOT BETWEEN 1 AND 2048
  OR p_failure->>''source_review_id''!~''^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$''
  OR p_failure->>''candidate_commit''!~''^[0-9a-f]{40}$'' OR p_failure->>''integration_base''!~''^[0-9a-f]{40}$''
  OR p_failure->>''tested_candidate_commit'' IS DISTINCT FROM q.candidate_commit
  OR p_failure->>''candidate_commit''=q.candidate_commit
  OR EXISTS(SELECT FROM jsonb_each(p_failure) x WHERE x.key IN(''candidate_workspace_digest'',''gate_manifest_sha256'',''gate_source_sha256'',''evidence_digest'')
    AND(jsonb_typeof(x.value)<>''string'' OR x.value#>>''{}''!~CASE WHEN x.key=''gate_manifest_sha256'' THEN ''^([0-9a-f]{64}|forge-sdlc-jenkins-v[1-3]:(main|detached|branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?))$'' ELSE ''^[0-9a-f]{64}$'' END))
  OR jsonb_typeof(p_failure->''findings'')<>''array'' OR jsonb_array_length(p_failure->''findings'') NOT BETWEEN 1 AND 3
  OR jsonb_typeof(p_failure->''build'')<>''object''
  OR (p_failure->''build'')-ARRAY[''provider'',''job'',''action_id'',''queue_id'',''build_number'']<>''{}''::jsonb
  OR NOT(p_failure->''build'')?&ARRAY[''provider'',''job'',''action_id'',''queue_id'',''build_number'']
  OR p_failure->''build''->>''provider''<>''jenkins'' OR p_failure->''build''->>''job''<>''forge-native-candidate''
  OR p_failure->''build''->>''action_id''!~''^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$''
  OR jsonb_typeof(p_failure->''build''->''build_number'')<>''number''
  OR p_failure->''build''->>''build_number''!~''^[1-9][0-9]{0,12}$''
  OR(jsonb_typeof(p_failure->''build''->''queue_id'')<>''null'' AND
   (jsonb_typeof(p_failure->''build''->''queue_id'')<>''number'' OR p_failure->''build''->>''queue_id''!~''^[1-9][0-9]{0,12}$''))
  OR jsonb_typeof(p_failure->''delivery_failure'')<>''object''
  OR(p_failure->''delivery_failure'')-ARRAY[''version'',''tested_candidate'',''current_main'',''build'',''conflict_paths'']<>''{}''::jsonb
  OR NOT(p_failure->''delivery_failure'')?&ARRAY[''version'',''tested_candidate'',''current_main'',''build'',''conflict_paths'']
  OR p_failure->''delivery_failure''->''version''<>''1''::jsonb
  OR p_failure->''delivery_failure''->>''tested_candidate'' IS DISTINCT FROM q.candidate_commit
  OR p_failure->''delivery_failure''->>''current_main'' IS DISTINCT FROM p_failure->>''integration_base''
  OR p_failure->''delivery_failure''->''build'' IS DISTINCT FROM p_failure->''build''
  OR jsonb_typeof(p_failure->''delivery_failure''->''conflict_paths'')<>''array''
  OR jsonb_array_length(p_failure->''delivery_failure''->''conflict_paths'')>32
 ) IS DISTINCT FROM false THEN RAISE EXCEPTION ''Invalid native merge failure'' USING ERRCODE=''22023'';END IF;
 IF EXISTS(SELECT FROM jsonb_array_elements(p_failure->''delivery_failure''->''conflict_paths'') x
  WHERE(jsonb_typeof(x)<>''string'' OR octet_length(x#>>''{}'') NOT BETWEEN 1 AND 1024
   OR x#>>''{}''~''(^/|(^|/)\.\.?(/|$)|[[:cntrl:]])'') IS DISTINCT FROM false) THEN
  RAISE EXCEPTION ''Invalid native conflict path'' USING ERRCODE=''22023'';END IF;
 IF EXISTS(SELECT FROM jsonb_array_elements(p_failure->''findings'') f WHERE(jsonb_typeof(f)<>''object''
  OR NOT f?&ARRAY[''finding_id'',''category'',''summary'']
  OR f-ARRAY[''finding_id'',''category'',''summary'',''check_id'',''path'',''diagnostic_code'',''reproduction'',''expected'',''actual'',''evidence_digest'',''status'']<>''{}''::jsonb
  OR f->>''finding_id''!~''^[A-Za-z0-9_.-]{1,64}$'' OR f->>''category'' NOT IN(''source'',''infrastructure'')
  OR jsonb_typeof(f->''summary'')<>''string'' OR octet_length(f->>''summary'') NOT BETWEEN 1 AND 2048
  OR coalesce(f->>''status'',''open'')<>''open''
  OR EXISTS(SELECT FROM jsonb_each(f) x WHERE x.key IN(''check_id'',''path'',''diagnostic_code'',''reproduction'',''expected'',''actual'',''evidence_digest'',''status'')
   AND(jsonb_typeof(x.value)<>''string'' OR octet_length(x.value#>>''{}'')>CASE WHEN x.key IN(''check_id'',''diagnostic_code'') THEN 128 WHEN x.key=''path'' THEN 1024 ELSE 2048 END))
  OR(f?''evidence_digest'' AND f->>''evidence_digest''<>'''' AND f->>''evidence_digest''!~''^[0-9a-f]{64}$'')
 ) IS DISTINCT FROM false) OR EXISTS(SELECT FROM jsonb_array_elements(p_failure->''findings'') f GROUP BY f->>''finding_id'' HAVING count(*)>1) THEN
  RAISE EXCEPTION ''Invalid native repair finding'' USING ERRCODE=''22023'';END IF;
 SELECT h.checks INTO checks FROM forge_agent.coding_hosted_checks h WHERE h.publication_id=p.publication_id
  AND h.candidate_commit=q.candidate_commit AND h.required_green ORDER BY h.observed_at DESC,h.observation_id DESC LIMIT 1;
 IF jsonb_array_length(checks) IS DISTINCT FROM 3 OR EXISTS(SELECT FROM jsonb_array_elements(checks) c
  WHERE c->>''provider''<>''jenkins'' OR c->>''job'' IS DISTINCT FROM p_failure->''build''->>''job''
   OR c->>''head_sha'' IS DISTINCT FROM q.candidate_commit
   OR c->>''status''<>''success'' OR c->>''action_id'' IS DISTINCT FROM p_failure->''build''->>''action_id''
   OR c->''queue_id'' IS DISTINCT FROM p_failure->''build''->''queue_id''
   OR c->''build_number'' IS DISTINCT FROM p_failure->''build''->''build_number'') THEN
  RAISE EXCEPTION ''Native repair build evidence changed'' USING ERRCODE=''P0006'';END IF;
 IF t.latest_attempt_id IS DISTINCT FROM q.attempt_id OR t.status<>''awaiting_review''
  OR q.repair_task_id IS NOT NULL OR q.verification->''cleanup_confirmed'' IS DISTINCT FROM ''true''::jsonb
  OR EXISTS(SELECT FROM forge_agent.coding_tasks x WHERE x.workflow_id=w.workflow_id AND x.status IN(''queued'',''running'',''cancel_requested'')) THEN
  RAISE EXCEPTION ''Native repair source superseded'' USING ERRCODE=''P0009'';END IF;
 owner_fenced:=forge_agent._coding_owner_continuation_fenced(t.id);
 seed:=(p_failure->>''source_review_id'')::uuid;
 INSERT INTO forge_agent.coding_reviews(review_id,owner_id,project_alias,task_id,attempt_id,plan_decision_id,scope_plan_decision_id,
  reconciliation_receipt_id,base_revision,workspace_digest,parent_result_sha256,source_revision,review_no,round,stagnant_rounds,
  infrastructure_retries,previous_review_id,status,candidate_commit,gate_manifest_sha256,findings,verification,summary,reason,phase,owner_decision,
  target_publication_id,source_publication_id)
 VALUES(seed,q.owner_id,q.project_alias,q.task_id,q.attempt_id,q.plan_decision_id,q.scope_plan_decision_id,q.reconciliation_receipt_id,
  p_failure->>''integration_base'',q.workspace_digest,q.parent_result_sha256,t.revision,
  (SELECT max(review_no)+1 FROM forge_agent.coding_reviews WHERE owner_id=q.owner_id AND project_alias=q.project_alias AND task_id=q.task_id),
  q.round,q.stagnant_rounds,q.infrastructure_retries,q.review_id,CASE WHEN owner_fenced THEN ''handoff'' ELSE ''repair'' END,p_failure->>''candidate_commit'',p_failure->>''gate_manifest_sha256'',
  p_failure->''findings'',jsonb_build_object(''source_import'',true,''checks_passed'',false,''cleanup_confirmed'',true,''checks'',''[]''::jsonb,
   ''candidate_workspace_digest'',p_failure->>''candidate_workspace_digest'',''repair_requires_new_workspace'',true,
   ''integration_base'',p_failure->>''integration_base'',''gate_source_sha256'',p_failure->>''gate_source_sha256'',''delivery_failure'',p_failure->''delivery_failure''),
  p_failure->>''summary'',CASE WHEN owner_fenced THEN ''owner_continuation_fenced'' ELSE p_failure->>''reason'' END,''source'',jsonb_build_object(''decision'',''repair_source'',''closed_by'',''native_merge_failure'',
   ''merge_id'',a.action_id,''tested_review_id'',q.review_id),NULL,p.publication_id);
 feedback:=forge_agent.coding_make_feedback(seed,t.id,''repair'',p_failure->>''summary'',p_failure->''findings'');
 UPDATE forge_agent.coding_reviews SET feedback_id=feedback WHERE review_id=seed;
 IF NOT owner_fenced THEN child:=forge_agent.coding_schedule_repair(seed,feedback);END IF;
 -- Supersede delivery, while retaining the old review''s approval and receipt.
 UPDATE forge_agent.coding_reviews SET repair_task_id=child WHERE review_id=q.review_id;
 receipt:=jsonb_build_object(''policy'',''forge-native-delivery-repair-v1'',''status'',CASE WHEN owner_fenced THEN ''handoff'' ELSE ''repair'' END,''merge_id'',a.action_id,
  ''workflow_id'',w.workflow_id,''source_review_id'',seed,''feedback_id'',feedback,''repair_task_id'',child,''failure'',p_failure);
 IF owner_fenced THEN receipt:=receipt||jsonb_build_object(''reason'',''owner_continuation_fenced'');END IF;
 UPDATE forge_agent.coding_workflow_actions SET status=''handoff'',finish_token=p_token,finish_receipt=receipt,
  lease_token=NULL,lease_expires_at=NULL,updated_at=clock_timestamp() WHERE action_id=a.action_id;
 IF owner_fenced THEN
  UPDATE forge_agent.coding_workflows SET enabled=false,phase=''escalated'',escalation_reason=''Owner continuation failed delivery; no further automatic writer is authorized.'',updated_at=clock_timestamp() WHERE workflow_id=w.workflow_id;
 ELSE
  UPDATE forge_agent.coding_workflows SET phase=''repair'',updated_at=clock_timestamp() WHERE workflow_id=w.workflow_id;
 END IF;
 INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
 SELECT t.owner_id,t.project_alias,t.id,coalesce(max(sequence),0)+1,''checkpoint'',receipt||jsonb_build_object(''actor'',jsonb_build_object(''kind'', ''service'', ''id'', ''forge-coding-reconciler''))
 FROM forge_agent.coding_events WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND task_id=t.id;
 PERFORM pg_notify(''forge_coding_jobs'','''');RETURN receipt;
END;
';after text:='
DECLARE a forge_agent.coding_workflow_actions%ROWTYPE;w forge_agent.coding_workflows%ROWTYPE;
 p forge_agent.coding_publications%ROWTYPE;q forge_agent.coding_reviews%ROWTYPE;
 t forge_agent.coding_tasks%ROWTYPE;seed uuid;feedback uuid;child uuid;receipt jsonb;checks jsonb;owner_fenced boolean;
BEGIN
 SELECT * INTO a FROM forge_agent.coding_workflow_actions WHERE action_id=p_merge
  AND owner_id=current_setting(''forge.coding_owner'',true) AND kind=''merge'';
 IF NOT FOUND THEN RAISE EXCEPTION ''Native merge source denied'' USING ERRCODE=''42501'';END IF;
 PERFORM pg_advisory_xact_lock(hashtextextended(a.owner_id||E''\n''||a.project_alias,2026111901));
 SELECT * INTO w FROM forge_agent.coding_workflows WHERE workflow_id=a.workflow_id FOR UPDATE;
 PERFORM 1 FROM forge_agent.coding_tasks WHERE id=w.root_task_id AND owner_id=a.owner_id FOR UPDATE;
 SELECT x.* INTO t FROM forge_agent.coding_tasks x JOIN forge_agent.coding_publications y
  ON(y.owner_id,y.project_alias,y.task_id)=(x.owner_id,x.project_alias,x.id)
  WHERE y.publication_id=a.source_publication_id FOR UPDATE OF x;
 SELECT * INTO a FROM forge_agent.coding_workflow_actions WHERE action_id=p_merge FOR UPDATE;
 IF a.finish_token=p_token AND a.finish_receipt->>''policy''=''forge-native-delivery-repair-v1'' THEN
  IF a.finish_receipt->''failure'' IS DISTINCT FROM p_failure THEN
   RAISE EXCEPTION ''Native merge failure conflict'' USING ERRCODE=''P0006'';END IF;
  RETURN a.finish_receipt;
 END IF;
 IF p_token IS NULL OR a.status<>''leased'' OR a.lease_token IS DISTINCT FROM p_token
  OR a.lease_expires_at<=clock_timestamp() OR NOT forge_agent.coding_workflow_merge_authorized(p_merge)
  OR NOT w.enabled OR w.phase IN(''completed'',''cancelled'',''escalated'') THEN
  RAISE EXCEPTION ''Native merge lease denied'' USING ERRCODE=''P0008'';END IF;
 SELECT * INTO p FROM forge_agent.coding_publications WHERE publication_id=a.source_publication_id FOR UPDATE;
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p.review_id FOR UPDATE;
 IF (jsonb_typeof(p_failure)<>''object'' OR octet_length(p_failure::text)>32768
  OR p_failure-ARRAY[''version'',''reason'',''summary'',''source_review_id'',''candidate_commit'',''candidate_workspace_digest'',
   ''integration_base'',''gate_manifest_sha256'',''gate_source_sha256'',''tested_candidate_commit'',''evidence_digest'',''findings'',''build'',''no_merge_confirmed'',''delivery_failure'']<>''{}''::jsonb
  OR NOT p_failure?&ARRAY[''version'',''reason'',''summary'',''source_review_id'',''candidate_commit'',''candidate_workspace_digest'',
   ''integration_base'',''gate_manifest_sha256'',''gate_source_sha256'',''tested_candidate_commit'',''evidence_digest'',''findings'',''build'',''no_merge_confirmed'',''delivery_failure'']
  OR p_failure->''version''<>''1''::jsonb OR p_failure->''no_merge_confirmed''<>''true''::jsonb
  OR p_failure->>''reason'' NOT IN(''merge_main_requires_reverification'',''merge_source_repair_required'')
  OR jsonb_typeof(p_failure->''summary'')<>''string'' OR octet_length(p_failure->>''summary'') NOT BETWEEN 1 AND 2048
  OR p_failure->>''source_review_id''!~''^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$''
  OR p_failure->>''candidate_commit''!~''^[0-9a-f]{40}$'' OR p_failure->>''integration_base''!~''^[0-9a-f]{40}$''
  OR p_failure->>''tested_candidate_commit'' IS DISTINCT FROM q.candidate_commit
  OR p_failure->>''candidate_commit''=q.candidate_commit
  OR EXISTS(SELECT FROM jsonb_each(p_failure) x WHERE x.key IN(''candidate_workspace_digest'',''gate_manifest_sha256'',''gate_source_sha256'',''evidence_digest'')
    AND(jsonb_typeof(x.value)<>''string'' OR x.value#>>''{}''!~CASE WHEN x.key=''gate_manifest_sha256'' THEN ''^([0-9a-f]{64}|forge-sdlc-jenkins-v[1-4]:(main|detached|branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?))$'' ELSE ''^[0-9a-f]{64}$'' END))
  OR jsonb_typeof(p_failure->''findings'')<>''array'' OR jsonb_array_length(p_failure->''findings'') NOT BETWEEN 1 AND 3
  OR jsonb_typeof(p_failure->''build'')<>''object''
  OR (p_failure->''build'')-ARRAY[''provider'',''job'',''action_id'',''queue_id'',''build_number'']<>''{}''::jsonb
  OR NOT(p_failure->''build'')?&ARRAY[''provider'',''job'',''action_id'',''queue_id'',''build_number'']
  OR p_failure->''build''->>''provider''<>''jenkins'' OR p_failure->''build''->>''job''<>''forge-native-candidate''
  OR p_failure->''build''->>''action_id''!~''^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$''
  OR jsonb_typeof(p_failure->''build''->''build_number'')<>''number''
  OR p_failure->''build''->>''build_number''!~''^[1-9][0-9]{0,12}$''
  OR(jsonb_typeof(p_failure->''build''->''queue_id'')<>''null'' AND
   (jsonb_typeof(p_failure->''build''->''queue_id'')<>''number'' OR p_failure->''build''->>''queue_id''!~''^[1-9][0-9]{0,12}$''))
  OR jsonb_typeof(p_failure->''delivery_failure'')<>''object''
  OR(p_failure->''delivery_failure'')-ARRAY[''version'',''tested_candidate'',''current_main'',''build'',''conflict_paths'']<>''{}''::jsonb
  OR NOT(p_failure->''delivery_failure'')?&ARRAY[''version'',''tested_candidate'',''current_main'',''build'',''conflict_paths'']
  OR p_failure->''delivery_failure''->''version''<>''1''::jsonb
  OR p_failure->''delivery_failure''->>''tested_candidate'' IS DISTINCT FROM q.candidate_commit
  OR p_failure->''delivery_failure''->>''current_main'' IS DISTINCT FROM p_failure->>''integration_base''
  OR p_failure->''delivery_failure''->''build'' IS DISTINCT FROM p_failure->''build''
  OR jsonb_typeof(p_failure->''delivery_failure''->''conflict_paths'')<>''array''
  OR jsonb_array_length(p_failure->''delivery_failure''->''conflict_paths'')>32
 ) IS DISTINCT FROM false THEN RAISE EXCEPTION ''Invalid native merge failure'' USING ERRCODE=''22023'';END IF;
 IF EXISTS(SELECT FROM jsonb_array_elements(p_failure->''delivery_failure''->''conflict_paths'') x
  WHERE(jsonb_typeof(x)<>''string'' OR octet_length(x#>>''{}'') NOT BETWEEN 1 AND 1024
   OR x#>>''{}''~''(^/|(^|/)\.\.?(/|$)|[[:cntrl:]])'') IS DISTINCT FROM false) THEN
  RAISE EXCEPTION ''Invalid native conflict path'' USING ERRCODE=''22023'';END IF;
 IF EXISTS(SELECT FROM jsonb_array_elements(p_failure->''findings'') f WHERE(jsonb_typeof(f)<>''object''
  OR NOT f?&ARRAY[''finding_id'',''category'',''summary'']
  OR f-ARRAY[''finding_id'',''category'',''summary'',''check_id'',''path'',''diagnostic_code'',''reproduction'',''expected'',''actual'',''evidence_digest'',''status'']<>''{}''::jsonb
  OR f->>''finding_id''!~''^[A-Za-z0-9_.-]{1,64}$'' OR f->>''category'' NOT IN(''source'',''infrastructure'')
  OR jsonb_typeof(f->''summary'')<>''string'' OR octet_length(f->>''summary'') NOT BETWEEN 1 AND 2048
  OR coalesce(f->>''status'',''open'')<>''open''
  OR EXISTS(SELECT FROM jsonb_each(f) x WHERE x.key IN(''check_id'',''path'',''diagnostic_code'',''reproduction'',''expected'',''actual'',''evidence_digest'',''status'')
   AND(jsonb_typeof(x.value)<>''string'' OR octet_length(x.value#>>''{}'')>CASE WHEN x.key IN(''check_id'',''diagnostic_code'') THEN 128 WHEN x.key=''path'' THEN 1024 ELSE 2048 END))
  OR(f?''evidence_digest'' AND f->>''evidence_digest''<>'''' AND f->>''evidence_digest''!~''^[0-9a-f]{64}$'')
 ) IS DISTINCT FROM false) OR EXISTS(SELECT FROM jsonb_array_elements(p_failure->''findings'') f GROUP BY f->>''finding_id'' HAVING count(*)>1) THEN
  RAISE EXCEPTION ''Invalid native repair finding'' USING ERRCODE=''22023'';END IF;
 SELECT h.checks INTO checks FROM forge_agent.coding_hosted_checks h WHERE h.publication_id=p.publication_id
  AND h.candidate_commit=q.candidate_commit AND h.required_green ORDER BY h.observed_at DESC,h.observation_id DESC LIMIT 1;
 IF jsonb_array_length(checks) IS DISTINCT FROM 3 OR EXISTS(SELECT FROM jsonb_array_elements(checks) c
  WHERE c->>''provider''<>''jenkins'' OR c->>''job'' IS DISTINCT FROM p_failure->''build''->>''job''
   OR c->>''head_sha'' IS DISTINCT FROM q.candidate_commit
   OR c->>''status''<>''success'' OR c->>''action_id'' IS DISTINCT FROM p_failure->''build''->>''action_id''
   OR c->''queue_id'' IS DISTINCT FROM p_failure->''build''->''queue_id''
   OR c->''build_number'' IS DISTINCT FROM p_failure->''build''->''build_number'') THEN
  RAISE EXCEPTION ''Native repair build evidence changed'' USING ERRCODE=''P0006'';END IF;
 IF t.latest_attempt_id IS DISTINCT FROM q.attempt_id OR t.status<>''awaiting_review''
  OR q.repair_task_id IS NOT NULL OR q.verification->''cleanup_confirmed'' IS DISTINCT FROM ''true''::jsonb
  OR EXISTS(SELECT FROM forge_agent.coding_tasks x WHERE x.workflow_id=w.workflow_id AND x.status IN(''queued'',''running'',''cancel_requested'')) THEN
  RAISE EXCEPTION ''Native repair source superseded'' USING ERRCODE=''P0009'';END IF;
 owner_fenced:=forge_agent._coding_owner_continuation_fenced(t.id);
 seed:=(p_failure->>''source_review_id'')::uuid;
 INSERT INTO forge_agent.coding_reviews(review_id,owner_id,project_alias,task_id,attempt_id,plan_decision_id,scope_plan_decision_id,
  reconciliation_receipt_id,base_revision,workspace_digest,parent_result_sha256,source_revision,review_no,round,stagnant_rounds,
  infrastructure_retries,previous_review_id,status,candidate_commit,gate_manifest_sha256,findings,verification,summary,reason,phase,owner_decision,
  target_publication_id,source_publication_id)
 VALUES(seed,q.owner_id,q.project_alias,q.task_id,q.attempt_id,q.plan_decision_id,q.scope_plan_decision_id,q.reconciliation_receipt_id,
  p_failure->>''integration_base'',q.workspace_digest,q.parent_result_sha256,t.revision,
  (SELECT max(review_no)+1 FROM forge_agent.coding_reviews WHERE owner_id=q.owner_id AND project_alias=q.project_alias AND task_id=q.task_id),
  q.round,q.stagnant_rounds,q.infrastructure_retries,q.review_id,CASE WHEN owner_fenced THEN ''handoff'' ELSE ''repair'' END,p_failure->>''candidate_commit'',p_failure->>''gate_manifest_sha256'',
  p_failure->''findings'',jsonb_build_object(''source_import'',true,''checks_passed'',false,''cleanup_confirmed'',true,''checks'',''[]''::jsonb,
   ''candidate_workspace_digest'',p_failure->>''candidate_workspace_digest'',''repair_requires_new_workspace'',true,
   ''integration_base'',p_failure->>''integration_base'',''gate_source_sha256'',p_failure->>''gate_source_sha256'',''delivery_failure'',p_failure->''delivery_failure''),
  p_failure->>''summary'',CASE WHEN owner_fenced THEN ''owner_continuation_fenced'' ELSE p_failure->>''reason'' END,''source'',jsonb_build_object(''decision'',''repair_source'',''closed_by'',''native_merge_failure'',
   ''merge_id'',a.action_id,''tested_review_id'',q.review_id),NULL,p.publication_id);
 feedback:=forge_agent.coding_make_feedback(seed,t.id,''repair'',p_failure->>''summary'',p_failure->''findings'');
 UPDATE forge_agent.coding_reviews SET feedback_id=feedback WHERE review_id=seed;
 IF NOT owner_fenced THEN child:=forge_agent.coding_schedule_repair(seed,feedback);END IF;
 -- Supersede delivery, while retaining the old review''s approval and receipt.
 UPDATE forge_agent.coding_reviews SET repair_task_id=child WHERE review_id=q.review_id;
 receipt:=jsonb_build_object(''policy'',''forge-native-delivery-repair-v1'',''status'',CASE WHEN owner_fenced THEN ''handoff'' ELSE ''repair'' END,''merge_id'',a.action_id,
  ''workflow_id'',w.workflow_id,''source_review_id'',seed,''feedback_id'',feedback,''repair_task_id'',child,''failure'',p_failure);
 IF owner_fenced THEN receipt:=receipt||jsonb_build_object(''reason'',''owner_continuation_fenced'');END IF;
 UPDATE forge_agent.coding_workflow_actions SET status=''handoff'',finish_token=p_token,finish_receipt=receipt,
  lease_token=NULL,lease_expires_at=NULL,updated_at=clock_timestamp() WHERE action_id=a.action_id;
 IF owner_fenced THEN
  UPDATE forge_agent.coding_workflows SET enabled=false,phase=''escalated'',escalation_reason=''Owner continuation failed delivery; no further automatic writer is authorized.'',updated_at=clock_timestamp() WHERE workflow_id=w.workflow_id;
 ELSE
  UPDATE forge_agent.coding_workflows SET phase=''repair'',updated_at=clock_timestamp() WHERE workflow_id=w.workflow_id;
 END IF;
 INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
 SELECT t.owner_id,t.project_alias,t.id,coalesce(max(sequence),0)+1,''checkpoint'',receipt||jsonb_build_object(''actor'',jsonb_build_object(''kind'', ''service'', ''id'', ''forge-coding-reconciler''))
 FROM forge_agent.coding_events WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND task_id=t.id;
 PERFORM pg_notify(''forge_coding_jobs'','''');RETURN receipt;
END;
';definition text;
BEGIN
 IF (SELECT encode(sha256(convert_to(prosrc,'UTF8')),'hex') FROM pg_proc WHERE oid=signature::regprocedure)<>'a4aa27c70b362780b6ffe79b4304e331ed8611691411a56de3b7d5e869408140' THEN RAISE EXCEPTION 'M56 predecessor function drift: %',signature;END IF;
 definition:=pg_get_functiondef(signature::regprocedure);
 IF (length(definition)-length(replace(definition,before,'')))/length(before)<>1 THEN RAISE EXCEPTION 'M56 source target not unique: %',signature;END IF;
 EXECUTE replace(definition,before,after);
END;$native_v4_patch$;
DO $native_v4_patch$
DECLARE signature text:='forge_coding_reconciler_api.post_merge_checkpoint(uuid,integer,uuid,jsonb)';before text:='
DECLARE a forge_agent.post_merge_verifications%ROWTYPE;keys text[]:=ARRAY[''action_id'',''prepare_action_id'',''workflow_id'',''project_alias'',''candidate_commit'',''candidate_tree'',''integration_base'',''gate_manifest_sha256'',''branch_scope'',''source_branch'',''verification_profile'',''correlation_id''];
BEGIN
 a:=forge_agent.post_merge_lock(p_id);
 PERFORM forge_coding_reconciler_api.post_merge_renew(p_id,p_generation,p_token);
 IF NOT forge_coding_reconciler_api.post_merge_start_allowed(p_id,p_generation,p_token) THEN RAISE EXCEPTION ''Post merge start permission elapsed'' USING ERRCODE=''P0008'';END IF;
 IF jsonb_typeof(p_request) IS DISTINCT FROM ''object'' OR p_request-keys<>''{}''::jsonb OR NOT p_request?&keys
 OR octet_length(p_request::text)>65536 OR EXISTS(SELECT FROM jsonb_each(p_request) x WHERE jsonb_typeof(x.value)<>''string'')
 OR p_request->>''action_id'' IS DISTINCT FROM a.post_merge_id::text OR p_request->>''workflow_id'' IS DISTINCT FROM a.workflow_id::text
 OR p_request->>''project_alias'' IS DISTINCT FROM a.project_alias OR p_request->>''correlation_id'' IS DISTINCT FROM a.correlation_id::text
 OR p_request->>''candidate_commit'' IS DISTINCT FROM a.merge_commit OR p_request->>''integration_base'' IS DISTINCT FROM a.merge_commit
 OR p_request->>''candidate_tree'' IS DISTINCT FROM a.merge_tree OR p_request->>''branch_scope'' IS DISTINCT FROM ''main''
 OR p_request->>''source_branch'' IS DISTINCT FROM ''main'' OR (p_request->>''verification_profile'' IN(''forge-sdlc-jenkins-v2'',''forge-sdlc-jenkins-v3'')) IS DISTINCT FROM true
 OR p_request->>''gate_manifest_sha256''!~''^([0-9a-f]{64}|forge-sdlc-jenkins-v[1-3]:(main|detached|branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?))$'' OR p_request->>''prepare_action_id''!~''^[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$''
 THEN RAISE EXCEPTION ''Invalid exact main verification request'' USING ERRCODE=''22023'';END IF;
 IF a.request IS NOT NULL AND a.request IS DISTINCT FROM p_request THEN RAISE EXCEPTION ''Post merge request conflict'' USING ERRCODE=''P0009'';END IF;
 UPDATE forge_agent.post_merge_verifications SET request=p_request,updated_at=clock_timestamp() WHERE post_merge_id=p_id;RETURN true;
END;';after text:='
DECLARE a forge_agent.post_merge_verifications%ROWTYPE;keys text[]:=ARRAY[''action_id'',''prepare_action_id'',''workflow_id'',''project_alias'',''candidate_commit'',''candidate_tree'',''integration_base'',''gate_manifest_sha256'',''branch_scope'',''source_branch'',''verification_profile'',''correlation_id''];
BEGIN
 a:=forge_agent.post_merge_lock(p_id);
 PERFORM forge_coding_reconciler_api.post_merge_renew(p_id,p_generation,p_token);
 IF NOT forge_coding_reconciler_api.post_merge_start_allowed(p_id,p_generation,p_token) THEN RAISE EXCEPTION ''Post merge start permission elapsed'' USING ERRCODE=''P0008'';END IF;
 IF jsonb_typeof(p_request) IS DISTINCT FROM ''object'' OR p_request-keys<>''{}''::jsonb OR NOT p_request?&keys
 OR octet_length(p_request::text)>65536 OR EXISTS(SELECT FROM jsonb_each(p_request) x WHERE jsonb_typeof(x.value)<>''string'')
 OR p_request->>''action_id'' IS DISTINCT FROM a.post_merge_id::text OR p_request->>''workflow_id'' IS DISTINCT FROM a.workflow_id::text
 OR p_request->>''project_alias'' IS DISTINCT FROM a.project_alias OR p_request->>''correlation_id'' IS DISTINCT FROM a.correlation_id::text
 OR p_request->>''candidate_commit'' IS DISTINCT FROM a.merge_commit OR p_request->>''integration_base'' IS DISTINCT FROM a.merge_commit
 OR p_request->>''candidate_tree'' IS DISTINCT FROM a.merge_tree OR p_request->>''branch_scope'' IS DISTINCT FROM ''main''
 OR p_request->>''source_branch'' IS DISTINCT FROM ''main'' OR (p_request->>''verification_profile'' IN(''forge-sdlc-jenkins-v2'',''forge-sdlc-jenkins-v3'',''forge-sdlc-jenkins-v4'')) IS DISTINCT FROM true
 OR p_request->>''gate_manifest_sha256''!~''^([0-9a-f]{64}|forge-sdlc-jenkins-v[1-4]:(main|detached|branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?))$'' OR p_request->>''prepare_action_id''!~''^[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$''
 THEN RAISE EXCEPTION ''Invalid exact main verification request'' USING ERRCODE=''22023'';END IF;
 IF a.request IS NOT NULL AND a.request IS DISTINCT FROM p_request THEN RAISE EXCEPTION ''Post merge request conflict'' USING ERRCODE=''P0009'';END IF;
 UPDATE forge_agent.post_merge_verifications SET request=p_request,updated_at=clock_timestamp() WHERE post_merge_id=p_id;RETURN true;
END;';definition text;
BEGIN
 IF (SELECT encode(sha256(convert_to(prosrc,'UTF8')),'hex') FROM pg_proc WHERE oid=signature::regprocedure)<>'107d2150a26ec825c246511db3ba8456212658d84bb420760c32652223ce836c' THEN RAISE EXCEPTION 'M56 predecessor function drift: %',signature;END IF;
 definition:=pg_get_functiondef(signature::regprocedure);
 IF (length(definition)-length(replace(definition,before,'')))/length(before)<>1 THEN RAISE EXCEPTION 'M56 source target not unique: %',signature;END IF;
 EXECUTE replace(definition,before,after);
END;$native_v4_patch$;
DO $native_v4_patch$
DECLARE signature text:='forge_coding_reconciler_api.post_merge_finish(uuid,integer,uuid,text,jsonb,text)';before text:='
DECLARE a forge_agent.post_merge_verifications%ROWTYPE;value jsonb;identity jsonb;own_verification boolean;own_cancel boolean;
BEGIN
 a:=forge_agent.post_merge_lock(p_id);value:=jsonb_build_object(''status'',p_status,''receipt'',p_receipt,''summary'',p_summary);
 IF a.finish_token=p_token AND a.finish_generation=p_generation AND a.finish_role=session_user THEN
  IF a.finish_value IS DISTINCT FROM value THEN RAISE EXCEPTION ''Post merge outcome conflict'' USING ERRCODE=''P0009'';END IF;RETURN true;END IF;
 PERFORM forge_coding_reconciler_api.post_merge_renew(p_id,p_generation,p_token);
 IF p_status IS NULL OR p_status NOT IN(''pending'',''passed'',''failed'',''superseded'') OR jsonb_typeof(p_receipt) IS DISTINCT FROM ''object''
 OR octet_length(p_receipt::text)>262144 OR p_summary IS NULL OR octet_length(p_summary)>4096
 THEN RAISE EXCEPTION ''Invalid post merge result'' USING ERRCODE=''22023'';END IF;
 IF p_status<>''pending'' AND p_receipt->''cleanup_confirmed'' IS DISTINCT FROM ''true''::jsonb AND p_receipt->''process_started'' IS DISTINCT FROM ''false''::jsonb THEN
  RAISE EXCEPTION ''Post merge cleanup unconfirmed'' USING ERRCODE=''P0009'';END IF;
 identity:=(a.request-ARRAY[''prepare_action_id'',''integration_base'',''source_branch''])||jsonb_build_object(''version'',CASE WHEN a.request->>''verification_profile''=''forge-sdlc-jenkins-v3'' THEN 3 ELSE 2 END,''expected_main'',a.merge_commit);
 own_verification:=a.request IS NOT NULL AND p_receipt@>identity AND p_receipt->>''provider''=''jenkins''
 AND p_receipt->''cleanup_confirmed''=''true''::jsonb AND p_receipt->>''check_receipt_sha256''~''^[0-9a-f]{64}$'';
 own_cancel:=a.request IS NOT NULL AND p_receipt->>''provider''=''jenkins'' AND p_receipt->>''target_action_id''=a.post_merge_id::text
 AND(p_receipt->''process_started''=''false''::jsonb OR(p_receipt->''cleanup_confirmed''=''true''::jsonb AND p_receipt->>''check_receipt_sha256''~''^[0-9a-f]{64}$''));
 IF p_status IN(''failed'',''superseded'') AND a.request IS NOT NULL AND own_verification IS DISTINCT FROM true AND own_cancel IS DISTINCT FROM true THEN
  RAISE EXCEPTION ''Foreign or unproven post merge cleanup'' USING ERRCODE=''P0009'';END IF;
 IF p_status=''passed'' THEN
  IF a.cancel_requested THEN RAISE EXCEPTION ''Cancelled post merge action cannot pass'' USING ERRCODE=''P0009'';END IF;
  IF NOT EXISTS(SELECT FROM forge_agent.post_merge_verification_grants WHERE worker_role=session_user AND(owner_id,project_alias)=(a.owner_id,a.project_alias) AND enabled) THEN RAISE EXCEPTION ''Post merge pass grant revoked'' USING ERRCODE=''42501'';END IF;
  identity:=a.request-ARRAY[''prepare_action_id'',''integration_base'',''source_branch''];
  identity:=identity||jsonb_build_object(''version'',CASE WHEN a.request->>''verification_profile''=''forge-sdlc-jenkins-v3'' THEN 3 ELSE 2 END,''expected_main'',a.merge_commit);
  IF NOT forge_agent.delivery_main_receipt_valid(a.request,p_receipt,a.merge_commit)
  THEN RAISE EXCEPTION ''Unverified main result'' USING ERRCODE=''P0009'';END IF;
 END IF;
 UPDATE forge_agent.post_merge_verifications SET status=p_status,receipt=p_receipt,summary=p_summary,
 cancel_requested=cancel_requested OR(p_receipt->''cancellation_required''=''true''::jsonb) IS TRUE,
 next_poll_at=CASE WHEN p_status=''pending'' THEN clock_timestamp()+interval ''5 seconds'' ELSE next_poll_at END,
 lease_token=NULL,lease_role=NULL,lease_until=NULL,finish_token=p_token,finish_generation=p_generation,finish_role=session_user,finish_value=value,updated_at=clock_timestamp() WHERE post_merge_id=p_id;
 IF p_status=''failed'' THEN
  INSERT INTO forge_agent.post_merge_remediations(post_merge_id,owner_id,project_alias,workflow_id,root_task_id,merge_commit,summary,receipt)
  VALUES(a.post_merge_id,a.owner_id,a.project_alias,a.workflow_id,a.root_task_id,a.merge_commit,p_summary,p_receipt);
 END IF;
 PERFORM pg_notify(''forge_coding_jobs'','''');RETURN true;
END;';after text:='
DECLARE a forge_agent.post_merge_verifications%ROWTYPE;value jsonb;identity jsonb;own_verification boolean;own_cancel boolean;
BEGIN
 a:=forge_agent.post_merge_lock(p_id);value:=jsonb_build_object(''status'',p_status,''receipt'',p_receipt,''summary'',p_summary);
 IF a.finish_token=p_token AND a.finish_generation=p_generation AND a.finish_role=session_user THEN
  IF a.finish_value IS DISTINCT FROM value THEN RAISE EXCEPTION ''Post merge outcome conflict'' USING ERRCODE=''P0009'';END IF;RETURN true;END IF;
 PERFORM forge_coding_reconciler_api.post_merge_renew(p_id,p_generation,p_token);
 IF p_status IS NULL OR p_status NOT IN(''pending'',''passed'',''failed'',''superseded'') OR jsonb_typeof(p_receipt) IS DISTINCT FROM ''object''
 OR octet_length(p_receipt::text)>262144 OR p_summary IS NULL OR octet_length(p_summary)>4096
 THEN RAISE EXCEPTION ''Invalid post merge result'' USING ERRCODE=''22023'';END IF;
 IF p_status<>''pending'' AND p_receipt->''cleanup_confirmed'' IS DISTINCT FROM ''true''::jsonb AND p_receipt->''process_started'' IS DISTINCT FROM ''false''::jsonb THEN
  RAISE EXCEPTION ''Post merge cleanup unconfirmed'' USING ERRCODE=''P0009'';END IF;
 identity:=(a.request-ARRAY[''prepare_action_id'',''integration_base'',''source_branch''])||jsonb_build_object(''version'',CASE WHEN a.request->>''verification_profile''=''forge-sdlc-jenkins-v4'' THEN 4 WHEN a.request->>''verification_profile''=''forge-sdlc-jenkins-v3'' THEN 3 ELSE 2 END,''expected_main'',a.merge_commit);
 own_verification:=a.request IS NOT NULL AND p_receipt@>identity AND p_receipt->>''provider''=''jenkins''
 AND p_receipt->''cleanup_confirmed''=''true''::jsonb AND p_receipt->>''check_receipt_sha256''~''^[0-9a-f]{64}$'';
 own_cancel:=a.request IS NOT NULL AND p_receipt->>''provider''=''jenkins'' AND p_receipt->>''target_action_id''=a.post_merge_id::text
 AND(p_receipt->''process_started''=''false''::jsonb OR(p_receipt->''cleanup_confirmed''=''true''::jsonb AND p_receipt->>''check_receipt_sha256''~''^[0-9a-f]{64}$''));
 IF p_status IN(''failed'',''superseded'') AND a.request IS NOT NULL AND own_verification IS DISTINCT FROM true AND own_cancel IS DISTINCT FROM true THEN
  RAISE EXCEPTION ''Foreign or unproven post merge cleanup'' USING ERRCODE=''P0009'';END IF;
 IF p_status=''passed'' THEN
  IF a.cancel_requested THEN RAISE EXCEPTION ''Cancelled post merge action cannot pass'' USING ERRCODE=''P0009'';END IF;
  IF NOT EXISTS(SELECT FROM forge_agent.post_merge_verification_grants WHERE worker_role=session_user AND(owner_id,project_alias)=(a.owner_id,a.project_alias) AND enabled) THEN RAISE EXCEPTION ''Post merge pass grant revoked'' USING ERRCODE=''42501'';END IF;
  identity:=a.request-ARRAY[''prepare_action_id'',''integration_base'',''source_branch''];
  identity:=identity||jsonb_build_object(''version'',CASE WHEN a.request->>''verification_profile''=''forge-sdlc-jenkins-v4'' THEN 4 WHEN a.request->>''verification_profile''=''forge-sdlc-jenkins-v3'' THEN 3 ELSE 2 END,''expected_main'',a.merge_commit);
  IF NOT forge_agent.delivery_main_receipt_valid(a.request,p_receipt,a.merge_commit)
  THEN RAISE EXCEPTION ''Unverified main result'' USING ERRCODE=''P0009'';END IF;
 END IF;
 UPDATE forge_agent.post_merge_verifications SET status=p_status,receipt=p_receipt,summary=p_summary,
 cancel_requested=cancel_requested OR(p_receipt->''cancellation_required''=''true''::jsonb) IS TRUE,
 next_poll_at=CASE WHEN p_status=''pending'' THEN clock_timestamp()+interval ''5 seconds'' ELSE next_poll_at END,
 lease_token=NULL,lease_role=NULL,lease_until=NULL,finish_token=p_token,finish_generation=p_generation,finish_role=session_user,finish_value=value,updated_at=clock_timestamp() WHERE post_merge_id=p_id;
 IF p_status=''failed'' THEN
  INSERT INTO forge_agent.post_merge_remediations(post_merge_id,owner_id,project_alias,workflow_id,root_task_id,merge_commit,summary,receipt)
  VALUES(a.post_merge_id,a.owner_id,a.project_alias,a.workflow_id,a.root_task_id,a.merge_commit,p_summary,p_receipt);
 END IF;
 PERFORM pg_notify(''forge_coding_jobs'','''');RETURN true;
END;';definition text;
BEGIN
 IF (SELECT encode(sha256(convert_to(prosrc,'UTF8')),'hex') FROM pg_proc WHERE oid=signature::regprocedure)<>'a0e6992dbba7c75ec4a726ceeccbddb55fdd6bc8f0ed0578b976ca1e8d3de09a' THEN RAISE EXCEPTION 'M56 predecessor function drift: %',signature;END IF;
 definition:=pg_get_functiondef(signature::regprocedure);
 IF (length(definition)-length(replace(definition,before,'')))/length(before)<>1 THEN RAISE EXCEPTION 'M56 source target not unique: %',signature;END IF;
 EXECUTE replace(definition,before,after);
END;$native_v4_patch$;
DO $native_v4_patch$
DECLARE signature text:='forge_coding_reconciler_api.review_finish(uuid,uuid,jsonb)';before text:='
DECLARE q forge_agent.coding_reviews%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;p forge_agent.coding_review_policy%ROWTYPE;
 previous forge_agent.coding_reviews%ROWTYPE;disposition text;receipt jsonb;id uuid;next_id uuid;stagnant integer;v_candidate text;v_gate text;event_receipt jsonb;has_retry boolean;owner_fenced boolean;
BEGIN
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p_review AND owner_id=current_setting(''forge.coding_owner'',true);
 IF NOT FOUND THEN RAISE EXCEPTION ''Review not found'' USING ERRCODE=''P0002'';END IF;
 -- Every path acquires task before review. Re-read the review and check the
 -- wall clock only after both locks; a blocked task lock can outlive a lease.
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE owner_id=q.owner_id AND project_alias=q.project_alias AND coding_tasks.id=q.task_id FOR UPDATE;
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p_review AND owner_id=current_setting(''forge.coding_owner'',true) FOR UPDATE;
 IF NOT FOUND OR q.task_id IS DISTINCT FROM t.id OR q.project_alias IS DISTINCT FROM t.project_alias OR q.owner_id IS DISTINCT FROM t.owner_id THEN RAISE EXCEPTION ''Review source changed while waiting'' USING ERRCODE=''P0008'';END IF;
 IF q.finish_token=p_token AND q.finish_receipt->''outcome''=p_outcome THEN RETURN q.finish_receipt;END IF;
 IF q.status<>''checking'' OR q.lease_token IS DISTINCT FROM p_token OR q.lease_expires_at<=clock_timestamp() THEN RAISE EXCEPTION ''Review lease expired'' USING ERRCODE=''P0008'';END IF;
 IF t.latest_attempt_id<>q.attempt_id OR t.plan_decision_id IS DISTINCT FROM q.plan_decision_id OR t.scope_plan_decision_id IS DISTINCT FROM q.scope_plan_decision_id OR t.status NOT IN(''awaiting_review'',''completed'') THEN RAISE EXCEPTION ''Review source superseded'' USING ERRCODE=''P0008'';END IF;
 IF jsonb_typeof(p_outcome)<>''object'' OR octet_length(p_outcome::text)>131072 THEN RAISE EXCEPTION ''Invalid review outcome'' USING ERRCODE=''22023'';END IF;
 disposition:=p_outcome->>''disposition'';v_candidate:=p_outcome->>''candidate_commit'';v_gate:=p_outcome->>''gate_manifest_sha256'';
 IF disposition NOT IN(''repair'',''retry_infrastructure'',''awaiting_review'',''handoff'',''cancelled'',''needs_reconcile'',''revise_plan'')
 OR (v_candidate IS NOT NULL AND v_candidate!~''^[0-9a-f]{40}$'') OR(v_gate IS NOT NULL AND v_gate!~''^([0-9a-f]{64}|forge-sdlc-jenkins-v[1-3]:(main|detached|branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?))$'')
 OR jsonb_typeof(coalesce(p_outcome->''findings'',''[]''))<>''array'' OR jsonb_typeof(coalesce(p_outcome->''verification'',''{}''))<>''object'' THEN
 RAISE EXCEPTION ''Invalid review outcome'' USING ERRCODE=''22023'';END IF;
 IF disposition IN(''repair'',''retry_infrastructure'',''awaiting_review'') AND coalesce(p_outcome->''cleanup_confirmed'',p_outcome->''verification''->''cleanup_confirmed'',''false''::jsonb) IS DISTINCT FROM ''true''::jsonb THEN disposition:=''needs_reconcile'';END IF;
 IF q.phase=''source'' THEN
  IF q.owner_decision->>''decision'' IS DISTINCT FROM ''repair_source'' OR disposition NOT IN(''repair'',''handoff'',''needs_reconcile'',''cancelled'',''revise_plan'',''retry_infrastructure'')
  OR(disposition=''repair'' AND(v_candidate IS DISTINCT FROM q.candidate_commit OR p_outcome->''verification''->>''candidate_workspace_digest'' IS NULL)) THEN
   RAISE EXCEPTION ''Source import lacks bound owner authorization'' USING ERRCODE=''P0009'';END IF;
  v_candidate:=coalesce(v_candidate,q.candidate_commit);
 END IF;
 IF q.phase=''hosted'' THEN
  IF q.created_at+interval ''24 hours''<=clock_timestamp() THEN
   UPDATE forge_agent.coding_reviews SET status=''handoff'',reason=''hosted_wait_exhausted'',lease_token=NULL,lease_expires_at=NULL,finish_token=p_token,finish_receipt=jsonb_build_object(''review_id'',q.review_id,''status'',''handoff'',''reason'',''hosted_wait_exhausted'',''outcome'',p_outcome),updated_at=clock_timestamp() WHERE review_id=q.review_id;
   SELECT finish_receipt INTO receipt FROM forge_agent.coding_reviews WHERE review_id=q.review_id;RETURN receipt;
  END IF;
  IF p_outcome->''verification''->''hosted'' IS DISTINCT FROM ''true''::jsonb OR v_candidate IS DISTINCT FROM q.candidate_commit THEN
   RAISE EXCEPTION ''Hosted source mismatch'' USING ERRCODE=''P0006'';END IF;
  receipt:=forge_coding_reconciler_api.record_hosted_checks(q.source_publication_id,q.candidate_commit,coalesce(p_outcome->''verification''->''checks'',''[]''));
  IF coalesce(p_outcome->''verification''->>''hosted_status'',p_outcome->>''hosted_status'')=''pending'' THEN
   UPDATE forge_agent.coding_reviews SET status=''pending'',lease_token=NULL,lease_expires_at=NULL,next_poll_at=clock_timestamp()+interval ''60 seconds'',
    verification=verification||coalesce(p_outcome->''verification'',''{}''),finish_token=p_token,
    finish_receipt=jsonb_build_object(''review_id'',q.review_id,''status'',''waiting_hosted'',''outcome'',p_outcome),updated_at=clock_timestamp() WHERE review_id=q.review_id;
   SELECT finish_receipt INTO receipt FROM forge_agent.coding_reviews WHERE review_id=q.review_id;RETURN receipt;
  ELSIF coalesce(p_outcome->''verification''->>''hosted_status'',p_outcome->>''hosted_status'')=''passed'' THEN
   IF receipt->''required_green'' IS DISTINCT FROM ''true''::jsonb THEN RAISE EXCEPTION ''Required hosted checks incomplete'' USING ERRCODE=''P0009'';END IF;
   UPDATE forge_agent.coding_reviews SET status=''approved'',lease_token=NULL,lease_expires_at=NULL,verification=verification||coalesce(p_outcome->''verification'',''{}''),
    finish_token=p_token,finish_receipt=jsonb_build_object(''review_id'',q.review_id,''status'',''hosted_green'',''outcome'',p_outcome),updated_at=clock_timestamp() WHERE review_id=q.review_id;
   SELECT finish_receipt INTO receipt FROM forge_agent.coding_reviews WHERE review_id=q.review_id;RETURN receipt;
  END IF;
 END IF;
 owner_fenced:=forge_agent._coding_owner_continuation_fenced(t.id);
 IF owner_fenced AND disposition IN(''repair'',''retry_infrastructure'') THEN disposition:=''handoff'';END IF;
 event_receipt:=forge_agent._retry_native_observe(q.review_id,p_outcome->''retry_observation'');
 has_retry:=p_outcome->''retry_observation'' IS NOT NULL;
 SELECT * INTO p FROM forge_agent.coding_review_policy WHERE owner_id=t.owner_id AND project_alias=t.project_alias;
 p.max_rounds:=coalesce((SELECT(b.policy->>''max_rounds'')::integer FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),p.max_rounds);
 p.max_stagnant_rounds:=coalesce((SELECT(b.policy->>''max_stagnant_rounds'')::integer FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),p.max_stagnant_rounds);
 p.max_infrastructure_retries:=coalesce((SELECT(b.policy->>''max_infrastructure_retries'')::integer FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),p.max_infrastructure_retries);
 SELECT r.* INTO previous FROM forge_agent.coding_reviews r JOIN forge_agent.coding_tasks x ON(x.owner_id,x.project_alias,x.id)=(r.owner_id,r.project_alias,r.task_id)
 WHERE r.owner_id=t.owner_id AND r.project_alias=t.project_alias AND coalesce(x.repair_root_task_id,x.id)=coalesce(t.repair_root_task_id,t.id)
 AND r.review_id<>q.review_id AND r.status NOT IN(''pending'',''checking'') ORDER BY r.updated_at DESC LIMIT 1;
 IF EXISTS(SELECT 1 FROM jsonb_array_elements_text(coalesce(p_outcome->''verified_finding_ids'',''[]'')) closed
 WHERE NOT EXISTS(SELECT 1 FROM jsonb_array_elements(coalesce(previous.findings,''[]''))f
 WHERE f->>''finding_id''=closed AND coalesce(f->>''check_id'','''')<>'''' AND EXISTS(
 SELECT 1 FROM jsonb_array_elements(coalesce(p_outcome->''verification''->''checks'',''[]''))c
 WHERE coalesce(c->>''check_id'',c->>''name'')=f->>''check_id'' AND c->>''status'' IN(''passed'',''success'') AND c->>''head_sha''=v_candidate))) THEN
 RAISE EXCEPTION ''Finding closure lacks independent matching witness'' USING ERRCODE=''P0009'';END IF;
 stagnant:=CASE WHEN jsonb_array_length(coalesce(previous.findings,''[]''))>0
 AND jsonb_array_length(coalesce(p_outcome->''verified_finding_ids'',''[]''))=0 THEN previous.stagnant_rounds+1 ELSE 0 END;
 UPDATE forge_agent.coding_review_budget SET stagnant_rounds=stagnant WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND root_task_id=coalesce(t.repair_root_task_id,t.id);
 IF disposition=''repair'' AND((has_retry AND event_receipt->>''status''=''exhausted'') OR(NOT has_retry AND(greatest(q.round,coalesce((SELECT b.repair_rounds FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),0))>=p.max_rounds OR stagnant>=p.max_stagnant_rounds))) THEN disposition:=''handoff'';END IF;
 IF disposition=''retry_infrastructure'' AND((has_retry AND event_receipt->>''status''=''exhausted'') OR(NOT has_retry AND q.infrastructure_retries>=p.max_infrastructure_retries)) THEN disposition:=''handoff'';END IF;
 -- An automatic verifier can report local success; publication still waits for
 -- an explicit independent owner/critic decision bound to this exact commit.
 UPDATE forge_agent.coding_reviews SET status=disposition,candidate_commit=v_candidate,gate_manifest_sha256=v_gate,
 findings=(SELECT coalesce(jsonb_agg(v),''[]'') FROM(SELECT DISTINCT ON(x->>''finding_id'')x AS v FROM jsonb_array_elements(coalesce(previous.findings,''[]'')||coalesce(p_outcome->''findings'',''[]''))WITH ORDINALITY AS entries(x,ordinal) ORDER BY x->>''finding_id'',ordinal DESC)ledger),verified_finding_ids=coalesce(p_outcome->''verified_finding_ids'',''[]''),
 verification=verification||coalesce(p_outcome->''verification'',''{}'')||jsonb_build_object(''cleanup_confirmed'',coalesce(p_outcome->''cleanup_confirmed'',p_outcome->''verification''->''cleanup_confirmed'',''false''::jsonb)),fingerprint=p_outcome->>''fingerprint'',summary=coalesce(p_outcome->>''summary'',''''),reason=CASE WHEN owner_fenced AND p_outcome->>''disposition'' IN(''repair'',''retry_infrastructure'') THEN ''owner_continuation_fenced'' ELSE coalesce(p_outcome->>''reason'','''') END,
 stagnant_rounds=stagnant,lease_token=NULL,lease_expires_at=NULL,finish_token=p_token,updated_at=clock_timestamp() WHERE review_id=q.review_id RETURNING * INTO q;
 IF disposition=''repair'' AND v_candidate IS NOT NULL AND v_gate IS NOT NULL THEN
  id:=forge_agent.coding_make_feedback(q.review_id,t.id,''repair'',q.summary,q.findings);
  UPDATE forge_agent.coding_reviews SET feedback_id=id WHERE review_id=q.review_id;
  IF t.auto_repair OR(q.phase=''source'' AND q.owner_decision->>''decision''=''repair_source'') THEN next_id:=forge_agent.coding_schedule_repair(q.review_id,id);END IF;
 ELSIF disposition=''retry_infrastructure'' AND(t.auto_repair OR(q.phase=''source'' AND q.owner_decision->>''decision''=''repair_source'')) THEN
  IF has_retry THEN PERFORM forge_agent.retry_event_reserve((event_receipt->>''event_id'')::uuid,''infra-review.''||q.review_id::text);END IF;
  INSERT INTO forge_agent.coding_reviews(owner_id,project_alias,task_id,attempt_id,plan_decision_id,reconciliation_receipt_id,base_revision,workspace_digest,parent_result_sha256,
  source_revision,review_no,round,stagnant_rounds,infrastructure_retries,previous_review_id,candidate_commit,phase,owner_decision,verification,target_publication_id,source_publication_id,gate_manifest_sha256)
  VALUES(q.owner_id,q.project_alias,q.task_id,q.attempt_id,q.plan_decision_id,q.reconciliation_receipt_id,q.base_revision,q.workspace_digest,q.parent_result_sha256,
  t.revision,q.review_no+1,q.round,q.stagnant_rounds,q.infrastructure_retries+1,q.review_id,q.candidate_commit,q.phase,q.owner_decision,q.verification,q.target_publication_id,q.source_publication_id,q.gate_manifest_sha256) RETURNING review_id INTO next_id;
 END IF;
 receipt:=jsonb_build_object(''review_id'',q.review_id,''status'',disposition,''feedback_id'',id,''repair_task_id'',CASE WHEN disposition=''repair'' THEN next_id ELSE NULL END,''next_review_id'',CASE WHEN disposition=''retry_infrastructure'' THEN next_id ELSE NULL END,''outcome'',p_outcome);
 IF owner_fenced AND p_outcome->>''disposition'' IN(''repair'',''retry_infrastructure'') THEN receipt:=receipt||jsonb_build_object(''reason'',''owner_continuation_fenced'');END IF;
 UPDATE forge_agent.coding_reviews SET finish_receipt=receipt WHERE review_id=q.review_id;
 INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
 SELECT t.owner_id,t.project_alias,t.id,coalesce(max(sequence),0)+1,''checkpoint'',jsonb_build_object(''review_id'',q.review_id,''review_status'',disposition,''feedback_id'',id,''repair_task_id'',next_id,''actor'',jsonb_build_object(''kind'', ''service'', ''id'', ''forge-coding-reconciler''))
 FROM forge_agent.coding_events WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND task_id=t.id;
 PERFORM pg_notify(''forge_coding_jobs'','''');RETURN receipt;
END;';after text:='
DECLARE q forge_agent.coding_reviews%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;p forge_agent.coding_review_policy%ROWTYPE;
 previous forge_agent.coding_reviews%ROWTYPE;disposition text;receipt jsonb;id uuid;next_id uuid;stagnant integer;v_candidate text;v_gate text;event_receipt jsonb;has_retry boolean;owner_fenced boolean;
BEGIN
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p_review AND owner_id=current_setting(''forge.coding_owner'',true);
 IF NOT FOUND THEN RAISE EXCEPTION ''Review not found'' USING ERRCODE=''P0002'';END IF;
 -- Every path acquires task before review. Re-read the review and check the
 -- wall clock only after both locks; a blocked task lock can outlive a lease.
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE owner_id=q.owner_id AND project_alias=q.project_alias AND coding_tasks.id=q.task_id FOR UPDATE;
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p_review AND owner_id=current_setting(''forge.coding_owner'',true) FOR UPDATE;
 IF NOT FOUND OR q.task_id IS DISTINCT FROM t.id OR q.project_alias IS DISTINCT FROM t.project_alias OR q.owner_id IS DISTINCT FROM t.owner_id THEN RAISE EXCEPTION ''Review source changed while waiting'' USING ERRCODE=''P0008'';END IF;
 IF q.finish_token=p_token AND q.finish_receipt->''outcome''=p_outcome THEN RETURN q.finish_receipt;END IF;
 IF q.status<>''checking'' OR q.lease_token IS DISTINCT FROM p_token OR q.lease_expires_at<=clock_timestamp() THEN RAISE EXCEPTION ''Review lease expired'' USING ERRCODE=''P0008'';END IF;
 IF t.latest_attempt_id<>q.attempt_id OR t.plan_decision_id IS DISTINCT FROM q.plan_decision_id OR t.scope_plan_decision_id IS DISTINCT FROM q.scope_plan_decision_id OR t.status NOT IN(''awaiting_review'',''completed'') THEN RAISE EXCEPTION ''Review source superseded'' USING ERRCODE=''P0008'';END IF;
 IF jsonb_typeof(p_outcome)<>''object'' OR octet_length(p_outcome::text)>131072 THEN RAISE EXCEPTION ''Invalid review outcome'' USING ERRCODE=''22023'';END IF;
 disposition:=p_outcome->>''disposition'';v_candidate:=p_outcome->>''candidate_commit'';v_gate:=p_outcome->>''gate_manifest_sha256'';
 IF disposition NOT IN(''repair'',''retry_infrastructure'',''awaiting_review'',''handoff'',''cancelled'',''needs_reconcile'',''revise_plan'')
 OR (v_candidate IS NOT NULL AND v_candidate!~''^[0-9a-f]{40}$'') OR(v_gate IS NOT NULL AND v_gate!~''^([0-9a-f]{64}|forge-sdlc-jenkins-v[1-4]:(main|detached|branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?))$'')
 OR jsonb_typeof(coalesce(p_outcome->''findings'',''[]''))<>''array'' OR jsonb_typeof(coalesce(p_outcome->''verification'',''{}''))<>''object'' THEN
 RAISE EXCEPTION ''Invalid review outcome'' USING ERRCODE=''22023'';END IF;
 IF disposition IN(''repair'',''retry_infrastructure'',''awaiting_review'') AND coalesce(p_outcome->''cleanup_confirmed'',p_outcome->''verification''->''cleanup_confirmed'',''false''::jsonb) IS DISTINCT FROM ''true''::jsonb THEN disposition:=''needs_reconcile'';END IF;
 IF q.phase=''source'' THEN
  IF q.owner_decision->>''decision'' IS DISTINCT FROM ''repair_source'' OR disposition NOT IN(''repair'',''handoff'',''needs_reconcile'',''cancelled'',''revise_plan'',''retry_infrastructure'')
  OR(disposition=''repair'' AND(v_candidate IS DISTINCT FROM q.candidate_commit OR p_outcome->''verification''->>''candidate_workspace_digest'' IS NULL)) THEN
   RAISE EXCEPTION ''Source import lacks bound owner authorization'' USING ERRCODE=''P0009'';END IF;
  v_candidate:=coalesce(v_candidate,q.candidate_commit);
 END IF;
 IF q.phase=''hosted'' THEN
  IF q.created_at+interval ''24 hours''<=clock_timestamp() THEN
   UPDATE forge_agent.coding_reviews SET status=''handoff'',reason=''hosted_wait_exhausted'',lease_token=NULL,lease_expires_at=NULL,finish_token=p_token,finish_receipt=jsonb_build_object(''review_id'',q.review_id,''status'',''handoff'',''reason'',''hosted_wait_exhausted'',''outcome'',p_outcome),updated_at=clock_timestamp() WHERE review_id=q.review_id;
   SELECT finish_receipt INTO receipt FROM forge_agent.coding_reviews WHERE review_id=q.review_id;RETURN receipt;
  END IF;
  IF p_outcome->''verification''->''hosted'' IS DISTINCT FROM ''true''::jsonb OR v_candidate IS DISTINCT FROM q.candidate_commit THEN
   RAISE EXCEPTION ''Hosted source mismatch'' USING ERRCODE=''P0006'';END IF;
  receipt:=forge_coding_reconciler_api.record_hosted_checks(q.source_publication_id,q.candidate_commit,coalesce(p_outcome->''verification''->''checks'',''[]''));
  IF coalesce(p_outcome->''verification''->>''hosted_status'',p_outcome->>''hosted_status'')=''pending'' THEN
   UPDATE forge_agent.coding_reviews SET status=''pending'',lease_token=NULL,lease_expires_at=NULL,next_poll_at=clock_timestamp()+interval ''60 seconds'',
    verification=verification||coalesce(p_outcome->''verification'',''{}''),finish_token=p_token,
    finish_receipt=jsonb_build_object(''review_id'',q.review_id,''status'',''waiting_hosted'',''outcome'',p_outcome),updated_at=clock_timestamp() WHERE review_id=q.review_id;
   SELECT finish_receipt INTO receipt FROM forge_agent.coding_reviews WHERE review_id=q.review_id;RETURN receipt;
  ELSIF coalesce(p_outcome->''verification''->>''hosted_status'',p_outcome->>''hosted_status'')=''passed'' THEN
   IF receipt->''required_green'' IS DISTINCT FROM ''true''::jsonb THEN RAISE EXCEPTION ''Required hosted checks incomplete'' USING ERRCODE=''P0009'';END IF;
   UPDATE forge_agent.coding_reviews SET status=''approved'',lease_token=NULL,lease_expires_at=NULL,verification=verification||coalesce(p_outcome->''verification'',''{}''),
    finish_token=p_token,finish_receipt=jsonb_build_object(''review_id'',q.review_id,''status'',''hosted_green'',''outcome'',p_outcome),updated_at=clock_timestamp() WHERE review_id=q.review_id;
   SELECT finish_receipt INTO receipt FROM forge_agent.coding_reviews WHERE review_id=q.review_id;RETURN receipt;
  END IF;
 END IF;
 owner_fenced:=forge_agent._coding_owner_continuation_fenced(t.id);
 IF owner_fenced AND disposition IN(''repair'',''retry_infrastructure'') THEN disposition:=''handoff'';END IF;
 event_receipt:=forge_agent._retry_native_observe(q.review_id,p_outcome->''retry_observation'');
 has_retry:=p_outcome->''retry_observation'' IS NOT NULL;
 SELECT * INTO p FROM forge_agent.coding_review_policy WHERE owner_id=t.owner_id AND project_alias=t.project_alias;
 p.max_rounds:=coalesce((SELECT(b.policy->>''max_rounds'')::integer FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),p.max_rounds);
 p.max_stagnant_rounds:=coalesce((SELECT(b.policy->>''max_stagnant_rounds'')::integer FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),p.max_stagnant_rounds);
 p.max_infrastructure_retries:=coalesce((SELECT(b.policy->>''max_infrastructure_retries'')::integer FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),p.max_infrastructure_retries);
 SELECT r.* INTO previous FROM forge_agent.coding_reviews r JOIN forge_agent.coding_tasks x ON(x.owner_id,x.project_alias,x.id)=(r.owner_id,r.project_alias,r.task_id)
 WHERE r.owner_id=t.owner_id AND r.project_alias=t.project_alias AND coalesce(x.repair_root_task_id,x.id)=coalesce(t.repair_root_task_id,t.id)
 AND r.review_id<>q.review_id AND r.status NOT IN(''pending'',''checking'') ORDER BY r.updated_at DESC LIMIT 1;
 IF EXISTS(SELECT 1 FROM jsonb_array_elements_text(coalesce(p_outcome->''verified_finding_ids'',''[]'')) closed
 WHERE NOT EXISTS(SELECT 1 FROM jsonb_array_elements(coalesce(previous.findings,''[]''))f
 WHERE f->>''finding_id''=closed AND coalesce(f->>''check_id'','''')<>'''' AND EXISTS(
 SELECT 1 FROM jsonb_array_elements(coalesce(p_outcome->''verification''->''checks'',''[]''))c
 WHERE coalesce(c->>''check_id'',c->>''name'')=f->>''check_id'' AND c->>''status'' IN(''passed'',''success'') AND c->>''head_sha''=v_candidate))) THEN
 RAISE EXCEPTION ''Finding closure lacks independent matching witness'' USING ERRCODE=''P0009'';END IF;
 stagnant:=CASE WHEN jsonb_array_length(coalesce(previous.findings,''[]''))>0
 AND jsonb_array_length(coalesce(p_outcome->''verified_finding_ids'',''[]''))=0 THEN previous.stagnant_rounds+1 ELSE 0 END;
 UPDATE forge_agent.coding_review_budget SET stagnant_rounds=stagnant WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND root_task_id=coalesce(t.repair_root_task_id,t.id);
 IF disposition=''repair'' AND((has_retry AND event_receipt->>''status''=''exhausted'') OR(NOT has_retry AND(greatest(q.round,coalesce((SELECT b.repair_rounds FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),0))>=p.max_rounds OR stagnant>=p.max_stagnant_rounds))) THEN disposition:=''handoff'';END IF;
 IF disposition=''retry_infrastructure'' AND((has_retry AND event_receipt->>''status''=''exhausted'') OR(NOT has_retry AND q.infrastructure_retries>=p.max_infrastructure_retries)) THEN disposition:=''handoff'';END IF;
 -- An automatic verifier can report local success; publication still waits for
 -- an explicit independent owner/critic decision bound to this exact commit.
 UPDATE forge_agent.coding_reviews SET status=disposition,candidate_commit=v_candidate,gate_manifest_sha256=v_gate,
 findings=(SELECT coalesce(jsonb_agg(v),''[]'') FROM(SELECT DISTINCT ON(x->>''finding_id'')x AS v FROM jsonb_array_elements(coalesce(previous.findings,''[]'')||coalesce(p_outcome->''findings'',''[]''))WITH ORDINALITY AS entries(x,ordinal) ORDER BY x->>''finding_id'',ordinal DESC)ledger),verified_finding_ids=coalesce(p_outcome->''verified_finding_ids'',''[]''),
 verification=verification||coalesce(p_outcome->''verification'',''{}'')||jsonb_build_object(''cleanup_confirmed'',coalesce(p_outcome->''cleanup_confirmed'',p_outcome->''verification''->''cleanup_confirmed'',''false''::jsonb)),fingerprint=p_outcome->>''fingerprint'',summary=coalesce(p_outcome->>''summary'',''''),reason=CASE WHEN owner_fenced AND p_outcome->>''disposition'' IN(''repair'',''retry_infrastructure'') THEN ''owner_continuation_fenced'' ELSE coalesce(p_outcome->>''reason'','''') END,
 stagnant_rounds=stagnant,lease_token=NULL,lease_expires_at=NULL,finish_token=p_token,updated_at=clock_timestamp() WHERE review_id=q.review_id RETURNING * INTO q;
 IF disposition=''repair'' AND v_candidate IS NOT NULL AND v_gate IS NOT NULL THEN
  id:=forge_agent.coding_make_feedback(q.review_id,t.id,''repair'',q.summary,q.findings);
  UPDATE forge_agent.coding_reviews SET feedback_id=id WHERE review_id=q.review_id;
  IF t.auto_repair OR(q.phase=''source'' AND q.owner_decision->>''decision''=''repair_source'') THEN next_id:=forge_agent.coding_schedule_repair(q.review_id,id);END IF;
 ELSIF disposition=''retry_infrastructure'' AND(t.auto_repair OR(q.phase=''source'' AND q.owner_decision->>''decision''=''repair_source'')) THEN
  IF has_retry THEN PERFORM forge_agent.retry_event_reserve((event_receipt->>''event_id'')::uuid,''infra-review.''||q.review_id::text);END IF;
  INSERT INTO forge_agent.coding_reviews(owner_id,project_alias,task_id,attempt_id,plan_decision_id,reconciliation_receipt_id,base_revision,workspace_digest,parent_result_sha256,
  source_revision,review_no,round,stagnant_rounds,infrastructure_retries,previous_review_id,candidate_commit,phase,owner_decision,verification,target_publication_id,source_publication_id,gate_manifest_sha256)
  VALUES(q.owner_id,q.project_alias,q.task_id,q.attempt_id,q.plan_decision_id,q.reconciliation_receipt_id,q.base_revision,q.workspace_digest,q.parent_result_sha256,
  t.revision,q.review_no+1,q.round,q.stagnant_rounds,q.infrastructure_retries+1,q.review_id,q.candidate_commit,q.phase,q.owner_decision,q.verification,q.target_publication_id,q.source_publication_id,q.gate_manifest_sha256) RETURNING review_id INTO next_id;
 END IF;
 receipt:=jsonb_build_object(''review_id'',q.review_id,''status'',disposition,''feedback_id'',id,''repair_task_id'',CASE WHEN disposition=''repair'' THEN next_id ELSE NULL END,''next_review_id'',CASE WHEN disposition=''retry_infrastructure'' THEN next_id ELSE NULL END,''outcome'',p_outcome);
 IF owner_fenced AND p_outcome->>''disposition'' IN(''repair'',''retry_infrastructure'') THEN receipt:=receipt||jsonb_build_object(''reason'',''owner_continuation_fenced'');END IF;
 UPDATE forge_agent.coding_reviews SET finish_receipt=receipt WHERE review_id=q.review_id;
 INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
 SELECT t.owner_id,t.project_alias,t.id,coalesce(max(sequence),0)+1,''checkpoint'',jsonb_build_object(''review_id'',q.review_id,''review_status'',disposition,''feedback_id'',id,''repair_task_id'',next_id,''actor'',jsonb_build_object(''kind'', ''service'', ''id'', ''forge-coding-reconciler''))
 FROM forge_agent.coding_events WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND task_id=t.id;
 PERFORM pg_notify(''forge_coding_jobs'','''');RETURN receipt;
END;';definition text;
BEGIN
 IF (SELECT encode(sha256(convert_to(prosrc,'UTF8')),'hex') FROM pg_proc WHERE oid=signature::regprocedure)<>'95c9d657e33894a9b73b5b7f2728a0dcd1fc77e9f07436324aa22418e17a7a55' THEN RAISE EXCEPTION 'M56 predecessor function drift: %',signature;END IF;
 definition:=pg_get_functiondef(signature::regprocedure);
 IF (length(definition)-length(replace(definition,before,'')))/length(before)<>1 THEN RAISE EXCEPTION 'M56 source target not unique: %',signature;END IF;
 EXECUTE replace(definition,before,after);
END;$native_v4_patch$;
ALTER TABLE forge_agent.coding_reviews DROP CONSTRAINT coding_reviews_gate_manifest_sha256_check,
 ADD CONSTRAINT coding_reviews_gate_manifest_sha256_check CHECK(gate_manifest_sha256 IS NULL OR gate_manifest_sha256~'^([0-9a-f]{64}|forge-sdlc-jenkins-v[1-4]:(main|detached|branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?))$');
ALTER TABLE forge_agent.coding_workflow_reviews DROP CONSTRAINT coding_workflow_reviews_gate_manifest_sha256_check,
 ADD CONSTRAINT coding_workflow_reviews_gate_manifest_sha256_check CHECK(gate_manifest_sha256 IS NULL OR gate_manifest_sha256~'^([0-9a-f]{64}|forge-sdlc-jenkins-v[1-4]:(main|detached|branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?))$');
