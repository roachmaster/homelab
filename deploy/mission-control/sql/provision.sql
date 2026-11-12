\set ON_ERROR_STOP on

SELECT 'CREATE ROLE forge_agent_owner NOLOGIN'
WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'forge_agent_owner')
\gexec

SELECT 'CREATE ROLE forge_agent LOGIN'
WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'forge_agent')
\gexec

DO $check$
BEGIN
    IF EXISTS (
        SELECT FROM pg_roles
        WHERE rolname IN ('forge_agent_owner', 'forge_agent')
          AND (rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls
               OR rolcanlogin <> (rolname = 'forge_agent'))
    ) OR EXISTS (
        SELECT FROM pg_auth_members
        WHERE member IN ('forge_agent_owner'::regrole, 'forge_agent'::regrole)
    ) THEN
        RAISE EXCEPTION 'Existing Forge roles have unexpected privileges';
    END IF;
    IF EXISTS (
        SELECT FROM pg_database
        WHERE datname = 'forge_agent' AND datdba <> 'forge_agent_owner'::regrole
    ) THEN
        RAISE EXCEPTION 'Existing forge_agent database has a different owner';
    END IF;
END;
$check$;

SELECT 'CREATE DATABASE forge_agent OWNER forge_agent_owner'
WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'forge_agent')
\gexec

REVOKE CONNECT, TEMP ON DATABASE forge_agent FROM PUBLIC;
GRANT CONNECT ON DATABASE forge_agent TO forge_agent;
