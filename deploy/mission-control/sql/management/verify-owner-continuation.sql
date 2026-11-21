-- M26 exact source/owner/ACL pins; no source normalization precedes this guard.
DO $owner_continuation_verify$
DECLARE expected record;actual text;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=26 AND name='coding_owner_continuation') THEN
  RAISE EXCEPTION 'Missing owner continuation migration26';END IF;
 FOR expected IN SELECT * FROM(VALUES
  ('forge_agent._coding_owner_continuation_fenced(uuid)','245121de26e34ee55a80cc192e936b5e6ff3c8cba8e0daeed23fd957e986a64e'),
  ('forge_agent.coding_schedule_repair(uuid,uuid)','0f5f8c0891d7aff543246d582879f8893fe6804aff64b67a5f4442265f845187'),
  ('forge_agent.owner_continuation_capability()','bb0d3d842372691a43669cf94a7a05bddb726814c12d4269e84a73082fd0c434'),
  ('forge_coding_reconciler_api.native_branch_first_replay(uuid,bigint)','e8a8a79743e96d944101ff5363e653425f9d26983f001e2950a1fabfe78a7024'),
  ('forge_coding_reconciler_api.native_merge_failure(uuid,uuid,jsonb)','925d26d339071f5ef52c200d279d6ec3cf9c105cfcc2e3484e751cdc8250b530'),
  ('forge_coding_reconciler_api.owner_continuation_capability()','63b32a27728da10c0b31239044a23193b5a3068361ea914d1e6a4494de01e543'),
  ('forge_coding_reconciler_api.owner_continuation_fenced(jsonb)','956d9197560fab8fd931fb9d590b3d6a3d81fccb2cb3bf295e25114d809a536d'),
  ('forge_coding_reconciler_api.review_finish(uuid,uuid,jsonb)','7f954cb3256b9d3b289b7e703ac23b039acef77750f3bb4fb051f2e8fddbdade')
 )x(signature,sha256) LOOP
  SELECT encode(sha256(convert_to(jsonb_build_array(pg_get_functiondef(p.oid),pg_get_userbyid(p.proowner),p.proacl)::text,'UTF8')),'hex')
  INTO actual FROM pg_proc p WHERE p.oid=expected.signature::regprocedure;
  IF actual IS DISTINCT FROM expected.sha256 THEN RAISE EXCEPTION 'Owner continuation function drift: %',expected.signature;END IF;
 END LOOP;
END;
$owner_continuation_verify$;
