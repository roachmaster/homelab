-- Migration 0036: actor + cause attribution on every SQL-emitted event
-- (event-stream reliability plan section 4.3, fix 2).
--
-- Every function below builds `actor: {kind, id}` (frozen vocabulary, plan
-- 4.1) into its event payload in the same statement that already emits the
-- event; negative kinds also carry `cause` from a frozen per-kind set.
-- Historical events stay as written (no CHECK constraint on existing rows).
--
-- Actor derivations (from data each function already holds; no caller can
-- pass an actor as free text):
--   worker API (claim/checkpoint/publish/record_stop/report_progress):
--     the worker holding the attempt lease (claim: the claiming worker id).
--   reconciler API (expire_leases/publication_finish/review_finish/
--     native_branch_first_replay/native_branch_published/native_merge_failure):
--     the reconciling service (forge-coding-reconciler, or
--     forge-publication-worker for publication_finish).
--   coding_schedule_repair: coordinator:feedback-loop.
--   amend_agent_retry_policy: coordinator:retry-policy-amendment.
--   external _event callers (claim_legacy/progress/finish/release): the
--     authenticated external agent (profile from the scope grant / assignment).
--   _housekeep: by entry point -- reconcile under the service role ->
--     service:forge-external-reconciler; claim under the external role ->
--     agent:<profile>; neither -> RAISE (fail closed). The assigned profile
--     is preserved as SUBJECT metadata, never as the actor.
--
-- Cause boundaries: `cause` is specified only where plan 4.1 defines a
-- frozen vocabulary for the kind (needs_reconcile, cancelled,
-- cancel_requested). Publish outcomes failed/quota_blocked/blocked_auth/
-- blocked_permission and _housekeep failed/blocked_permission events carry
-- actor only; no cause values are invented for them.
--
-- record_stop gains a p_cause parameter (frozen needs_reconcile vocabulary).
-- CREATE OR REPLACE cannot change a signature, so the 4-argument function is
-- dropped and recreated; the EXECUTE grant to forge_coding_worker is restored
-- here and in management/grant-runner.sql.
--
-- _retry_native_patches advances the 0031 pins for coding_schedule_repair
-- and review_finish (the two registered functions whose source changes here):
-- new_sha to the new prosrc hash, old_sha to the reversed form of the new
-- source; see the registry block below.
--
-- report_progress now carries the worker actor in the progress payload, and
-- migration 0012's exact-keys progress check forbids any fifth key, so the
-- constraint is restated below with 'actor' allowed and bound to the worker
-- holding the attempt lease. Historical rows are unaffected: the check only
-- constrains rows written after this migration.

-- _retry_native_patches: advance the 0031 pins for the two
-- registered functions whose source 0036 changes (coding_schedule_repair,
-- review_finish): new_sha to the new prosrc hash, old_sha to the
-- reversed form of the new source (the 0036 actor lines survive the
-- patch reversal). _retry_legacy_source() raises unless the current
-- prosrc matches new_sha and the reversal lands on old_sha, and the
-- verifiers normalize through it; the patch pairs are unchanged.
CREATE OR REPLACE FUNCTION forge_agent._retry_native_patches() RETURNS jsonb LANGUAGE sql IMMUTABLE SET search_path=pg_catalog,pg_temp AS $f$
 SELECT $patches31$[{"name":"forge_agent.coding_schedule_repair(uuid,uuid)","old_sha":"1d45d88a48580d8599f8acdec67207165fab09033709a3a46d3df0ddabf87ea2","new_sha":"5abc3973890a92357f83e72fe01a34ceff1f8487af3d90ce36702eb40c5ed17d","patches":[["root_id uuid;","root_id uuid;event_link forge_agent.coding_retry_review_events%ROWTYPE;event_receipt jsonb;"],[" IF b.repair_rounds>=p.max_rounds OR greatest(b.stagnant_rounds,q.stagnant_rounds)>=p.max_stagnant_rounds THEN\n RAISE EXCEPTION 'Repair budget exhausted' USING ERRCODE='P0009';END IF;"," SELECT * INTO event_link FROM forge_agent.coding_retry_review_events WHERE review_id=q.review_id;\n IF FOUND THEN\n  event_receipt:=forge_agent.retry_event_reserve(event_link.event_id,event_link.reservation_key);\n  IF event_receipt->>'status'='exhausted' THEN RAISE EXCEPTION 'Retry event exhausted' USING ERRCODE='P0009';END IF;\n ELSE\n IF b.repair_rounds>=p.max_rounds OR greatest(b.stagnant_rounds,q.stagnant_rounds)>=p.max_stagnant_rounds THEN\n RAISE EXCEPTION 'Repair budget exhausted' USING ERRCODE='P0009';END IF;\n END IF;"]]},{"name":"forge_coding_reconciler_api.review_finish(uuid,uuid,jsonb)","old_sha":"0adb6f201d6aedcd83bd9a8709010e33128dc7802f593abc1273fe7d9daa3bd2","new_sha":"3e51a093bfcd14fea7162b06f3234ada1c3d9219ff74bb7816507b07d522df36","patches":[["v_candidate text;v_gate text;","v_candidate text;v_gate text;event_receipt jsonb;has_retry boolean;"],[" SELECT * INTO p FROM forge_agent.coding_review_policy WHERE owner_id=t.owner_id AND project_alias=t.project_alias;"," event_receipt:=forge_agent._retry_native_observe(q.review_id,p_outcome->'retry_observation');\n has_retry:=p_outcome->'retry_observation' IS NOT NULL;\n SELECT * INTO p FROM forge_agent.coding_review_policy WHERE owner_id=t.owner_id AND project_alias=t.project_alias;"],[" IF disposition='repair' AND(greatest(q.round,coalesce((SELECT b.repair_rounds FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),0))>=p.max_rounds OR stagnant>=p.max_stagnant_rounds) THEN disposition:='handoff';END IF;"," IF disposition='repair' AND((has_retry AND event_receipt->>'status'='exhausted') OR(NOT has_retry AND(greatest(q.round,coalesce((SELECT b.repair_rounds FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),0))>=p.max_rounds OR stagnant>=p.max_stagnant_rounds))) THEN disposition:='handoff';END IF;"],[" IF disposition='retry_infrastructure' AND q.infrastructure_retries>=p.max_infrastructure_retries THEN disposition:='handoff';END IF;"," IF disposition='retry_infrastructure' AND((has_retry AND event_receipt->>'status'='exhausted') OR(NOT has_retry AND q.infrastructure_retries>=p.max_infrastructure_retries)) THEN disposition:='handoff';END IF;"],[" ELSIF disposition='retry_infrastructure' AND(t.auto_repair OR(q.phase='source' AND q.owner_decision->>'decision'='repair_source')) THEN"," ELSIF disposition='retry_infrastructure' AND(t.auto_repair OR(q.phase='source' AND q.owner_decision->>'decision'='repair_source')) THEN\n  IF has_retry THEN PERFORM forge_agent.retry_event_reserve((event_receipt->>'event_id')::uuid,'infra-review.'||q.review_id::text);END IF;"]]},{"name":"forge_coding_reconciler_api.review_claim(text)","old_sha":"a147c3e2325ba6948f3e28270a457e60abc9e0e59dec5f2880ed8038af260ecd","new_sha":"e601931e658dc87b9df9192edcfeb24a39e3594e725782335a12b920415752e6","patches":[[" RETURN jsonb_build_object("," RETURN forge_agent._retry_native_context(j.task_id)||jsonb_build_object("]]},{"name":"forge_coding_worker_api.read_workflow_feedback(uuid,uuid)","old_sha":"889bef227a31a8db0f3818fcb175838a5295bee7d9ff780496a9aec0333ad9b0","new_sha":"875f39531d50753410e7f3607b185efcceb9318960610cf1ce0753ace9b93dbc","patches":[["(a.repair_source_review_id=r.source_review_id OR(t.parent_task_id IS NULL AND r.source_task_id=t.id))","(a.repair_source_review_id=r.source_review_id OR(t.parent_task_id IS NULL AND r.source_task_id=t.id)\n OR(t.parent_task_id IS NOT NULL AND r.source_task_id=t.id AND EXISTS(\n  SELECT 1 FROM forge_agent.coding_retry_review_events link\n  JOIN forge_agent.coding_retry_event_admissions admitted ON(admitted.event_id,admitted.event_key)=(link.event_id,link.reservation_key)\n  JOIN forge_agent.coding_retry_events event ON event.event_id=link.event_id\n  JOIN forge_agent.coding_reviews source ON source.review_id=link.review_id\n  WHERE link.review_id=r.source_review_id AND link.reservation_key='peer-revision.'||r.workflow_review_id::text\n  AND(event.owner_id,event.project_alias,event.scope_kind,event.scope_id,event.kind)=(a.owner_id,a.project_alias,'native_workflow',a.workflow_id,'review')\n  AND(source.owner_id,source.project_alias,source.task_id,source.attempt_id,source.parent_result_sha256,source.status)=\n   (a.owner_id,a.project_alias,t.id,r.source_attempt_id,r.source_result_sha256,'repair'))))"]]}]$patches31$::jsonb
$f$;

CREATE OR REPLACE FUNCTION forge_coding_worker_api.claim(p_worker_id text, p_incarnation uuid, p_allowed_profiles text[])
 RETURNS jsonb
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
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
          AND (t.pinned_recipe->>'resource_profile') IN ('forge-coding-v1', 'forge-coding-v2', 'forge-coding-v3', 'forge-coding-v4', 'forge-coding-v5', 'forge-coding-v6', 'forge-coding-v7', 'forge-coding-v8', 'forge-coding-v9', 'forge-coding-v10', 'forge-coding-agy-unattended-v1')
          AND (t.pinned_recipe->>'policy_digest') = r.policy_digest
          AND (t.pinned_recipe->>'resource_profile'<>'forge-coding-agy-unattended-v1' OR EXISTS (
              SELECT 1 FROM jsonb_array_elements(r.profiles) AS configured(profile)
              WHERE profile->>'alias'=t.agent_profile
                AND profile->>'backend'='antigravity'
                AND profile->>'model'=t.pinned_recipe->>'model'
                AND profile->'modes' ? t.mode
          ))
          AND (t.pinned_recipe->>'backend'<>'muse' OR EXISTS (
              SELECT 1 FROM jsonb_array_elements(r.profiles) AS configured(profile)
              WHERE profile->>'alias'=t.agent_profile
                AND profile->>'backend'='muse'
                AND profile->>'model'=t.pinned_recipe->>'model'
                AND profile->'modes' ? t.mode
          ))
          AND (t.pinned_recipe->>'backend'<>'grok' OR EXISTS (
              SELECT 1 FROM jsonb_array_elements(r.profiles) AS configured(profile)
              WHERE profile->>'alias'=t.agent_profile
                AND profile->>'backend'='grok'
                AND profile->>'model'=t.pinned_recipe->>'model'
                AND profile->'modes' ? t.mode
          ))
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
                   'revision', v_new_revision,
                   'actor', pg_catalog.jsonb_build_object('kind', 'worker', 'id', p_worker_id)
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
$function$;

CREATE OR REPLACE FUNCTION forge_coding_worker_api.checkpoint(p_attempt_id uuid, p_attempt_token uuid, p_summary text)
 RETURNS void
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE
    v_attempt forge_agent.coding_attempts%ROWTYPE;
    v_task_status text;
    v_now timestamptz;
