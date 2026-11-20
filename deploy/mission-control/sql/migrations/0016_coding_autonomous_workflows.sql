-- Owner-delegated peer review. Native agents retain only attempt-scoped API access.
ALTER TABLE forge_agent.coding_tasks ADD COLUMN workflow_required boolean NOT NULL DEFAULT false;
ALTER TABLE forge_agent.coding_tasks ADD COLUMN workflow_id uuid;
ALTER TABLE forge_agent.coding_tasks ADD COLUMN workflow_review_id uuid;
ALTER TABLE forge_agent.coding_tasks ADD COLUMN review_target_sha256 text CHECK(review_target_sha256 IS NULL OR review_target_sha256~'^[0-9a-f]{64}$');
ALTER TABLE forge_agent.coding_attempts ADD COLUMN workflow_id uuid;
ALTER TABLE forge_agent.coding_attempts ADD COLUMN workflow_review_id uuid;
ALTER TABLE forge_agent.coding_attempts ADD COLUMN review_target_sha256 text CHECK(review_target_sha256 IS NULL OR review_target_sha256~'^[0-9a-f]{64}$');
CREATE TABLE forge_agent.coding_workflows (
 workflow_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),owner_id text NOT NULL,project_alias text NOT NULL,
 root_task_id uuid NOT NULL,policy_digest text NOT NULL CHECK(policy_digest~'^[0-9a-f]{64}$'),snapshot jsonb NOT NULL CHECK(jsonb_typeof(snapshot)='object'),
 phase text NOT NULL DEFAULT 'plan' CHECK(phase IN('plan','plan_review','plan_revision','implement','checks','code_review','repair','publishing','hosted','merging','completed','cancelled','escalated')),
 plan_revisions integer NOT NULL DEFAULT 0 CHECK(plan_revisions BETWEEN 0 AND 3),
 escalation_reason text NOT NULL DEFAULT '' CHECK(octet_length(escalation_reason)<=4096),
 enabled boolean NOT NULL DEFAULT true,created_at timestamptz NOT NULL DEFAULT clock_timestamp(),updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 UNIQUE(owner_id,project_alias,root_task_id),UNIQUE(owner_id,project_alias,workflow_id),
 FOREIGN KEY(owner_id,project_alias,root_task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id)
);
CREATE TABLE forge_agent.coding_workflow_reviews (
 workflow_review_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),workflow_id uuid NOT NULL REFERENCES forge_agent.coding_workflows(workflow_id),
 owner_id text NOT NULL,project_alias text NOT NULL,target_kind text NOT NULL CHECK(target_kind IN('plan','code')),
 source_task_id uuid NOT NULL,source_attempt_id uuid NOT NULL,source_result_sha256 text NOT NULL CHECK(source_result_sha256~'^[0-9a-f]{64}$'),source_revision bigint NOT NULL CHECK(source_revision>0),
 source_review_id uuid REFERENCES forge_agent.coding_reviews(review_id),candidate_commit text CHECK(candidate_commit IS NULL OR candidate_commit~'^[0-9a-f]{40}$'),
 gate_manifest_sha256 text CHECK(gate_manifest_sha256 IS NULL OR gate_manifest_sha256~'^[0-9a-f]{64}$'),
 target_json text CHECK(target_json IS NULL OR octet_length(target_json)<=67174400),target_sha256 text CHECK(target_sha256 IS NULL OR target_sha256~'^[0-9a-f]{64}$'),
 reviewer_task_id uuid,reviewer_attempt_id uuid,reviewer_result_sha256 text CHECK(reviewer_result_sha256 IS NULL OR reviewer_result_sha256~'^[0-9a-f]{64}$'),
 verdict_json text CHECK(verdict_json IS NULL OR octet_length(verdict_json)<=65536),verdict_sha256 text CHECK(verdict_sha256 IS NULL OR verdict_sha256~'^[0-9a-f]{64}$'),
 status text NOT NULL DEFAULT 'pending' CHECK(status IN('pending','reviewing','decided','escalated','cancelled')),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 UNIQUE NULLS NOT DISTINCT(workflow_id,target_kind,source_attempt_id,source_review_id),
 FOREIGN KEY(owner_id,project_alias,source_task_id,source_attempt_id) REFERENCES forge_agent.coding_attempts(owner_id,project_alias,task_id,attempt_id),
 FOREIGN KEY(owner_id,project_alias,reviewer_task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id),
 CHECK((target_json IS NULL AND target_sha256 IS NULL) OR(target_json IS NOT NULL AND target_sha256=encode(sha256(convert_to(target_json,'UTF8')),'hex'))),
 CHECK((verdict_json IS NULL AND verdict_sha256 IS NULL) OR(verdict_json IS NOT NULL AND verdict_sha256=encode(sha256(convert_to(verdict_json,'UTF8')),'hex')))
);
CREATE TABLE forge_agent.coding_workflow_actions (
 action_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),workflow_id uuid NOT NULL REFERENCES forge_agent.coding_workflows(workflow_id),
 owner_id text NOT NULL,project_alias text NOT NULL,workflow_review_id uuid REFERENCES forge_agent.coding_workflow_reviews(workflow_review_id),
 kind text NOT NULL CHECK(kind IN('review','decide','merge')),source_key text NOT NULL CHECK(octet_length(source_key) BETWEEN 1 AND 256),
 source_publication_id uuid REFERENCES forge_agent.coding_publications(publication_id),
 status text NOT NULL DEFAULT 'pending' CHECK(status IN('pending','leased','completed','handoff','needs_reconcile','cancelled')),
 worker_id text,lease_token uuid,lease_expires_at timestamptz,intent jsonb,finish_receipt jsonb,finish_token uuid,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 UNIQUE(workflow_id,kind,source_key),
 CHECK((lease_token IS NULL AND lease_expires_at IS NULL) OR(lease_token IS NOT NULL AND lease_expires_at IS NOT NULL)),
 CHECK(intent IS NULL OR octet_length(intent::text)<=65536)
);
CREATE INDEX coding_workflow_actions_pending ON forge_agent.coding_workflow_actions(owner_id,kind,status,created_at);
ALTER TABLE forge_agent.coding_tasks ADD CONSTRAINT coding_tasks_workflow_fk FOREIGN KEY(workflow_id) REFERENCES forge_agent.coding_workflows(workflow_id);
ALTER TABLE forge_agent.coding_tasks ADD CONSTRAINT coding_tasks_workflow_review_fk FOREIGN KEY(workflow_review_id) REFERENCES forge_agent.coding_workflow_reviews(workflow_review_id);
ALTER TABLE forge_agent.coding_attempts ADD CONSTRAINT coding_attempts_workflow_fk FOREIGN KEY(workflow_id) REFERENCES forge_agent.coding_workflows(workflow_id);
ALTER TABLE forge_agent.coding_attempts ADD CONSTRAINT coding_attempts_workflow_review_fk FOREIGN KEY(workflow_review_id) REFERENCES forge_agent.coding_workflow_reviews(workflow_review_id);
REVOKE ALL ON forge_agent.coding_workflows,forge_agent.coding_workflow_reviews,forge_agent.coding_workflow_actions FROM PUBLIC;

