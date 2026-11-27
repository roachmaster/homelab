-- M51: explicit agent feature branches; preserve historical forge/* receipts.
ALTER TABLE forge_agent.coding_publications DROP CONSTRAINT coding_publications_branch_check;
ALTER TABLE forge_agent.coding_publications ADD CONSTRAINT coding_publications_branch_check CHECK ((branch IS NULL) OR ((octet_length(branch) <= 128) AND (branch ~ '^forge/[A-Za-z0-9][A-Za-z0-9._/-]*$' OR branch IN('feature/codex','feature/agy','feature/dot','feature/grok','feature/muse')) AND (branch !~ '(\.\.|//|/$|\.lock$)')));
DO $m51$
DECLARE item record;source text;
BEGIN
 FOR item IN SELECT * FROM(VALUES ($s$forge_coding_reconciler_api.publication_finish(uuid,uuid,text,text,text,text,text)$s$,$o$p_branch!~'^forge/[A-Za-z0-9][A-Za-z0-9._/-]*$'$o$,$n$(p_branch!~'^forge/[A-Za-z0-9][A-Za-z0-9._/-]*$' AND p_branch NOT IN('feature/codex','feature/agy','feature/dot','feature/grok','feature/muse'))$n$,$h$cdd6f7f43a0556de8f15cfc84520d0ee76501cbaaf09a3592581cfc65b15c936$h$),
($s$forge_coding_reconciler_api.native_agent_branch_published(uuid,uuid,jsonb)$s$,$o$p_receipt->>'branch' !~ '^forge/[A-Za-z0-9][A-Za-z0-9._/-]{0,121}$'$o$,$n$(p_receipt->>'branch' !~ '^forge/[A-Za-z0-9][A-Za-z0-9._/-]{0,121}$' AND p_receipt->>'branch' NOT IN('feature/codex','feature/agy','feature/dot','feature/grok','feature/muse'))$n$,$h$ad597570fb20df95a16fd0c80cfb90a53863553d6bec97e322add5bb4697f237$h$)) x(signature,old_fragment,new_fragment,source_sha) LOOP
  SELECT prosrc INTO source FROM pg_proc WHERE oid=item.signature::regprocedure;
  IF encode(sha256(convert_to(source,'UTF8')),'hex')<>item.source_sha THEN RAISE EXCEPTION 'M51 predecessor drift: %',item.signature;END IF;
  IF (length(source)-length(replace(source,item.old_fragment,'')))/length(item.old_fragment)<>1 THEN RAISE EXCEPTION 'M51 nonunique patch';END IF;
  EXECUTE replace(pg_get_functiondef(item.signature::regprocedure),item.old_fragment,item.new_fragment);
 END LOOP;
END
$m51$;