BEGIN
    IF p_summary IS NULL OR pg_catalog.octet_length(p_summary) > 4096 THEN
        RAISE EXCEPTION 'Invalid checkpoint summary' USING ERRCODE = '22023';
    END IF;

    v_attempt := forge_coding_worker_api._lock_task_and_attempt(p_attempt_id, p_attempt_token);
    v_now := clock_timestamp();
    IF v_attempt.attempt_id IS NULL THEN
        RAISE EXCEPTION 'Invalid attempt token or ownership' USING ERRCODE = '42501';
    END IF;

    SELECT t.status
    INTO v_task_status
    FROM forge_agent.coding_tasks t
    WHERE t.owner_id = v_attempt.owner_id
      AND t.project_alias = v_attempt.project_alias
      AND t.id = v_attempt.task_id;

    IF v_task_status NOT IN ('running', 'cancel_requested')
       OR v_attempt.published_at IS NOT NULL
       OR v_attempt.lease_expires_at IS NULL
       OR v_attempt.lease_expires_at <= v_now
       OR v_attempt.deadline_at <= v_now THEN
        RAISE EXCEPTION 'Attempt lease is not active' USING ERRCODE = 'P0008';
    END IF;

    UPDATE forge_agent.coding_attempts
    SET phase = CASE WHEN phase = 'starting' THEN 'running' ELSE phase END,
        summary = p_summary,
        updated_at = v_now
    WHERE owner_id = v_attempt.owner_id
      AND project_alias = v_attempt.project_alias
      AND task_id = v_attempt.task_id
      AND attempt_id = v_attempt.attempt_id;

    INSERT INTO forge_agent.coding_events (
        owner_id, project_alias, task_id, sequence, event_kind, payload
    )
    SELECT v_attempt.owner_id, v_attempt.project_alias, v_attempt.task_id,
           coalesce(max(e.sequence), 0) + 1,
           'checkpoint',
           pg_catalog.jsonb_build_object(
               'attempt_id', v_attempt.attempt_id,
               'summary', p_summary,
               'actor', pg_catalog.jsonb_build_object('kind', 'worker', 'id', v_attempt.worker_id)
           )
    FROM forge_agent.coding_events e
    WHERE e.owner_id = v_attempt.owner_id
      AND e.project_alias = v_attempt.project_alias
      AND e.task_id = v_attempt.task_id;
END;
$function$;

CREATE OR REPLACE FUNCTION forge_coding_worker_api.publish(p_attempt_id uuid, p_attempt_token uuid, p_outcome text, p_manifest jsonb, p_usage jsonb, p_summary text, p_stop_proof jsonb)
 RETURNS jsonb
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE
    v_attempt forge_agent.coding_attempts%ROWTYPE;
    v_task forge_agent.coding_tasks%ROWTYPE;
    v_now timestamptz;
    v_scope_stopped boolean;
    v_canonical_outcome jsonb;
    v_outcome_digest text;
    v_expected_sha text := NULL;
    v_expected_bytes bigint := NULL;
    v_expected_chunks integer := NULL;
    v_actual_chunks integer;
    v_min_index integer;
    v_max_index integer;
    v_actual_bytes bigint;
    v_full_text text;
    v_actual_sha text;
    v_new_revision bigint;
    v_receipt jsonb;
    v_cause text;
BEGIN
    IF p_outcome IS NULL OR p_outcome NOT IN (
            'awaiting_review', 'failed', 'quota_blocked', 'blocked_auth',
            'blocked_permission', 'cancelled', 'needs_reconcile'
        )
       OR p_summary IS NULL OR pg_catalog.octet_length(p_summary) > 4096 THEN
        RAISE EXCEPTION 'Invalid publish arguments' USING ERRCODE = '22023';
    END IF;

    IF p_manifest IS NOT NULL THEN
        IF pg_catalog.jsonb_typeof(p_manifest) IS DISTINCT FROM 'object'
           OR p_manifest - ARRAY['sha256', 'byte_count', 'chunk_count'] <> '{}'::jsonb
           OR NOT (p_manifest ?& ARRAY['sha256', 'byte_count', 'chunk_count'])
           OR pg_catalog.jsonb_typeof(p_manifest->'sha256') IS DISTINCT FROM 'string'
           OR (p_manifest->>'sha256') !~ '^[0-9a-f]{64}$'
           OR pg_catalog.jsonb_typeof(p_manifest->'byte_count') IS DISTINCT FROM 'number'
           OR (p_manifest->>'byte_count') !~ '^[1-9][0-9]{0,7}$'
           OR pg_catalog.jsonb_typeof(p_manifest->'chunk_count') IS DISTINCT FROM 'number'
           OR (p_manifest->>'chunk_count') !~ '^[1-9][0-9]{0,3}$' THEN
            RAISE EXCEPTION 'Invalid result manifest' USING ERRCODE = '22023';
        END IF;
        v_expected_sha := p_manifest->>'sha256';
        v_expected_bytes := (p_manifest->>'byte_count')::bigint;
        v_expected_chunks := (p_manifest->>'chunk_count')::integer;
        IF v_expected_bytes NOT BETWEEN 1 AND 16777216
           OR v_expected_chunks NOT BETWEEN 1 AND 1024 THEN
            RAISE EXCEPTION 'Result manifest out of bounds' USING ERRCODE = '22023';
        END IF;
    ELSIF p_outcome = 'awaiting_review' THEN
        RAISE EXCEPTION 'Complete success requires a nonempty result manifest' USING ERRCODE = '22023';
    END IF;

    IF p_usage IS NOT NULL AND NOT forge_coding_worker_api._validate_usage_json(p_usage) THEN
        RAISE EXCEPTION 'Invalid normalized attempt usage JSON' USING ERRCODE = '22023';
    END IF;

    v_attempt := forge_coding_worker_api._lock_task_and_attempt(p_attempt_id, p_attempt_token);
    v_now := clock_timestamp();
    IF v_attempt.attempt_id IS NULL THEN
        RAISE EXCEPTION 'Invalid attempt token or ownership' USING ERRCODE = '42501';
    END IF;

    IF NOT forge_coding_worker_api._validate_stop_proof(p_stop_proof, v_attempt.unit_name) THEN
        RAISE EXCEPTION 'Invalid stop proof for attempt' USING ERRCODE = '22023';
    END IF;

    v_scope_stopped := (p_stop_proof->>'scope_stopped')::boolean;
    IF p_outcome <> 'needs_reconcile' AND NOT v_scope_stopped THEN
        RAISE EXCEPTION 'Confirmed stopped scope required for outcome %', p_outcome USING ERRCODE = '22023';
    END IF;

    v_canonical_outcome := pg_catalog.jsonb_build_object(
        'attempt_id', v_attempt.attempt_id,
        'manifest', p_manifest,
        'outcome', p_outcome,
        'scope_stopped', v_scope_stopped,
        'summary', p_summary,
        'usage', p_usage
    );
    v_outcome_digest := pg_catalog.encode(
        pg_catalog.sha256(pg_catalog.convert_to(v_canonical_outcome::text, 'UTF8')),
        'hex'
    );

    -- Exact publish replay MUST precede live lease, expiry, and current task state checks.
    IF v_attempt.published_at IS NOT NULL THEN
        IF v_attempt.outcome_digest = v_outcome_digest THEN
            RETURN v_attempt.publish_receipt;
        END IF;
        RAISE EXCEPTION 'Outcome conflict for already published attempt' USING ERRCODE = 'P0009';
    END IF;

    SELECT t.*
    INTO v_task
    FROM forge_agent.coding_tasks t
    WHERE t.owner_id = v_attempt.owner_id
      AND t.project_alias = v_attempt.project_alias
      AND t.id = v_attempt.task_id;

    IF (v_task.status <> 'running'
        AND NOT (v_task.status = 'cancel_requested' AND p_outcome IN ('cancelled', 'needs_reconcile')))
       OR v_attempt.lease_expires_at IS NULL
       OR v_attempt.lease_expires_at <= v_now
       OR v_attempt.deadline_at <= v_now THEN
        RAISE EXCEPTION 'Attempt lease is not active' USING ERRCODE = 'P0008';
    END IF;

    IF v_task.revision >= 9007199254740991 THEN
        RAISE EXCEPTION 'Task revision limit reached' USING ERRCODE = 'P0009';
    END IF;

    IF p_manifest IS NOT NULL THEN
        SELECT count(*)::integer,
               coalesce(min(c.chunk_index), -1),
               coalesce(max(c.chunk_index), -1),
               coalesce(sum(c.byte_length), 0)::bigint,
               coalesce(pg_catalog.string_agg(c.content, '' ORDER BY c.chunk_index), '')
        INTO v_actual_chunks, v_min_index, v_max_index, v_actual_bytes, v_full_text
        FROM forge_agent.coding_result_chunks c
        WHERE c.owner_id = v_attempt.owner_id
          AND c.project_alias = v_attempt.project_alias
          AND c.task_id = v_attempt.task_id
          AND c.attempt_id = v_attempt.attempt_id;

        v_actual_sha := pg_catalog.encode(
            pg_catalog.sha256(pg_catalog.convert_to(v_full_text, 'UTF8')),
            'hex'
        );

        IF v_actual_chunks <> v_expected_chunks
           OR v_min_index <> 0
           OR v_max_index <> v_expected_chunks - 1
           OR v_actual_bytes <> v_expected_bytes
           OR v_actual_sha <> v_expected_sha THEN
            RAISE EXCEPTION 'Staged result chunks do not match manifest' USING ERRCODE = 'P0007';
        END IF;
    END IF;

    v_now := clock_timestamp();
    IF v_attempt.lease_expires_at <= v_now OR v_attempt.deadline_at <= v_now THEN
        RAISE EXCEPTION 'Attempt lease expired before publication' USING ERRCODE = 'P0008';
    END IF;

    v_new_revision := v_task.revision + 1;
    v_receipt := pg_catalog.jsonb_build_object(
        'task_id', v_task.id,
        'project_alias', v_task.project_alias,
        'attempt_id', v_attempt.attempt_id,
        'outcome', p_outcome,
        'outcome_digest', v_outcome_digest,
        'revision', v_new_revision,
        'status', p_outcome,
        'result', CASE WHEN p_manifest IS NULL THEN NULL ELSE pg_catalog.jsonb_build_object(
            'attempt_id', v_attempt.attempt_id,
            'sha256', v_expected_sha,
            'byte_count', v_expected_bytes,
            'chunk_count', v_expected_chunks
        ) END
    );

    UPDATE forge_agent.coding_attempts
    SET outcome = p_outcome,
        outcome_digest = v_outcome_digest,
        publish_receipt = v_receipt,
        result_sha256 = v_expected_sha,
        result_byte_count = v_expected_bytes,
        result_chunk_count = v_expected_chunks,
        summary = p_summary,
        usage_json = p_usage,
        stop_proof = p_stop_proof,
        phase = CASE WHEN v_scope_stopped THEN 'stopped' ELSE 'unknown' END,
        reservation_active = NOT v_scope_stopped,
        lease_expires_at = NULL,
        finished_at = CASE WHEN v_scope_stopped THEN coalesce(finished_at, v_now) ELSE finished_at END,
        published_at = v_now,
        updated_at = v_now
    WHERE owner_id = v_attempt.owner_id
      AND project_alias = v_attempt.project_alias
      AND task_id = v_attempt.task_id
      AND attempt_id = v_attempt.attempt_id;

    UPDATE forge_agent.coding_tasks
    SET status = p_outcome,
        revision = v_new_revision,
        cancelled_at = CASE WHEN p_outcome = 'cancelled' THEN v_now ELSE NULL END,
        updated_at = v_now
    WHERE owner_id = v_task.owner_id
      AND project_alias = v_task.project_alias
      AND id = v_task.id;

    -- The stop cause for a needs_reconcile outcome is the worker's own stop
    -- reason (scope stopped or interrupted); for a cancelled outcome it is
    -- the cancellation reason (workflow binding or direct operator request).
    v_cause := CASE WHEN p_outcome = 'needs_reconcile' THEN CASE WHEN v_scope_stopped THEN 'scope_stopped' ELSE 'interrupted' END
                    WHEN p_outcome = 'cancelled' THEN CASE WHEN v_task.workflow_required THEN 'workflow_cancelled' ELSE 'operator_request' END
                    ELSE NULL END;

    INSERT INTO forge_agent.coding_events (
        owner_id, project_alias, task_id, sequence, event_kind, payload
    )
    SELECT v_task.owner_id, v_task.project_alias, v_task.id,
           coalesce(max(e.sequence), 0) + 1,
           p_outcome,
           pg_catalog.jsonb_strip_nulls(pg_catalog.jsonb_build_object(
               'attempt_id', v_attempt.attempt_id,
               'revision', v_new_revision,
               'actor', pg_catalog.jsonb_build_object('kind', 'worker', 'id', v_attempt.worker_id),
               'cause', v_cause
           ))
    FROM forge_agent.coding_events e
    WHERE e.owner_id = v_task.owner_id
      AND e.project_alias = v_task.project_alias
      AND e.task_id = v_task.id;

    RETURN v_receipt;
END;
$function$;

-- record_stop: signature change (see header).
DROP FUNCTION IF EXISTS forge_coding_worker_api.record_stop(uuid, uuid, jsonb, text);