CREATE FUNCTION forge_agent.coding_workflow_evidence_guard() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 IF TG_TABLE_NAME='coding_workflows' THEN
 IF (NEW.owner_id,NEW.project_alias,NEW.root_task_id,NEW.policy_digest,NEW.snapshot) IS DISTINCT FROM(OLD.owner_id,OLD.project_alias,OLD.root_task_id,OLD.policy_digest,OLD.snapshot) THEN RAISE EXCEPTION 'Immutable workflow authority' USING ERRCODE='P0009';END IF;END IF;
 IF TG_TABLE_NAME='coding_workflow_reviews' THEN
  IF (NEW.workflow_id,NEW.owner_id,NEW.project_alias,NEW.target_kind,NEW.source_task_id,NEW.source_attempt_id,NEW.source_result_sha256,NEW.source_revision,NEW.source_review_id,NEW.candidate_commit,NEW.gate_manifest_sha256) IS DISTINCT FROM(OLD.workflow_id,OLD.owner_id,OLD.project_alias,OLD.target_kind,OLD.source_task_id,OLD.source_attempt_id,OLD.source_result_sha256,OLD.source_revision,OLD.source_review_id,OLD.candidate_commit,OLD.gate_manifest_sha256)
   OR(OLD.target_json IS NOT NULL AND(NEW.target_json,NEW.target_sha256,NEW.reviewer_task_id) IS DISTINCT FROM(OLD.target_json,OLD.target_sha256,OLD.reviewer_task_id))
   OR(OLD.verdict_json IS NOT NULL AND(NEW.verdict_json,NEW.verdict_sha256,NEW.reviewer_attempt_id,NEW.reviewer_result_sha256) IS DISTINCT FROM(OLD.verdict_json,OLD.verdict_sha256,OLD.reviewer_attempt_id,OLD.reviewer_result_sha256)) THEN RAISE EXCEPTION 'Immutable peer review evidence' USING ERRCODE='P0009';END IF;
 END IF;
 IF TG_TABLE_NAME='coding_workflow_actions' THEN
 IF (OLD.intent IS NOT NULL AND NEW.intent IS DISTINCT FROM OLD.intent OR OLD.finish_receipt IS NOT NULL AND NEW.finish_receipt IS DISTINCT FROM OLD.finish_receipt) THEN RAISE EXCEPTION 'Immutable workflow receipt' USING ERRCODE='P0009';END IF;END IF;
 RETURN NEW;
END;$fn$;
CREATE TRIGGER coding_workflows_evidence_guard BEFORE UPDATE ON forge_agent.coding_workflows FOR EACH ROW EXECUTE FUNCTION forge_agent.coding_workflow_evidence_guard();
CREATE TRIGGER coding_workflow_reviews_evidence_guard BEFORE UPDATE ON forge_agent.coding_workflow_reviews FOR EACH ROW EXECUTE FUNCTION forge_agent.coding_workflow_evidence_guard();
CREATE TRIGGER coding_workflow_actions_evidence_guard BEFORE UPDATE ON forge_agent.coding_workflow_actions FOR EACH ROW EXECUTE FUNCTION forge_agent.coding_workflow_evidence_guard();
REVOKE ALL ON FUNCTION forge_agent.coding_workflow_evidence_guard() FROM PUBLIC;

