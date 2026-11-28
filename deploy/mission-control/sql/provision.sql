\set ON_ERROR_STOP on

SELECT 'CREATE ROLE forge_agent_owner NOLOGIN'
WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'forge_agent_owner')
\gexec

SELECT 'CREATE ROLE forge_agent LOGIN'
WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'forge_agent')
\gexec

SELECT 'CREATE ROLE forge_gpu_worker LOGIN NOINHERIT'
WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'forge_gpu_worker')
\gexec

SELECT 'CREATE ROLE forge_coding_worker LOGIN NOINHERIT'
WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'forge_coding_worker')
\gexec

SELECT 'CREATE ROLE forge_coding_reconciler LOGIN NOINHERIT'
WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'forge_coding_reconciler')
\gexec

SELECT 'CREATE ROLE forge_pg_proxy LOGIN NOINHERIT'
WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'forge_pg_proxy')
\gexec

SELECT 'CREATE ROLE muse_proxy_writer LOGIN NOINHERIT'
WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'muse_proxy_writer')
\gexec

DO $check$
BEGIN
    IF EXISTS (
        SELECT FROM pg_roles
        WHERE rolname IN (
            'forge_agent_owner', 'forge_agent', 'forge_gpu_worker',
            'forge_coding_worker', 'forge_coding_reconciler',
            'forge_pg_proxy', 'muse_proxy_writer'
        )
          AND (rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls
               OR rolcanlogin <> (rolname <> 'forge_agent_owner')
               OR (rolname IN ('forge_gpu_worker', 'forge_coding_worker', 'forge_coding_reconciler', 'forge_pg_proxy', 'muse_proxy_writer') AND rolinherit))
    ) OR EXISTS (
        SELECT FROM pg_auth_members
        WHERE member IN (
            'forge_agent_owner'::regrole, 'forge_agent'::regrole, 'forge_gpu_worker'::regrole,
            'forge_coding_worker'::regrole, 'forge_coding_reconciler'::regrole,
            'forge_pg_proxy'::regrole, 'muse_proxy_writer'::regrole
        )
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
GRANT CONNECT ON DATABASE forge_agent TO forge_gpu_worker;
GRANT CONNECT ON DATABASE forge_agent TO forge_coding_worker;
GRANT CONNECT ON DATABASE forge_agent TO forge_coding_reconciler;
GRANT CONNECT ON DATABASE forge_agent TO forge_pg_proxy;
GRANT CONNECT ON DATABASE forge_agent TO muse_proxy_writer;
