-- Optional, existing Muse reader: exact read access only, never task authority.
-- Included within verify.sql's READ ONLY transaction; does not repair any ACL.
DO $muse_read_verify$
DECLARE
    reader oid;
    owner_role oid := 'forge_agent_owner'::regrole;
    allowed_tables constant text[] := ARRAY['coding_events', 'coding_plan_decisions', 'coding_result_chunks', 'coding_reconciliation_receipts', 'coding_publications', 'coding_hosted_checks', 'coding_feedback', 'coding_reviews', 'coding_tasks', 'coding_workflows', 'coding_workflow_reviews', 'coding_workflow_actions', 'coding_attempts'];
    relation record;
    permitted boolean;
    privilege_name text;
    direct_count bigint;
BEGIN
    SELECT oid INTO reader FROM pg_roles WHERE rolname='muse_events';
    IF reader IS NULL THEN RETURN; END IF;
    IF current_database()<>'forge_agent' THEN
        RAISE EXCEPTION 'Muse reader contract requires forge_agent database';
    END IF;
    -- Preserve INHERIT/NOINHERIT and account configuration; neither confers
    -- other authority without memberships. Do not change login or credentials.
    IF EXISTS(SELECT FROM pg_roles WHERE oid=reader AND
              (NOT rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls))
       OR EXISTS(SELECT FROM pg_auth_members WHERE member=reader OR roleid=reader) THEN
        RAISE EXCEPTION 'Unexpected Muse reader role authority';
    END IF;
    IF NOT has_database_privilege(reader,current_database(),'CONNECT')
       OR has_database_privilege(reader,current_database(),'CREATE,TEMP')
       OR EXISTS(SELECT FROM pg_database WHERE datname=current_database() AND datdba=reader) THEN
        RAISE EXCEPTION 'Unexpected Muse reader database privileges';
    END IF;
    SELECT count(*) INTO direct_count FROM pg_database d,
        LATERAL aclexplode(d.datacl) a
        WHERE d.datname=current_database() AND a.grantee=reader
          AND a.grantor=owner_role AND a.privilege_type='CONNECT' AND NOT a.is_grantable;
    IF direct_count<>1 OR EXISTS(SELECT FROM pg_database d,LATERAL aclexplode(d.datacl) a
        WHERE d.datname=current_database() AND a.grantee=reader
          AND (a.grantor<>owner_role OR a.privilege_type<>'CONNECT' OR a.is_grantable)) THEN
        RAISE EXCEPTION 'Unexpected direct Muse reader database ACL';
    END IF;
    IF NOT has_schema_privilege(reader,'forge_agent','USAGE')
       OR has_schema_privilege(reader,'forge_agent','CREATE') THEN
        RAISE EXCEPTION 'Unexpected Muse reader schema privileges';
    END IF;
    SELECT count(*) INTO direct_count FROM pg_namespace n,LATERAL aclexplode(n.nspacl) a
        WHERE n.nspname='forge_agent' AND a.grantee=reader
          AND a.grantor=owner_role AND a.privilege_type='USAGE' AND NOT a.is_grantable;
    IF direct_count<>1 OR EXISTS(SELECT FROM pg_namespace n,LATERAL aclexplode(n.nspacl) a
        WHERE n.nspname='forge_agent' AND a.grantee=reader
          AND (a.grantor<>owner_role OR a.privilege_type<>'USAGE' OR a.is_grantable))
       OR EXISTS(SELECT FROM pg_namespace n
          WHERE n.nspname NOT LIKE 'pg\_%' ESCAPE '\' AND n.nspname<>'information_schema'
            AND (n.nspowner=reader OR has_schema_privilege(reader,n.oid,'CREATE')
              OR (n.nspname NOT IN('forge_agent','public') AND has_schema_privilege(reader,n.oid,'USAGE'))
              OR (n.nspname<>'forge_agent' AND EXISTS(
                  SELECT FROM aclexplode(n.nspacl) a WHERE a.grantee=reader)))) THEN
        RAISE EXCEPTION 'Unexpected direct Muse reader schema ACL';
    END IF;
    FOREACH privilege_name IN ARRAY allowed_tables LOOP
        IF to_regclass('forge_agent.'||privilege_name) IS NULL THEN
            RAISE EXCEPTION 'Missing authorized Muse reader table %',privilege_name;
        END IF;
    END LOOP;
    FOR relation IN SELECT c.*,n.nspname FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
        WHERE n.nspname NOT LIKE 'pg\_%' ESCAPE '\' AND n.nspname<>'information_schema'
          AND c.relkind IN('r','p','v','m','f') LOOP
        permitted := relation.nspname='forge_agent' AND relation.relname=ANY(allowed_tables)
                     AND relation.relkind IN('r','p');
        IF relation.relowner=reader THEN RAISE EXCEPTION 'Muse reader owns application relation'; END IF;
        SELECT count(*) INTO direct_count FROM aclexplode(relation.relacl) a
            WHERE a.grantee=reader AND a.grantor=owner_role
              AND a.privilege_type='SELECT' AND NOT a.is_grantable;
        IF direct_count<>(CASE WHEN permitted THEN 1 ELSE 0 END)
           OR EXISTS(SELECT FROM aclexplode(relation.relacl) a
                     WHERE a.grantee IN(reader,0) AND
                       (a.grantee=0 OR NOT permitted OR a.grantor<>owner_role OR a.privilege_type<>'SELECT' OR a.is_grantable))
           OR EXISTS(SELECT FROM pg_attribute c,LATERAL aclexplode(c.attacl) a
                     WHERE c.attrelid=relation.oid AND a.grantee IN(reader,0)) THEN
            RAISE EXCEPTION 'Unexpected Muse reader relation ACL: %.%',relation.nspname,relation.relname;
        END IF;
        FOREACH privilege_name IN ARRAY ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER'] LOOP
            IF has_table_privilege(reader,relation.oid,privilege_name)
               IS DISTINCT FROM (permitted AND privilege_name='SELECT') THEN
                RAISE EXCEPTION 'Unexpected effective Muse reader privilege % on %',privilege_name,relation.relname;
            END IF;
            IF privilege_name IN('SELECT','INSERT','UPDATE','REFERENCES')
               AND NOT(permitted AND privilege_name='SELECT')
               AND has_any_column_privilege(reader,relation.oid,privilege_name) THEN
                RAISE EXCEPTION 'Unexpected effective Muse reader column privilege on %',relation.relname;
            END IF;
        END LOOP;
    END LOOP;
    IF EXISTS(SELECT FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
        WHERE n.nspname NOT LIKE 'pg\_%' ESCAPE '\' AND n.nspname<>'information_schema'
          AND (p.proowner=reader OR has_function_privilege(reader,p.oid,'EXECUTE')))
       OR EXISTS(SELECT FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
        WHERE n.nspname NOT LIKE 'pg\_%' ESCAPE '\' AND n.nspname<>'information_schema' AND c.relkind='S'
          AND (c.relowner=reader OR has_sequence_privilege(reader,c.oid,'USAGE,SELECT,UPDATE')))
       OR EXISTS(SELECT FROM pg_default_acl d,LATERAL aclexplode(d.defaclacl) a
        WHERE a.grantee IN(reader,0) AND (d.defaclnamespace=0 OR d.defaclnamespace IN
            (SELECT oid FROM pg_namespace WHERE nspname NOT LIKE 'pg\_%' ESCAPE '\'
              AND nspname<>'information_schema'))) THEN
        RAISE EXCEPTION 'Unexpected Muse reader function, sequence or default privileges';
    END IF;
END;
$muse_read_verify$;