CREATE FUNCTION forge_agent.coding_workflow_attempt_context() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 SELECT workflow_id,workflow_review_id,review_target_sha256 INTO NEW.workflow_id,NEW.workflow_review_id,NEW.review_target_sha256
 FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(NEW.owner_id,NEW.project_alias,NEW.task_id);
 RETURN NEW;
END;$fn$;
CREATE TRIGGER coding_attempts_workflow_context BEFORE INSERT ON forge_agent.coding_attempts FOR EACH ROW EXECUTE FUNCTION forge_agent.coding_workflow_attempt_context();
REVOKE ALL ON FUNCTION forge_agent.coding_workflow_attempt_context() FROM PUBLIC;

CREATE FUNCTION forge_coding_worker_api.read_review_target(p_attempt uuid,p_token uuid) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.coding_attempts%ROWTYPE;r forge_agent.coding_workflow_reviews%ROWTYPE;
BEGIN
 a:=forge_coding_worker_api._lock_task_and_attempt(p_attempt,p_token);
 IF a.attempt_id IS NULL OR a.lease_expires_at IS NULL OR a.lease_expires_at<=clock_timestamp() OR a.deadline_at<=clock_timestamp() OR a.pinned_recipe->>'mode'<>'review' THEN RAISE EXCEPTION 'Invalid reviewer attempt' USING ERRCODE='42501';END IF;
 IF a.workflow_review_id IS NULL THEN RETURN NULL;END IF;
 SELECT * INTO r FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=a.workflow_review_id AND workflow_id=a.workflow_id AND owner_id=a.owner_id AND project_alias=a.project_alias AND reviewer_task_id=a.task_id;
 IF NOT FOUND OR r.target_sha256 IS DISTINCT FROM a.review_target_sha256 OR r.target_json IS NULL OR r.status<>'reviewing' OR NOT EXISTS(SELECT 1 FROM forge_agent.coding_workflows w JOIN forge_agent.coding_tasks t ON t.workflow_id=w.workflow_id WHERE w.workflow_id=a.workflow_id AND w.enabled AND w.phase NOT IN('cancelled','escalated','completed') AND t.id=a.task_id AND t.status='running' AND t.latest_attempt_id=a.attempt_id) THEN RAISE EXCEPTION 'Review target no longer authorized' USING ERRCODE='P0009';END IF;
 RETURN jsonb_build_object('target_json',r.target_json,'target_sha256',r.target_sha256);
END;$fn$;
REVOKE ALL ON FUNCTION forge_coding_worker_api.read_review_target(uuid,uuid) FROM PUBLIC;

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
             AND pinned_recipe->>'resource_profile' IN ('forge-coding-v1', 'forge-coding-v2', 'forge-coding-v3', 'forge-coding-v4', 'forge-coding-v5', 'forge-coding-v6')
             AND pinned_recipe->>'mode' IN ('plan', 'review', 'implement')
             AND jsonb_typeof(pinned_recipe->'generation_token_limit') = 'null'
             AND (pinned_recipe->>'policy_digest') ~ '^[0-9a-f]{64}$') IS TRUE
        );

ALTER TABLE forge_agent.coding_tasks ADD COLUMN workflow_feedback_review_id uuid REFERENCES forge_agent.coding_workflow_reviews(workflow_review_id);
ALTER TABLE forge_agent.coding_tasks ADD COLUMN workflow_feedback_sha256 text CHECK(workflow_feedback_sha256 IS NULL OR workflow_feedback_sha256~'^[0-9a-f]{64}$');
ALTER TABLE forge_agent.coding_attempts ADD COLUMN workflow_feedback_review_id uuid REFERENCES forge_agent.coding_workflow_reviews(workflow_review_id);
ALTER TABLE forge_agent.coding_attempts ADD COLUMN workflow_feedback_sha256 text CHECK(workflow_feedback_sha256 IS NULL OR workflow_feedback_sha256~'^[0-9a-f]{64}$');

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
          AND(t.workflow_id IS NULL OR EXISTS(SELECT 1 FROM forge_agent.coding_workflows w WHERE w.workflow_id=t.workflow_id AND w.enabled AND w.phase NOT IN('completed','cancelled','escalated')))
          AND t.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
          AND t.agent_profile = ANY(p_allowed_profiles)
          AND (t.pinned_recipe->'version') = '2'::jsonb
          AND (t.pinned_recipe->>'resource_profile') IN ('forge-coding-v1', 'forge-coding-v2', 'forge-coding-v3', 'forge-coding-v4', 'forge-coding-v5', 'forge-coding-v6')
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
          AND(t.workflow_id IS NULL OR EXISTS(SELECT 1 FROM forge_agent.coding_workflows w WHERE w.workflow_id=t.workflow_id AND w.enabled AND w.phase NOT IN('completed','cancelled','escalated')))
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
            'repair_source_review_id',v_task.repair_source_review_id,'repair_candidate_commit',v_task.repair_candidate_commit,'scope_plan_decision_id',v_task.scope_plan_decision_id,
            'parent_task_id',v_task.parent_task_id,'workflow_id',v_task.workflow_id,'workflow_review_id',v_task.workflow_review_id,'review_target_sha256',v_task.review_target_sha256,
            'workflow_feedback_review_id',v_task.workflow_feedback_review_id,'workflow_feedback_sha256',v_task.workflow_feedback_sha256
        );
    END LOOP;

    RETURN NULL;
