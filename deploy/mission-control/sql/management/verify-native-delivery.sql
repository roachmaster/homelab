-- M25 exact source/owner/ACL pins; run in the full strict verifier closure.
DO $native_delivery_verify$
DECLARE expected record;actual text;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=25 AND name='native_delivery_repair') THEN
  RAISE EXCEPTION 'Missing native delivery repair migration25';
 END IF;
 FOR expected IN SELECT * FROM(VALUES
  ('forge_coding_reconciler_api.native_merge_failure(uuid,uuid,jsonb)',CASE WHEN EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=26 AND name='coding_owner_continuation') THEN '925d26d339071f5ef52c200d279d6ec3cf9c105cfcc2e3484e751cdc8250b530' ELSE '1893187612aecee06b87a277f38be8d4b54dd48a93843516bf2b5e76cfbec231' END),
  ('forge_coding_reconciler_api.native_branch_first_replay(uuid,bigint)',CASE WHEN EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=26 AND name='coding_owner_continuation') THEN 'e8a8a79743e96d944101ff5363e653425f9d26983f001e2950a1fabfe78a7024' ELSE '132e5f4a859c181b90b9fa4f1792f42c1c6603e804258f4084a720222952c6a6' END),
  ('forge_coding_reconciler_api.native_branch_published(uuid,uuid,jsonb)','929f3178156072059f6301bb3974c164806f1f29c7ce28b791521cd179e575da'),
  ('forge_coding_worker_api.read_native_delivery_context(uuid,uuid)','b41e4cb0cc6f3418e7f691f0530086088e8cf5055939f55bf95edc58df2fc0a9')
 )x(signature,sha256) LOOP
  SELECT encode(sha256(convert_to(jsonb_build_array(pg_get_functiondef(p.oid),pg_get_userbyid(p.proowner),p.proacl)::text,'UTF8')),'hex')
  INTO actual FROM pg_proc p WHERE p.oid=expected.signature::regprocedure;
  IF actual IS DISTINCT FROM expected.sha256 THEN RAISE EXCEPTION 'Native delivery function drift: %',expected.signature;END IF;
 END LOOP;
END;
$native_delivery_verify$;