CREATE FUNCTION forge_coding_worker_api.record_stop(p_attempt_id uuid, p_attempt_token uuid, p_stop_proof jsonb, p_summary text, p_cause text)
 RETURNS void
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE
    v_attempt forge_agent.coding_attempts%ROWTYPE;
    v_now timestamptz;
    v_scope_stopped boolean;
    v_task forge_agent.coding_tasks%ROWTYPE;
    v_target_status text;
    v_new_revision bigint;
BEGIN
    IF p_summary IS NULL OR pg_catalog.octet_length(p_summary) > 4096 THEN
        RAISE EXCEPTION 'Invalid stop summary' USING ERRCODE = '22023';
    END IF;
    IF p_cause IS NULL OR p_cause NOT IN ('lease_lost', 'deadline_exceeded', 'scope_stopped', 'interrupted') THEN
        RAISE EXCEPTION 'Invalid stop cause' USING ERRCODE = '22023';
    END IF;

    v_attempt := forge_coding_worker_api._lock_task_and_attempt(p_attempt_id, p_attempt_token);
    v_now := clock_timestamp();
    IF v_attempt.attempt_id IS NULL THEN
        RAISE EXCEPTION 'Invalid attempt token or ownership' USING ERRCODE = '42501';
    END IF;

    IF NOT forge_coding_worker_api._validate_stop_proof(p_stop_proof, v_attempt.unit_name) THEN
        RAISE EXCEPTION 'Invalid stop proof for attempt' USING ERRCODE = '22023';
    END IF;

    v_scope_stopped := (p_stop_proof->>'scope_stopped')::boolean;

    IF v_scope_stopped THEN
        UPDATE forge_agent.coding_attempts
        SET phase = 'stopped',
            stop_proof = p_stop_proof,
            reservation_active = false,
            lease_expires_at = NULL,
            finished_at = coalesce(finished_at, v_now),
            summary = CASE WHEN published_at IS NULL THEN p_summary ELSE summary END,
            updated_at = v_now
        WHERE owner_id = v_attempt.owner_id
          AND project_alias = v_attempt.project_alias
          AND task_id = v_attempt.task_id
          AND attempt_id = v_attempt.attempt_id;
        -- A stopped unpublished current writer must leave a recoverable task.
        -- A late stop for a published/superseded attempt never changes task state.
        IF v_attempt.published_at IS NULL THEN
            SELECT t.* INTO v_task FROM forge_agent.coding_tasks t
            WHERE t.owner_id = v_attempt.owner_id
              AND t.project_alias = v_attempt.project_alias
              AND t.id = v_attempt.task_id
              AND t.latest_attempt_id = v_attempt.attempt_id;
            IF FOUND AND v_task.status IN ('running', 'cancel_requested') THEN
                IF v_task.revision >= 9007199254740991 THEN
                    RAISE EXCEPTION 'Task revision limit reached' USING ERRCODE = 'P0009';
                END IF;
                v_target_status := CASE WHEN v_task.status = 'cancel_requested'
                    THEN 'cancelled' ELSE 'needs_reconcile' END;
                v_new_revision := v_task.revision + 1;
                UPDATE forge_agent.coding_tasks
                SET status = v_target_status, revision = v_new_revision,
                    cancelled_at = CASE WHEN v_target_status = 'cancelled' THEN v_now ELSE NULL END,
                    updated_at = v_now
                WHERE owner_id = v_task.owner_id AND project_alias = v_task.project_alias
                  AND id = v_task.id;
                INSERT INTO forge_agent.coding_events (
                    owner_id, project_alias, task_id, sequence, event_kind, payload
                )
                SELECT v_task.owner_id, v_task.project_alias, v_task.id,
                       coalesce(max(e.sequence), 0) + 1, v_target_status,
                       pg_catalog.jsonb_strip_nulls(pg_catalog.jsonb_build_object(
                           'attempt_id', v_attempt.attempt_id,
                           'reason', 'stopped_without_publication', 'revision', v_new_revision,
                           'actor', pg_catalog.jsonb_build_object('kind', 'worker', 'id', v_attempt.worker_id),
                           'cause', CASE WHEN v_target_status = 'needs_reconcile' THEN p_cause
                                        WHEN v_target_status = 'cancelled' THEN CASE WHEN v_task.workflow_required THEN 'workflow_cancelled' ELSE 'operator_request' END
                                        ELSE NULL END
                       ))
                FROM forge_agent.coding_events e
                WHERE e.owner_id = v_task.owner_id AND e.project_alias = v_task.project_alias
                  AND e.task_id = v_task.id;
            END IF;
        END IF;
    ELSIF v_attempt.phase <> 'stopped' THEN
        UPDATE forge_agent.coding_attempts
        SET phase = CASE WHEN phase = 'unknown' THEN 'unknown' ELSE 'stopping' END,
            stop_proof = p_stop_proof,
            summary = CASE WHEN published_at IS NULL THEN p_summary ELSE summary END,
            updated_at = v_now
        WHERE owner_id = v_attempt.owner_id
          AND project_alias = v_attempt.project_alias
          AND task_id = v_attempt.task_id
          AND attempt_id = v_attempt.attempt_id;
    END IF;
END;
$function$;

REVOKE ALL ON FUNCTION forge_coding_worker_api.record_stop(uuid, uuid, jsonb, text, text) FROM PUBLIC;

GRANT EXECUTE ON FUNCTION forge_coding_worker_api.record_stop(uuid, uuid, jsonb, text, text) TO forge_coding_worker;

CREATE OR REPLACE FUNCTION forge_coding_worker_api.report_progress(p_attempt_id uuid, p_attempt_token uuid, p_sequence integer, p_kind text, p_summary text)
 RETURNS void
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE
    v_attempt forge_agent.coding_attempts%ROWTYPE;
    v_task forge_agent.coding_tasks%ROWTYPE;
    v_existing jsonb;
    v_next_sequence integer;
    v_event_sequence bigint;
    v_now timestamptz;
BEGIN
    IF p_sequence IS NULL OR p_sequence NOT BETWEEN 1 AND 128
       OR p_kind IS NULL OR p_kind NOT IN ('plan', 'progress')
       OR p_summary IS NULL OR octet_length(p_summary) NOT BETWEEN 1 AND 4096
       OR btrim(p_summary, E' \t\r\n') = ''
       OR p_summary ~ U&'[\0001-\0008\000B-\001F\007F-\009F]' THEN
        RAISE EXCEPTION 'Invalid coding progress arguments' USING ERRCODE = '22023';
    END IF;

    -- This helper validates session_user, the configured owner and attempt token,
    -- then locks task before attempt. It never grants a new execution lease.
    v_attempt := forge_coding_worker_api._lock_task_and_attempt(p_attempt_id, p_attempt_token);
    IF v_attempt.attempt_id IS NULL THEN
        RAISE EXCEPTION 'Invalid attempt token or ownership' USING ERRCODE = '42501';
    END IF;

    SELECT e.payload INTO v_existing
    FROM forge_agent.coding_events e
    WHERE e.owner_id = v_attempt.owner_id AND e.project_alias = v_attempt.project_alias
      AND e.task_id = v_attempt.task_id AND e.progress_attempt_id = v_attempt.attempt_id
      AND e.progress_sequence = p_sequence;
    IF FOUND THEN
        IF v_existing->>'stage' = p_kind AND v_existing->>'summary' = p_summary THEN
            RETURN; -- An exact lost-ack replay never mutates even terminal state.
        END IF;
        RAISE EXCEPTION 'Progress sequence content conflict' USING ERRCODE = 'P0009';
    END IF;

    SELECT coalesce(max(e.progress_sequence), 0) + 1 INTO v_next_sequence
    FROM forge_agent.coding_events e
    WHERE e.owner_id = v_attempt.owner_id AND e.project_alias = v_attempt.project_alias
      AND e.task_id = v_attempt.task_id AND e.progress_attempt_id = v_attempt.attempt_id;
    IF p_sequence <> v_next_sequence
       OR (p_sequence = 1 AND p_kind <> 'plan')
       OR (p_sequence > 1 AND p_kind <> 'progress') THEN
        RAISE EXCEPTION 'Progress requires plan first and consecutive sequence' USING ERRCODE = 'P0009';
    END IF;

    SELECT t.* INTO v_task FROM forge_agent.coding_tasks t
    WHERE t.owner_id = v_attempt.owner_id AND t.project_alias = v_attempt.project_alias
      AND t.id = v_attempt.task_id;
    -- Capture DB time after every potentially blocking lock and before mutation.
    v_now := clock_timestamp();
    IF v_task.latest_attempt_id IS DISTINCT FROM v_attempt.attempt_id
       OR v_task.status NOT IN ('running', 'cancel_requested')
       OR v_attempt.published_at IS NOT NULL OR v_attempt.lease_expires_at IS NULL
       OR v_attempt.lease_expires_at <= v_now OR v_attempt.deadline_at <= v_now THEN
        RAISE EXCEPTION 'Attempt lease is not active' USING ERRCODE = 'P0008';
    END IF;

    SELECT coalesce(max(e.sequence), 0) + 1 INTO v_event_sequence
    FROM forge_agent.coding_events e
    WHERE e.owner_id = v_attempt.owner_id AND e.project_alias = v_attempt.project_alias
      AND e.task_id = v_attempt.task_id;
    IF v_event_sequence > 9007199254740991 THEN
        RAISE EXCEPTION 'Task event sequence limit reached' USING ERRCODE = 'P0009';
    END IF;

    UPDATE forge_agent.coding_attempts SET summary = p_summary, updated_at = v_now
    WHERE owner_id = v_attempt.owner_id AND project_alias = v_attempt.project_alias
      AND task_id = v_attempt.task_id AND attempt_id = v_attempt.attempt_id;
    INSERT INTO forge_agent.coding_events (
        owner_id, project_alias, task_id, sequence, event_kind, payload,
        progress_attempt_id, progress_sequence, created_at
    ) VALUES (
        v_attempt.owner_id, v_attempt.project_alias, v_attempt.task_id,
        v_event_sequence, 'checkpoint', pg_catalog.jsonb_build_object(
            'attempt_id', v_attempt.attempt_id, 'progress_sequence', p_sequence,
            'stage', p_kind, 'summary', p_summary,
            'actor', pg_catalog.jsonb_build_object('kind', 'worker', 'id', v_attempt.worker_id)
        ), v_attempt.attempt_id, p_sequence, v_now
    );
    PERFORM pg_catalog.pg_notify('forge_coding_jobs', '');
END;
$function$;

CREATE OR REPLACE FUNCTION forge_coding_reconciler_api.expire_leases()
 RETURNS integer
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE
    v_candidate record;
    v_task forge_agent.coding_tasks%ROWTYPE;
    v_attempt forge_agent.coding_attempts%ROWTYPE;
    v_now timestamptz := clock_timestamp();
    v_expired integer := 0;
    v_new_revision bigint;
BEGIN
    FOR v_candidate IN
        SELECT a.owner_id, a.project_alias, a.task_id, a.attempt_id
        FROM forge_agent.coding_attempts a
        WHERE a.owner_id = pg_catalog.current_setting('forge.coding_owner', true)
          AND a.lease_expires_at IS NOT NULL
          AND (a.lease_expires_at <= v_now OR a.deadline_at <= v_now)
        ORDER BY a.task_id ASC, a.attempt_id ASC
    LOOP
        SELECT t.*
        INTO v_task
        FROM forge_agent.coding_tasks t
        WHERE t.owner_id = v_candidate.owner_id
          AND t.project_alias = v_candidate.project_alias
          AND t.id = v_candidate.task_id
        FOR UPDATE SKIP LOCKED;
        IF NOT FOUND THEN
            CONTINUE;
        END IF;

        SELECT a.*
        INTO v_attempt
        FROM forge_agent.coding_attempts a
        WHERE a.owner_id = v_candidate.owner_id
          AND a.project_alias = v_candidate.project_alias
          AND a.task_id = v_candidate.task_id
          AND a.attempt_id = v_candidate.attempt_id
          AND a.lease_expires_at IS NOT NULL
          AND (a.lease_expires_at <= v_now OR a.deadline_at <= v_now)
        FOR UPDATE;
        IF NOT FOUND THEN
            CONTINUE;
        END IF;

        UPDATE forge_agent.coding_attempts
        SET lease_expires_at = NULL,
            phase = 'unknown',
            updated_at = v_now
        WHERE owner_id = v_attempt.owner_id
          AND project_alias = v_attempt.project_alias
          AND task_id = v_attempt.task_id
          AND attempt_id = v_attempt.attempt_id;

        IF v_task.status IN ('running', 'cancel_requested') AND v_task.revision < 9007199254740991 THEN
            v_new_revision := v_task.revision + 1;
            UPDATE forge_agent.coding_tasks
            SET status = 'needs_reconcile',
                revision = v_new_revision,
                updated_at = v_now
            WHERE owner_id = v_task.owner_id
              AND project_alias = v_task.project_alias
              AND id = v_task.id;

            INSERT INTO forge_agent.coding_events (
                owner_id, project_alias, task_id, sequence, event_kind, payload
            )
            SELECT v_task.owner_id, v_task.project_alias, v_task.id,
                   coalesce(max(e.sequence), 0) + 1,
                   'needs_reconcile',
                   pg_catalog.jsonb_build_object(
                       'attempt_id', v_attempt.attempt_id,
                       'reason', CASE WHEN v_attempt.deadline_at <= v_now THEN 'deadline_exceeded' ELSE 'lease_expired' END,
                       'revision', v_new_revision,
                       'actor', jsonb_build_object('kind', 'service', 'id', 'forge-coding-reconciler'),
                       'cause', CASE WHEN v_attempt.deadline_at <= v_now THEN 'deadline_exceeded' ELSE 'lease_lost' END
                   )
            FROM forge_agent.coding_events e
            WHERE e.owner_id = v_task.owner_id
              AND e.project_alias = v_task.project_alias
              AND e.task_id = v_task.id;
        END IF;

        v_expired := v_expired + 1;
    END LOOP;

    RETURN v_expired;