END;
$fn$;



CREATE FUNCTION forge_agent.coding_workflow_inherit() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 IF NEW.workflow_id IS NULL AND NEW.repair_root_task_id IS NOT NULL THEN
  SELECT workflow_id,workflow_required INTO NEW.workflow_id,NEW.workflow_required FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(NEW.owner_id,NEW.project_alias,NEW.repair_root_task_id);
 END IF;RETURN NEW;
END;$fn$;
CREATE TRIGGER coding_tasks_workflow_inherit BEFORE INSERT ON forge_agent.coding_tasks FOR EACH ROW EXECUTE FUNCTION forge_agent.coding_workflow_inherit();
REVOKE ALL ON FUNCTION forge_agent.coding_workflow_inherit() FROM PUBLIC;

CREATE FUNCTION forge_agent.coding_workflow_merge_authorized(p_action uuid) RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
 SELECT EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions a JOIN forge_agent.coding_workflows w USING(workflow_id)
 JOIN forge_agent.coding_publications p ON p.publication_id=a.source_publication_id
 JOIN forge_agent.coding_reviews q ON q.review_id=p.review_id
 JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)=(p.owner_id,p.project_alias,p.task_id,p.attempt_id)
 WHERE a.action_id=p_action AND a.owner_id=current_setting('forge.coding_owner',true) AND a.owner_id=w.owner_id AND a.project_alias=w.project_alias
 AND w.enabled AND w.phase NOT IN('cancelled','completed','escalated') AND w.snapshot->'auto_merge'='true'::jsonb AND w.snapshot->>'merge_method'='merge'
 AND t.workflow_id=w.workflow_id AND t.status='awaiting_review' AND p.status='published' AND p.commit_sha=q.candidate_commit
 AND q.status='approved' AND q.owner_decision->>'closed_by'='peer_reviewer' AND q.owner_decision->>'workflow_id'=w.workflow_id::text
 AND forge_agent.coding_review_authorized(q.review_id)
 AND (SELECT h.required_green FROM forge_agent.coding_hosted_checks h WHERE h.publication_id=p.publication_id AND h.candidate_commit=p.commit_sha ORDER BY h.observed_at DESC,h.observation_id DESC LIMIT 1) IS TRUE
 AND EXISTS(SELECT 1 FROM forge_agent.coding_workflow_reviews r WHERE r.workflow_review_id::text=q.owner_decision->>'workflow_review_id' AND r.workflow_id=w.workflow_id AND r.status='decided' AND r.candidate_commit=q.candidate_commit AND r.gate_manifest_sha256=q.gate_manifest_sha256 AND r.verdict_json::jsonb->>'decision'='approve')
 AND NOT EXISTS(SELECT 1 FROM forge_agent.coding_attempts n JOIN forge_agent.coding_tasks z ON(z.owner_id,z.project_alias,z.id)=(n.owner_id,n.project_alias,n.task_id) WHERE z.workflow_id=w.workflow_id AND(n.reservation_active OR n.lease_expires_at IS NOT NULL)))
$fn$;
REVOKE ALL ON FUNCTION forge_agent.coding_workflow_merge_authorized(uuid) FROM PUBLIC;

