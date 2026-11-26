-- M49 (phase 0d): gate identity may be a named contract, not only a digest.
--
-- Purely widening. Every column and function that required the gate value to
-- be a 64-hex manifest digest now also accepts a named gate contract
-- '<profile>:<branch_scope>' (for example 'forge-sdlc-jenkins-v3:main'), the
-- form the Jenkins verifier accepts once its contract-name change is applied.
-- Nothing previously accepted is rejected, no row is rewritten, and the
-- runner keeps sending digests until a later step switches it over.
-- Live function definitions are patched in place (owner and grants unchanged)
-- and every patch must apply exactly.

ALTER TABLE forge_agent.coding_reviews
 DROP CONSTRAINT coding_reviews_gate_manifest_sha256_check,
 ADD CONSTRAINT coding_reviews_gate_manifest_sha256_check CHECK(gate_manifest_sha256 IS NULL
  OR gate_manifest_sha256~'^([0-9a-f]{64}|forge-sdlc-jenkins-v[1-3]:(main|detached|branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?))$');
ALTER TABLE forge_agent.coding_workflow_reviews
 DROP CONSTRAINT coding_workflow_reviews_gate_manifest_sha256_check,
 ADD CONSTRAINT coding_workflow_reviews_gate_manifest_sha256_check CHECK(gate_manifest_sha256 IS NULL
  OR gate_manifest_sha256~'^([0-9a-f]{64}|forge-sdlc-jenkins-v[1-3]:(main|detached|branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?))$');

DO $m49$
DECLARE
 gate constant text:=$g$'^([0-9a-f]{64}|forge-sdlc-jenkins-v[1-3]:(main|detached|branch/[a-z0-9]([a-z0-9._-]{0,56}[a-z0-9])?))$'$g$;
 fn text;src text;patched text;old text;new text;
BEGIN
 FOR fn,old,new IN VALUES
  ('forge_agent.delivery_request_valid(jsonb,uuid,uuid,text,text,text,uuid)',
   $o$p_request->>'gate_manifest_sha256'~'^[0-9a-f]{64}$'$o$,
   $n$p_request->>'gate_manifest_sha256'~$n$||gate),
  ('forge_coding_reconciler_api.post_merge_checkpoint(uuid,integer,uuid,jsonb)',
   $o$p_request->>'gate_manifest_sha256'!~'^[0-9a-f]{64}$'$o$,
   $n$p_request->>'gate_manifest_sha256'!~$n$||gate),
  ('forge_coding_reconciler_api.review_finish(uuid,uuid,jsonb)',
   $o$v_gate!~'^[0-9a-f]{64}$'$o$,
   $n$v_gate!~$n$||gate),
  ('forge_coding_reconciler_api.native_merge_failure(uuid,uuid,jsonb)',
   $o$x.value#>>'{}'!~'^[0-9a-f]{64}$')$o$,
   $n$x.value#>>'{}'!~CASE WHEN x.key='gate_manifest_sha256' THEN $n$||gate||$n$ ELSE '^[0-9a-f]{64}$' END)$n$)
 LOOP
  src:=pg_get_functiondef(fn::regprocedure);
  IF (length(src)-length(replace(src,old,'')))/length(old)<>1 THEN
   RAISE EXCEPTION 'M49 gate contract patch target not unique in %',fn;
  END IF;
  patched:=replace(src,old,new);
  EXECUTE patched;
 END LOOP;
 -- review_finish is verified through _retry_legacy_source(), which pins the
 -- M36 source by hash. Undo exactly this widening first so the pinned
 -- predecessor restoration (and every verifier using it) is unchanged.
 src:=pg_get_functiondef('forge_agent._retry_legacy_source(oid,text)'::regprocedure);
 old:=$o$DECLARE item jsonb;pair jsonb;source text:=p_source;
BEGIN
$o$;
 new:=old||$n$ IF p_oid='forge_coding_reconciler_api.review_finish(uuid,uuid,jsonb)'::regprocedure THEN
  source:=replace(source,$n$||quote_literal('v_gate!~'||gate)||$n$,$n$||quote_literal($q$v_gate!~'^[0-9a-f]{64}$'$q$)||$n$);
 END IF;
$n$;
 IF (length(src)-length(replace(src,old,'')))/length(old)<>1 THEN
  RAISE EXCEPTION 'M49 retry legacy source patch target not unique';
 END IF;
 EXECUTE replace(src,old,new);
END
$m49$;