END;
$function$;

CREATE OR REPLACE FUNCTION forge_coding_reconciler_api.publication_finish(p_publication_id uuid, p_lease_token uuid, p_status text, p_branch text, p_commit_sha text, p_pr_url text, p_summary text)
 RETURNS jsonb
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
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
      SELECT j.owner_id,j.project_alias,j.task_id,coalesce(max(sequence),0)+1,'checkpoint',jsonb_build_object('publication_id',j.publication_id,'status',p_status,'actor',jsonb_build_object('kind', 'service', 'id', 'forge-publication-worker'))
      FROM forge_agent.coding_events WHERE owner_id=j.owner_id AND project_alias=j.project_alias AND task_id=j.task_id;
    RETURN receipt;
END; $function$;

CREATE OR REPLACE FUNCTION forge_coding_reconciler_api.review_finish(p_review uuid, p_token uuid, p_outcome jsonb)
 RETURNS jsonb
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE q forge_agent.coding_reviews%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;p forge_agent.coding_review_policy%ROWTYPE;
 previous forge_agent.coding_reviews%ROWTYPE;disposition text;receipt jsonb;id uuid;next_id uuid;stagnant integer;v_candidate text;v_gate text;event_receipt jsonb;has_retry boolean;owner_fenced boolean;
BEGIN
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p_review AND owner_id=current_setting('forge.coding_owner',true);
 IF NOT FOUND THEN RAISE EXCEPTION 'Review not found' USING ERRCODE='P0002';END IF;
 -- Every path acquires task before review. Re-read the review and check the
 -- wall clock only after both locks; a blocked task lock can outlive a lease.
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE owner_id=q.owner_id AND project_alias=q.project_alias AND coding_tasks.id=q.task_id FOR UPDATE;
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p_review AND owner_id=current_setting('forge.coding_owner',true) FOR UPDATE;
 IF NOT FOUND OR q.task_id IS DISTINCT FROM t.id OR q.project_alias IS DISTINCT FROM t.project_alias OR q.owner_id IS DISTINCT FROM t.owner_id THEN RAISE EXCEPTION 'Review source changed while waiting' USING ERRCODE='P0008';END IF;
 IF q.finish_token=p_token AND q.finish_receipt->'outcome'=p_outcome THEN RETURN q.finish_receipt;END IF;
 IF q.status<>'checking' OR q.lease_token IS DISTINCT FROM p_token OR q.lease_expires_at<=clock_timestamp() THEN RAISE EXCEPTION 'Review lease expired' USING ERRCODE='P0008';END IF;
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
 owner_fenced:=forge_agent._coding_owner_continuation_fenced(t.id);
 IF owner_fenced AND disposition IN('repair','retry_infrastructure') THEN disposition:='handoff';END IF;
 event_receipt:=forge_agent._retry_native_observe(q.review_id,p_outcome->'retry_observation');
 has_retry:=p_outcome->'retry_observation' IS NOT NULL;
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
 IF disposition='repair' AND((has_retry AND event_receipt->>'status'='exhausted') OR(NOT has_retry AND(greatest(q.round,coalesce((SELECT b.repair_rounds FROM forge_agent.coding_review_budget b WHERE b.owner_id=t.owner_id AND b.project_alias=t.project_alias AND b.root_task_id=coalesce(t.repair_root_task_id,t.id)),0))>=p.max_rounds OR stagnant>=p.max_stagnant_rounds))) THEN disposition:='handoff';END IF;
 IF disposition='retry_infrastructure' AND((has_retry AND event_receipt->>'status'='exhausted') OR(NOT has_retry AND q.infrastructure_retries>=p.max_infrastructure_retries)) THEN disposition:='handoff';END IF;
 -- An automatic verifier can report local success; publication still waits for
 -- an explicit independent owner/critic decision bound to this exact commit.
 UPDATE forge_agent.coding_reviews SET status=disposition,candidate_commit=v_candidate,gate_manifest_sha256=v_gate,
 findings=(SELECT coalesce(jsonb_agg(v),'[]') FROM(SELECT DISTINCT ON(x->>'finding_id')x AS v FROM jsonb_array_elements(coalesce(previous.findings,'[]')||coalesce(p_outcome->'findings','[]'))WITH ORDINALITY AS entries(x,ordinal) ORDER BY x->>'finding_id',ordinal DESC)ledger),verified_finding_ids=coalesce(p_outcome->'verified_finding_ids','[]'),
 verification=verification||coalesce(p_outcome->'verification','{}')||jsonb_build_object('cleanup_confirmed',coalesce(p_outcome->'cleanup_confirmed',p_outcome->'verification'->'cleanup_confirmed','false'::jsonb)),fingerprint=p_outcome->>'fingerprint',summary=coalesce(p_outcome->>'summary',''),reason=CASE WHEN owner_fenced AND p_outcome->>'disposition' IN('repair','retry_infrastructure') THEN 'owner_continuation_fenced' ELSE coalesce(p_outcome->>'reason','') END,
 stagnant_rounds=stagnant,lease_token=NULL,lease_expires_at=NULL,finish_token=p_token,updated_at=clock_timestamp() WHERE review_id=q.review_id RETURNING * INTO q;
 IF disposition='repair' AND v_candidate IS NOT NULL AND v_gate IS NOT NULL THEN
  id:=forge_agent.coding_make_feedback(q.review_id,t.id,'repair',q.summary,q.findings);
  UPDATE forge_agent.coding_reviews SET feedback_id=id WHERE review_id=q.review_id;
  IF t.auto_repair OR(q.phase='source' AND q.owner_decision->>'decision'='repair_source') THEN next_id:=forge_agent.coding_schedule_repair(q.review_id,id);END IF;
 ELSIF disposition='retry_infrastructure' AND(t.auto_repair OR(q.phase='source' AND q.owner_decision->>'decision'='repair_source')) THEN
  IF has_retry THEN PERFORM forge_agent.retry_event_reserve((event_receipt->>'event_id')::uuid,'infra-review.'||q.review_id::text);END IF;
  INSERT INTO forge_agent.coding_reviews(owner_id,project_alias,task_id,attempt_id,plan_decision_id,reconciliation_receipt_id,base_revision,workspace_digest,parent_result_sha256,
  source_revision,review_no,round,stagnant_rounds,infrastructure_retries,previous_review_id,candidate_commit,phase,owner_decision,verification,target_publication_id,source_publication_id,gate_manifest_sha256)
  VALUES(q.owner_id,q.project_alias,q.task_id,q.attempt_id,q.plan_decision_id,q.reconciliation_receipt_id,q.base_revision,q.workspace_digest,q.parent_result_sha256,
  t.revision,q.review_no+1,q.round,q.stagnant_rounds,q.infrastructure_retries+1,q.review_id,q.candidate_commit,q.phase,q.owner_decision,q.verification,q.target_publication_id,q.source_publication_id,q.gate_manifest_sha256) RETURNING review_id INTO next_id;
 END IF;
 receipt:=jsonb_build_object('review_id',q.review_id,'status',disposition,'feedback_id',id,'repair_task_id',CASE WHEN disposition='repair' THEN next_id ELSE NULL END,'next_review_id',CASE WHEN disposition='retry_infrastructure' THEN next_id ELSE NULL END,'outcome',p_outcome);
 IF owner_fenced AND p_outcome->>'disposition' IN('repair','retry_infrastructure') THEN receipt:=receipt||jsonb_build_object('reason','owner_continuation_fenced');END IF;
 UPDATE forge_agent.coding_reviews SET finish_receipt=receipt WHERE review_id=q.review_id;
 INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
 SELECT t.owner_id,t.project_alias,t.id,coalesce(max(sequence),0)+1,'checkpoint',jsonb_build_object('review_id',q.review_id,'review_status',disposition,'feedback_id',id,'repair_task_id',next_id,'actor',jsonb_build_object('kind', 'service', 'id', 'forge-coding-reconciler'))
 FROM forge_agent.coding_events WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND task_id=t.id;
 PERFORM pg_notify('forge_coding_jobs','');RETURN receipt;
END;$function$;

CREATE OR REPLACE FUNCTION forge_coding_reconciler_api.native_branch_first_replay(p_review uuid, p_revision bigint)
 RETURNS jsonb
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE q forge_agent.coding_reviews%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;w forge_agent.coding_workflows%ROWTYPE;
 a forge_agent.coding_attempts%ROWTYPE;r forge_agent.coding_reconciliation_receipts%ROWTYPE;
 prior forge_agent.coding_workflow_actions%ROWTYPE;next_review uuid;receipt jsonb;
