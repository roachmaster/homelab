-- M26 exact source/owner/ACL pins; no source normalization precedes this guard.
DO $owner_continuation_verify$
DECLARE expected record;actual text;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=26 AND name='coding_owner_continuation') THEN
  RAISE EXCEPTION 'Missing owner continuation migration26';END IF;
 FOR expected IN SELECT * FROM(VALUES
  ('forge_agent._coding_owner_continuation_fenced(uuid)','69c4f503bb5002e547ff3bd426f94d819b3932882a7aa52167ca72fda5a5ce87'),
  ('forge_agent.coding_schedule_repair(uuid,uuid)','ab3fd8ef6cb48f60025f4c65e3f5f08500da3bf3a66f77218969263df99c3c0c'),
  ('forge_agent.owner_continuation_capability()','bb0d3d842372691a43669cf94a7a05bddb726814c12d4269e84a73082fd0c434'),
  ('forge_coding_reconciler_api.native_branch_first_replay(uuid,bigint)','1f7d62b96d7fb8ba04cadfe9e77f07303f2f45f4e9a7a03f92be938a00adb52e'),
  ('forge_coding_reconciler_api.native_merge_failure(uuid,uuid,jsonb)','37d0e32b3f38ceaf9ed565fcc50286bb0108fccca54e98be4dab17c4dc8fe1aa'),
  ('forge_coding_reconciler_api.owner_continuation_capability()','63b32a27728da10c0b31239044a23193b5a3068361ea914d1e6a4494de01e543'),
  ('forge_coding_reconciler_api.owner_continuation_fenced(jsonb)','956d9197560fab8fd931fb9d590b3d6a3d81fccb2cb3bf295e25114d809a536d'),
  ('forge_coding_reconciler_api.review_finish(uuid,uuid,jsonb)','d0d1b2b2debbd9a26934d9dad82103f856f8fe0943bd76895ab8afd6d956ac16')
 )x(signature,sha256) LOOP
  IF EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=31) THEN
  SELECT encode(sha256(convert_to(jsonb_build_array(replace(pg_get_functiondef(p.oid),p.prosrc,forge_agent._retry_legacy_source(p.oid,p.prosrc)),pg_get_userbyid(p.proowner),p.proacl)::text,'UTF8')),'hex')
  INTO actual FROM pg_proc p WHERE p.oid=expected.signature::regprocedure;
  ELSE
  SELECT encode(sha256(convert_to(jsonb_build_array(pg_get_functiondef(p.oid),pg_get_userbyid(p.proowner),p.proacl)::text,'UTF8')),'hex')
  INTO actual FROM pg_proc p WHERE p.oid=expected.signature::regprocedure;
  END IF;
  IF actual IS DISTINCT FROM expected.sha256 THEN RAISE EXCEPTION 'Owner continuation function drift: %',expected.signature;END IF;
 END LOOP;
END;
$owner_continuation_verify$;
