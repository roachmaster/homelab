-- Additive source-bound review/repair and append-only verification runs. Historical
-- v1-v4 recipes/results/receipts remain unchanged; v5 is explicit admission.
ALTER TABLE forge_agent.coding_tasks
 ADD COLUMN feedback_id uuid,
 ADD COLUMN feedback_sha256 text CHECK (feedback_sha256 IS NULL OR feedback_sha256 ~ '^[0-9a-f]{64}$'),
 ADD COLUMN legacy_resume_note text NOT NULL DEFAULT '' CHECK (octet_length(legacy_resume_note)<=2048),
 ADD COLUMN review_required boolean NOT NULL DEFAULT false,
 ADD COLUMN parent_task_id uuid,
 ADD COLUMN repair_source_review_id uuid,
 ADD COLUMN repair_candidate_commit text CHECK (repair_candidate_commit IS NULL OR repair_candidate_commit ~ '^[0-9a-f]{40}$');
ALTER TABLE forge_agent.coding_tasks ALTER COLUMN review_required SET DEFAULT true;
ALTER TABLE forge_agent.coding_attempts
 ADD COLUMN feedback_id uuid,
 ADD COLUMN feedback_sha256 text,
 ADD COLUMN legacy_resume_note text NOT NULL DEFAULT '',
 ADD COLUMN repair_source_review_id uuid,
 ADD COLUMN repair_candidate_commit text;
ALTER TABLE forge_agent.coding_tasks ADD CONSTRAINT coding_tasks_parent_fk
 FOREIGN KEY(owner_id,project_alias,parent_task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id);

CREATE TABLE forge_agent.coding_review_policy (
 owner_id text NOT NULL, project_alias text NOT NULL,
 version text NOT NULL DEFAULT 'forge-repair-v1' CHECK(version='forge-repair-v1'),
 max_rounds integer NOT NULL DEFAULT 3 CHECK(max_rounds BETWEEN 0 AND 10),
 max_stagnant_rounds integer NOT NULL DEFAULT 2 CHECK(max_stagnant_rounds BETWEEN 1 AND 10),
 max_infrastructure_retries integer NOT NULL DEFAULT 1 CHECK(max_infrastructure_retries BETWEEN 0 AND 3),
 required_hosted_checks text[] NOT NULL DEFAULT ARRAY['check','pr-fast-gate','real-browser'] CHECK(cardinality(required_hosted_checks) BETWEEN 1 AND 16),
 PRIMARY KEY(owner_id,project_alias)
);
CREATE TABLE forge_agent.coding_reviews (
 review_id uuid PRIMARY KEY DEFAULT gen_random_uuid(), owner_id text NOT NULL,project_alias text NOT NULL,
 task_id uuid NOT NULL,attempt_id uuid NOT NULL,plan_decision_id uuid,
 reconciliation_receipt_id uuid NOT NULL REFERENCES forge_agent.coding_reconciliation_receipts(receipt_id),
 base_revision text NOT NULL CHECK(base_revision ~ '^[0-9a-f]{40}$'),
 workspace_digest text NOT NULL CHECK(workspace_digest ~ '^[0-9a-f]{64}$'),
 parent_result_sha256 text NOT NULL CHECK(parent_result_sha256 ~ '^[0-9a-f]{64}$'),
 source_revision bigint NOT NULL CHECK(source_revision BETWEEN 1 AND 9007199254740990),
 review_no integer NOT NULL CHECK(review_no>0),round integer NOT NULL DEFAULT 0 CHECK(round BETWEEN 0 AND 32),
 stagnant_rounds integer NOT NULL DEFAULT 0 CHECK(stagnant_rounds BETWEEN 0 AND 32),
 infrastructure_retries integer NOT NULL DEFAULT 0 CHECK(infrastructure_retries BETWEEN 0 AND 32),
 previous_review_id uuid REFERENCES forge_agent.coding_reviews(review_id),
 status text NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','checking','approved','repair','retry_infrastructure','awaiting_review','handoff','cancelled','needs_reconcile','revise_plan')),
 worker_id text,lease_token uuid,lease_expires_at timestamptz,
 candidate_commit text CHECK(candidate_commit IS NULL OR candidate_commit ~ '^[0-9a-f]{40}$'),
 gate_manifest_sha256 text CHECK(gate_manifest_sha256 IS NULL OR gate_manifest_sha256 ~ '^[0-9a-f]{64}$'),
 findings jsonb NOT NULL DEFAULT '[]' CHECK(jsonb_typeof(findings)='array' AND octet_length(findings::text)<=65536),
 verified_finding_ids jsonb NOT NULL DEFAULT '[]' CHECK(jsonb_typeof(verified_finding_ids)='array'),
 verification jsonb NOT NULL DEFAULT '{}' CHECK(jsonb_typeof(verification)='object' AND octet_length(verification::text)<=65536),
 fingerprint text CHECK(fingerprint IS NULL OR fingerprint ~ '^[0-9a-f]{64}$'),
 summary text NOT NULL DEFAULT '' CHECK(octet_length(summary)<=4096),reason text NOT NULL DEFAULT '' CHECK(octet_length(reason)<=4096),
 feedback_id uuid,finish_token uuid,finish_receipt jsonb,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 UNIQUE(owner_id,project_alias,task_id,review_no),
 FOREIGN KEY(owner_id,project_alias,task_id,attempt_id) REFERENCES forge_agent.coding_attempts(owner_id,project_alias,task_id,attempt_id),
 CHECK((status='checking')=(lease_token IS NOT NULL AND lease_expires_at IS NOT NULL))
);
CREATE INDEX coding_reviews_pending ON forge_agent.coding_reviews(owner_id,status,created_at);
CREATE TABLE forge_agent.coding_feedback (
 feedback_id uuid PRIMARY KEY,owner_id text NOT NULL,project_alias text NOT NULL,task_id uuid NOT NULL,
 parent_attempt_id uuid NOT NULL REFERENCES forge_agent.coding_attempts(attempt_id),
 review_id uuid REFERENCES forge_agent.coding_reviews(review_id),
 feedback_json text NOT NULL CHECK(octet_length(feedback_json) BETWEEN 2 AND 65536 AND jsonb_typeof(feedback_json::jsonb)='object'),
 feedback_sha256 text NOT NULL CHECK(feedback_sha256=encode(sha256(convert_to(feedback_json,'UTF8')),'hex')),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 FOREIGN KEY(owner_id,project_alias,task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id)
);
ALTER TABLE forge_agent.coding_tasks ADD CONSTRAINT coding_tasks_feedback_fk FOREIGN KEY(feedback_id) REFERENCES forge_agent.coding_feedback(feedback_id);
ALTER TABLE forge_agent.coding_reviews ADD CONSTRAINT coding_reviews_feedback_fk FOREIGN KEY(feedback_id) REFERENCES forge_agent.coding_feedback(feedback_id);
ALTER TABLE forge_agent.coding_tasks ADD CONSTRAINT coding_tasks_source_review_fk FOREIGN KEY(repair_source_review_id) REFERENCES forge_agent.coding_reviews(review_id);
ALTER TABLE forge_agent.coding_publications
 ADD COLUMN review_id uuid REFERENCES forge_agent.coding_reviews(review_id),
 ADD COLUMN parent_publication_id uuid REFERENCES forge_agent.coding_publications(publication_id),
 ADD COLUMN expected_remote_head text CHECK(expected_remote_head IS NULL OR expected_remote_head ~ '^[0-9a-f]{40}$');
ALTER TABLE forge_agent.coding_publications ALTER COLUMN plan_decision_id DROP NOT NULL;
ALTER TABLE forge_agent.coding_publications DROP CONSTRAINT coding_publications_attempt_unique;
ALTER TABLE forge_agent.coding_publications ADD CONSTRAINT coding_publications_attempt_unique
 UNIQUE NULLS NOT DISTINCT(owner_id,project_alias,task_id,attempt_id,review_id);
CREATE TABLE forge_agent.coding_hosted_checks (
 observation_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),owner_id text NOT NULL,project_alias text NOT NULL,
 task_id uuid NOT NULL,publication_id uuid NOT NULL REFERENCES forge_agent.coding_publications(publication_id),
 candidate_commit text NOT NULL CHECK(candidate_commit ~ '^[0-9a-f]{40}$'),
 checks jsonb NOT NULL CHECK(jsonb_typeof(checks)='array' AND octet_length(checks::text)<=65536),
 required_green boolean NOT NULL,observed_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 FOREIGN KEY(owner_id,project_alias,task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id)
);
REVOKE ALL ON forge_agent.coding_review_policy,forge_agent.coding_reviews,forge_agent.coding_feedback,forge_agent.coding_hosted_checks FROM PUBLIC;

CREATE FUNCTION forge_agent.coding_immutable_record() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN RAISE EXCEPTION 'Immutable coding evidence' USING ERRCODE='P0009'; END;$fn$;
CREATE TRIGGER coding_feedback_immutable BEFORE UPDATE OR DELETE ON forge_agent.coding_feedback
 FOR EACH ROW EXECUTE FUNCTION forge_agent.coding_immutable_record();
CREATE TRIGGER coding_hosted_checks_immutable BEFORE UPDATE OR DELETE ON forge_agent.coding_hosted_checks
 FOR EACH ROW EXECUTE FUNCTION forge_agent.coding_immutable_record();
REVOKE ALL ON FUNCTION forge_agent.coding_immutable_record() FROM PUBLIC;

ALTER TABLE forge_agent.coding_control_receipts DROP CONSTRAINT coding_control_receipts_action_check;
ALTER TABLE forge_agent.coding_control_receipts ADD CONSTRAINT coding_control_receipts_action_check
 CHECK(action IN('cancel','accept','resume','plan_decide','review_decide','repair','publication_retry'));

ALTER TABLE forge_agent.coding_attempts
    DROP CONSTRAINT coding_attempts_pinned_recipe_check,
    ADD CONSTRAINT coding_attempts_pinned_recipe_check CHECK (
            (jsonb_typeof(pinned_recipe) = 'object'
             AND pinned_recipe - ARRAY[
                 'version', 'backend', 'model', 'resource_profile', 'mode',
                 'generation_token_limit', 'policy_digest'
             ] = '{}'::jsonb
             AND pinned_recipe ?& ARRAY[
                 'version', 'backend', 'model', 'resource_profile', 'mode',
                 'generation_token_limit', 'policy_digest'
             ]
             AND pinned_recipe->'version' = '2'::jsonb
             AND pinned_recipe->>'backend' IN ('antigravity', 'codex')
             AND jsonb_typeof(pinned_recipe->'model') = 'string'
             AND octet_length(pinned_recipe->>'model') BETWEEN 1 AND 128
             AND pinned_recipe->>'resource_profile' IN ('forge-coding-v1', 'forge-coding-v2', 'forge-coding-v3', 'forge-coding-v4', 'forge-coding-v5')
             AND pinned_recipe->>'mode' IN ('plan', 'review', 'implement')
             AND jsonb_typeof(pinned_recipe->'generation_token_limit') = 'null'
             AND (pinned_recipe->>'policy_digest') ~ '^[0-9a-f]{64}$') IS TRUE
        );


