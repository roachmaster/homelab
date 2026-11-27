-- Exact M51 validation precedes historical catalog/source normalization.
DO $m51_verify$
DECLARE item record;source text;actual text;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=51) THEN RETURN;END IF;
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=51 AND name='agent_feature_branches') THEN RAISE EXCEPTION 'M51 identity drift';END IF;
 SELECT pg_get_constraintdef(oid) INTO actual FROM pg_constraint WHERE conrelid='forge_agent.coding_publications'::regclass AND conname='coding_publications_branch_check' AND convalidated;
 IF actual IS NULL OR encode(sha256(convert_to(actual,'UTF8')),'hex')<>'c7fe6dc173a62d051432fe680002a5fce6e200d161f8ccf6ec5d05f37f173f3f' THEN RAISE EXCEPTION 'M51 branch constraint drift: %',actual;END IF;
 FOR item IN SELECT * FROM(VALUES ($s$forge_coding_reconciler_api.publication_finish(uuid,uuid,text,text,text,text,text)$s$,$o$p_branch!~'^forge/[A-Za-z0-9][A-Za-z0-9._/-]*$'$o$,$n$(p_branch!~'^forge/[A-Za-z0-9][A-Za-z0-9._/-]*$' AND p_branch NOT IN('feature/codex','feature/agy','feature/dot','feature/grok','feature/muse'))$n$,$h$cdd6f7f43a0556de8f15cfc84520d0ee76501cbaaf09a3592581cfc65b15c936$h$),
($s$forge_coding_reconciler_api.native_agent_branch_published(uuid,uuid,jsonb)$s$,$o$p_receipt->>'branch' !~ '^forge/[A-Za-z0-9][A-Za-z0-9._/-]{0,121}$'$o$,$n$(p_receipt->>'branch' !~ '^forge/[A-Za-z0-9][A-Za-z0-9._/-]{0,121}$' AND p_receipt->>'branch' NOT IN('feature/codex','feature/agy','feature/dot','feature/grok','feature/muse'))$n$,$h$ad597570fb20df95a16fd0c80cfb90a53863553d6bec97e322add5bb4697f237$h$)) x(signature,old_fragment,new_fragment,source_sha) LOOP
  SELECT prosrc INTO source FROM pg_proc WHERE oid=item.signature::regprocedure AND proowner='forge_agent_owner'::regrole AND prosecdef
    AND prorettype='jsonb'::regtype AND prokind='f'
    AND prolang=(SELECT oid FROM pg_language WHERE lanname='plpgsql')
    AND proconfig=ARRAY['search_path=pg_catalog, pg_temp'] AND provolatile='v'
    AND ARRAY(SELECT unnest(proacl::text[]) ORDER BY 1)=ARRAY['forge_agent_owner=X/forge_agent_owner','forge_coding_reconciler=X/forge_agent_owner'];
  IF source IS NULL OR (length(source)-length(replace(source,item.new_fragment,'')))/length(item.new_fragment)<>1
     OR encode(sha256(convert_to(replace(source,item.new_fragment,item.old_fragment),'UTF8')),'hex')<>item.source_sha
  THEN RAISE EXCEPTION 'M51 function drift: %',item.signature;END IF;
 END LOOP;
END
$m51_verify$;
