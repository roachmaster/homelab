-- Exact M31 predecessor patches; no applied migration or receipt is rewritten.
CREATE FUNCTION forge_agent._delivery_lineage_patches() RETURNS jsonb LANGUAGE sql IMMUTABLE SET search_path=pg_catalog,pg_temp AS $f$
 SELECT $patches32$[{"name":"forge_agent.delivery_register_post_merge(uuid)","old_sha":"73ed8466625b260c8fb79f1d6b9c22d317c7333a9de2c7752e0b04eee363fb0b","new_sha":"41a9e14222f686da0104863711ae77964c12cccb1e9592a87cfd8dbe382c80d1","patches":[[" IF d.tree_sha IS DISTINCT FROM r.merge_tree THEN RAISE EXCEPTION 'Delivery commit tree conflict' USING ERRCODE='P0009';END IF;"," IF d.tree_sha IS DISTINCT FROM r.merge_tree THEN RAISE EXCEPTION 'Delivery commit tree conflict' USING ERRCODE='P0009';END IF;\n PERFORM forge_agent._delivery_adopt_repair_lineage(r.post_merge_id,d.verification_id);\n SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=d.verification_id;"]]},{"name":"forge_agent.delivery_repair_claim(text)","old_sha":"da926b1e50f9a43e15652466cafd5d94dd1d96b880367c3975f80f089274fb11","new_sha":"355ae482bcca09d11a0b629e47e97d8f3ab12f36d14e98e00f039717a1b772cc","patches":[[" SELECT * INTO l FROM forge_agent.delivery_lineages WHERE lineage_id=d.lineage_id FOR UPDATE;"," SELECT * INTO l FROM forge_agent.delivery_lineages WHERE lineage_id=d.lineage_id FOR UPDATE;\n IF forge_agent._delivery_lineage_blocked(a.owner_id,a.project_alias,d.lineage_id) THEN\n  PERFORM forge_agent.delivery_operation_finish(a.operation_id,a.generation,a.lease_token,'needs_attention',jsonb_build_object('reason','delivery_lineage_conflict'),\n   'Confirmed repair lineage conflicts with existing observed work; retained counters and receipts require reconciliation.');RETURN NULL;END IF;"]]},{"name":"forge_agent.delivery_repair_checkpoint(uuid,integer,uuid,jsonb)","old_sha":"54bac58fb46383719fc430f59db957348aaeca0c71fb2d4433288fbe6e317495","new_sha":"8dbe576e4b8027a585b30cd31309b8cc4786b5c68f7948b61926f76ca48a3ba7","patches":[[" IF a.request IS NOT NULL THEN IF a.request IS DISTINCT FROM p_intent THEN RAISE EXCEPTION 'Repair intent conflict' USING ERRCODE='P0009';END IF;RETURN true;END IF;"," IF EXISTS(SELECT FROM forge_agent.delivery_ledger ledger_row WHERE ledger_row.verification_id=a.verification_id AND forge_agent._delivery_lineage_blocked(a.owner_id,a.project_alias,ledger_row.lineage_id)) THEN\n  PERFORM forge_agent.delivery_operation_finish(a.operation_id,a.generation,a.lease_token,'needs_attention',jsonb_build_object('reason','delivery_lineage_conflict'),\n   'Confirmed repair lineage conflicts with existing observed work; retained counters and receipts require reconciliation.');RETURN false;END IF;\n IF a.request IS NOT NULL THEN IF a.request IS DISTINCT FROM p_intent THEN RAISE EXCEPTION 'Repair intent conflict' USING ERRCODE='P0009';END IF;RETURN true;END IF;"]]},{"name":"forge_agent.delivery_repair_admit(uuid,integer,uuid,text)","old_sha":"a2eaf2f242b7a1c356d316b9b69a344bd28f334c19e75e1f985f34edf0e6d1ec","new_sha":"16b41c928d9cb794ef14daf2685887d81d69342d019f3be6fba860d6fea65733","patches":[[" RETURN true;"," IF EXISTS(SELECT FROM forge_agent.delivery_ledger ledger_row WHERE ledger_row.verification_id=a.verification_id AND forge_agent._delivery_lineage_blocked(a.owner_id,a.project_alias,ledger_row.lineage_id)) THEN RAISE EXCEPTION 'Confirmed delivery lineage requires reconciliation' USING ERRCODE='P0009';END IF;\n RETURN true;"]]}]$patches32$::jsonb
$f$;
CREATE FUNCTION forge_agent._delivery_lineage_legacy_source(p_oid oid,p_source text) RETURNS text LANGUAGE plpgsql STABLE SET search_path=pg_catalog,pg_temp AS $f$
DECLARE item jsonb;pair jsonb;source text:=p_source;
BEGIN
 FOR item IN SELECT jsonb_array_elements(forge_agent._delivery_lineage_patches()) LOOP
  IF p_oid=(item->>'name')::regprocedure THEN
   IF encode(sha256(convert_to(source,'UTF8')),'hex')<>item->>'new_sha' THEN RAISE EXCEPTION 'Delivery lineage authority source drift';END IF;
   FOR pair IN SELECT value FROM jsonb_array_elements(item->'patches') WITH ORDINALITY p(value,n) ORDER BY n DESC LOOP source:=replace(source,pair->>1,pair->>0);END LOOP;
   IF encode(sha256(convert_to(source,'UTF8')),'hex')<>item->>'old_sha' THEN RAISE EXCEPTION 'Delivery lineage predecessor restoration drift';END IF;
   RETURN source;
  END IF;
 END LOOP;
 RETURN source;
END;$f$;
DO $patch$
DECLARE item jsonb;pair jsonb;source text;definition text;
BEGIN
 FOR item IN SELECT jsonb_array_elements(forge_agent._delivery_lineage_patches()) LOOP
  SELECT prosrc,pg_get_functiondef(oid) INTO source,definition FROM pg_proc WHERE oid=(item->>'name')::regprocedure;
  IF encode(sha256(convert_to(source,'UTF8')),'hex')<>item->>'old_sha' THEN RAISE EXCEPTION 'Unexpected delivery lineage predecessor: %',item->>'name';END IF;
  FOR pair IN SELECT jsonb_array_elements(item->'patches') LOOP source:=replace(source,pair->>0,pair->>1);END LOOP;
  IF encode(sha256(convert_to(source,'UTF8')),'hex')<>item->>'new_sha' THEN RAISE EXCEPTION 'Unexpected delivery lineage source: %',item->>'name';END IF;
  EXECUTE replace(definition,(SELECT prosrc FROM pg_proc WHERE oid=(item->>'name')::regprocedure),source);
 END LOOP;
END;$patch$;
REVOKE ALL ON FUNCTION forge_agent._delivery_lineage_patches(),forge_agent._delivery_lineage_legacy_source(oid,text) FROM PUBLIC;
