\set ON_ERROR_STOP on
-- Read-only check of the homelab_mcp reader: no write, DDL or role authority,
-- read-only defaults set on the role. Changes nothing.
BEGIN READ ONLY;
DO $mcp_reader_verify$
DECLARE
    reader oid;
    settings text[];
BEGIN
    SELECT oid INTO reader FROM pg_roles WHERE rolname='homelab_mcp';
    IF reader IS NULL THEN
        RAISE EXCEPTION 'homelab_mcp role does not exist';
    END IF;
    IF EXISTS(SELECT FROM pg_roles WHERE oid=reader AND
              (NOT rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls))
       OR EXISTS(SELECT FROM pg_auth_members WHERE member=reader OR roleid=reader) THEN
        RAISE EXCEPTION 'homelab_mcp has unexpected role authority';
    END IF;
    IF has_database_privilege(reader, current_database(), 'CREATE')
       OR has_schema_privilege(reader, 'forge_agent', 'CREATE')
       OR EXISTS(SELECT FROM pg_namespace WHERE nspowner=reader)
       OR EXISTS(SELECT FROM pg_class WHERE relowner=reader) THEN
        RAISE EXCEPTION 'homelab_mcp can create or owns objects';
    END IF;
    IF EXISTS(SELECT FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
              WHERE n.nspname NOT IN ('pg_catalog','information_schema','pg_toast')
                AND CASE WHEN c.relkind IN ('r','p','v','m','f') THEN has_table_privilege(reader, c.oid, 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER') ELSE false END) THEN
        RAISE EXCEPTION 'homelab_mcp has write privileges on a table';
    END IF;
    IF EXISTS(SELECT FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
              WHERE n.nspname NOT IN ('pg_catalog','information_schema','pg_toast')
                AND CASE WHEN c.relkind='S' THEN has_sequence_privilege(reader, c.oid, 'USAGE,UPDATE') ELSE false END) THEN
        RAISE EXCEPTION 'homelab_mcp can advance a sequence';
    END IF;
    SELECT setconfig INTO settings FROM pg_db_role_setting WHERE setrole=reader AND setdatabase=0;
    IF settings IS NULL OR NOT ('default_transaction_read_only=on' = ANY(settings)) THEN
        RAISE EXCEPTION 'homelab_mcp must have default_transaction_read_only=on';
    END IF;
END;
$mcp_reader_verify$;
SELECT 'homelab_mcp reader verified' AS result;
COMMIT;