CREATE OR REPLACE FUNCTION forge_coding_worker_api.claim(
    p_worker_id text,
    p_incarnation uuid,
    p_allowed_profiles text[]
)
RETURNS jsonb
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_candidate record;
    v_task forge_agent.coding_tasks%ROWTYPE;
    v_attempt_id uuid;
    v_attempt_token uuid;
    v_attempt_no integer;
    v_unit_name text;
    v_now timestamptz;
    v_lease_expires timestamptz;
    v_deadline timestamptz;
    v_resume_digest text;
    v_new_revision bigint;
BEGIN
    IF p_worker_id IS NULL OR p_worker_id !~ '^[A-Za-z0-9_.-]{1,64}$'
       OR p_incarnation IS NULL
       OR p_allowed_profiles IS NULL
       OR pg_catalog.array_length(p_allowed_profiles, 1) IS NULL
       OR pg_catalog.array_length(p_allowed_profiles, 1) > 64 THEN
        RAISE EXCEPTION 'Invalid coding worker claim parameters' USING ERRCODE = '22023';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM forge_agent.coding_worker_grants g
        WHERE g.worker_role = session_user
          AND g.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
    ) THEN
        RAISE EXCEPTION 'Coding worker role has no grant for configured owner' USING ERRCODE = '42501';
    END IF;

    FOR v_candidate IN
        SELECT t.owner_id, t.project_alias, t.id
        FROM forge_agent.coding_tasks t
        JOIN forge_agent.coding_worker_readiness r
          ON r.worker_role = session_user
         AND r.owner_id = t.owner_id
         AND r.worker_id = p_worker_id
         AND r.incarnation = p_incarnation
         AND r.observed_at > clock_timestamp() - interval '90 seconds'
        JOIN forge_agent.coding_worker_grants g
          ON g.worker_role = session_user
              AND g.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
         AND g.owner_id = t.owner_id
         AND g.project_alias = t.project_alias
         AND g.agent_profile = t.agent_profile
         AND g.mode = t.mode
        WHERE t.status = 'queued'
          AND t.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
          AND t.agent_profile = ANY(p_allowed_profiles)
          AND (t.pinned_recipe->'version') = '2'::jsonb
          AND (t.pinned_recipe->>'resource_profile') IN ('forge-coding-v1', 'forge-coding-v2', 'forge-coding-v3', 'forge-coding-v4', 'forge-coding-v5')
          AND (t.pinned_recipe->>'policy_digest') = r.policy_digest
          AND NOT EXISTS (
              SELECT 1
              FROM forge_agent.coding_attempts a
              WHERE a.owner_id = t.owner_id
                AND a.project_alias = t.project_alias
                AND a.task_id = t.id
                AND (a.reservation_active OR a.lease_expires_at IS NOT NULL)
          )
        ORDER BY t.created_at ASC, t.id ASC
    LOOP
        SELECT t.*
        INTO v_task
        FROM forge_agent.coding_tasks t
        WHERE t.owner_id = v_candidate.owner_id
          AND t.project_alias = v_candidate.project_alias
          AND t.id = v_candidate.id
          AND t.status = 'queued'
        FOR UPDATE SKIP LOCKED;
        IF NOT FOUND THEN
            CONTINUE;
        END IF;

        IF EXISTS (
            SELECT 1
            FROM forge_agent.coding_attempts a
            WHERE a.owner_id = v_task.owner_id
              AND a.project_alias = v_task.project_alias
              AND a.task_id = v_task.id
              AND (a.reservation_active OR a.lease_expires_at IS NOT NULL)
        ) THEN
            CONTINUE;
        END IF;

        IF v_task.revision >= 9007199254740991 THEN
            CONTINUE;
        END IF;

        v_now := clock_timestamp();
        v_lease_expires := v_now + interval '60 seconds';
        v_deadline := v_now + interval '1800 seconds';
        v_attempt_id := pg_catalog.gen_random_uuid();
        v_attempt_token := pg_catalog.gen_random_uuid();
        v_unit_name := 'forge-coding-' || pg_catalog.replace(v_attempt_id::text, '-', '');
        v_resume_digest := v_task.resume_workspace_digest;

        SELECT coalesce(max(a.attempt_no), 0) + 1
        INTO v_attempt_no
        FROM forge_agent.coding_attempts a
        WHERE a.owner_id = v_task.owner_id
          AND a.project_alias = v_task.project_alias
          AND a.task_id = v_task.id;

        INSERT INTO forge_agent.coding_attempts (
            owner_id, project_alias, task_id, attempt_id, attempt_no,
            attempt_token, agent_profile, pinned_recipe, recipe_digest,
            worker_role, worker_id, worker_incarnation,
            unit_name, phase, reservation_active, resume_workspace_digest, plan_decision_id,
            lease_expires_at, deadline_at, started_at, updated_at,feedback_id,feedback_sha256,legacy_resume_note,repair_source_review_id,repair_candidate_commit,scope_plan_decision_id
        )
        VALUES (
            v_task.owner_id, v_task.project_alias, v_task.id, v_attempt_id, v_attempt_no,
            v_attempt_token, v_task.agent_profile, v_task.pinned_recipe, v_task.recipe_digest,
            session_user, p_worker_id, p_incarnation,
            v_unit_name, 'starting', true, v_resume_digest, v_task.plan_decision_id,
            v_lease_expires, v_deadline, v_now, v_now,v_task.feedback_id,v_task.feedback_sha256,v_task.legacy_resume_note,v_task.repair_source_review_id,v_task.repair_candidate_commit,v_task.scope_plan_decision_id
        );

        UPDATE forge_agent.coding_tasks
        SET status = 'running',
            revision = revision + 1,
            latest_attempt_id = v_attempt_id,
            resume_workspace_digest = NULL,
            updated_at = v_now
        WHERE owner_id = v_task.owner_id
          AND project_alias = v_task.project_alias
          AND id = v_task.id
        RETURNING revision INTO v_new_revision;

        INSERT INTO forge_agent.coding_events (
            owner_id, project_alias, task_id, sequence, event_kind, payload
        )
        SELECT v_task.owner_id, v_task.project_alias, v_task.id,
               coalesce(max(e.sequence), 0) + 1,
               'started',
               pg_catalog.jsonb_build_object(
                   'attempt_id', v_attempt_id,
                   'attempt_no', v_attempt_no,
                   'worker_id', p_worker_id,
                   'revision', v_new_revision
               )
        FROM forge_agent.coding_events e
        WHERE e.owner_id = v_task.owner_id
          AND e.project_alias = v_task.project_alias
          AND e.task_id = v_task.id;

        RETURN pg_catalog.jsonb_build_object(
            'task_id', v_task.id,
            'project_alias', v_task.project_alias,
            'base_revision', v_task.base_revision,
            'mode', v_task.mode,
            'agent_profile', v_task.agent_profile,
            'prompt', v_task.prompt,
            'acceptance_criteria', v_task.acceptance_criteria,
            'recipe', v_task.pinned_recipe,
            'attempt_id', v_attempt_id,
            'attempt_token', v_attempt_token,
            'worker_id', p_worker_id,
            'incarnation', p_incarnation,
            'lease_expires_at', pg_catalog.to_char(v_lease_expires AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
            'deadline_at', pg_catalog.to_char(v_deadline AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
            'unit_name', v_unit_name,
            'plan_decision_id', v_task.plan_decision_id,
            'resume_workspace_digest', v_resume_digest,
            'resume_attempt_id', CASE WHEN v_resume_digest IS NOT NULL THEN coalesce(v_task.latest_attempt_id,(SELECT f.parent_attempt_id FROM forge_agent.coding_feedback f WHERE f.feedback_id=v_task.feedback_id)) ELSE NULL END,
            'feedback_id',v_task.feedback_id,'feedback_sha256',v_task.feedback_sha256,
            'repair_source_review_id',v_task.repair_source_review_id,'repair_candidate_commit',v_task.repair_candidate_commit,'scope_plan_decision_id',v_task.scope_plan_decision_id
        );
    END LOOP;

    RETURN NULL;
END;
$fn$;

-- CREATE OR REPLACE preserves the existing scoped EXECUTE grants.
REVOKE ALL ON FUNCTION forge_coding_worker_api.claim(text, uuid, text[]) FROM PUBLIC;


ALTER TABLE forge_agent.coding_tasks ADD COLUMN auto_repair boolean NOT NULL DEFAULT false;

ALTER TABLE forge_agent.coding_reviews ADD COLUMN phase text NOT NULL DEFAULT 'local' CHECK(phase IN('local','hosted'));
ALTER TABLE forge_agent.coding_reviews ADD COLUMN next_poll_at timestamptz NOT NULL DEFAULT clock_timestamp();
ALTER TABLE forge_agent.coding_reviews ADD COLUMN target_publication_id uuid REFERENCES forge_agent.coding_publications(publication_id);
ALTER TABLE forge_agent.coding_reviews ADD COLUMN source_publication_id uuid REFERENCES forge_agent.coding_publications(publication_id);
ALTER TABLE forge_agent.coding_tasks ADD COLUMN repair_target_publication_id uuid REFERENCES forge_agent.coding_publications(publication_id);
ALTER TABLE forge_agent.coding_tasks ADD COLUMN repair_expected_remote_head text CHECK(repair_expected_remote_head IS NULL OR repair_expected_remote_head ~ '^[0-9a-f]{40}$');

ALTER TABLE forge_agent.coding_reviews DROP CONSTRAINT coding_reviews_phase_check;
ALTER TABLE forge_agent.coding_reviews ADD CONSTRAINT coding_reviews_phase_check CHECK(phase IN('local','hosted','source'));
ALTER TABLE forge_agent.coding_tasks ADD COLUMN scope_plan_decision_id uuid REFERENCES forge_agent.coding_plan_decisions(decision_id);
ALTER TABLE forge_agent.coding_attempts ADD COLUMN scope_plan_decision_id uuid REFERENCES forge_agent.coding_plan_decisions(decision_id);
ALTER TABLE forge_agent.coding_reviews ADD COLUMN scope_plan_decision_id uuid REFERENCES forge_agent.coding_plan_decisions(decision_id);

CREATE FUNCTION forge_coding_worker_api.read_feedback(p_attempt_id uuid,p_attempt_token uuid)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.coding_attempts%ROWTYPE;f forge_agent.coding_feedback%ROWTYPE;
BEGIN
 a:=forge_coding_worker_api._lock_task_and_attempt(p_attempt_id,p_attempt_token);
 IF a.attempt_id IS NULL THEN RAISE EXCEPTION 'Invalid attempt ownership' USING ERRCODE='42501'; END IF;
 IF a.lease_expires_at IS NULL OR a.lease_expires_at<=clock_timestamp() OR a.deadline_at<=clock_timestamp()
 OR NOT EXISTS(SELECT 1 FROM forge_agent.coding_tasks t WHERE t.owner_id=a.owner_id AND t.project_alias=a.project_alias
 AND t.id=a.task_id AND t.latest_attempt_id=a.attempt_id AND t.status='running') THEN
 RAISE EXCEPTION 'Attempt lease is not active' USING ERRCODE='P0008'; END IF;
 IF a.feedback_id IS NULL THEN
  IF a.legacy_resume_note='' THEN RETURN NULL; END IF;
  RETURN jsonb_build_object('feedback',NULL,'feedback_json',NULL,'legacy_resume_note',a.legacy_resume_note);
 END IF;
 SELECT * INTO f FROM forge_agent.coding_feedback x WHERE x.feedback_id=a.feedback_id
 AND x.owner_id=a.owner_id AND x.project_alias=a.project_alias AND x.task_id=a.task_id;
 IF NOT FOUND OR f.feedback_sha256 IS DISTINCT FROM a.feedback_sha256
 OR encode(sha256(convert_to(f.feedback_json,'UTF8')),'hex')<>a.feedback_sha256
 OR f.feedback_json::jsonb->>'workspace_digest' IS DISTINCT FROM a.resume_workspace_digest THEN
 RAISE EXCEPTION 'Feedback artifact no longer matches' USING ERRCODE='P0006'; END IF;
 RETURN jsonb_build_object('feedback',f.feedback_json::jsonb,'feedback_json',f.feedback_json,'legacy_resume_note',a.legacy_resume_note);
END;$fn$;

CREATE FUNCTION forge_coding_worker_api.queue_review(p_attempt_id uuid,p_attempt_token uuid,p_receipt_id uuid)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.coding_attempts%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;r forge_agent.coding_reconciliation_receipts%ROWTYPE;
 previous forge_agent.coding_reviews%ROWTYPE;id uuid;
BEGIN
 a:=forge_coding_worker_api._lock_task_and_attempt(p_attempt_id,p_attempt_token);
 IF a.attempt_id IS NULL THEN RAISE EXCEPTION 'Invalid attempt ownership' USING ERRCODE='42501';END IF;
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE owner_id=a.owner_id AND project_alias=a.project_alias AND coding_tasks.id=a.task_id;
 IF t.mode<>'implement' OR NOT t.review_required THEN RETURN NULL; END IF;
 SELECT * INTO r FROM forge_agent.coding_reconciliation_receipts WHERE receipt_id=p_receipt_id
 AND owner_id=a.owner_id AND project_alias=a.project_alias AND task_id=a.task_id AND attempt_id=a.attempt_id;
 IF NOT FOUND OR t.latest_attempt_id<>a.attempt_id OR t.status NOT IN('awaiting_review','completed')
 OR a.phase<>'stopped' OR a.reservation_active OR a.lease_expires_at IS NOT NULL
 OR a.result_sha256 IS NULL OR a.published_at IS NULL THEN
 RAISE EXCEPTION 'Review requires reconciled implementation' USING ERRCODE='P0009';END IF;
 SELECT review_id INTO id FROM forge_agent.coding_reviews WHERE owner_id=a.owner_id AND project_alias=a.project_alias
 AND task_id=a.task_id AND attempt_id=a.attempt_id ORDER BY review_no DESC LIMIT 1;
 IF id IS NOT NULL THEN RETURN id; END IF;
 SELECT x.* INTO previous FROM forge_agent.coding_reviews x JOIN forge_agent.coding_tasks z ON(z.owner_id,z.project_alias,z.id)=(x.owner_id,x.project_alias,x.task_id)
 WHERE x.owner_id=a.owner_id AND x.project_alias=a.project_alias AND coalesce(z.repair_root_task_id,z.id)=coalesce(t.repair_root_task_id,t.id)
 ORDER BY x.created_at DESC LIMIT 1;
 INSERT INTO forge_agent.coding_review_policy(owner_id,project_alias) VALUES(a.owner_id,a.project_alias) ON CONFLICT DO NOTHING;
 INSERT INTO forge_agent.coding_review_budget(owner_id,project_alias,root_task_id) VALUES(a.owner_id,a.project_alias,coalesce(t.repair_root_task_id,t.id)) ON CONFLICT DO NOTHING;
 INSERT INTO forge_agent.coding_reviews(owner_id,project_alias,task_id,attempt_id,plan_decision_id,reconciliation_receipt_id,
 base_revision,workspace_digest,parent_result_sha256,source_revision,review_no,round,stagnant_rounds,infrastructure_retries,previous_review_id)
 VALUES(a.owner_id,a.project_alias,a.task_id,a.attempt_id,a.plan_decision_id,r.receipt_id,t.base_revision,r.workspace_digest,
 a.result_sha256,t.revision,coalesce(previous.review_no,0)+1,
 coalesce((SELECT repair_rounds FROM forge_agent.coding_review_budget WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND root_task_id=coalesce(t.repair_root_task_id,t.id)),0),
 coalesce(previous.stagnant_rounds,0),0,previous.review_id) RETURNING review_id INTO id;
 PERFORM pg_notify('forge_coding_jobs','');RETURN id;
END;$fn$;

CREATE FUNCTION forge_coding_reconciler_api.review_claim(p_worker_id text)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE j forge_agent.coding_reviews%ROWTYPE;p forge_agent.coding_review_policy%ROWTYPE;frozen_policy jsonb;
BEGIN
 IF p_worker_id IS NULL OR p_worker_id!~'^[A-Za-z0-9_.-]{1,64}$' THEN RAISE EXCEPTION 'Invalid review worker' USING ERRCODE='22023';END IF;
 -- Reconciliation callback recovery queues only review, never publication.
 INSERT INTO forge_agent.coding_reviews(owner_id,project_alias,task_id,attempt_id,plan_decision_id,reconciliation_receipt_id,
 base_revision,workspace_digest,parent_result_sha256,source_revision,review_no,round)
 SELECT a.owner_id,a.project_alias,a.task_id,a.attempt_id,a.plan_decision_id,r.receipt_id,t.base_revision,r.workspace_digest,
 a.result_sha256,t.revision,coalesce((SELECT max(q.review_no) FROM forge_agent.coding_reviews q WHERE q.owner_id=a.owner_id AND q.project_alias=a.project_alias AND q.task_id=a.task_id),0)+1,
 coalesce((SELECT b.repair_rounds FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),0)
 FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)=(a.owner_id,a.project_alias,a.task_id,a.attempt_id)
 JOIN LATERAL(SELECT x.* FROM forge_agent.coding_reconciliation_receipts x WHERE x.attempt_id=a.attempt_id ORDER BY x.created_at DESC LIMIT 1)r ON true
 WHERE a.owner_id=current_setting('forge.coding_owner',true) AND t.review_required AND t.mode='implement'
 AND t.status='awaiting_review' AND a.phase='stopped' AND NOT a.reservation_active AND a.lease_expires_at IS NULL
 AND a.published_at IS NOT NULL AND a.result_sha256 IS NOT NULL
 AND NOT EXISTS(SELECT 1 FROM forge_agent.coding_reviews q WHERE q.owner_id=a.owner_id AND q.project_alias=a.project_alias AND q.task_id=a.task_id AND q.attempt_id=a.attempt_id)
 ON CONFLICT DO NOTHING;
 UPDATE forge_agent.coding_reviews q SET status='cancelled',lease_token=NULL,lease_expires_at=NULL,reason='Source superseded or cancelled',updated_at=clock_timestamp()
 WHERE q.owner_id=current_setting('forge.coding_owner',true) AND q.status IN('pending','checking')
 AND NOT EXISTS(SELECT 1 FROM forge_agent.coding_tasks t WHERE(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)=(q.owner_id,q.project_alias,q.task_id,q.attempt_id)
 AND t.status IN('awaiting_review','completed') AND t.plan_decision_id IS NOT DISTINCT FROM q.plan_decision_id AND t.scope_plan_decision_id IS NOT DISTINCT FROM q.scope_plan_decision_id);
 SELECT q.* INTO j FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t
 ON(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)=(q.owner_id,q.project_alias,q.task_id,q.attempt_id)
 WHERE q.owner_id=current_setting('forge.coding_owner',true) AND (q.status='pending' OR(q.status='checking' AND q.lease_expires_at<=clock_timestamp()))
 AND t.status IN('awaiting_review','completed') AND t.plan_decision_id IS NOT DISTINCT FROM q.plan_decision_id AND t.scope_plan_decision_id IS NOT DISTINCT FROM q.scope_plan_decision_id AND q.next_poll_at<=clock_timestamp() ORDER BY q.created_at,q.review_id FOR UPDATE OF q SKIP LOCKED LIMIT 1;
 IF NOT FOUND THEN RETURN NULL;END IF;
 UPDATE forge_agent.coding_reviews SET status='checking',worker_id=p_worker_id,lease_token=gen_random_uuid(),
 lease_expires_at=clock_timestamp()+interval '60 seconds',updated_at=clock_timestamp() WHERE review_id=j.review_id RETURNING * INTO j;
 INSERT INTO forge_agent.coding_review_policy(owner_id,project_alias) VALUES(j.owner_id,j.project_alias) ON CONFLICT DO NOTHING;
 SELECT * INTO p FROM forge_agent.coding_review_policy WHERE owner_id=j.owner_id AND project_alias=j.project_alias;
 SELECT b.policy INTO frozen_policy FROM forge_agent.coding_review_budget b JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,coalesce(t.repair_root_task_id,t.id))=(b.owner_id,b.project_alias,b.root_task_id) WHERE t.owner_id=j.owner_id AND t.project_alias=j.project_alias AND t.id=j.task_id;
 RETURN jsonb_build_object('review_id',j.review_id,'hosted_deadline_at',to_char((j.created_at+interval '24 hours') AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),'phase',j.phase,'target_publication_id',j.target_publication_id,
 'source_publication_id',j.source_publication_id,'publication_id',j.source_publication_id,
 'pr_url',(SELECT pr_url FROM forge_agent.coding_publications WHERE publication_id=j.source_publication_id),
 'task_id',j.task_id,'attempt_id',j.attempt_id,'project_alias',j.project_alias,
 'base_revision',j.base_revision,'workspace_digest',j.workspace_digest,'plan_decision_id',j.plan_decision_id,'scope_plan_decision_id',j.scope_plan_decision_id,
 'owner_source_authorized',j.phase='source' AND j.owner_decision->>'decision'='repair_source','summary',j.summary,'findings',j.findings,
 'reconciliation_receipt_id',j.reconciliation_receipt_id,'parent_result_sha256',j.parent_result_sha256,'source_revision',j.source_revision,
 'lease_token',j.lease_token,'status',j.status,'round',j.round,'stagnant_rounds',j.stagnant_rounds,'infrastructure_retries',j.infrastructure_retries,
 'candidate_commit',j.candidate_commit,'gate_manifest_sha256',j.gate_manifest_sha256,'previous_review_id',j.previous_review_id,
 'source_review_id',CASE WHEN j.phase='hosted' THEN(SELECT review_id FROM forge_agent.coding_publications WHERE publication_id=j.source_publication_id) WHEN j.phase='local' AND j.infrastructure_retries>0 THEN j.previous_review_id ELSE NULL END,
 'direct_implementation',(SELECT mode='implement' AND review_required AND pinned_recipe->>'resource_profile'='forge-coding-v5' AND plan_decision_id IS NULL AND parent_task_id IS NULL FROM forge_agent.coding_tasks WHERE owner_id=j.owner_id AND project_alias=j.project_alias AND id=j.task_id),
 'parent_task_id',(SELECT parent_task_id FROM forge_agent.coding_tasks WHERE owner_id=j.owner_id AND project_alias=j.project_alias AND id=j.task_id),
 'feedback_id',(SELECT feedback_id FROM forge_agent.coding_tasks WHERE owner_id=j.owner_id AND project_alias=j.project_alias AND id=j.task_id),
 'auto_repair',(SELECT auto_repair FROM forge_agent.coding_tasks WHERE owner_id=j.owner_id AND project_alias=j.project_alias AND coding_tasks.id=j.task_id),
 'open_findings',coalesce((SELECT findings FROM forge_agent.coding_reviews WHERE review_id=j.previous_review_id),'[]'::jsonb),
 'previous_fingerprint',(SELECT fingerprint FROM forge_agent.coding_reviews WHERE review_id=j.previous_review_id),
 'policy',coalesce(frozen_policy,jsonb_build_object('version',p.version,'max_rounds',p.max_rounds,'max_stagnant_rounds',p.max_stagnant_rounds,'max_infrastructure_retries',p.max_infrastructure_retries)));
