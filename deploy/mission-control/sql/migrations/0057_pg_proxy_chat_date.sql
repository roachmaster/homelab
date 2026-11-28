-- M57 fixes only the proven M54 text-to-date insertion error. No cutover/grants.
DO $guard$
BEGIN
 IF NOT EXISTS(SELECT FROM pg_proc WHERE oid='forge_pg_proxy_api.ide_chat_insert(text,uuid,text,jsonb,bigint,text,text,text)'::regprocedure
   AND encode(sha256(convert_to(prosrc,'UTF8')),'hex')='29db3437bcd064dc52318e9e5d9f373b4565d4f9abe97989fb6d8b1ecdfda370'
   AND proowner='forge_agent_owner'::regrole AND prosecdef AND prokind='f'
   AND prorettype='jsonb'::regtype AND proconfig=ARRAY['search_path=pg_catalog, pg_temp']
   AND provolatile='v' AND prolang=(SELECT oid FROM pg_language WHERE lanname='plpgsql')
   AND (SELECT array_agg(r.rolname::text ORDER BY r.rolname) FROM aclexplode(proacl) a JOIN pg_roles r ON r.oid=a.grantee WHERE a.privilege_type='EXECUTE' AND NOT a.is_grantable AND a.grantor='forge_agent_owner'::regrole)=ARRAY['forge_agent','forge_agent_owner']::text[]
   AND NOT EXISTS(SELECT FROM aclexplode(proacl) a WHERE a.grantee=0 OR a.is_grantable OR a.privilege_type<>'EXECUTE' OR a.grantor<>'forge_agent_owner'::regrole)) THEN
  RAISE EXCEPTION 'Unexpected proxy chat insertion predecessor';
 END IF;
END;
$guard$;
CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ide_chat_insert(p_owner_id text, p_id uuid, p_title text, p_messages jsonb, p_revision bigint, p_expires_on text, p_time_zone text, p_expires_at text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.ide_chats%ROWTYPE;
BEGIN
  IF p_owner_id IS NOT NULL AND p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.ide_chats (owner_id, id, title, messages, revision, expires_on, time_zone, expires_at)
    VALUES (p_owner_id, p_id, p_title, COALESCE(p_messages, '[]'::jsonb), COALESCE(p_revision, 0), p_expires_on::date, p_time_zone, p_expires_at::timestamptz)
  ON CONFLICT (owner_id, id) DO NOTHING
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
