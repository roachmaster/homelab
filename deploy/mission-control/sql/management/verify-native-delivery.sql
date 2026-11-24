-- M25 exact source/owner/ACL pins; run in the full strict verifier closure.
DO $native_delivery_verify$
DECLARE expected record;actual text;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=25 AND name='native_delivery_repair') THEN
  RAISE EXCEPTION 'Missing native delivery repair migration25';
 END IF;
 FOR expected IN SELECT * FROM(VALUES
  ('forge_coding_reconciler_api.native_merge_failure(uuid,uuid,jsonb)',CASE WHEN EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=26 AND name='coding_owner_continuation') THEN '37d0e32b3f38ceaf9ed565fcc50286bb0108fccca54e98be4dab17c4dc8fe1aa' ELSE '1893187612aecee06b87a277f38be8d4b54dd48a93843516bf2b5e76cfbec231' END),
  ('forge_coding_reconciler_api.native_branch_first_replay(uuid,bigint)',CASE WHEN EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=26 AND name='coding_owner_continuation') THEN '1f7d62b96d7fb8ba04cadfe9e77f07303f2f45f4e9a7a03f92be938a00adb52e' ELSE '132e5f4a859c181b90b9fa4f1792f42c1c6603e804258f4084a720222952c6a6' END),
  ('forge_coding_reconciler_api.native_branch_published(uuid,uuid,jsonb)','dfead8657a659cc6c76e17997086c95642b9c27641e5d8edecf4f792f2dcb309'),
  ('forge_coding_worker_api.read_native_delivery_context(uuid,uuid)','b41e4cb0cc6f3418e7f691f0530086088e8cf5055939f55bf95edc58df2fc0a9')
 )x(signature,sha256) LOOP
  SELECT encode(sha256(convert_to(jsonb_build_array(pg_get_functiondef(p.oid),pg_get_userbyid(p.proowner),p.proacl)::text,'UTF8')),'hex')
  INTO actual FROM pg_proc p WHERE p.oid=expected.signature::regprocedure;
  IF actual IS DISTINCT FROM expected.sha256 THEN RAISE EXCEPTION 'Native delivery function drift: %',expected.signature;END IF;
 END LOOP;
END;
$native_delivery_verify$;