END;$fn$;

CREATE FUNCTION forge_coding_reconciler_api.review_renew(p_review_id uuid,p_token uuid)
RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 UPDATE forge_agent.coding_reviews q SET lease_expires_at=clock_timestamp()+interval '60 seconds',updated_at=clock_timestamp()
 FROM forge_agent.coding_tasks t WHERE q.review_id=p_review_id AND q.lease_token=p_token AND q.status='checking'
 AND q.owner_id=current_setting('forge.coding_owner',true) AND q.lease_expires_at>clock_timestamp()
 AND(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)=(q.owner_id,q.project_alias,q.task_id,q.attempt_id)
 AND t.status IN('awaiting_review','completed') AND t.plan_decision_id IS NOT DISTINCT FROM q.plan_decision_id AND t.scope_plan_decision_id IS NOT DISTINCT FROM q.scope_plan_decision_id;RETURN FOUND;
END;$fn$;
ALTER TABLE forge_agent.coding_tasks ADD COLUMN repair_root_task_id uuid;
ALTER TABLE forge_agent.coding_reviews ADD COLUMN owner_decision jsonb;
ALTER TABLE forge_agent.coding_reviews ADD COLUMN repair_task_id uuid;
CREATE TABLE forge_agent.coding_review_budget (
 owner_id text NOT NULL,project_alias text NOT NULL,root_task_id uuid NOT NULL,
 repair_rounds integer NOT NULL DEFAULT 0 CHECK(repair_rounds BETWEEN 0 AND 10),
 stagnant_rounds integer NOT NULL DEFAULT 0 CHECK(stagnant_rounds BETWEEN 0 AND 10),
 policy jsonb NOT NULL DEFAULT '{"version":"forge-repair-v1","max_rounds":3,"max_stagnant_rounds":2,"max_infrastructure_retries":1,"required_hosted_checks":["check","pr-fast-gate","real-browser"]}' CHECK(jsonb_typeof(policy)='object'),
 PRIMARY KEY(owner_id,project_alias,root_task_id),
 FOREIGN KEY(owner_id,project_alias,root_task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id)
);
REVOKE ALL ON forge_agent.coding_review_budget FROM PUBLIC;

