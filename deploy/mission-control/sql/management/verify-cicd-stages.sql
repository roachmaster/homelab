-- Strict M41 schema, immutability, and least-privilege checks; read only.
DO $cicd_stages_verify$
DECLARE
    relation oid;
    rel_name text;
    priv text;
    worker text;
    fn_sig text;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM forge_agent.schema_migrations
        WHERE version = 41 AND name = 'agent_branch_and_cicd_stages'
    ) THEN
        RAISE EXCEPTION 'Missing M41 migration agent_branch_and_cicd_stages';
    END IF;

    FOREACH rel_name IN ARRAY ARRAY[
        'agent_branch_state',
        'jenkins_stage_results',
        'jenkins_stage_log_chunks'
    ] LOOP
        relation := to_regclass('forge_agent.' || rel_name);
        IF relation IS NULL THEN
            RAISE EXCEPTION 'Missing M41 relation forge_agent.%', rel_name;
        END IF;
        FOREACH priv IN ARRAY ARRAY['SELECT','INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER'] LOOP
            IF has_table_privilege('forge_agent', relation, priv) IS DISTINCT FROM (priv = 'SELECT')
               OR (priv IN ('INSERT','UPDATE','REFERENCES') AND has_any_column_privilege('forge_agent', relation, priv))
               OR has_table_privilege('public', relation, priv)
               OR (priv IN ('SELECT','INSERT','UPDATE','REFERENCES') AND has_any_column_privilege('public', relation, priv)) THEN
                RAISE EXCEPTION 'Unexpected runtime/PUBLIC privilege % on forge_agent.%', priv, rel_name;
            END IF;
            FOREACH worker IN ARRAY ARRAY['forge_gpu_worker','forge_coding_worker','forge_coding_reconciler'] LOOP
                IF has_table_privilege(worker, relation, priv)
                   OR (priv IN ('SELECT','INSERT','UPDATE','REFERENCES') AND has_any_column_privilege(worker, relation, priv)) THEN
                    RAISE EXCEPTION 'Unexpected direct M41 table privilege for % on %', worker, rel_name;
                END IF;
            END LOOP;
        END LOOP;
    END LOOP;

    -- Verify additive columns and indexes on jenkins_events.
    IF NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_attribute a
        WHERE a.attrelid = 'forge_agent.jenkins_events'::regclass
          AND a.attname = 'action_id' AND a.atttypid = 'uuid'::regtype
          AND NOT a.attnotnull AND NOT a.attisdropped
    ) OR NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_attribute a
        WHERE a.attrelid = 'forge_agent.jenkins_events'::regclass
          AND a.attname = 'branch_scope' AND a.atttypid = 'text'::regtype
          AND NOT a.attnotnull AND NOT a.attisdropped
    ) THEN
        RAISE EXCEPTION 'Missing M41 additive columns on forge_agent.jenkins_events';
    END IF;

    -- Verify immutability triggers on stage results and log chunks.
    IF NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_trigger t
        WHERE t.tgrelid = 'forge_agent.jenkins_stage_results'::regclass
          AND t.tgname = 'jenkins_stage_results_immutable' AND t.tgenabled = 'O'
    ) OR NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_trigger t
        WHERE t.tgrelid = 'forge_agent.jenkins_stage_log_chunks'::regclass
          AND t.tgname = 'jenkins_stage_log_chunks_immutable' AND t.tgenabled = 'O'
    ) THEN
        RAISE EXCEPTION 'Missing or disabled M41 immutability triggers';
    END IF;

    -- Verify scoped reconciler API functions and least-privilege EXECUTE grants.
    FOREACH fn_sig IN ARRAY ARRAY[
        'forge_coding_reconciler_api.task_branch_scope(text,uuid)',
        'forge_coding_reconciler_api.agent_branch_get(text,text)',
        'forge_coding_reconciler_api.agent_branch_upsert(text,text,text,text,text,text,timestamptz,text,jsonb,text,uuid,uuid)',
        'forge_coding_reconciler_api.agent_branch_acquire(text,text,text,text,text,uuid,uuid,uuid,uuid,uuid)',
        'forge_coding_reconciler_api.agent_branch_verify_owner(text,text,text,uuid,bigint)',
        'forge_coding_reconciler_api.agent_branch_record_publication(text,text,text,uuid,bigint,text,text,uuid)',
        'forge_coding_reconciler_api.agent_branch_set_blocker(text,text,text,uuid,bigint,text)',
        'forge_coding_reconciler_api.agent_branch_release(text,text,text,uuid,bigint,text,boolean,text)',
        'forge_coding_reconciler_api.agent_branch_reconcilable_reservations(integer)',
        'forge_coding_reconciler_api.review_claim_for_branch_policy(text)',
        'forge_coding_reconciler_api.publication_claim_for_branch_policy(text)',
        'forge_coding_reconciler_api.native_agent_branch_published(uuid,uuid,jsonb)',
        'forge_coding_reconciler_api.record_jenkins_stage_evidence(jsonb,jsonb)',
        'forge_external_delivery_api.agent_branch_acquire(text,text,text,text,text,uuid,uuid,uuid)',
        'forge_external_delivery_api.agent_branch_verify_owner(text,text,text,uuid,bigint)',
        'forge_external_delivery_api.agent_branch_upsert(text,text,text,text,text,text,text,timestamptz,text,jsonb,text,uuid,uuid)',
        'forge_external_delivery_api.agent_branch_release(text,text,text,uuid,bigint,text,boolean,text)'
    ] LOOP
        IF to_regprocedure(fn_sig) IS NULL THEN
            RAISE EXCEPTION 'Missing M41 function %', fn_sig;
        END IF;
        IF NOT has_function_privilege('forge_coding_reconciler', fn_sig, 'EXECUTE')
           OR has_function_privilege('public', fn_sig, 'EXECUTE')
           OR has_function_privilege('forge_agent', fn_sig, 'EXECUTE')
           OR has_function_privilege('forge_coding_worker', fn_sig, 'EXECUTE')
           OR has_function_privilege('forge_gpu_worker', fn_sig, 'EXECUTE') THEN
            RAISE EXCEPTION 'Unexpected M41 function execute privilege on %', fn_sig;
        END IF;
    END LOOP;

    IF NOT has_schema_privilege('forge_coding_reconciler', 'forge_external_delivery_api', 'USAGE')
       OR has_schema_privilege('forge_coding_reconciler', 'forge_external_delivery_api', 'CREATE')
       OR has_schema_privilege('public', 'forge_external_delivery_api', 'USAGE, CREATE')
       OR has_schema_privilege('forge_agent', 'forge_external_delivery_api', 'USAGE, CREATE')
       OR has_schema_privilege('forge_coding_worker', 'forge_external_delivery_api', 'USAGE, CREATE')
       OR has_schema_privilege('forge_gpu_worker', 'forge_external_delivery_api', 'USAGE, CREATE') THEN
        RAISE EXCEPTION 'Unexpected schema privileges on forge_external_delivery_api';
    END IF;
END;
$cicd_stages_verify$;