CREATE FUNCTION forge_coding_reconciler_api.merge_claim(p_worker text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.coding_workflow_actions%ROWTYPE;w forge_agent.coding_workflows%ROWTYPE;p forge_agent.coding_publications%ROWTYPE;q forge_agent.coding_reviews%ROWTYPE;
BEGIN
 IF p_worker IS NULL OR p_worker!~'^[A-Za-z0-9_.-]{1,64}$' THEN RAISE EXCEPTION 'Invalid merge worker' USING ERRCODE='22023';END IF;
 SELECT * INTO a FROM forge_agent.coding_workflow_actions x WHERE x.owner_id=current_setting('forge.coding_owner',true) AND x.kind='merge' AND(x.status='pending' OR(x.status='leased' AND x.lease_expires_at<=clock_timestamp())) AND forge_agent.coding_workflow_merge_authorized(x.action_id) ORDER BY x.created_at,x.action_id FOR UPDATE SKIP LOCKED LIMIT 1;
 IF NOT FOUND THEN RETURN NULL;END IF;
 UPDATE forge_agent.coding_workflow_actions SET status='leased',worker_id=p_worker,lease_token=gen_random_uuid(),lease_expires_at=clock_timestamp()+interval '60 seconds',updated_at=clock_timestamp() WHERE action_id=a.action_id RETURNING * INTO a;
 SELECT * INTO w FROM forge_agent.coding_workflows WHERE workflow_id=a.workflow_id;
 SELECT * INTO p FROM forge_agent.coding_publications WHERE publication_id=a.source_publication_id;
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p.review_id;
 RETURN jsonb_build_object('merge_id',a.action_id,'workflow_id',a.workflow_id,'task_id',p.task_id,'project_alias',a.project_alias,'lease_token',a.lease_token,
 'candidate_commit',q.candidate_commit,'review_id',q.review_id,'gate_manifest_sha256',q.gate_manifest_sha256,'target_publication_id',coalesce(p.parent_publication_id,p.publication_id),
 'source_publication_id',p.publication_id,'pr_url',p.pr_url,'branch',p.branch,'merge_method','merge','required_hosted_checks',w.snapshot->'repair_policy'->'required_hosted_checks','policy_digest',w.policy_digest,'intent',a.intent);
END;$fn$;

CREATE FUNCTION forge_coding_reconciler_api.merge_renew(p_id uuid,p_token uuid) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 UPDATE forge_agent.coding_workflow_actions SET lease_expires_at=clock_timestamp()+interval '60 seconds',updated_at=clock_timestamp()
 WHERE action_id=p_id AND owner_id=current_setting('forge.coding_owner',true) AND kind='merge' AND status='leased' AND lease_token=p_token AND lease_expires_at>clock_timestamp() AND forge_agent.coding_workflow_merge_authorized(p_id);
 RETURN FOUND;
END;$fn$;

CREATE FUNCTION forge_coding_reconciler_api.merge_intent(p_id uuid,p_token uuid,p_intent jsonb) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.coding_workflow_actions%ROWTYPE;p forge_agent.coding_publications%ROWTYPE;q forge_agent.coding_reviews%ROWTYPE;
BEGIN
 SELECT * INTO a FROM forge_agent.coding_workflow_actions WHERE action_id=p_id AND owner_id=current_setting('forge.coding_owner',true) FOR UPDATE;
 IF NOT FOUND OR a.kind<>'merge' OR a.status<>'leased' OR a.lease_token IS DISTINCT FROM p_token OR a.lease_expires_at<=clock_timestamp() OR NOT forge_agent.coding_workflow_merge_authorized(p_id) THEN RAISE EXCEPTION 'Invalid merge lease' USING ERRCODE='P0008';END IF;
 IF a.intent IS NOT NULL THEN IF a.intent IS DISTINCT FROM p_intent THEN RAISE EXCEPTION 'Merge intent conflict' USING ERRCODE='P0006';END IF;RETURN a.intent;END IF;
 SELECT * INTO p FROM forge_agent.coding_publications WHERE publication_id=a.source_publication_id;
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p.review_id;
 IF (jsonb_typeof(p_intent)<>'object' OR octet_length(p_intent::text)>65536 OR p_intent-ARRAY['version','merge_id','workflow_id','task_id','repository','pr_url','candidate_commit','candidate_tree','expected_main','merge_commit','review_id','gate_manifest_sha256','commit_payload']<>'{}'::jsonb
 OR NOT p_intent?&ARRAY['version','merge_id','workflow_id','task_id','repository','pr_url','candidate_commit','candidate_tree','expected_main','merge_commit','review_id','gate_manifest_sha256','commit_payload']
 OR p_intent->'version'<>'1'::jsonb OR p_intent->>'merge_id'<>a.action_id::text OR p_intent->>'workflow_id'<>a.workflow_id::text OR p_intent->>'task_id'<>p.task_id::text
 OR p_intent->>'candidate_commit'<>q.candidate_commit OR p_intent->>'review_id'<>q.review_id::text OR p_intent->>'gate_manifest_sha256'<>q.gate_manifest_sha256 OR p_intent->>'pr_url'<>p.pr_url
 OR p_intent->>'repository'!~'^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$' OR p_intent->>'candidate_tree'!~'^[0-9a-f]{40}$' OR p_intent->>'expected_main'!~'^[0-9a-f]{40}$' OR p_intent->>'merge_commit'!~'^[0-9a-f]{40}$'
 OR jsonb_typeof(p_intent->'commit_payload')<>'object' OR (p_intent->'commit_payload')-ARRAY['message','tree','parents','author','committer']<>'{}'::jsonb
 OR NOT(p_intent->'commit_payload')?&ARRAY['message','tree','parents','author','committer']
 OR (p_intent->'commit_payload'->>'tree')<>(p_intent->>'candidate_tree') OR (p_intent->'commit_payload'->'parents')<>jsonb_build_array(p_intent->>'expected_main',q.candidate_commit)
 OR jsonb_typeof(p_intent->'commit_payload'->'message')<>'string' OR octet_length(p_intent->'commit_payload'->>'message') NOT BETWEEN 1 AND 4096
 OR jsonb_typeof(p_intent->'commit_payload'->'author')<>'object' OR jsonb_typeof(p_intent->'commit_payload'->'committer')<>'object'
 OR (p_intent->'commit_payload'->'author')-ARRAY['name','email','date']<>'{}'::jsonb OR NOT(p_intent->'commit_payload'->'author')?&ARRAY['name','email','date']
 OR (p_intent->'commit_payload'->'committer')-ARRAY['name','email','date']<>'{}'::jsonb OR NOT(p_intent->'commit_payload'->'committer')?&ARRAY['name','email','date']
 OR EXISTS(SELECT 1 FROM jsonb_each(p_intent->'commit_payload'->'author') x WHERE jsonb_typeof(x.value)<>'string' OR octet_length(x.value#>>'{}') NOT BETWEEN 1 AND 256)
 OR EXISTS(SELECT 1 FROM jsonb_each(p_intent->'commit_payload'->'committer') x WHERE jsonb_typeof(x.value)<>'string' OR octet_length(x.value#>>'{}') NOT BETWEEN 1 AND 256)) IS DISTINCT FROM false
 THEN RAISE EXCEPTION 'Invalid merge intent' USING ERRCODE='22023';END IF;
 UPDATE forge_agent.coding_workflow_actions SET intent=p_intent,updated_at=clock_timestamp() WHERE action_id=p_id;
 RETURN p_intent;
END;$fn$;

CREATE FUNCTION forge_coding_reconciler_api.merge_finish(p_id uuid,p_token uuid,p_status text,p_receipt jsonb) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.coding_workflow_actions%ROWTYPE;p forge_agent.coding_publications%ROWTYPE;task uuid;root uuid;
BEGIN
 SELECT x.task_id INTO task FROM forge_agent.coding_publications x JOIN forge_agent.coding_workflow_actions y ON y.source_publication_id=x.publication_id WHERE y.action_id=p_id AND y.owner_id=current_setting('forge.coding_owner',true);
 IF task IS NULL THEN RAISE EXCEPTION 'Unknown merge' USING ERRCODE='42501';END IF;
 SELECT w.root_task_id INTO root FROM forge_agent.coding_workflows w JOIN forge_agent.coding_workflow_actions x USING(workflow_id) WHERE x.action_id=p_id;
 IF root<>task THEN PERFORM 1 FROM forge_agent.coding_tasks WHERE id=root AND owner_id=current_setting('forge.coding_owner',true) FOR UPDATE;END IF;
 PERFORM 1 FROM forge_agent.coding_tasks WHERE id=task AND owner_id=current_setting('forge.coding_owner',true) FOR UPDATE;
 SELECT * INTO a FROM forge_agent.coding_workflow_actions WHERE action_id=p_id AND owner_id=current_setting('forge.coding_owner',true) FOR UPDATE;
 IF a.finish_receipt IS NOT NULL AND a.finish_token=p_token THEN
  IF a.finish_receipt IS DISTINCT FROM jsonb_build_object('status',p_status,'receipt',p_receipt) THEN RAISE EXCEPTION 'Merge receipt conflict' USING ERRCODE='P0006';END IF;RETURN a.finish_receipt;END IF;
 IF a.status<>'leased' OR a.lease_token IS DISTINCT FROM p_token OR a.lease_expires_at<=clock_timestamp() THEN RAISE EXCEPTION 'Invalid merge lease' USING ERRCODE='P0008';END IF;
 SELECT * INTO p FROM forge_agent.coding_publications WHERE publication_id=a.source_publication_id;
 IF p_status NOT IN('merged','handoff','needs_reconcile') OR jsonb_typeof(p_receipt)<>'object' OR p_receipt-ARRAY['candidate_commit','merge_commit','pr_url','summary']<>'{}'::jsonb OR NOT p_receipt?&ARRAY['candidate_commit','merge_commit','pr_url','summary'] OR p_receipt->>'candidate_commit' IS DISTINCT FROM p.commit_sha OR p_receipt->>'pr_url' IS DISTINCT FROM p.pr_url OR jsonb_typeof(p_receipt->'summary')<>'string' OR octet_length(p_receipt->>'summary')>4096 THEN RAISE EXCEPTION 'Invalid merge receipt' USING ERRCODE='22023';END IF;
 IF p_status='merged' AND(a.intent IS NULL OR p_receipt->>'merge_commit' IS DISTINCT FROM a.intent->>'merge_commit' OR NOT forge_agent.coding_workflow_merge_authorized(p_id)) THEN RAISE EXCEPTION 'Unapproved merge receipt' USING ERRCODE='P0009';END IF;
 UPDATE forge_agent.coding_workflow_actions SET status=CASE WHEN p_status='merged' THEN 'completed' ELSE p_status END,finish_token=p_token,finish_receipt=jsonb_build_object('status',p_status,'receipt',p_receipt),lease_token=NULL,lease_expires_at=NULL,updated_at=clock_timestamp() WHERE action_id=p_id RETURNING * INTO a;
 UPDATE forge_agent.coding_workflows SET phase=CASE WHEN p_status='merged' THEN 'completed' ELSE 'escalated' END,escalation_reason=CASE WHEN p_status='merged' THEN '' ELSE p_receipt->>'summary' END,updated_at=clock_timestamp() WHERE workflow_id=a.workflow_id;
 IF p_status='merged' THEN UPDATE forge_agent.coding_tasks SET status='completed',revision=revision+1,accepted_at=clock_timestamp(),updated_at=clock_timestamp() WHERE workflow_id=a.workflow_id AND workflow_review_id IS NULL AND status='awaiting_review';END IF;
 RETURN a.finish_receipt;
END;$fn$;
REVOKE ALL ON FUNCTION forge_coding_reconciler_api.merge_claim(text),forge_coding_reconciler_api.merge_renew(uuid,uuid),forge_coding_reconciler_api.merge_intent(uuid,uuid,jsonb),forge_coding_reconciler_api.merge_finish(uuid,uuid,text,jsonb) FROM PUBLIC;

CREATE OR REPLACE FUNCTION forge_agent.coding_workflow_attempt_context() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 SELECT workflow_id,workflow_review_id,review_target_sha256,workflow_feedback_review_id,workflow_feedback_sha256
 INTO NEW.workflow_id,NEW.workflow_review_id,NEW.review_target_sha256,NEW.workflow_feedback_review_id,NEW.workflow_feedback_sha256
 FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(NEW.owner_id,NEW.project_alias,NEW.task_id);
 RETURN NEW;
END;$fn$;
CREATE FUNCTION forge_coding_worker_api.read_workflow_feedback(p_attempt uuid,p_token uuid) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.coding_attempts%ROWTYPE;r forge_agent.coding_workflow_reviews%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;
BEGIN
 a:=forge_coding_worker_api._lock_task_and_attempt(p_attempt,p_token);
 IF a.attempt_id IS NULL OR a.lease_expires_at IS NULL OR a.lease_expires_at<=clock_timestamp() OR a.deadline_at<=clock_timestamp() THEN RAISE EXCEPTION 'Invalid author attempt' USING ERRCODE='42501';END IF;
 IF a.workflow_feedback_review_id IS NULL THEN RETURN NULL;END IF;
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id);
 SELECT * INTO r FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=a.workflow_feedback_review_id AND workflow_id=a.workflow_id AND owner_id=a.owner_id AND project_alias=a.project_alias;
 IF NOT FOUND OR t.status<>'running' OR t.latest_attempt_id<>a.attempt_id OR r.status<>'decided' OR r.verdict_sha256 IS DISTINCT FROM a.workflow_feedback_sha256 OR r.verdict_json IS NULL
 OR NOT EXISTS(SELECT 1 FROM forge_agent.coding_workflows w WHERE w.workflow_id=a.workflow_id AND w.enabled AND w.phase NOT IN('cancelled','completed','escalated') AND w.snapshot->>'author_profile'=a.agent_profile)
 OR NOT((r.target_kind='plan' AND EXISTS(SELECT 1 FROM forge_agent.coding_plan_decisions d WHERE d.decision_id=a.plan_decision_id AND d.owner_id=a.owner_id AND d.project_alias=a.project_alias AND d.task_id=a.task_id AND d.plan_attempt_id=r.source_attempt_id AND d.plan_sha256=r.source_result_sha256))
 OR(r.target_kind='code' AND(a.repair_source_review_id=r.source_review_id OR(t.parent_task_id IS NULL AND r.source_task_id=t.id)) AND EXISTS(SELECT 1 FROM forge_agent.coding_feedback f WHERE f.feedback_id=a.feedback_id AND f.owner_id=a.owner_id AND f.project_alias=a.project_alias AND f.task_id=a.task_id AND f.parent_attempt_id=r.source_attempt_id AND f.review_id=r.source_review_id)))
 THEN RAISE EXCEPTION 'Peer feedback no longer authorized' USING ERRCODE='P0009';END IF;
 RETURN jsonb_build_object('target_json',r.target_json,'target_sha256',r.target_sha256,'verdict_json',r.verdict_json,'verdict_sha256',r.verdict_sha256);
END;$fn$;
REVOKE ALL ON FUNCTION forge_coding_worker_api.read_workflow_feedback(uuid,uuid) FROM PUBLIC;

CREATE FUNCTION forge_agent.coding_workflow_signal() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN PERFORM pg_notify('forge_coding_jobs','');RETURN NEW;END;$fn$;
CREATE TRIGGER coding_reconciliation_workflow_signal AFTER INSERT ON forge_agent.coding_reconciliation_receipts FOR EACH ROW EXECUTE FUNCTION forge_agent.coding_workflow_signal();
CREATE TRIGGER coding_review_workflow_signal AFTER UPDATE ON forge_agent.coding_reviews FOR EACH ROW EXECUTE FUNCTION forge_agent.coding_workflow_signal();
CREATE TRIGGER coding_hosted_workflow_signal AFTER INSERT ON forge_agent.coding_hosted_checks FOR EACH ROW EXECUTE FUNCTION forge_agent.coding_workflow_signal();
REVOKE ALL ON FUNCTION forge_agent.coding_workflow_signal() FROM PUBLIC;

DO $upgrade$ DECLARE name text;definition text;BEGIN
 FOREACH name IN ARRAY ARRAY['forge_coding_reconciler_api.review_claim(text)','forge_coding_reconciler_api.publication_claim(text)'] LOOP
  definition:=pg_get_functiondef(name::regprocedure);
  definition:=replace(definition,'pinned_recipe->>''resource_profile''=''forge-coding-v5''','pinned_recipe->>''resource_profile'' IN(''forge-coding-v5'',''forge-coding-v6'')');
  EXECUTE definition;
 END LOOP;
END;$upgrade$;

CREATE FUNCTION forge_agent.coding_workflow_role_granted(p_workflow uuid,p_profile text,p_mode text) RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
 SELECT EXISTS(SELECT 1 FROM forge_agent.coding_workflows w JOIN forge_agent.coding_worker_grants g ON(g.owner_id,g.project_alias)=(w.owner_id,w.project_alias)
 WHERE w.workflow_id=p_workflow AND w.owner_id=current_setting('forge.coding_owner',true) AND w.enabled AND w.phase NOT IN('completed','cancelled','escalated')
 AND g.agent_profile=p_profile AND g.mode=p_mode AND((p_mode='review' AND p_profile=w.snapshot->>'reviewer_profile') OR(p_mode IN('plan','implement') AND p_profile=w.snapshot->>'author_profile')))
$fn$;
REVOKE ALL ON FUNCTION forge_agent.coding_workflow_role_granted(uuid,text,text) FROM PUBLIC;

CREATE FUNCTION forge_agent.coding_workflow_clear_feedback() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 IF NEW.feedback_id IS DISTINCT FROM OLD.feedback_id AND NEW.workflow_feedback_review_id IS NOT DISTINCT FROM OLD.workflow_feedback_review_id THEN NEW.workflow_feedback_review_id:=NULL;NEW.workflow_feedback_sha256:=NULL;END IF;
 RETURN NEW;
END;$fn$;
CREATE TRIGGER coding_tasks_clear_peer_feedback BEFORE UPDATE ON forge_agent.coding_tasks FOR EACH ROW EXECUTE FUNCTION forge_agent.coding_workflow_clear_feedback();
REVOKE ALL ON FUNCTION forge_agent.coding_workflow_clear_feedback() FROM PUBLIC;

CREATE OR REPLACE FUNCTION forge_coding_worker_api.renew(
    p_attempt_id uuid,
    p_attempt_token uuid
)
RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    v_attempt forge_agent.coding_attempts%ROWTYPE;
    v_task_status text;
    v_now timestamptz;
BEGIN
    v_attempt := forge_coding_worker_api._lock_task_and_attempt(p_attempt_id, p_attempt_token);
    v_now := clock_timestamp();
    IF v_attempt.attempt_id IS NULL THEN
        RETURN false;
    END IF;

    SELECT t.status
    INTO v_task_status
    FROM forge_agent.coding_tasks t
    WHERE t.owner_id = v_attempt.owner_id
      AND t.project_alias = v_attempt.project_alias
      AND t.id = v_attempt.task_id;

    IF(v_attempt.workflow_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM forge_agent.coding_workflows w WHERE w.workflow_id=v_attempt.workflow_id AND w.enabled AND w.phase NOT IN('completed','cancelled','escalated')))
       OR v_task_status NOT IN ('running', 'cancel_requested')
       OR v_attempt.published_at IS NOT NULL
       OR v_attempt.lease_expires_at IS NULL
       OR v_attempt.lease_expires_at <= v_now
       OR v_attempt.deadline_at <= v_now THEN
        RETURN false;
    END IF;

    UPDATE forge_agent.coding_attempts
    SET lease_expires_at = least(deadline_at, v_now + interval '60 seconds'),
        phase = CASE WHEN phase = 'starting' THEN 'running' ELSE phase END,
        updated_at = v_now
    WHERE owner_id = v_attempt.owner_id
      AND project_alias = v_attempt.project_alias
      AND task_id = v_attempt.task_id
      AND attempt_id = v_attempt.attempt_id;

    RETURN true;
END;
$fn$;


CREATE OR REPLACE FUNCTION forge_agent.coding_make_feedback(p_review uuid,p_task uuid,p_disposition text,p_summary text,p_findings jsonb)
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
 IF t.pinned_recipe->>'resource_profile'='forge-coding-v6' THEN payload:=payload||jsonb_build_object('workspace_digest_kind',CASE WHEN q.task_id<>t.id THEN 'candidate_tree' ELSE 'native_receipt' END);END IF;
 raw:=payload::text;digest:=encode(sha256(convert_to(raw,'UTF8')),'hex');
 INSERT INTO forge_agent.coding_feedback(feedback_id,owner_id,project_alias,task_id,parent_attempt_id,review_id,feedback_json,feedback_sha256)
 VALUES(id,t.owner_id,t.project_alias,t.id,q.attempt_id,q.review_id,raw,digest);
 RETURN id;
END;$fn$;


CREATE OR REPLACE FUNCTION forge_coding_worker_api.read_feedback(p_attempt_id uuid,p_attempt_token uuid)
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
 THEN
 RAISE EXCEPTION 'Feedback artifact no longer matches' USING ERRCODE='P0006'; END IF;
 IF f.feedback_json::jsonb->>'workspace_digest_kind'='candidate_tree' THEN
  IF NOT EXISTS(SELECT 1 FROM forge_agent.coding_reviews q JOIN forge_agent.coding_attempts source ON source.attempt_id=f.parent_attempt_id
   WHERE q.review_id=f.review_id AND q.attempt_id=f.parent_attempt_id AND source.task_id<>a.task_id AND a.repair_source_review_id=q.review_id
   AND a.repair_candidate_commit=q.candidate_commit AND EXISTS(SELECT 1 FROM forge_agent.coding_tasks t WHERE(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)=(a.owner_id,a.project_alias,a.task_id,a.attempt_id) AND t.base_revision=q.candidate_commit)
   AND f.feedback_json::jsonb->>'workspace_digest'=q.verification->>'candidate_workspace_digest')
   THEN RAISE EXCEPTION 'Candidate feedback source mismatch' USING ERRCODE='P0006';END IF;
 ELSE
  IF f.feedback_json::jsonb->>'workspace_digest' IS DISTINCT FROM a.resume_workspace_digest THEN RAISE EXCEPTION 'Native feedback receipt mismatch' USING ERRCODE='P0006';END IF;
 END IF;
 RETURN jsonb_build_object('feedback',f.feedback_json::jsonb,'feedback_json',f.feedback_json,'legacy_resume_note',a.legacy_resume_note);
END;$fn$;

