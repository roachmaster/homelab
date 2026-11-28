-- Full body and security identity; historical M54 remains immutable.
DO $verify_proxy_chat$
DECLARE p record;
BEGIN
 IF EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=57 AND name<>'pg_proxy_chat_date') THEN RAISE EXCEPTION 'Unexpected migration57 identity'; END IF;
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=57 AND name='pg_proxy_chat_date') THEN RETURN; END IF;
 SELECT * INTO p FROM pg_proc WHERE oid='forge_pg_proxy_api.ide_chat_insert(text,uuid,text,jsonb,bigint,text,text,text)'::regprocedure;
 IF NOT FOUND OR encode(sha256(convert_to(p.prosrc,'UTF8')),'hex')<>'0d4000357cdde5ff6cbe1cbd610b61c41e17bf5324e5f40fa2b057b88db1c901'
   OR p.proowner<>'forge_agent_owner'::regrole OR NOT p.prosecdef OR p.prokind<>'f'
   OR p.prorettype<>'jsonb'::regtype OR p.prolang<>(SELECT oid FROM pg_language WHERE lanname='plpgsql')
   OR p.provolatile<>'v' OR p.proconfig IS DISTINCT FROM ARRAY['search_path=pg_catalog, pg_temp'] THEN
  RAISE EXCEPTION 'Unexpected proxy chat date function';
 END IF;
 IF (SELECT array_agg(role_name ORDER BY role_name) FROM (SELECT r.rolname::text AS role_name FROM aclexplode(p.proacl) a JOIN pg_roles r ON r.oid=a.grantee WHERE a.privilege_type='EXECUTE' AND NOT a.is_grantable AND a.grantor='forge_agent_owner'::regrole) q) IS DISTINCT FROM ARRAY['forge_agent','forge_agent_owner']::text[]
 OR EXISTS(SELECT FROM aclexplode(p.proacl) a WHERE a.grantee=0 OR a.is_grantable OR a.privilege_type<>'EXECUTE' OR a.grantor<>'forge_agent_owner'::regrole) THEN
  RAISE EXCEPTION 'Unexpected proxy chat date privileges';
 END IF;
END;
$verify_proxy_chat$;