CREATE FUNCTION forge_agent.coding_make_feedback(p_review uuid,p_task uuid,p_disposition text,p_summary text,p_findings jsonb)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE q forge_agent.coding_reviews%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;id uuid:=gen_random_uuid();payload jsonb;raw text;digest text;input_digest text;
BEGIN
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p_review;
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE owner_id=q.owner_id AND project_alias=q.project_alias AND coding_tasks.id=p_task;
 IF q.owner_id IS DISTINCT FROM current_setting('forge.coding_owner',true) OR t.id IS NULL THEN RAISE EXCEPTION 'Feedback scope mismatch' USING ERRCODE='42501';END IF;
 input_digest:=CASE WHEN t.repair_source_review_id=q.review_id THEN q.verification->>'candidate_workspace_digest' ELSE q.workspace_digest END;
 IF input_digest IS NULL OR input_digest!~'^[0-9a-f]{64}$' THEN RAISE EXCEPTION 'Candidate input digest missing' USING ERRCODE='P0009';END IF;
 payload:=jsonb_build_object('version',1,'feedback_id',id,'task_id',t.id,'parent_attempt_id',q.attempt_id,
 'plan_decision_id',coalesce(q.plan_decision_id,q.scope_plan_decision_id),'source_revision',q.source_revision,'parent_result_sha256',q.parent_result_sha256,
 'reconciliation_receipt_id',q.reconciliation_receipt_id,'workspace_digest',input_digest,
 'candidate_commit',q.candidate_commit,'integration_base',q.base_revision,'gate_manifest_sha256',q.gate_manifest_sha256,
 'round',coalesce((SELECT b.repair_rounds FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),q.round)+1,'disposition',p_disposition,'agent_profile',t.agent_profile,'summary',left(p_summary,2048),'findings',(SELECT coalesce(jsonb_agg(x),'[]') FROM(SELECT x FROM jsonb_array_elements(p_findings) WITH ORDINALITY AS items(x,n) WHERE x->>'finding_id' NOT IN(SELECT jsonb_array_elements_text(q.verified_finding_ids)) ORDER BY n LIMIT 3)batch));
 raw:=payload::text;digest:=encode(sha256(convert_to(raw,'UTF8')),'hex');
 INSERT INTO forge_agent.coding_feedback(feedback_id,owner_id,project_alias,task_id,parent_attempt_id,review_id,feedback_json,feedback_sha256)
 VALUES(id,t.owner_id,t.project_alias,t.id,q.attempt_id,q.review_id,raw,digest);
 RETURN id;
END;$fn$;

CREATE FUNCTION forge_agent.coding_schedule_repair(p_review uuid,p_feedback uuid)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE q forge_agent.coding_reviews%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;p forge_agent.coding_review_policy%ROWTYPE;
 b forge_agent.coding_review_budget%ROWTYPE;child uuid;fid uuid;f forge_agent.coding_feedback%ROWTYPE;new_revision bigint;root_id uuid;
BEGIN
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p_review FOR UPDATE;
 IF q.owner_id IS DISTINCT FROM current_setting('forge.coding_owner',true) THEN RAISE EXCEPTION 'Repair scope mismatch' USING ERRCODE='42501';END IF;
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE owner_id=q.owner_id AND project_alias=q.project_alias AND coding_tasks.id=q.task_id FOR UPDATE;
 IF t.latest_attempt_id<>q.attempt_id OR t.plan_decision_id IS DISTINCT FROM q.plan_decision_id OR t.scope_plan_decision_id IS DISTINCT FROM q.scope_plan_decision_id OR t.status NOT IN('awaiting_review','completed') OR q.candidate_commit IS NULL
 OR EXISTS(SELECT 1 FROM forge_agent.coding_attempts a WHERE a.owner_id=t.owner_id AND a.project_alias=t.project_alias AND a.task_id=t.id AND(a.reservation_active OR a.lease_expires_at IS NOT NULL)) THEN
 RAISE EXCEPTION 'Repair requires stopped current source' USING ERRCODE='P0009';END IF;
 SELECT * INTO p FROM forge_agent.coding_review_policy WHERE owner_id=t.owner_id AND project_alias=t.project_alias;
 root_id:=coalesce(t.repair_root_task_id,t.id);
 INSERT INTO forge_agent.coding_review_budget(owner_id,project_alias,root_task_id) VALUES(t.owner_id,t.project_alias,root_id) ON CONFLICT DO NOTHING;
 SELECT * INTO b FROM forge_agent.coding_review_budget WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND root_task_id=root_id FOR UPDATE;
 p.max_rounds:=(b.policy->>'max_rounds')::integer;p.max_stagnant_rounds:=(b.policy->>'max_stagnant_rounds')::integer;p.max_infrastructure_retries:=(b.policy->>'max_infrastructure_retries')::integer;
 IF EXISTS(SELECT 1 FROM forge_agent.coding_tasks x WHERE x.owner_id=t.owner_id AND x.project_alias=t.project_alias AND coalesce(x.repair_root_task_id,x.id)=root_id AND x.status IN('queued','running','cancel_requested')) THEN
 RAISE EXCEPTION 'Repair scope already has a writer' USING ERRCODE='P0009';END IF;
 IF b.repair_rounds>=p.max_rounds OR greatest(b.stagnant_rounds,q.stagnant_rounds)>=p.max_stagnant_rounds THEN
 RAISE EXCEPTION 'Repair budget exhausted' USING ERRCODE='P0009';END IF;
 SELECT * INTO f FROM forge_agent.coding_feedback WHERE feedback_id=p_feedback AND owner_id=t.owner_id AND project_alias=t.project_alias AND task_id=t.id;
 IF NOT FOUND THEN RAISE EXCEPTION 'Bound repair feedback missing' USING ERRCODE='P0006';END IF;
 IF t.status='completed' OR coalesce((q.verification->>'repair_requires_new_workspace')::boolean,false) THEN
  IF q.verification->>'candidate_workspace_digest' IS NULL THEN RAISE EXCEPTION 'Exact candidate input required' USING ERRCODE='P0009';END IF;
  child:=gen_random_uuid();
  INSERT INTO forge_agent.coding_tasks(owner_id,project_alias,id,idempotency_key,intent_digest,base_revision,mode,agent_profile,title,prompt,acceptance_criteria,
   pinned_recipe,recipe_digest,status,revision,resume_workspace_digest,review_required,parent_task_id,repair_root_task_id,repair_source_review_id,repair_candidate_commit,auto_repair,repair_target_publication_id,scope_plan_decision_id,repair_expected_remote_head)
  VALUES(t.owner_id,t.project_alias,child,'repair-'||p_feedback::text,f.feedback_sha256,q.candidate_commit,'implement',coalesce(q.owner_decision->>'agent_profile',t.agent_profile),left(t.title,128),t.prompt,t.acceptance_criteria,
   coalesce(q.owner_decision->'recipe',t.pinned_recipe),coalesce(q.owner_decision->>'recipe_digest',t.recipe_digest),'queued',1,q.verification->>'candidate_workspace_digest',true,t.id,root_id,q.review_id,q.candidate_commit,t.auto_repair,coalesce(q.source_publication_id,q.target_publication_id,(SELECT publication_id FROM forge_agent.coding_publications WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND task_id=t.id AND status='published' ORDER BY created_at DESC LIMIT 1)),coalesce(t.plan_decision_id,t.scope_plan_decision_id),CASE WHEN q.target_publication_id IS NOT NULL THEN q.candidate_commit ELSE NULL END);
  fid:=forge_agent.coding_make_feedback(q.review_id,child,'repair',f.feedback_json::jsonb->>'summary',f.feedback_json::jsonb->'findings');
  UPDATE forge_agent.coding_tasks x SET feedback_id=fid,feedback_sha256=y.feedback_sha256 FROM forge_agent.coding_feedback y WHERE x.id=child AND y.feedback_id=fid;
  INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
  VALUES(t.owner_id,t.project_alias,child,1,'queued',jsonb_build_object('task_id',child,'project_alias',t.project_alias,'revision',1,'status','queued','recipe_digest',t.recipe_digest,'parent_task_id',t.id));
  UPDATE forge_agent.coding_reviews SET repair_task_id=child WHERE review_id=q.review_id;
 ELSE
  child:=t.id;
  UPDATE forge_agent.coding_tasks SET status='queued',revision=revision+1,feedback_id=f.feedback_id,feedback_sha256=f.feedback_sha256,
   resume_workspace_digest=q.workspace_digest,legacy_resume_note='',updated_at=clock_timestamp() WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND coding_tasks.id=t.id;
  INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
  SELECT t.owner_id,t.project_alias,t.id,coalesce(max(sequence),0)+1,'resumed',jsonb_build_object('feedback_id',f.feedback_id,'review_id',q.review_id,'round',b.repair_rounds+1)
  FROM forge_agent.coding_events WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND task_id=t.id;
 END IF;
 UPDATE forge_agent.coding_review_budget SET repair_rounds=repair_rounds+1,stagnant_rounds=q.stagnant_rounds WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND root_task_id=root_id;
 PERFORM pg_notify('forge_coding_jobs','');RETURN child;
