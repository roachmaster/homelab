-- M40 exact source-bound author handoff authority, before predecessor normalization.
DO $author_handoff_verify$
DECLARE item record;actual text;
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=40) THEN RETURN;END IF;
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=40 AND name='provider_author_handoff') THEN RAISE EXCEPTION 'Author handoff migration identity drift';END IF;
 FOR item IN SELECT * FROM(VALUES ('forge_agent._coding_author_canonical_json(jsonb)','f8fb0e70a7a76d1f356c32797ca8d7c3080f63a6569c1c964b57e1d86eaeee8d'),('forge_agent._coding_author_feedback_proof(uuid,text)','cca3219412f4f6f8a406ae8832c906815ded944e715cf8046bd9167fc3b9f92f'),('forge_agent._coding_author_handoff_legacy_source(oid,text)','8e3580870dc840623df7387e37ffa6de153bde3cf1283b6310af2fc0de22cf9d'),('forge_agent.coding_workflow_author_feedback(uuid,text)','82f1a2a9eb4e8aa10a3d32f0e346e260cdf1adb5327897fe042013996e17e305'),('forge_agent.coding_workflow_author_handoff(uuid)','cc0cddabe9ca1ee88b312aa89bf050b872fe88e406dc79ba38b8f25b6eb2999f'),('forge_agent.coding_workflow_author_profile(uuid)','daeed5d10cf9e0fd2ce2e186c68e7b08aada841b071d2483182868ff613d5a95'),('forge_agent.coding_workflow_role_granted(uuid,text,text)','2fdd7c1e5f38837ff9957ae5ccfa0359c218b29468aa8353fdb9279fd3d7ee3a'),('forge_coding_worker_api.claim(text,uuid,text[])','e0c21116e0f982eadaf5fd2119125abe6ee8dd8cac88fb85de2f367d6217aee0'),('forge_coding_worker_api.read_feedback(uuid,uuid)','6e51a27649499c1273d27fd69861136111c1366733390e0a7dedd47ee00ecab1'),('forge_coding_worker_api.read_workflow_feedback(uuid,uuid)','6386ace6d7c657feb3b9d69e56d59b94f4e533783133924594b67cdddbdac617')) expected(signature,fingerprint) LOOP
  SELECT encode(sha256(convert_to(jsonb_build_array(pg_get_functiondef(p.oid),pg_get_userbyid(p.proowner),p.proacl)::text,'UTF8')),'hex') INTO actual FROM pg_proc p WHERE p.oid=to_regprocedure(item.signature);
  IF actual IS DISTINCT FROM item.fingerprint THEN RAISE EXCEPTION 'Author handoff function drift: %',item.signature;END IF;
 END LOOP;
END;$author_handoff_verify$;