BEGIN
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p_review AND owner_id=current_setting('forge.coding_owner',true);
 IF NOT FOUND THEN RAISE EXCEPTION 'Native replay source denied' USING ERRCODE='42501';END IF;
 PERFORM pg_advisory_xact_lock(hashtextextended(q.owner_id||E'\n'||q.project_alias,2026111901));
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(q.owner_id,q.project_alias,q.task_id) FOR UPDATE;
 SELECT * INTO w FROM forge_agent.coding_workflows WHERE workflow_id=t.workflow_id FOR UPDATE;
 SELECT * INTO prior FROM forge_agent.coding_workflow_actions WHERE workflow_id=w.workflow_id AND kind='decide'
  AND source_key='branch-first-replay-v1:'||p_review::text;
 IF FOUND THEN
  IF prior.intent IS DISTINCT FROM jsonb_build_object('policy','forge-native-branch-first-replay-v1','review_id',p_review,'source_revision',p_revision)
   OR prior.status<>'completed' THEN RAISE EXCEPTION 'Native replay conflict' USING ERRCODE='P0006';END IF;
  RETURN prior.finish_receipt;
 END IF;
 IF forge_agent._coding_owner_continuation_fenced(t.id) THEN
  RAISE EXCEPTION 'Owner continuation cannot admit another automatic replay' USING ERRCODE='P0009';END IF;
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p_review FOR UPDATE;
 SELECT * INTO a FROM forge_agent.coding_attempts WHERE attempt_id=q.attempt_id;
 SELECT * INTO r FROM forge_agent.coding_reconciliation_receipts WHERE receipt_id=q.reconciliation_receipt_id;
 IF(p_revision IS NULL OR p_revision<>t.revision OR q.source_revision<>t.revision OR NOT t.workflow_required
  OR t.workflow_review_id IS NOT NULL OR t.mode<>'implement' OR t.status<>'awaiting_review' OR t.cancelled_at IS NOT NULL
  OR t.latest_attempt_id IS DISTINCT FROM q.attempt_id OR t.plan_decision_id IS DISTINCT FROM q.plan_decision_id
  OR t.scope_plan_decision_id IS DISTINCT FROM q.scope_plan_decision_id
  OR w.enabled OR w.phase<>'escalated' OR w.escalation_reason<>'main_merge_conflict'
  OR q.phase<>'local' OR q.status<>'handoff' OR q.reason<>'main_merge_conflict' OR q.candidate_commit IS NOT NULL
  OR q.verification->'checks_passed'<>'false'::jsonb OR q.verification->'checks'<>'[]'::jsonb
  OR q.verification->'cleanup_confirmed'<>'true'::jsonb OR q.verification->>'failure_stage'<>'prepare'
  OR q.finish_token IS NULL OR q.finish_receipt->>'status'<>'handoff'
  OR q.finish_receipt->'outcome'->>'reason'<>'main_merge_conflict'
  OR q.finish_receipt->'outcome'->'verification'->>'failure_stage'<>'prepare'
  OR q.finish_receipt->'outcome'->'verification'->'checks'<>'[]'::jsonb
  OR q.finish_receipt->'outcome'->'verification'->'cleanup_confirmed'<>'true'::jsonb
  OR q.infrastructure_retries>=(w.snapshot->'repair_policy'->>'max_infrastructure_retries')::integer
  OR t.pinned_recipe IS DISTINCT FROM w.snapshot->'recipes'->'author_implement'->'recipe'
  OR t.recipe_digest IS DISTINCT FROM w.snapshot->'recipes'->'author_implement'->>'digest'
  OR a.phase<>'stopped' OR a.reservation_active OR a.lease_expires_at IS NOT NULL OR a.published_at IS NULL
  OR a.result_sha256 IS DISTINCT FROM q.parent_result_sha256
  OR(r.owner_id,r.project_alias,r.task_id,r.attempt_id,r.workspace_digest,r.task_revision)
    IS DISTINCT FROM(q.owner_id,q.project_alias,q.task_id,q.attempt_id,q.workspace_digest,t.revision)
  OR r.stop_proof->'scope_stopped'<>'true'::jsonb OR r.stop_proof->>'source'<>'systemd'
  OR r.stop_proof->>'unit_name' IS DISTINCT FROM a.unit_name
  OR NOT EXISTS(SELECT FROM forge_agent.coding_worker_grants g WHERE(g.owner_id,g.project_alias,g.agent_profile,g.mode)
   =(w.owner_id,w.project_alias,w.snapshot->>'author_profile','implement'))
  OR NOT EXISTS(SELECT FROM forge_agent.coding_worker_grants g WHERE(g.owner_id,g.project_alias,g.agent_profile,g.mode)
   =(w.owner_id,w.project_alias,w.snapshot->>'reviewer_profile','review'))
  OR q.review_no<>(SELECT max(review_no) FROM forge_agent.coding_reviews WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND task_id=t.id)
  OR EXISTS(SELECT FROM forge_agent.coding_tasks x WHERE x.workflow_id=w.workflow_id AND x.status IN('queued','running','cancel_requested','cancelled','needs_reconcile','failed'))
  OR EXISTS(SELECT FROM forge_agent.coding_attempts x JOIN forge_agent.coding_tasks y ON y.id=x.task_id WHERE y.workflow_id=w.workflow_id AND(x.reservation_active OR x.lease_expires_at IS NOT NULL))
  OR EXISTS(SELECT FROM forge_agent.coding_workflow_actions WHERE workflow_id=w.workflow_id AND status IN('pending','leased'))
  OR EXISTS(SELECT FROM forge_agent.coding_publications x JOIN forge_agent.coding_tasks y ON y.id=x.task_id WHERE y.workflow_id=w.workflow_id)
  OR EXISTS(SELECT FROM forge_agent.coding_reviews x JOIN forge_agent.coding_tasks y ON y.id=x.task_id WHERE y.workflow_id=w.workflow_id AND x.status IN('pending','checking'))
 ) IS DISTINCT FROM false THEN RAISE EXCEPTION 'Native branch-first replay denied' USING ERRCODE='P0009';END IF;
 UPDATE forge_agent.coding_tasks SET revision=revision+1,updated_at=clock_timestamp() WHERE id=t.id RETURNING revision INTO p_revision;
 INSERT INTO forge_agent.coding_reviews(owner_id,project_alias,task_id,attempt_id,plan_decision_id,scope_plan_decision_id,
  reconciliation_receipt_id,base_revision,workspace_digest,parent_result_sha256,source_revision,review_no,round,stagnant_rounds,
  infrastructure_retries,previous_review_id,phase)
 VALUES(q.owner_id,q.project_alias,q.task_id,q.attempt_id,q.plan_decision_id,q.scope_plan_decision_id,q.reconciliation_receipt_id,
  q.base_revision,q.workspace_digest,q.parent_result_sha256,p_revision,q.review_no+1,q.round,q.stagnant_rounds,q.infrastructure_retries+1,q.review_id,'local')
 RETURNING review_id INTO next_review;
 receipt:=jsonb_build_object('policy','forge-native-branch-first-replay-v1','status','pending','task_id',t.id,
  'review_id',next_review,'previous_review_id',q.review_id,'revision',p_revision,'infrastructure_retries',q.infrastructure_retries+1);
 INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt)
 VALUES(w.workflow_id,t.owner_id,t.project_alias,'decide','branch-first-replay-v1:'||q.review_id::text,'completed',
  jsonb_build_object('policy','forge-native-branch-first-replay-v1','review_id',q.review_id,'source_revision',p_revision-1),receipt);
 UPDATE forge_agent.coding_workflows SET enabled=true,phase='checks',escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=w.workflow_id;
 INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
 SELECT t.owner_id,t.project_alias,t.id,coalesce(max(sequence),0)+1,'checkpoint',receipt||jsonb_build_object('actor',jsonb_build_object('kind', 'service', 'id', 'forge-coding-reconciler')) FROM forge_agent.coding_events
 WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND task_id=t.id;
 PERFORM pg_notify('forge_coding_jobs','');RETURN receipt;
END;
$function$;

CREATE OR REPLACE FUNCTION forge_coding_reconciler_api.native_branch_published(p_review uuid, p_token uuid, p_receipt jsonb)
 RETURNS jsonb
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE context jsonb;scope jsonb;v_payload jsonb;event uuid;previous forge_agent.coding_events%ROWTYPE;
BEGIN
 IF(jsonb_typeof(p_receipt)<>'object' OR octet_length(p_receipt::text)>4096
  OR p_receipt-ARRAY['version','scope','candidate_commit','branch_publication_id','branch']<>'{}'::jsonb
  OR NOT p_receipt?&ARRAY['version','scope','candidate_commit','branch_publication_id','branch']
  OR p_receipt->>'version'<>'forge-native-branch-v1'
  OR p_receipt->>'candidate_commit'!~'^[0-9a-f]{40}$'
  OR p_receipt->>'branch_publication_id' IS DISTINCT FROM p_review::text
  OR p_receipt->>'branch' IS DISTINCT FROM 'forge/task-'||p_review::text
  OR jsonb_typeof(p_receipt->'scope')<>'object'
  OR(p_receipt->'scope')-ARRAY['owner_id','project_alias','task_id','attempt_id','review_id','workflow_id','correlation_id']<>'{}'::jsonb
  OR NOT(p_receipt->'scope')?&ARRAY['owner_id','project_alias','task_id','attempt_id','review_id','workflow_id','correlation_id']
  OR p_receipt->'scope'->>'project_alias'!~'^[A-Za-z0-9_.-]{1,64}$'
  OR p_receipt->'scope'->>'task_id'!~'^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
  OR p_receipt->'scope'->>'attempt_id'!~'^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
 ) IS DISTINCT FROM false THEN RAISE EXCEPTION 'Invalid native branch receipt' USING ERRCODE='22023';END IF;
 context:=forge_coding_reconciler_api.native_ci_context(p_review,p_token,p_receipt->'scope'->>'project_alias',
  (p_receipt->'scope'->>'task_id')::uuid,(p_receipt->'scope'->>'attempt_id')::uuid);
 scope:=context-ARRAY['version','candidate_commit','active','state'];
 IF p_receipt->'scope' IS DISTINCT FROM scope THEN
  RAISE EXCEPTION 'Native branch scope changed' USING ERRCODE='P0006';END IF;
 PERFORM 1 FROM forge_agent.coding_tasks WHERE id=(scope->>'task_id')::uuid AND owner_id=scope->>'owner_id' FOR UPDATE;
 event:=md5('forge-native-branch-v1:'||p_review::text)::uuid;
 v_payload:=p_receipt||jsonb_build_object('stage','branch_pushed','review_id',p_review,'actor',jsonb_build_object('kind', 'service', 'id', 'forge-coding-reconciler'));
 SELECT * INTO previous FROM forge_agent.coding_events WHERE event_id=event;
 IF FOUND THEN
  IF(previous.owner_id,previous.project_alias,previous.task_id,previous.payload)
   IS DISTINCT FROM(scope->>'owner_id',scope->>'project_alias',(scope->>'task_id')::uuid,v_payload) THEN
   RAISE EXCEPTION 'Native branch receipt conflict' USING ERRCODE='P0006';END IF;
  RETURN jsonb_build_object('event_id',previous.event_id,'correlation_id',previous.correlation_id);
 END IF;
 INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload,event_id)
 SELECT scope->>'owner_id',scope->>'project_alias',(scope->>'task_id')::uuid,coalesce(max(sequence),0)+1,'checkpoint',v_payload,event
 FROM forge_agent.coding_events WHERE owner_id=scope->>'owner_id' AND project_alias=scope->>'project_alias' AND task_id=(scope->>'task_id')::uuid;
 PERFORM pg_notify('forge_coding_jobs','');
 RETURN jsonb_build_object('event_id',event,'correlation_id',scope->>'correlation_id');
END;
$function$;

CREATE OR REPLACE FUNCTION forge_coding_reconciler_api.native_merge_failure(p_merge uuid, p_token uuid, p_failure jsonb)
 RETURNS jsonb
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE a forge_agent.coding_workflow_actions%ROWTYPE;w forge_agent.coding_workflows%ROWTYPE;
 p forge_agent.coding_publications%ROWTYPE;q forge_agent.coding_reviews%ROWTYPE;
 t forge_agent.coding_tasks%ROWTYPE;seed uuid;feedback uuid;child uuid;receipt jsonb;checks jsonb;owner_fenced boolean;