END;$fn$;

CREATE FUNCTION forge_agent.coding_queue_approved_review(p_review uuid)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE q forge_agent.coding_reviews%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;id uuid;parent forge_agent.coding_publications%ROWTYPE;
BEGIN
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p_review;
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE owner_id=q.owner_id AND project_alias=q.project_alias AND coding_tasks.id=q.task_id;
 IF q.owner_id IS DISTINCT FROM current_setting('forge.coding_owner',true) OR q.phase<>'local' OR q.status<>'approved' OR q.owner_decision IS NULL
 OR q.owner_decision->>'candidate_commit' IS DISTINCT FROM q.candidate_commit
 OR q.verification->'checks_passed' IS DISTINCT FROM 'true'::jsonb OR q.verification->'cleanup_confirmed' IS DISTINCT FROM 'true'::jsonb OR t.latest_attempt_id<>q.attempt_id
 OR t.plan_decision_id IS DISTINCT FROM q.plan_decision_id OR t.scope_plan_decision_id IS DISTINCT FROM q.scope_plan_decision_id
 OR t.status NOT IN('awaiting_review','completed') THEN RETURN NULL;END IF;
 SELECT * INTO parent FROM forge_agent.coding_publications WHERE owner_id=t.owner_id AND project_alias=t.project_alias
 AND (publication_id=t.repair_target_publication_id OR(t.repair_target_publication_id IS NULL AND task_id=t.id)) ORDER BY created_at DESC LIMIT 1;
 INSERT INTO forge_agent.coding_publications(owner_id,project_alias,task_id,attempt_id,plan_decision_id,reconciliation_receipt_id,
 base_revision,workspace_digest,review_id,parent_publication_id,expected_remote_head)
 VALUES(t.owner_id,t.project_alias,t.id,q.attempt_id,q.plan_decision_id,q.reconciliation_receipt_id,q.base_revision,q.workspace_digest,
 q.review_id,coalesce(parent.parent_publication_id,parent.publication_id),coalesce(t.repair_expected_remote_head,parent.commit_sha)) ON CONFLICT DO NOTHING;
 SELECT publication_id INTO id FROM forge_agent.coding_publications WHERE review_id=q.review_id;
 PERFORM pg_notify('forge_coding_jobs','');RETURN id;
END;$fn$;

CREATE FUNCTION forge_coding_reconciler_api.review_finish(p_review uuid,p_token uuid,p_outcome jsonb)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE q forge_agent.coding_reviews%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;p forge_agent.coding_review_policy%ROWTYPE;
 previous forge_agent.coding_reviews%ROWTYPE;disposition text;receipt jsonb;id uuid;next_id uuid;stagnant integer;v_candidate text;v_gate text;
BEGIN
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p_review AND owner_id=current_setting('forge.coding_owner',true) FOR UPDATE;
 IF NOT FOUND THEN RAISE EXCEPTION 'Review not found' USING ERRCODE='P0002';END IF;
 IF q.finish_token=p_token AND q.finish_receipt->'outcome'=p_outcome THEN RETURN q.finish_receipt;END IF;
 IF q.status<>'checking' OR q.lease_token IS DISTINCT FROM p_token OR q.lease_expires_at<=clock_timestamp() THEN RAISE EXCEPTION 'Review lease expired' USING ERRCODE='P0008';END IF;
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE owner_id=q.owner_id AND project_alias=q.project_alias AND coding_tasks.id=q.task_id FOR UPDATE;
 IF t.latest_attempt_id<>q.attempt_id OR t.plan_decision_id IS DISTINCT FROM q.plan_decision_id OR t.scope_plan_decision_id IS DISTINCT FROM q.scope_plan_decision_id OR t.status NOT IN('awaiting_review','completed') THEN RAISE EXCEPTION 'Review source superseded' USING ERRCODE='P0008';END IF;
 IF jsonb_typeof(p_outcome)<>'object' OR octet_length(p_outcome::text)>131072 THEN RAISE EXCEPTION 'Invalid review outcome' USING ERRCODE='22023';END IF;
 disposition:=p_outcome->>'disposition';v_candidate:=p_outcome->>'candidate_commit';v_gate:=p_outcome->>'gate_manifest_sha256';
 IF disposition NOT IN('repair','retry_infrastructure','awaiting_review','handoff','cancelled','needs_reconcile','revise_plan')
 OR (v_candidate IS NOT NULL AND v_candidate!~'^[0-9a-f]{40}$') OR(v_gate IS NOT NULL AND v_gate!~'^[0-9a-f]{64}$')
 OR jsonb_typeof(coalesce(p_outcome->'findings','[]'))<>'array' OR jsonb_typeof(coalesce(p_outcome->'verification','{}'))<>'object' THEN
 RAISE EXCEPTION 'Invalid review outcome' USING ERRCODE='22023';END IF;
 IF disposition IN('repair','retry_infrastructure','awaiting_review') AND coalesce(p_outcome->'cleanup_confirmed',p_outcome->'verification'->'cleanup_confirmed','false'::jsonb) IS DISTINCT FROM 'true'::jsonb THEN disposition:='needs_reconcile';END IF;
 IF q.phase='source' THEN
  IF q.owner_decision->>'decision' IS DISTINCT FROM 'repair_source' OR disposition NOT IN('repair','handoff','needs_reconcile','cancelled','revise_plan','retry_infrastructure')
  OR(disposition='repair' AND(v_candidate IS DISTINCT FROM q.candidate_commit OR p_outcome->'verification'->>'candidate_workspace_digest' IS NULL)) THEN
   RAISE EXCEPTION 'Source import lacks bound owner authorization' USING ERRCODE='P0009';END IF;
  v_candidate:=coalesce(v_candidate,q.candidate_commit);
 END IF;
 IF q.phase='hosted' THEN
  IF q.created_at+interval '24 hours'<=clock_timestamp() THEN
   UPDATE forge_agent.coding_reviews SET status='handoff',reason='hosted_wait_exhausted',lease_token=NULL,lease_expires_at=NULL,finish_token=p_token,finish_receipt=jsonb_build_object('review_id',q.review_id,'status','handoff','reason','hosted_wait_exhausted','outcome',p_outcome),updated_at=clock_timestamp() WHERE review_id=q.review_id;
   SELECT finish_receipt INTO receipt FROM forge_agent.coding_reviews WHERE review_id=q.review_id;RETURN receipt;
  END IF;
  IF p_outcome->'verification'->'hosted' IS DISTINCT FROM 'true'::jsonb OR v_candidate IS DISTINCT FROM q.candidate_commit THEN
   RAISE EXCEPTION 'Hosted source mismatch' USING ERRCODE='P0006';END IF;
  receipt:=forge_coding_reconciler_api.record_hosted_checks(q.source_publication_id,q.candidate_commit,coalesce(p_outcome->'verification'->'checks','[]'));
  IF coalesce(p_outcome->'verification'->>'hosted_status',p_outcome->>'hosted_status')='pending' THEN
   UPDATE forge_agent.coding_reviews SET status='pending',lease_token=NULL,lease_expires_at=NULL,next_poll_at=clock_timestamp()+interval '60 seconds',
    verification=verification||coalesce(p_outcome->'verification','{}'),finish_token=p_token,
    finish_receipt=jsonb_build_object('review_id',q.review_id,'status','waiting_hosted','outcome',p_outcome),updated_at=clock_timestamp() WHERE review_id=q.review_id;
   SELECT finish_receipt INTO receipt FROM forge_agent.coding_reviews WHERE review_id=q.review_id;RETURN receipt;
  ELSIF coalesce(p_outcome->'verification'->>'hosted_status',p_outcome->>'hosted_status')='passed' THEN
   IF receipt->'required_green' IS DISTINCT FROM 'true'::jsonb THEN RAISE EXCEPTION 'Required hosted checks incomplete' USING ERRCODE='P0009';END IF;
   UPDATE forge_agent.coding_reviews SET status='approved',lease_token=NULL,lease_expires_at=NULL,verification=verification||coalesce(p_outcome->'verification','{}'),
    finish_token=p_token,finish_receipt=jsonb_build_object('review_id',q.review_id,'status','hosted_green','outcome',p_outcome),updated_at=clock_timestamp() WHERE review_id=q.review_id;
   SELECT finish_receipt INTO receipt FROM forge_agent.coding_reviews WHERE review_id=q.review_id;RETURN receipt;
  END IF;
 END IF;
 SELECT * INTO p FROM forge_agent.coding_review_policy WHERE owner_id=t.owner_id AND project_alias=t.project_alias;
 p.max_rounds:=coalesce((SELECT(b.policy->>'max_rounds')::integer FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),p.max_rounds);
 p.max_stagnant_rounds:=coalesce((SELECT(b.policy->>'max_stagnant_rounds')::integer FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),p.max_stagnant_rounds);
 p.max_infrastructure_retries:=coalesce((SELECT(b.policy->>'max_infrastructure_retries')::integer FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),p.max_infrastructure_retries);
 SELECT r.* INTO previous FROM forge_agent.coding_reviews r JOIN forge_agent.coding_tasks x ON(x.owner_id,x.project_alias,x.id)=(r.owner_id,r.project_alias,r.task_id)
 WHERE r.owner_id=t.owner_id AND r.project_alias=t.project_alias AND coalesce(x.repair_root_task_id,x.id)=coalesce(t.repair_root_task_id,t.id)
 AND r.review_id<>q.review_id AND r.status NOT IN('pending','checking') ORDER BY r.updated_at DESC LIMIT 1;
 IF EXISTS(SELECT 1 FROM jsonb_array_elements_text(coalesce(p_outcome->'verified_finding_ids','[]')) closed
 WHERE NOT EXISTS(SELECT 1 FROM jsonb_array_elements(coalesce(previous.findings,'[]'))f
 WHERE f->>'finding_id'=closed AND coalesce(f->>'check_id','')<>'' AND EXISTS(
 SELECT 1 FROM jsonb_array_elements(coalesce(p_outcome->'verification'->'checks','[]'))c
 WHERE coalesce(c->>'check_id',c->>'name')=f->>'check_id' AND c->>'status' IN('passed','success') AND c->>'head_sha'=v_candidate))) THEN
 RAISE EXCEPTION 'Finding closure lacks independent matching witness' USING ERRCODE='P0009';END IF;
 stagnant:=CASE WHEN jsonb_array_length(coalesce(previous.findings,'[]'))>0
 AND jsonb_array_length(coalesce(p_outcome->'verified_finding_ids','[]'))=0 THEN previous.stagnant_rounds+1 ELSE 0 END;
 UPDATE forge_agent.coding_review_budget SET stagnant_rounds=stagnant WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND root_task_id=coalesce(t.repair_root_task_id,t.id);
 IF disposition='repair' AND(greatest(q.round,coalesce((SELECT b.repair_rounds FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),0))>=p.max_rounds OR stagnant>=p.max_stagnant_rounds) THEN disposition:='handoff';END IF;
 IF disposition='retry_infrastructure' AND q.infrastructure_retries>=p.max_infrastructure_retries THEN disposition:='handoff';END IF;
 -- An automatic verifier can report local success; publication still waits for
 -- an explicit independent owner/critic decision bound to this exact commit.
 UPDATE forge_agent.coding_reviews SET status=disposition,candidate_commit=v_candidate,gate_manifest_sha256=v_gate,
 findings=(SELECT coalesce(jsonb_agg(v),'[]') FROM(SELECT DISTINCT ON(x->>'finding_id')x AS v FROM jsonb_array_elements(coalesce(previous.findings,'[]')||coalesce(p_outcome->'findings','[]'))WITH ORDINALITY AS entries(x,ordinal) ORDER BY x->>'finding_id',ordinal DESC)ledger),verified_finding_ids=coalesce(p_outcome->'verified_finding_ids','[]'),
 verification=verification||coalesce(p_outcome->'verification','{}')||jsonb_build_object('cleanup_confirmed',coalesce(p_outcome->'cleanup_confirmed',p_outcome->'verification'->'cleanup_confirmed','false'::jsonb)),fingerprint=p_outcome->>'fingerprint',summary=coalesce(p_outcome->>'summary',''),reason=coalesce(p_outcome->>'reason',''),
 stagnant_rounds=stagnant,lease_token=NULL,lease_expires_at=NULL,finish_token=p_token,updated_at=clock_timestamp() WHERE review_id=q.review_id RETURNING * INTO q;
 IF disposition='repair' AND v_candidate IS NOT NULL AND v_gate IS NOT NULL THEN
  id:=forge_agent.coding_make_feedback(q.review_id,t.id,'repair',q.summary,q.findings);
  UPDATE forge_agent.coding_reviews SET feedback_id=id WHERE review_id=q.review_id;
  IF t.auto_repair OR(q.phase='source' AND q.owner_decision->>'decision'='repair_source') THEN next_id:=forge_agent.coding_schedule_repair(q.review_id,id);END IF;
 ELSIF disposition='retry_infrastructure' AND(t.auto_repair OR(q.phase='source' AND q.owner_decision->>'decision'='repair_source')) THEN
  INSERT INTO forge_agent.coding_reviews(owner_id,project_alias,task_id,attempt_id,plan_decision_id,reconciliation_receipt_id,base_revision,workspace_digest,parent_result_sha256,
  source_revision,review_no,round,stagnant_rounds,infrastructure_retries,previous_review_id,candidate_commit,phase,owner_decision,verification,target_publication_id,source_publication_id,gate_manifest_sha256)
  VALUES(q.owner_id,q.project_alias,q.task_id,q.attempt_id,q.plan_decision_id,q.reconciliation_receipt_id,q.base_revision,q.workspace_digest,q.parent_result_sha256,
  t.revision,q.review_no+1,q.round,q.stagnant_rounds,q.infrastructure_retries+1,q.review_id,q.candidate_commit,q.phase,q.owner_decision,q.verification,q.target_publication_id,q.source_publication_id,q.gate_manifest_sha256) RETURNING review_id INTO next_id;
 END IF;
 receipt:=jsonb_build_object('review_id',q.review_id,'status',disposition,'feedback_id',id,'repair_task_id',CASE WHEN disposition='repair' THEN next_id ELSE NULL END,'next_review_id',CASE WHEN disposition='retry_infrastructure' THEN next_id ELSE NULL END,'outcome',p_outcome);
 UPDATE forge_agent.coding_reviews SET finish_receipt=receipt WHERE review_id=q.review_id;
 INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
 SELECT t.owner_id,t.project_alias,t.id,coalesce(max(sequence),0)+1,'checkpoint',jsonb_build_object('review_id',q.review_id,'review_status',disposition,'feedback_id',id,'repair_task_id',next_id)
 FROM forge_agent.coding_events WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND task_id=t.id;
 PERFORM pg_notify('forge_coding_jobs','');RETURN receipt;
END;$fn$;
REVOKE ALL ON FUNCTION forge_agent.coding_make_feedback(uuid,uuid,text,text,jsonb),forge_agent.coding_schedule_repair(uuid,uuid),forge_agent.coding_queue_approved_review(uuid) FROM PUBLIC;
REVOKE ALL ON FUNCTION forge_coding_worker_api.read_feedback(uuid,uuid),forge_coding_worker_api.queue_review(uuid,uuid,uuid),
 forge_coding_reconciler_api.review_claim(text),forge_coding_reconciler_api.review_renew(uuid,uuid),forge_coding_reconciler_api.review_finish(uuid,uuid,jsonb) FROM PUBLIC;

CREATE OR REPLACE FUNCTION forge_coding_worker_api.queue_publication(p_attempt_id uuid,p_attempt_token uuid,p_receipt_id uuid)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE
    a forge_agent.coding_attempts%ROWTYPE;
    t forge_agent.coding_tasks%ROWTYPE;
    r forge_agent.coding_reconciliation_receipts%ROWTYPE;
    d forge_agent.coding_plan_decisions%ROWTYPE;
    id uuid;
BEGIN
    a:=forge_coding_worker_api._lock_task_and_attempt(p_attempt_id,p_attempt_token);
    IF a.attempt_id IS NULL THEN RAISE EXCEPTION 'Invalid attempt ownership' USING ERRCODE='42501'; END IF;
    SELECT * INTO t FROM forge_agent.coding_tasks x WHERE x.owner_id=a.owner_id AND x.project_alias=a.project_alias AND x.id=a.task_id;
    IF t.review_required THEN
       RETURN forge_agent.coding_queue_approved_review((SELECT review_id FROM forge_agent.coding_reviews WHERE owner_id=a.owner_id AND project_alias=a.project_alias AND task_id=a.task_id AND attempt_id=a.attempt_id AND status='approved' ORDER BY review_no DESC LIMIT 1));
    END IF;
    IF a.plan_decision_id IS NULL OR a.pinned_recipe->>'mode'<>'implement' THEN RETURN NULL; END IF;
    SELECT * INTO d FROM forge_agent.coding_plan_decisions x WHERE x.owner_id=a.owner_id AND x.project_alias=a.project_alias
       AND x.task_id=a.task_id AND x.decision_id=a.plan_decision_id AND x.decision='resume';
    IF NOT FOUND THEN RETURN NULL; END IF;
    SELECT * INTO r FROM forge_agent.coding_reconciliation_receipts x WHERE x.receipt_id=p_receipt_id
       AND x.owner_id=a.owner_id AND x.project_alias=a.project_alias AND x.task_id=a.task_id AND x.attempt_id=a.attempt_id;
    IF NOT FOUND OR t.latest_attempt_id IS DISTINCT FROM a.attempt_id OR t.plan_decision_id IS DISTINCT FROM d.decision_id
       OR t.status NOT IN ('awaiting_review','completed') OR a.phase<>'stopped' OR a.reservation_active
       OR a.lease_expires_at IS NOT NULL OR a.published_at IS NULL OR a.result_sha256 IS NULL THEN
        RAISE EXCEPTION 'Publication requires reconciled approved implementation' USING ERRCODE='P0009';
    END IF;
    INSERT INTO forge_agent.coding_publications(owner_id,project_alias,task_id,attempt_id,plan_decision_id,
        reconciliation_receipt_id,base_revision,workspace_digest)
    VALUES(a.owner_id,a.project_alias,a.task_id,a.attempt_id,d.decision_id,r.receipt_id,t.base_revision,r.workspace_digest)
    ON CONFLICT DO NOTHING;
    SELECT publication_id INTO id FROM forge_agent.coding_publications x WHERE x.owner_id=a.owner_id
       AND x.project_alias=a.project_alias AND x.task_id=a.task_id AND x.attempt_id=a.attempt_id;
    PERFORM pg_notify('forge_coding_jobs','');
    RETURN id;
END; $fn$;