BEGIN
 SELECT * INTO a FROM forge_agent.coding_workflow_actions WHERE action_id=p_merge
  AND owner_id=current_setting('forge.coding_owner',true) AND kind='merge';
 IF NOT FOUND THEN RAISE EXCEPTION 'Native merge source denied' USING ERRCODE='42501';END IF;
 PERFORM pg_advisory_xact_lock(hashtextextended(a.owner_id||E'\n'||a.project_alias,2026111901));
 SELECT * INTO w FROM forge_agent.coding_workflows WHERE workflow_id=a.workflow_id FOR UPDATE;
 PERFORM 1 FROM forge_agent.coding_tasks WHERE id=w.root_task_id AND owner_id=a.owner_id FOR UPDATE;
 SELECT x.* INTO t FROM forge_agent.coding_tasks x JOIN forge_agent.coding_publications y
  ON(y.owner_id,y.project_alias,y.task_id)=(x.owner_id,x.project_alias,x.id)
  WHERE y.publication_id=a.source_publication_id FOR UPDATE OF x;
 SELECT * INTO a FROM forge_agent.coding_workflow_actions WHERE action_id=p_merge FOR UPDATE;
 IF a.finish_token=p_token AND a.finish_receipt->>'policy'='forge-native-delivery-repair-v1' THEN
  IF a.finish_receipt->'failure' IS DISTINCT FROM p_failure THEN
   RAISE EXCEPTION 'Native merge failure conflict' USING ERRCODE='P0006';END IF;
  RETURN a.finish_receipt;
 END IF;
 IF p_token IS NULL OR a.status<>'leased' OR a.lease_token IS DISTINCT FROM p_token
  OR a.lease_expires_at<=clock_timestamp() OR NOT forge_agent.coding_workflow_merge_authorized(p_merge)
  OR NOT w.enabled OR w.phase IN('completed','cancelled','escalated') THEN
  RAISE EXCEPTION 'Native merge lease denied' USING ERRCODE='P0008';END IF;
 SELECT * INTO p FROM forge_agent.coding_publications WHERE publication_id=a.source_publication_id FOR UPDATE;
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p.review_id FOR UPDATE;
 IF (jsonb_typeof(p_failure)<>'object' OR octet_length(p_failure::text)>32768
  OR p_failure-ARRAY['version','reason','summary','source_review_id','candidate_commit','candidate_workspace_digest',
   'integration_base','gate_manifest_sha256','gate_source_sha256','tested_candidate_commit','evidence_digest','findings','build','no_merge_confirmed','delivery_failure']<>'{}'::jsonb
  OR NOT p_failure?&ARRAY['version','reason','summary','source_review_id','candidate_commit','candidate_workspace_digest',
   'integration_base','gate_manifest_sha256','gate_source_sha256','tested_candidate_commit','evidence_digest','findings','build','no_merge_confirmed','delivery_failure']
  OR p_failure->'version'<>'1'::jsonb OR p_failure->'no_merge_confirmed'<>'true'::jsonb
  OR p_failure->>'reason' NOT IN('merge_main_requires_reverification','merge_source_repair_required')
  OR jsonb_typeof(p_failure->'summary')<>'string' OR octet_length(p_failure->>'summary') NOT BETWEEN 1 AND 2048
  OR p_failure->>'source_review_id'!~'^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
  OR p_failure->>'candidate_commit'!~'^[0-9a-f]{40}$' OR p_failure->>'integration_base'!~'^[0-9a-f]{40}$'
  OR p_failure->>'tested_candidate_commit' IS DISTINCT FROM q.candidate_commit
  OR p_failure->>'candidate_commit'=q.candidate_commit
  OR EXISTS(SELECT FROM jsonb_each(p_failure) x WHERE x.key IN('candidate_workspace_digest','gate_manifest_sha256','gate_source_sha256','evidence_digest')
    AND(jsonb_typeof(x.value)<>'string' OR x.value#>>'{}'!~'^[0-9a-f]{64}$'))
  OR jsonb_typeof(p_failure->'findings')<>'array' OR jsonb_array_length(p_failure->'findings') NOT BETWEEN 1 AND 3
  OR jsonb_typeof(p_failure->'build')<>'object'
  OR (p_failure->'build')-ARRAY['provider','job','action_id','queue_id','build_number']<>'{}'::jsonb
  OR NOT(p_failure->'build')?&ARRAY['provider','job','action_id','queue_id','build_number']
  OR p_failure->'build'->>'provider'<>'jenkins' OR p_failure->'build'->>'job'<>'forge-native-candidate'
  OR p_failure->'build'->>'action_id'!~'^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
  OR jsonb_typeof(p_failure->'build'->'build_number')<>'number'
  OR p_failure->'build'->>'build_number'!~'^[1-9][0-9]{0,12}$'
  OR(jsonb_typeof(p_failure->'build'->'queue_id')<>'null' AND
   (jsonb_typeof(p_failure->'build'->'queue_id')<>'number' OR p_failure->'build'->>'queue_id'!~'^[1-9][0-9]{0,12}$'))
  OR jsonb_typeof(p_failure->'delivery_failure')<>'object'
  OR(p_failure->'delivery_failure')-ARRAY['version','tested_candidate','current_main','build','conflict_paths']<>'{}'::jsonb
  OR NOT(p_failure->'delivery_failure')?&ARRAY['version','tested_candidate','current_main','build','conflict_paths']
  OR p_failure->'delivery_failure'->'version'<>'1'::jsonb
  OR p_failure->'delivery_failure'->>'tested_candidate' IS DISTINCT FROM q.candidate_commit
  OR p_failure->'delivery_failure'->>'current_main' IS DISTINCT FROM p_failure->>'integration_base'
  OR p_failure->'delivery_failure'->'build' IS DISTINCT FROM p_failure->'build'
  OR jsonb_typeof(p_failure->'delivery_failure'->'conflict_paths')<>'array'
  OR jsonb_array_length(p_failure->'delivery_failure'->'conflict_paths')>32
 ) IS DISTINCT FROM false THEN RAISE EXCEPTION 'Invalid native merge failure' USING ERRCODE='22023';END IF;
 IF EXISTS(SELECT FROM jsonb_array_elements(p_failure->'delivery_failure'->'conflict_paths') x
  WHERE(jsonb_typeof(x)<>'string' OR octet_length(x#>>'{}') NOT BETWEEN 1 AND 1024
   OR x#>>'{}'~'(^/|(^|/)\.\.?(/|$)|[[:cntrl:]])') IS DISTINCT FROM false) THEN
  RAISE EXCEPTION 'Invalid native conflict path' USING ERRCODE='22023';END IF;
 IF EXISTS(SELECT FROM jsonb_array_elements(p_failure->'findings') f WHERE(jsonb_typeof(f)<>'object'
  OR NOT f?&ARRAY['finding_id','category','summary']
  OR f-ARRAY['finding_id','category','summary','check_id','path','diagnostic_code','reproduction','expected','actual','evidence_digest','status']<>'{}'::jsonb
  OR f->>'finding_id'!~'^[A-Za-z0-9_.-]{1,64}$' OR f->>'category' NOT IN('source','infrastructure')
  OR jsonb_typeof(f->'summary')<>'string' OR octet_length(f->>'summary') NOT BETWEEN 1 AND 2048
  OR coalesce(f->>'status','open')<>'open'
  OR EXISTS(SELECT FROM jsonb_each(f) x WHERE x.key IN('check_id','path','diagnostic_code','reproduction','expected','actual','evidence_digest','status')
   AND(jsonb_typeof(x.value)<>'string' OR octet_length(x.value#>>'{}')>CASE WHEN x.key IN('check_id','diagnostic_code') THEN 128 WHEN x.key='path' THEN 1024 ELSE 2048 END))
  OR(f?'evidence_digest' AND f->>'evidence_digest'<>'' AND f->>'evidence_digest'!~'^[0-9a-f]{64}$')
 ) IS DISTINCT FROM false) OR EXISTS(SELECT FROM jsonb_array_elements(p_failure->'findings') f GROUP BY f->>'finding_id' HAVING count(*)>1) THEN
  RAISE EXCEPTION 'Invalid native repair finding' USING ERRCODE='22023';END IF;
 SELECT h.checks INTO checks FROM forge_agent.coding_hosted_checks h WHERE h.publication_id=p.publication_id
  AND h.candidate_commit=q.candidate_commit AND h.required_green ORDER BY h.observed_at DESC,h.observation_id DESC LIMIT 1;
 IF jsonb_array_length(checks) IS DISTINCT FROM 3 OR EXISTS(SELECT FROM jsonb_array_elements(checks) c
  WHERE c->>'provider'<>'jenkins' OR c->>'job' IS DISTINCT FROM p_failure->'build'->>'job'
   OR c->>'head_sha' IS DISTINCT FROM q.candidate_commit
   OR c->>'status'<>'success' OR c->>'action_id' IS DISTINCT FROM p_failure->'build'->>'action_id'
   OR c->'queue_id' IS DISTINCT FROM p_failure->'build'->'queue_id'
   OR c->'build_number' IS DISTINCT FROM p_failure->'build'->'build_number') THEN
  RAISE EXCEPTION 'Native repair build evidence changed' USING ERRCODE='P0006';END IF;
 IF t.latest_attempt_id IS DISTINCT FROM q.attempt_id OR t.status<>'awaiting_review'
  OR q.repair_task_id IS NOT NULL OR q.verification->'cleanup_confirmed' IS DISTINCT FROM 'true'::jsonb
  OR EXISTS(SELECT FROM forge_agent.coding_tasks x WHERE x.workflow_id=w.workflow_id AND x.status IN('queued','running','cancel_requested')) THEN
  RAISE EXCEPTION 'Native repair source superseded' USING ERRCODE='P0009';END IF;
 owner_fenced:=forge_agent._coding_owner_continuation_fenced(t.id);
 seed:=(p_failure->>'source_review_id')::uuid;
 INSERT INTO forge_agent.coding_reviews(review_id,owner_id,project_alias,task_id,attempt_id,plan_decision_id,scope_plan_decision_id,
  reconciliation_receipt_id,base_revision,workspace_digest,parent_result_sha256,source_revision,review_no,round,stagnant_rounds,
  infrastructure_retries,previous_review_id,status,candidate_commit,gate_manifest_sha256,findings,verification,summary,reason,phase,owner_decision,
  target_publication_id,source_publication_id)
 VALUES(seed,q.owner_id,q.project_alias,q.task_id,q.attempt_id,q.plan_decision_id,q.scope_plan_decision_id,q.reconciliation_receipt_id,
  p_failure->>'integration_base',q.workspace_digest,q.parent_result_sha256,t.revision,
  (SELECT max(review_no)+1 FROM forge_agent.coding_reviews WHERE owner_id=q.owner_id AND project_alias=q.project_alias AND task_id=q.task_id),
  q.round,q.stagnant_rounds,q.infrastructure_retries,q.review_id,CASE WHEN owner_fenced THEN 'handoff' ELSE 'repair' END,p_failure->>'candidate_commit',p_failure->>'gate_manifest_sha256',
  p_failure->'findings',jsonb_build_object('source_import',true,'checks_passed',false,'cleanup_confirmed',true,'checks','[]'::jsonb,
   'candidate_workspace_digest',p_failure->>'candidate_workspace_digest','repair_requires_new_workspace',true,
   'integration_base',p_failure->>'integration_base','gate_source_sha256',p_failure->>'gate_source_sha256','delivery_failure',p_failure->'delivery_failure'),
  p_failure->>'summary',CASE WHEN owner_fenced THEN 'owner_continuation_fenced' ELSE p_failure->>'reason' END,'source',jsonb_build_object('decision','repair_source','closed_by','native_merge_failure',
   'merge_id',a.action_id,'tested_review_id',q.review_id),NULL,p.publication_id);
 feedback:=forge_agent.coding_make_feedback(seed,t.id,'repair',p_failure->>'summary',p_failure->'findings');
 UPDATE forge_agent.coding_reviews SET feedback_id=feedback WHERE review_id=seed;
 IF NOT owner_fenced THEN child:=forge_agent.coding_schedule_repair(seed,feedback);END IF;
 -- Supersede delivery, while retaining the old review's approval and receipt.
 UPDATE forge_agent.coding_reviews SET repair_task_id=child WHERE review_id=q.review_id;
 receipt:=jsonb_build_object('policy','forge-native-delivery-repair-v1','status',CASE WHEN owner_fenced THEN 'handoff' ELSE 'repair' END,'merge_id',a.action_id,
  'workflow_id',w.workflow_id,'source_review_id',seed,'feedback_id',feedback,'repair_task_id',child,'failure',p_failure);
 IF owner_fenced THEN receipt:=receipt||jsonb_build_object('reason','owner_continuation_fenced');END IF;
 UPDATE forge_agent.coding_workflow_actions SET status='handoff',finish_token=p_token,finish_receipt=receipt,
  lease_token=NULL,lease_expires_at=NULL,updated_at=clock_timestamp() WHERE action_id=a.action_id;
 IF owner_fenced THEN
  UPDATE forge_agent.coding_workflows SET enabled=false,phase='escalated',escalation_reason='Owner continuation failed delivery; no further automatic writer is authorized.',updated_at=clock_timestamp() WHERE workflow_id=w.workflow_id;
 ELSE
  UPDATE forge_agent.coding_workflows SET phase='repair',updated_at=clock_timestamp() WHERE workflow_id=w.workflow_id;
 END IF;
 INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
 SELECT t.owner_id,t.project_alias,t.id,coalesce(max(sequence),0)+1,'checkpoint',receipt||jsonb_build_object('actor',jsonb_build_object('kind', 'service', 'id', 'forge-coding-reconciler'))
 FROM forge_agent.coding_events WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND task_id=t.id;
 PERFORM pg_notify('forge_coding_jobs','');RETURN receipt;
END;
$function$;

CREATE OR REPLACE FUNCTION forge_agent.coding_schedule_repair(p_review uuid, p_feedback uuid)
 RETURNS uuid
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE q forge_agent.coding_reviews%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;p forge_agent.coding_review_policy%ROWTYPE;
 b forge_agent.coding_review_budget%ROWTYPE;child uuid;fid uuid;f forge_agent.coding_feedback%ROWTYPE;new_revision bigint;root_id uuid;event_link forge_agent.coding_retry_review_events%ROWTYPE;event_receipt jsonb;
BEGIN
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p_review;
 IF q.owner_id IS DISTINCT FROM current_setting('forge.coding_owner',true) THEN RAISE EXCEPTION 'Repair scope mismatch' USING ERRCODE='42501';END IF;
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE owner_id=q.owner_id AND project_alias=q.project_alias AND coding_tasks.id=q.task_id FOR UPDATE;
 SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p_review FOR UPDATE;
 IF t.latest_attempt_id<>q.attempt_id OR t.plan_decision_id IS DISTINCT FROM q.plan_decision_id OR t.scope_plan_decision_id IS DISTINCT FROM q.scope_plan_decision_id OR t.status NOT IN('awaiting_review','completed') OR q.candidate_commit IS NULL
 OR EXISTS(SELECT 1 FROM forge_agent.coding_attempts a WHERE a.owner_id=t.owner_id AND a.project_alias=t.project_alias AND a.task_id=t.id AND(a.reservation_active OR a.lease_expires_at IS NOT NULL)) THEN
 RAISE EXCEPTION 'Repair requires stopped current source' USING ERRCODE='P0009';END IF;
 SELECT * INTO p FROM forge_agent.coding_review_policy WHERE owner_id=t.owner_id AND project_alias=t.project_alias;
 root_id:=coalesce(t.repair_root_task_id,t.id);
 IF forge_agent._coding_owner_continuation_fenced(t.id) THEN
  RAISE EXCEPTION 'Explicit owner continuation forbids another automatic writer' USING ERRCODE='P0009';END IF;
 INSERT INTO forge_agent.coding_review_budget(owner_id,project_alias,root_task_id) VALUES(t.owner_id,t.project_alias,root_id) ON CONFLICT DO NOTHING;
 SELECT * INTO b FROM forge_agent.coding_review_budget WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND root_task_id=root_id FOR UPDATE;
 p.max_rounds:=(b.policy->>'max_rounds')::integer;p.max_stagnant_rounds:=(b.policy->>'max_stagnant_rounds')::integer;p.max_infrastructure_retries:=(b.policy->>'max_infrastructure_retries')::integer;
 IF EXISTS(SELECT 1 FROM forge_agent.coding_tasks x WHERE x.owner_id=t.owner_id AND x.project_alias=t.project_alias AND coalesce(x.repair_root_task_id,x.id)=root_id AND x.status IN('queued','running','cancel_requested')) THEN
 RAISE EXCEPTION 'Repair scope already has a writer' USING ERRCODE='P0009';END IF;
 SELECT * INTO event_link FROM forge_agent.coding_retry_review_events WHERE review_id=q.review_id;
 IF FOUND THEN
  event_receipt:=forge_agent.retry_event_reserve(event_link.event_id,event_link.reservation_key);
  IF event_receipt->>'status'='exhausted' THEN RAISE EXCEPTION 'Retry event exhausted' USING ERRCODE='P0009';END IF;
 ELSE
 IF b.repair_rounds>=p.max_rounds OR greatest(b.stagnant_rounds,q.stagnant_rounds)>=p.max_stagnant_rounds THEN
 RAISE EXCEPTION 'Repair budget exhausted' USING ERRCODE='P0009';END IF;
 END IF;
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
  VALUES(t.owner_id,t.project_alias,child,1,'queued',jsonb_build_object('task_id',child,'project_alias',t.project_alias,'revision',1,'status','queued','recipe_digest',t.recipe_digest,'parent_task_id',t.id,'actor',jsonb_build_object('kind', 'coordinator', 'id', 'feedback-loop')));
  UPDATE forge_agent.coding_reviews SET repair_task_id=child WHERE review_id=q.review_id;
 ELSE
  child:=t.id;
  UPDATE forge_agent.coding_tasks SET status='queued',revision=revision+1,feedback_id=f.feedback_id,feedback_sha256=f.feedback_sha256,
   resume_workspace_digest=q.workspace_digest,legacy_resume_note='',updated_at=clock_timestamp() WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND coding_tasks.id=t.id;
  INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
  SELECT t.owner_id,t.project_alias,t.id,coalesce(max(sequence),0)+1,'resumed',jsonb_build_object('feedback_id',f.feedback_id,'review_id',q.review_id,'round',b.repair_rounds+1,'actor',jsonb_build_object('kind', 'coordinator', 'id', 'feedback-loop'))
  FROM forge_agent.coding_events WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND task_id=t.id;
 END IF;
 UPDATE forge_agent.coding_review_budget SET repair_rounds=repair_rounds+1,stagnant_rounds=q.stagnant_rounds WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND root_task_id=root_id;
 PERFORM pg_notify('forge_coding_jobs','');RETURN child;
END;$function$;

CREATE OR REPLACE FUNCTION forge_agent.amend_agent_retry_policy(p_project text, p_root uuid, p_policy jsonb)
 RETURNS jsonb
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE t forge_agent.coding_tasks%ROWTYPE;w forge_agent.coding_workflows%ROWTYPE;
 b forge_agent.coding_review_budget%ROWTYPE;previous jsonb;packet jsonb;key text;
BEGIN
 IF session_user<>'forge_agent' OR current_setting('forge.coding_owner',true) IS NULL THEN
  RAISE EXCEPTION 'Retry allowance requires the scoped owner service' USING ERRCODE='42501';END IF;
 PERFORM pg_advisory_xact_lock(hashtextextended(current_setting('forge.coding_owner',true)||E'\n'||p_project,2026111901));
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE owner_id=current_setting('forge.coding_owner',true)
 AND project_alias=p_project AND id=p_root FOR UPDATE;
 IF NOT FOUND THEN RAISE EXCEPTION 'Retry source is not active' USING ERRCODE='P0009';END IF;
 IF t.workflow_id IS NOT NULL THEN
  SELECT * INTO w FROM forge_agent.coding_workflows WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND workflow_id=t.workflow_id;
  IF NOT FOUND OR w.phase IN('completed','cancelled') OR w.root_task_id<>t.id
  OR NOT EXISTS(SELECT FROM forge_agent.coding_worker_grants g WHERE(g.owner_id,g.project_alias,g.agent_profile,g.mode)=(w.owner_id,w.project_alias,w.snapshot->>'author_profile','implement'))
  OR NOT EXISTS(SELECT FROM forge_agent.coding_worker_grants g WHERE(g.owner_id,g.project_alias,g.agent_profile,g.mode)=(w.owner_id,w.project_alias,w.snapshot->>'reviewer_profile','review'))
  THEN RAISE EXCEPTION 'Retry workflow scope is no longer delegated' USING ERRCODE='P0009';END IF;
 ELSIF NOT EXISTS(SELECT FROM forge_agent.coding_worker_grants g WHERE(g.owner_id,g.project_alias,g.agent_profile,g.mode)=(t.owner_id,t.project_alias,t.agent_profile,t.mode))
 THEN RAISE EXCEPTION 'Retry source grant is no longer delegated' USING ERRCODE='P0009';
 END IF;
 IF t.status IN('completed','cancelled') AND w.workflow_id IS NULL
 AND NOT EXISTS(SELECT FROM forge_agent.coding_tasks child WHERE(child.owner_id,child.project_alias,child.repair_root_task_id)=(t.owner_id,t.project_alias,t.id)
                AND child.status NOT IN('completed','cancelled'))
 THEN RAISE EXCEPTION 'Retry source has no active lineage' USING ERRCODE='P0009';END IF;
 SELECT * INTO b FROM forge_agent.coding_review_budget WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND root_task_id=t.id FOR UPDATE;
 IF NOT FOUND OR jsonb_typeof(p_policy)<>'object' OR p_policy->>'version'<>'forge-repair-v1'
 OR (SELECT count(*) FROM jsonb_object_keys(p_policy))<>5
 OR p_policy->'max_rounds'<>'20'::jsonb OR p_policy->'max_stagnant_rounds'<>'20'::jsonb
 OR p_policy->'max_infrastructure_retries'<>'20'::jsonb
 OR (p_policy-ARRAY['max_rounds','max_stagnant_rounds','max_infrastructure_retries']) IS DISTINCT FROM
    (b.policy-ARRAY['max_rounds','max_stagnant_rounds','max_infrastructure_retries'])
 OR EXISTS(SELECT FROM unnest(ARRAY['max_rounds','max_stagnant_rounds','max_infrastructure_retries']) k
           WHERE jsonb_typeof(b.policy->k)<>'number' OR (b.policy->>k)::integer NOT BETWEEN 0 AND 20)
 THEN RAISE EXCEPTION 'Retry allowance can only widen numeric limits' USING ERRCODE='P0009';END IF;
 IF b.policy=p_policy AND(w.workflow_id IS NULL OR(w.snapshot->>'max_plan_revisions')::integer>=20 AND(w.snapshot->>'max_code_revisions')::integer>=20) THEN RETURN jsonb_build_object('policy','forge-agent-retry-v1','status','unchanged');END IF;
 key:=coalesce(w.workflow_id,t.id)::text;
 SELECT payload INTO previous FROM forge_agent.coding_events WHERE owner_id=t.owner_id AND project_alias=t.project_alias
 AND task_id=t.id AND event_kind='checkpoint' AND payload->>'stage'='retry_policy_updated'
 AND payload->>'policy'='forge-agent-retry-v1' AND payload->>'scope_id'=key ORDER BY sequence DESC LIMIT 1;
 IF FOUND THEN
  IF previous->'new_policy' IS DISTINCT FROM p_policy OR b.policy IS DISTINCT FROM p_policy
  THEN RAISE EXCEPTION 'Recorded retry allowance no longer matches' USING ERRCODE='P0006';END IF;
  RETURN previous;
 END IF;
 UPDATE forge_agent.coding_review_budget SET policy=p_policy WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND root_task_id=t.id;
 packet:=jsonb_build_object('stage','retry_policy_updated','policy','forge-agent-retry-v1','scope_id',key,
  'workflow_id',w.workflow_id,'snapshot_digest',w.policy_digest,'old_policy',b.policy,'new_policy',p_policy,
  'max_plan_revisions',20,'max_code_revisions',20,'repair_rounds',b.repair_rounds,'stagnant_rounds',b.stagnant_rounds,
  'actor',jsonb_build_object('kind','coordinator','id','retry-policy-amendment'),
  'summary','The shared agent retry allowance is now twenty; existing counters and evidence are preserved.');
 INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
 SELECT t.owner_id,t.project_alias,t.id,coalesce(max(sequence),0)+1,'checkpoint',packet FROM forge_agent.coding_events
 WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND task_id=t.id;
 PERFORM pg_notify('forge_coding_jobs','');
 RETURN packet;
END;$function$;

CREATE OR REPLACE FUNCTION forge_external_agent._housekeep(p_g forge_external_agent.scope_grants, p_budget integer)
 RETURNS integer
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE a forge_external_agent.assignments%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;s text;ev text;n integer:=0;
 actor jsonb;entry_point text;cancel_cause text;originator jsonb;cancel_receipt jsonb;
BEGIN
 -- The actor is derived from the authenticated entry-point context (plan 4.3,
 -- round 10): the reconciler service calls _housekeep under its service role,
 -- and an agent claim calls it under its external role. Any other caller fails
 -- closed instead of emitting an event with an unidentifiable initiator.
 IF session_user=p_g.service_role THEN actor:=jsonb_build_object('kind','service','id','forge-external-reconciler');entry_point:='reconcile';
 ELSIF session_user=p_g.external_role THEN actor:=jsonb_build_object('kind','agent','id',p_g.profile_alias);entry_point:='claim';
 ELSE RAISE EXCEPTION 'External housekeeping has no attributable entry point' USING ERRCODE='42501';END IF;
 FOR a IN SELECT x.* FROM forge_external_agent.assignments x JOIN forge_agent.coding_tasks q ON(q.owner_id,q.project_alias,q.id)=(x.owner_id,x.project_alias,x.task_id)
 WHERE(x.external_role,x.owner_id,x.project_alias,x.profile_alias)=(p_g.external_role,p_g.owner_id,p_g.project_alias,p_g.profile_alias)
 AND x.status IN('queued','leased') AND q.latest_attempt_id IS NULL AND q.status IN('queued','running','cancel_requested','cancelled')
 AND(NOT p_g.enabled OR q.status IN('cancelled','cancel_requested') OR x.deadline_at<=clock_timestamp() OR(x.status='leased' AND x.lease_until<=clock_timestamp())) ORDER BY x.created_at,x.assignment_id FOR UPDATE OF q,x SKIP LOCKED LIMIT least(64,greatest(0,p_budget)) LOOP
 n:=n+1;
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id);
 s:=CASE WHEN t.status IN('cancelled','cancel_requested') THEN 'cancelled' WHEN NOT p_g.enabled THEN 'blocked_permission' WHEN a.deadline_at<=clock_timestamp() OR a.generation>=3 OR forge_external_agent._intent(t) IS DISTINCT FROM a.intent THEN 'failed' ELSE 'queued' END;
 UPDATE forge_agent.coding_tasks SET status=s,cancelled_at=CASE WHEN s='cancelled' THEN coalesce(cancelled_at,clock_timestamp()) ELSE NULL END,revision=revision+1,updated_at=clock_timestamp() WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id) RETURNING revision INTO a.task_revision;
 UPDATE forge_external_agent.assignments SET status=CASE WHEN s='blocked_permission' THEN 'failed' ELSE s END,task_revision=a.task_revision,lease_token=NULL,lease_until=NULL,updated_at=clock_timestamp() WHERE assignment_id=a.assignment_id;
 ev:=CASE WHEN s='queued' THEN 'resumed' ELSE s END;
 cancel_cause:=NULL;originator:=NULL;
 IF ev='cancelled' THEN
  -- Cancellation propagation: the cause comes from the cancellation's control
  -- receipt where one names it, else from the workflow binding; the
  -- originating actor is preserved where the receipt carries one. A
  -- cancellation the system propagates is never recorded as the agent's act.
  SELECT receipt_json INTO cancel_receipt FROM forge_agent.coding_control_receipts WHERE owner_id=a.owner_id AND project_alias=a.project_alias AND task_id=a.task_id
   AND(action='cancel' OR(action IN('plan_decide','review_decide') AND receipt_json->>'status'='cancelled')) ORDER BY created_at DESC LIMIT 1;
  IF cancel_receipt IS NOT NULL THEN originator:=cancel_receipt->'actor';cancel_cause:=cancel_receipt->>'cause';END IF;
  IF cancel_cause IS NULL THEN cancel_cause:=CASE WHEN t.workflow_id IS NOT NULL THEN 'workflow_cancelled' ELSE 'operator_request' END;END IF;
 END IF;
 PERFORM forge_external_agent._event(a,ev,jsonb_strip_nulls(jsonb_build_object('provider','muse-db','assignment_id',a.assignment_id,'reason','external_lease_reconciled','actor',actor,'entry_point',entry_point,'subject',jsonb_build_object('profile',a.profile_alias,'assignment_id',a.assignment_id,'external_role',a.external_role),'cause',cancel_cause,'originator',originator)));
 END LOOP;
 IF n>0 THEN PERFORM pg_notify('forge_external_agent_jobs','');END IF;RETURN n;
END;
$function$;

CREATE OR REPLACE FUNCTION forge_external_workflow.claim_legacy(p_project text, p_profile text)
 RETURNS jsonb
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE g forge_external_agent.scope_grants%ROWTYPE;a forge_external_agent.assignments%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;
BEGIN
 g:=forge_external_agent._scope(p_project,p_profile);
 IF NOT EXISTS(SELECT FROM forge_external_agent.readiness WHERE(external_role,owner_id,project_alias,profile_alias)=(g.external_role,g.owner_id,p_project,p_profile) AND observed_at>clock_timestamp()-interval '90 seconds') THEN RAISE EXCEPTION 'External runner not advertised' USING ERRCODE='P0009';END IF;
 PERFORM forge_external_agent._housekeep(g,64);
 SELECT x.* INTO a FROM forge_external_agent.assignments x JOIN forge_agent.coding_tasks q ON(q.owner_id,q.project_alias,q.id)=(x.owner_id,x.project_alias,x.task_id)
 WHERE(x.external_role,x.owner_id,x.project_alias,x.profile_alias)=(g.external_role,g.owner_id,p_project,p_profile)
 AND x.status IN('queued','leased') AND(x.status='queued' OR x.lease_until<=clock_timestamp()) AND x.generation<3 AND x.deadline_at>clock_timestamp()
 AND q.status IN('queued','running') AND q.latest_attempt_id IS NULL
 ORDER BY x.created_at,x.assignment_id FOR UPDATE OF q,x SKIP LOCKED LIMIT 1;
 IF NOT FOUND THEN RETURN NULL;END IF;
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id);
 IF forge_external_agent._intent(t) IS DISTINCT FROM a.intent OR t.revision<>a.task_revision THEN RAISE EXCEPTION 'External task binding changed' USING ERRCODE='P0009';END IF;
 UPDATE forge_agent.coding_tasks SET status='running',revision=revision+1,updated_at=clock_timestamp() WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id) RETURNING revision INTO a.task_revision;
 UPDATE forge_external_agent.assignments SET status='leased',task_revision=a.task_revision,generation=generation+1,lease_token=gen_random_uuid(),lease_until=least(clock_timestamp()+interval '5 minutes',deadline_at),updated_at=clock_timestamp() WHERE assignment_id=a.assignment_id RETURNING * INTO a;
 PERFORM forge_external_agent._event(a,'started',jsonb_build_object('provider','muse-db','assignment_id',a.assignment_id,'generation',a.generation,'actor',jsonb_build_object('kind','agent','id',g.profile_alias)));
 RETURN a.intent||jsonb_build_object('assignment_id',a.assignment_id,'provider','muse-db','generation',a.generation,'token',a.lease_token,'lease_until',to_char(a.lease_until AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),'deadline_at',to_char(a.deadline_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),'revision',a.task_revision);
END;
$function$;

CREATE OR REPLACE FUNCTION forge_external_agent_api.progress(p_id uuid, p_gen integer, p_token uuid, p_summary text)
 RETURNS jsonb
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE a forge_external_agent.assignments%ROWTYPE;
BEGIN a:=forge_external_agent._active(p_id,p_gen,p_token);
 IF p_summary IS NULL OR octet_length(p_summary) NOT BETWEEN 1 AND 4096 OR btrim(p_summary)='' OR translate(p_summary,E'\n\r\t','')~'[[:cntrl:]]' THEN RAISE EXCEPTION 'Invalid external summary' USING ERRCODE='22023';END IF;
 PERFORM forge_external_agent._event(a,'checkpoint',jsonb_build_object('provider','muse-db','assignment_id',p_id,'generation',p_gen,'summary',p_summary,'actor',jsonb_build_object('kind','agent','id',a.profile_alias)));
 RETURN jsonb_build_object('status','checkpoint');END;
$function$;

CREATE OR REPLACE FUNCTION forge_external_agent_api.finish(p_id uuid, p_gen integer, p_token uuid, p_content text, p_summary text)
 RETURNS jsonb
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE a forge_external_agent.assignments%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;digest text;intent_hash text;r jsonb;stamp timestamptz:=clock_timestamp();
BEGIN
 a:=forge_external_agent._locked(p_id);
 IF p_gen IS NULL OR p_token IS NULL OR p_content IS NULL OR octet_length(p_content) NOT BETWEEN 1 AND 262144 OR btrim(p_content)='' OR translate(p_content,E'\n\r\t','')~'[[:cntrl:]]'
 OR p_summary IS NULL OR octet_length(p_summary) NOT BETWEEN 1 AND 4096 OR btrim(p_summary)='' OR translate(p_summary,E'\n\r\t','')~'[[:cntrl:]]' THEN RAISE EXCEPTION 'Invalid external result' USING ERRCODE='22023';END IF;
 digest:=encode(sha256(convert_to(p_content,'UTF8')),'hex');
 intent_hash:=encode(sha256(convert_to(jsonb_build_object('assignment',p_id,'scope',a.intent_digest,'principal',session_user,'generation',p_gen,'sha256',digest,'summary',p_summary)::text,'UTF8')),'hex');
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id);
 IF t.status IN('cancelled','cancel_requested') THEN RAISE EXCEPTION 'External task cancelled' USING ERRCODE='P0009';END IF;
 IF a.status='submitted' THEN
 IF a.generation<>p_gen OR a.finish_token IS DISTINCT FROM p_token OR a.finish_intent IS DISTINCT FROM intent_hash THEN RAISE EXCEPTION 'External finish conflict' USING ERRCODE='P0009';END IF;
 RETURN a.receipt;END IF;
 a:=forge_external_agent._active(p_id,p_gen,p_token);
 r:=jsonb_build_object('assignment_id',p_id,'task_id',a.task_id,'project_alias',a.project_alias,'provider','muse-db','status','awaiting_review','sha256',digest,'generation',p_gen,'submitted_at',to_char(stamp AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'));
 INSERT INTO forge_external_agent.artifacts(assignment_id,sha256,content,summary,submitted_at,principal,generation,recipe_digest) VALUES(p_id,digest,p_content,p_summary,stamp,session_user,p_gen,a.intent->>'recipe_digest');
 UPDATE forge_agent.coding_tasks SET status='awaiting_review',revision=revision+1,updated_at=stamp WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id);
 UPDATE forge_external_agent.assignments SET status='submitted',lease_token=NULL,lease_until=NULL,finish_token=p_token,finish_intent=intent_hash,receipt=r,updated_at=stamp WHERE assignment_id=p_id;
 PERFORM forge_external_agent._event(a,'awaiting_review',r||jsonb_build_object('actor',jsonb_build_object('kind','agent','id',a.profile_alias)));RETURN r;
END;
$function$;

CREATE OR REPLACE FUNCTION forge_external_agent_api.release(p_id uuid, p_gen integer, p_token uuid)
 RETURNS jsonb
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE a forge_external_agent.assignments%ROWTYPE;s text;
BEGIN a:=forge_external_agent._active(p_id,p_gen,p_token);s:=CASE WHEN a.generation>=3 THEN 'failed' ELSE 'queued' END;
 UPDATE forge_agent.coding_tasks SET status=s,revision=revision+1,updated_at=clock_timestamp() WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id) RETURNING revision INTO a.task_revision;
 UPDATE forge_external_agent.assignments SET status=s,task_revision=a.task_revision,lease_token=NULL,lease_until=NULL,updated_at=clock_timestamp() WHERE assignment_id=p_id;
 PERFORM forge_external_agent._event(a,CASE WHEN s='queued' THEN 'resumed' ELSE 'failed' END,jsonb_build_object('provider','muse-db','assignment_id',p_id,'generation',p_gen,'actor',jsonb_build_object('kind','agent','id',a.profile_alias)));
 PERFORM pg_notify('forge_external_agent_jobs','');RETURN jsonb_build_object('status',s);END;
$function$;

-- coding_events_progress_check: restate migration 0012's exact-keys
-- check with the worker 'actor' allowed (report_progress carries it now).
-- NOT VALID by design: legacy progress rows predate the actor field and
-- are grandfathered untouched (audit history stays byte-identical); the
-- constraint enforces the shape on every new row. verify.sql exempts
-- this constraint from the convalidated requirement for exactly this reason.
ALTER TABLE forge_agent.coding_events
    DROP CONSTRAINT IF EXISTS coding_events_progress_check,
    ADD CONSTRAINT coding_events_progress_check CHECK (
        ((progress_attempt_id IS NULL AND progress_sequence IS NULL)
         OR (progress_attempt_id IS NOT NULL
             AND progress_sequence BETWEEN 1 AND 128
             AND event_kind = 'checkpoint'
             AND payload - ARRAY['attempt_id', 'progress_sequence', 'stage', 'summary', 'actor'] = '{}'::jsonb
             AND payload ?& ARRAY['attempt_id', 'progress_sequence', 'stage', 'summary', 'actor']
             AND jsonb_typeof(payload->'attempt_id') = 'string'
             AND payload->>'attempt_id' = progress_attempt_id::text
             AND jsonb_typeof(payload->'progress_sequence') = 'number'
             AND payload->>'progress_sequence' = progress_sequence::text
             AND jsonb_typeof(payload->'stage') = 'string'
             AND ((progress_sequence = 1 AND payload->>'stage' = 'plan')
                  OR (progress_sequence > 1 AND payload->>'stage' = 'progress'))
             AND jsonb_typeof(payload->'summary') = 'string'
             AND octet_length(payload->>'summary') BETWEEN 1 AND 4096
             AND btrim(payload->>'summary', E' \t\r\n') <> ''
             AND (payload->>'summary') !~ U&'[\0001-\0008\000B-\001F\007F-\009F]'
             AND jsonb_typeof(payload->'actor') = 'object'
             AND (payload->'actor') ?& ARRAY['kind', 'id']
             AND (payload->'actor'->>'kind') = 'worker'
             AND octet_length(btrim(payload->'actor'->>'id')) BETWEEN 1 AND 128)) IS TRUE
    ) NOT VALID;