CREATE OR REPLACE FUNCTION forge_coding_reconciler_api.publication_claim(p_worker_id text)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE j forge_agent.coding_publications%ROWTYPE; approved_review record; now_at timestamptz:=clock_timestamp();
BEGIN
    IF p_worker_id IS NULL OR p_worker_id!~'^[A-Za-z0-9_.-]{1,64}$' THEN
       RAISE EXCEPTION 'Invalid publication worker' USING ERRCODE='22023'; END IF;
    -- Recover a missed worker callback from committed approval, result and
    -- reconciler receipts. No execution or model request is recreated.
    INSERT INTO forge_agent.coding_publications(owner_id,project_alias,task_id,attempt_id,plan_decision_id,
        reconciliation_receipt_id,base_revision,workspace_digest)
    SELECT a.owner_id,a.project_alias,a.task_id,a.attempt_id,d.decision_id,r.receipt_id,t.base_revision,r.workspace_digest
    FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t
      ON (t.owner_id,t.project_alias,t.id,t.latest_attempt_id)=(a.owner_id,a.project_alias,a.task_id,a.attempt_id)
    JOIN forge_agent.coding_plan_decisions d ON (d.owner_id,d.project_alias,d.task_id,d.decision_id)=
      (a.owner_id,a.project_alias,a.task_id,a.plan_decision_id)
    JOIN LATERAL (SELECT x.* FROM forge_agent.coding_reconciliation_receipts x
       WHERE (x.owner_id,x.project_alias,x.task_id,x.attempt_id)=(a.owner_id,a.project_alias,a.task_id,a.attempt_id)
       ORDER BY x.created_at DESC LIMIT 1) r ON true
    WHERE a.owner_id=current_setting('forge.coding_owner',true) AND NOT t.review_required AND d.decision='resume'
      AND t.plan_decision_id=d.decision_id AND t.mode='implement' AND a.pinned_recipe->>'mode'='implement'
      AND t.status IN ('awaiting_review','completed') AND a.phase='stopped' AND NOT a.reservation_active
      AND a.lease_expires_at IS NULL AND a.published_at IS NOT NULL AND a.result_sha256 IS NOT NULL
    ON CONFLICT DO NOTHING;
    FOR approved_review IN SELECT review_id FROM forge_agent.coding_reviews WHERE owner_id=current_setting('forge.coding_owner',true) AND status='approved' LOOP
        PERFORM forge_agent.coding_queue_approved_review(approved_review.review_id);
    END LOOP;
    -- A superseded/revoked task cannot publish, and must not retain an active
    -- outbox state that permanently blocks later explicit task controls.
    UPDATE forge_agent.coding_publications p SET status='failed',lease_token=NULL,lease_expires_at=NULL,
      summary='Publication source is no longer current; review retained remote metadata before retrying.',updated_at=now_at
    WHERE p.owner_id=current_setting('forge.coding_owner',true) AND p.status IN ('pending','checking','publishing')
      AND NOT EXISTS (SELECT 1 FROM forge_agent.coding_tasks t
        WHERE (t.owner_id,t.project_alias,t.id,t.latest_attempt_id,t.plan_decision_id) IS NOT DISTINCT FROM
              (p.owner_id,p.project_alias,p.task_id,p.attempt_id,p.plan_decision_id)
          AND t.mode='implement' AND t.status IN ('awaiting_review','completed') AND(NOT t.review_required OR forge_agent.coding_review_authorized(p.review_id)));
    UPDATE forge_agent.coding_publications SET status='failed',lease_token=NULL,lease_expires_at=NULL,
      summary='Publication outcome is uncertain after its lease expired; review remote state before retrying.',updated_at=now_at
    WHERE owner_id=current_setting('forge.coding_owner',true) AND status='publishing' AND lease_expires_at<=now_at;
    SELECT p.* INTO j FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t
      ON (t.owner_id,t.project_alias,t.id,t.latest_attempt_id,t.plan_decision_id) IS NOT DISTINCT FROM
         (p.owner_id,p.project_alias,p.task_id,p.attempt_id,p.plan_decision_id)
    WHERE p.owner_id=current_setting('forge.coding_owner',true)
      AND (p.status='pending' OR (p.status='checking' AND p.lease_expires_at<=now_at))
      AND t.mode='implement' AND t.status IN ('awaiting_review','completed')
      AND (NOT t.review_required OR forge_agent.coding_review_authorized(p.review_id))
    ORDER BY p.created_at,p.publication_id FOR UPDATE OF p SKIP LOCKED LIMIT 1;
    IF NOT FOUND THEN RETURN NULL; END IF;
    UPDATE forge_agent.coding_publications SET status='checking',worker_id=p_worker_id,lease_token=gen_random_uuid(),
       lease_expires_at=clock_timestamp()+interval '60 seconds',updated_at=clock_timestamp()
    WHERE publication_id=j.publication_id RETURNING * INTO j;
    RETURN jsonb_build_object('publication_id',j.publication_id,'task_id',j.task_id,'attempt_id',j.attempt_id,
      'project_alias',j.project_alias,'plan_decision_id',j.plan_decision_id,'base_revision',j.base_revision,
      'workspace_digest',j.workspace_digest,'worker_id',j.worker_id,'lease_token',j.lease_token,
      'lease_expires_at',to_char(j.lease_expires_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
      'status',j.status,'branch',j.branch,'commit_sha',j.commit_sha,'pr_url',j.pr_url,
      'review_id',j.review_id,'candidate_commit',(SELECT candidate_commit FROM forge_agent.coding_reviews WHERE review_id=j.review_id),
      'gate_manifest_sha256',(SELECT gate_manifest_sha256 FROM forge_agent.coding_reviews WHERE review_id=j.review_id),
      'target_publication_id',coalesce(j.parent_publication_id,j.publication_id),'expected_remote_head',j.expected_remote_head,
      'direct_implementation',(SELECT mode='implement' AND review_required AND pinned_recipe->>'resource_profile'='forge-coding-v5' AND plan_decision_id IS NULL AND parent_task_id IS NULL FROM forge_agent.coding_tasks WHERE owner_id=j.owner_id AND project_alias=j.project_alias AND id=j.task_id),
      'parent_task_id',(SELECT parent_task_id FROM forge_agent.coding_tasks WHERE owner_id=j.owner_id AND project_alias=j.project_alias AND id=j.task_id),
      'feedback_id',(SELECT feedback_id FROM forge_agent.coding_tasks WHERE owner_id=j.owner_id AND project_alias=j.project_alias AND id=j.task_id));
END; $fn$;


CREATE OR REPLACE FUNCTION forge_coding_reconciler_api.publication_renew(p_publication_id uuid,p_lease_token uuid)
RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
    UPDATE forge_agent.coding_publications p SET lease_expires_at=clock_timestamp()+interval '60 seconds',updated_at=clock_timestamp()
    FROM forge_agent.coding_tasks t
    WHERE p.publication_id=p_publication_id AND p.lease_token=p_lease_token
      AND p.owner_id=current_setting('forge.coding_owner',true) AND p.status IN ('checking','publishing')
      AND p.lease_expires_at>clock_timestamp() AND (t.owner_id,t.project_alias,t.id,t.latest_attempt_id,t.plan_decision_id) IS NOT DISTINCT FROM
          (p.owner_id,p.project_alias,p.task_id,p.attempt_id,p.plan_decision_id)
      AND t.mode='implement' AND t.status IN ('awaiting_review','completed') AND(NOT t.review_required OR forge_agent.coding_review_authorized(p.review_id));
    RETURN FOUND;
END; $fn$;

CREATE OR REPLACE FUNCTION forge_coding_reconciler_api.publication_finish(
    p_publication_id uuid,p_lease_token uuid,p_status text,p_branch text,p_commit_sha text,p_pr_url text,p_summary text)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE j forge_agent.coding_publications%ROWTYPE; receipt jsonb;
BEGIN
    IF p_status IS NULL OR p_status NOT IN ('publishing','published','failed') OR p_summary IS NULL OR octet_length(p_summary)>4096
       OR (p_branch IS NOT NULL AND (octet_length(p_branch)>128 OR p_branch!~'^forge/[A-Za-z0-9][A-Za-z0-9._/-]*$' OR p_branch~'(\.\.|//|/$|\.lock$)'))
       OR (p_commit_sha IS NOT NULL AND p_commit_sha!~'^[0-9a-f]{40}$')
       OR (p_pr_url IS NOT NULL AND (octet_length(p_pr_url)>512 OR p_pr_url!~'^https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/pull/[1-9][0-9]*$')) THEN
      RAISE EXCEPTION 'Invalid publication result' USING ERRCODE='22023'; END IF;
    SELECT * INTO j FROM forge_agent.coding_publications p WHERE p.publication_id=p_publication_id
      AND p.owner_id=current_setting('forge.coding_owner',true) FOR UPDATE;
    IF NOT FOUND THEN RAISE EXCEPTION 'Publication not found' USING ERRCODE='P0002'; END IF;
    IF j.review_id IS NOT NULL AND p_commit_sha IS NOT NULL AND p_commit_sha IS DISTINCT FROM(SELECT candidate_commit FROM forge_agent.coding_reviews WHERE review_id=j.review_id) THEN RAISE EXCEPTION 'Publication candidate changed' USING ERRCODE='P0006';END IF;
    receipt:=jsonb_strip_nulls(jsonb_build_object('publication_id',j.publication_id,'status',p_status,'branch',coalesce(p_branch,j.branch),
       'commit_sha',coalesce(p_commit_sha,j.commit_sha),'pr_url',coalesce(p_pr_url,j.pr_url),'summary',p_summary));
    IF j.finish_token=p_lease_token AND j.finish_receipt=receipt THEN RETURN receipt; END IF;
    IF (j.review_id IS NOT NULL AND NOT forge_agent.coding_review_authorized(j.review_id)) OR j.lease_token IS DISTINCT FROM p_lease_token OR j.status NOT IN ('checking','publishing')
       OR j.lease_expires_at<=clock_timestamp() OR NOT EXISTS(SELECT 1 FROM forge_agent.coding_tasks t
         WHERE (t.owner_id,t.project_alias,t.id,t.latest_attempt_id,t.plan_decision_id) IS NOT DISTINCT FROM
               (j.owner_id,j.project_alias,j.task_id,j.attempt_id,j.plan_decision_id)
           AND t.mode='implement' AND t.status IN ('awaiting_review','completed')) THEN
       RAISE EXCEPTION 'Publication lease is not active' USING ERRCODE='P0008'; END IF;
    IF p_status IN ('publishing','published') AND (coalesce(p_branch,j.branch) IS NULL OR coalesce(p_commit_sha,j.commit_sha) IS NULL)
       OR (p_status='published' AND (j.status<>'publishing' OR coalesce(p_pr_url,j.pr_url) IS NULL))
       OR (j.branch IS NOT NULL AND p_branch IS NOT NULL AND j.branch<>p_branch)
       OR (j.commit_sha IS NOT NULL AND p_commit_sha IS NOT NULL AND j.commit_sha<>p_commit_sha) THEN
       RAISE EXCEPTION 'Publication identity changed' USING ERRCODE='P0006'; END IF;
    UPDATE forge_agent.coding_publications SET status=p_status,branch=coalesce(p_branch,j.branch),commit_sha=coalesce(p_commit_sha,j.commit_sha),
       pr_url=coalesce(p_pr_url,j.pr_url),summary=p_summary,updated_at=clock_timestamp(),
       lease_token=CASE WHEN p_status='publishing' THEN j.lease_token ELSE NULL END,
       lease_expires_at=CASE WHEN p_status='publishing' THEN j.lease_expires_at ELSE NULL END,
       finish_token=CASE WHEN p_status='publishing' THEN NULL ELSE p_lease_token END,
       finish_receipt=CASE WHEN p_status='publishing' THEN NULL ELSE receipt END
    WHERE publication_id=j.publication_id;
    INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
      SELECT j.owner_id,j.project_alias,j.task_id,coalesce(max(sequence),0)+1,'checkpoint',jsonb_build_object('publication_id',j.publication_id,'status',p_status)
      FROM forge_agent.coding_events WHERE owner_id=j.owner_id AND project_alias=j.project_alias AND task_id=j.task_id;
    RETURN receipt;
END; $fn$;

CREATE FUNCTION forge_coding_reconciler_api.record_hosted_checks(p_publication uuid,p_head text,p_checks jsonb)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE p forge_agent.coding_publications%ROWTYPE;policy forge_agent.coding_review_policy%ROWTYPE;green boolean;id uuid;
BEGIN
 SELECT * INTO p FROM forge_agent.coding_publications WHERE publication_id=p_publication AND owner_id=current_setting('forge.coding_owner',true);
 IF NOT FOUND OR p.status<>'published' OR p.commit_sha IS DISTINCT FROM p_head OR jsonb_typeof(p_checks)<>'array'
 OR octet_length(p_checks::text)>65536 THEN RAISE EXCEPTION 'Hosted check source mismatch' USING ERRCODE='P0006';END IF;
 IF EXISTS(SELECT 1 FROM jsonb_array_elements(p_checks)x GROUP BY x->>'name' HAVING count(*)>1)
 OR EXISTS(SELECT 1 FROM jsonb_array_elements(p_checks)x WHERE x->>'head_sha' IS DISTINCT FROM p_head OR jsonb_typeof(x->'name') IS DISTINCT FROM 'string' OR x->>'status' NOT IN('success','pending','failure','cancelled','skipped')) THEN
 RAISE EXCEPTION 'Ambiguous hosted check observations' USING ERRCODE='22023';END IF;
 SELECT * INTO policy FROM forge_agent.coding_review_policy WHERE owner_id=p.owner_id AND project_alias=p.project_alias;
 policy.required_hosted_checks:=coalesce((SELECT ARRAY(SELECT jsonb_array_elements_text(b.policy->'required_hosted_checks')) FROM forge_agent.coding_review_budget b JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,coalesce(t.repair_root_task_id,t.id))=(b.owner_id,b.project_alias,b.root_task_id) WHERE t.owner_id=p.owner_id AND t.project_alias=p.project_alias AND t.id=p.task_id),policy.required_hosted_checks);
 green:=policy.owner_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM unnest(policy.required_hosted_checks) name
 WHERE(SELECT count(*) FROM jsonb_array_elements(p_checks)x WHERE x->>'name'=name AND x->>'head_sha'=p_head AND x->>'status'='success')<>1);
 INSERT INTO forge_agent.coding_hosted_checks(owner_id,project_alias,task_id,publication_id,candidate_commit,checks,required_green)
 VALUES(p.owner_id,p.project_alias,p.task_id,p.publication_id,p_head,p_checks,green) RETURNING observation_id INTO id;
 RETURN jsonb_build_object('observation_id',id,'publication_id',p.publication_id,'candidate_commit',p_head,'required_green',green);
END;$fn$;
REVOKE ALL ON FUNCTION forge_coding_reconciler_api.record_hosted_checks(uuid,text,jsonb) FROM PUBLIC;

CREATE FUNCTION forge_agent.coding_publication_hosted_review() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE q forge_agent.coding_reviews%ROWTYPE;n integer;
BEGIN
 IF NEW.status<>'published' OR NEW.review_id IS NULL OR OLD.status='published' THEN RETURN NEW;END IF;
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=NEW.review_id AND status='approved' AND phase='local';
 IF NOT FOUND OR q.candidate_commit IS DISTINCT FROM NEW.commit_sha THEN RAISE EXCEPTION 'Published source lacks independent approval' USING ERRCODE='P0009';END IF;
 PERFORM 1 FROM forge_agent.coding_tasks WHERE owner_id=q.owner_id AND project_alias=q.project_alias AND id=q.task_id FOR UPDATE;
 SELECT coalesce(max(review_no),0)+1 INTO n FROM forge_agent.coding_reviews WHERE owner_id=q.owner_id AND project_alias=q.project_alias AND task_id=q.task_id;
 INSERT INTO forge_agent.coding_reviews(owner_id,project_alias,task_id,attempt_id,plan_decision_id,reconciliation_receipt_id,base_revision,workspace_digest,parent_result_sha256,
  source_revision,review_no,round,stagnant_rounds,infrastructure_retries,previous_review_id,candidate_commit,gate_manifest_sha256,verification,findings,verified_finding_ids,
  phase,target_publication_id,source_publication_id,owner_decision)
 VALUES(q.owner_id,q.project_alias,q.task_id,q.attempt_id,q.plan_decision_id,q.reconciliation_receipt_id,q.base_revision,q.workspace_digest,q.parent_result_sha256,
 q.source_revision,n,q.round,q.stagnant_rounds,0,q.review_id,q.candidate_commit,q.gate_manifest_sha256,
 q.verification||jsonb_build_object('repair_requires_new_workspace',true),q.findings,q.verified_finding_ids,'hosted',coalesce(NEW.parent_publication_id,NEW.publication_id),NEW.publication_id,q.owner_decision);
 PERFORM pg_notify('forge_coding_jobs','');RETURN NEW;
END;$fn$;
CREATE TRIGGER coding_publication_hosted_review AFTER UPDATE ON forge_agent.coding_publications FOR EACH ROW EXECUTE FUNCTION forge_agent.coding_publication_hosted_review();
REVOKE ALL ON FUNCTION forge_agent.coding_publication_hosted_review() FROM PUBLIC;

CREATE FUNCTION forge_agent.coding_review_scope_plan() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 SELECT scope_plan_decision_id INTO NEW.scope_plan_decision_id FROM forge_agent.coding_tasks
 WHERE owner_id=NEW.owner_id AND project_alias=NEW.project_alias AND id=NEW.task_id;
 RETURN NEW;
END;$fn$;
CREATE TRIGGER coding_review_scope_plan BEFORE INSERT ON forge_agent.coding_reviews FOR EACH ROW EXECUTE FUNCTION forge_agent.coding_review_scope_plan();
REVOKE ALL ON FUNCTION forge_agent.coding_review_scope_plan() FROM PUBLIC;

CREATE OR REPLACE FUNCTION forge_coding_worker_api.read_plan(p_attempt_id uuid, p_attempt_token uuid)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE
    a forge_agent.coding_attempts%ROWTYPE;
    d forge_agent.coding_plan_decisions%ROWTYPE;
    source forge_agent.coding_attempts%ROWTYPE;
    content text;
BEGIN
    a := forge_coding_worker_api._lock_task_and_attempt(p_attempt_id,p_attempt_token);
    IF a.attempt_id IS NULL THEN RAISE EXCEPTION 'Invalid attempt ownership' USING ERRCODE='42501'; END IF;
    IF a.lease_expires_at IS NULL OR a.lease_expires_at<=clock_timestamp() OR a.deadline_at<=clock_timestamp()
       OR NOT EXISTS (SELECT 1 FROM forge_agent.coding_tasks t WHERE t.owner_id=a.owner_id AND t.project_alias=a.project_alias
                      AND t.id=a.task_id AND t.latest_attempt_id=a.attempt_id AND t.status='running') THEN
        RAISE EXCEPTION 'Attempt lease is not active' USING ERRCODE='P0008';
    END IF;
    IF a.plan_decision_id IS NULL AND a.scope_plan_decision_id IS NULL THEN RETURN NULL; END IF;
    SELECT * INTO d FROM forge_agent.coding_plan_decisions x
    WHERE x.owner_id=a.owner_id AND x.project_alias=a.project_alias AND x.decision_id=coalesce(a.plan_decision_id,a.scope_plan_decision_id) AND EXISTS(SELECT 1 FROM forge_agent.coding_tasks t WHERE t.owner_id=a.owner_id AND t.project_alias=a.project_alias AND t.id=a.task_id AND(x.task_id=t.id OR x.task_id=coalesce(t.repair_root_task_id,t.id)));
    IF NOT FOUND OR d.decision NOT IN ('resume','revise') THEN RAISE EXCEPTION 'Invalid Plan decision' USING ERRCODE='P0009'; END IF;
    SELECT * INTO source FROM forge_agent.coding_attempts x WHERE x.owner_id=a.owner_id AND x.project_alias=a.project_alias
       AND x.task_id=d.task_id AND x.attempt_id=d.plan_attempt_id;
    IF source.pinned_recipe->>'mode' IS DISTINCT FROM 'plan' OR source.phase<>'stopped'
       OR source.published_at IS NULL OR source.result_sha256 IS DISTINCT FROM d.plan_sha256
       OR source.result_byte_count NOT BETWEEN 1 AND 16777216 THEN
        RAISE EXCEPTION 'Plan artifact no longer matches' USING ERRCODE='P0006';
    END IF;
    SELECT string_agg(c.content,'' ORDER BY c.chunk_index) INTO content FROM forge_agent.coding_result_chunks c
    WHERE c.owner_id=a.owner_id AND c.project_alias=a.project_alias AND c.task_id=d.task_id AND c.attempt_id=d.plan_attempt_id;
    IF content IS NULL OR octet_length(content)<>source.result_byte_count
       OR encode(sha256(convert_to(content,'UTF8')),'hex')<>d.plan_sha256 THEN
        RAISE EXCEPTION 'Plan artifact no longer matches' USING ERRCODE='P0006';
    END IF;
    RETURN jsonb_build_object('decision_id',d.decision_id,'decision',d.decision,'plan_attempt_id',d.plan_attempt_id,
                             'plan_sha256',d.plan_sha256,'text',content,'note',d.note);
END; $fn$;


CREATE FUNCTION forge_agent.coding_review_authorized(p_review uuid) RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
 SELECT EXISTS(SELECT 1 FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t
 ON(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)=(q.owner_id,q.project_alias,q.task_id,q.attempt_id)
 WHERE q.review_id=p_review AND q.owner_id=current_setting('forge.coding_owner',true)
 AND q.phase='local' AND q.status='approved' AND q.owner_decision IS NOT NULL
 AND q.owner_decision->>'candidate_commit'=q.candidate_commit AND q.owner_decision->>'gate_manifest_sha256'=q.gate_manifest_sha256
 AND q.verification->'checks_passed'='true'::jsonb AND q.verification->'cleanup_confirmed'='true'::jsonb
 AND t.plan_decision_id IS NOT DISTINCT FROM q.plan_decision_id AND t.scope_plan_decision_id IS NOT DISTINCT FROM q.scope_plan_decision_id
 AND t.mode='implement' AND t.status IN('awaiting_review','completed')
 AND NOT EXISTS(SELECT 1 FROM forge_agent.coding_attempts a WHERE a.owner_id=t.owner_id AND a.project_alias=t.project_alias AND a.task_id=t.id AND(a.reservation_active OR a.lease_expires_at IS NOT NULL)))
$fn$;
REVOKE ALL ON FUNCTION forge_agent.coding_review_authorized(uuid) FROM PUBLIC;
