-- Versioned Plan decisions and trusted publication outbox. Historical rows,
-- recipes, result chunks and control receipts are not rewritten.
CREATE TABLE forge_agent.coding_plan_decisions (
    decision_id uuid PRIMARY KEY,
    owner_id text NOT NULL,
    project_alias text NOT NULL,
    task_id uuid NOT NULL,
    plan_attempt_id uuid NOT NULL,
    plan_sha256 text NOT NULL CHECK (plan_sha256 ~ '^[0-9a-f]{64}$'),
    source_revision bigint NOT NULL CHECK (source_revision BETWEEN 1 AND 9007199254740990),
    base_revision text NOT NULL CHECK (base_revision ~ '^[0-9a-f]{40}$'),
    decision text NOT NULL CHECK (decision IN ('resume', 'revise', 'cancel')),
    agent_profile text NOT NULL CHECK (agent_profile ~ '^[A-Za-z0-9_.-]{1,64}$'),
    note text NOT NULL CHECK (octet_length(note) <= 2048 AND (decision <> 'revise' OR btrim(note) <> '')),
    idempotency_key text NOT NULL CHECK (idempotency_key ~ '^[A-Za-z0-9_.-]{1,128}$'),
    receipt_json jsonb NOT NULL CHECK (jsonb_typeof(receipt_json) = 'object'),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT coding_plan_decisions_scope_unique UNIQUE (owner_id, project_alias, task_id, decision_id),
    CONSTRAINT coding_plan_decisions_intent_unique UNIQUE (owner_id, project_alias, task_id, idempotency_key),
    CONSTRAINT coding_plan_decisions_attempt_fk FOREIGN KEY (owner_id, project_alias, task_id, plan_attempt_id)
        REFERENCES forge_agent.coding_attempts (owner_id, project_alias, task_id, attempt_id)
);
ALTER TABLE forge_agent.coding_tasks
    ADD COLUMN plan_decision_id uuid,
    ADD CONSTRAINT coding_tasks_plan_decision_fk FOREIGN KEY (owner_id, project_alias, id, plan_decision_id)
        REFERENCES forge_agent.coding_plan_decisions (owner_id, project_alias, task_id, decision_id);
ALTER TABLE forge_agent.coding_attempts
    ADD COLUMN plan_decision_id uuid,
    ADD CONSTRAINT coding_attempts_plan_decision_fk FOREIGN KEY (owner_id, project_alias, task_id, plan_decision_id)
        REFERENCES forge_agent.coding_plan_decisions (owner_id, project_alias, task_id, decision_id);
ALTER TABLE forge_agent.coding_control_receipts
    DROP CONSTRAINT coding_control_receipts_action_check,
    ADD CONSTRAINT coding_control_receipts_action_check CHECK (action IN ('cancel', 'accept', 'resume', 'plan_decide'));

CREATE TABLE forge_agent.coding_publications (
    publication_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_id text NOT NULL,
    project_alias text NOT NULL,
    task_id uuid NOT NULL,
    attempt_id uuid NOT NULL,
    plan_decision_id uuid NOT NULL,
    reconciliation_receipt_id uuid NOT NULL REFERENCES forge_agent.coding_reconciliation_receipts (receipt_id),
    base_revision text NOT NULL CHECK (base_revision ~ '^[0-9a-f]{40}$'),
    workspace_digest text NOT NULL CHECK (workspace_digest ~ '^[0-9a-f]{64}$'),
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'checking', 'publishing', 'published', 'failed')),
    worker_id text,
    lease_token uuid,
    lease_expires_at timestamptz,
    branch text CHECK (branch IS NULL OR (octet_length(branch) <= 128 AND branch ~ '^forge/[A-Za-z0-9][A-Za-z0-9._/-]*$' AND branch !~ '(\.\.|//|/$|\.lock$)')),
    commit_sha text CHECK (commit_sha IS NULL OR commit_sha ~ '^[0-9a-f]{40}$'),
    pr_url text CHECK (pr_url IS NULL OR (octet_length(pr_url) <= 512 AND pr_url ~ '^https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/pull/[1-9][0-9]*$')),
    summary text NOT NULL DEFAULT '' CHECK (octet_length(summary) <= 4096),
    finish_token uuid,
    finish_receipt jsonb,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT coding_publications_attempt_unique UNIQUE (owner_id, project_alias, task_id, attempt_id),
    CONSTRAINT coding_publications_attempt_fk FOREIGN KEY (owner_id, project_alias, task_id, attempt_id)
        REFERENCES forge_agent.coding_attempts (owner_id, project_alias, task_id, attempt_id),
    CONSTRAINT coding_publications_decision_fk FOREIGN KEY (owner_id, project_alias, task_id, plan_decision_id)
        REFERENCES forge_agent.coding_plan_decisions (owner_id, project_alias, task_id, decision_id),
    CONSTRAINT coding_publications_lease_check CHECK (
        ((status IN ('checking', 'publishing') AND worker_id IS NOT NULL AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL)
         OR (status NOT IN ('checking', 'publishing') AND lease_token IS NULL AND lease_expires_at IS NULL)) IS TRUE
    ),
    CONSTRAINT coding_publications_published_check CHECK (
        (status <> 'published' OR (branch IS NOT NULL AND commit_sha IS NOT NULL AND pr_url IS NOT NULL)) IS TRUE
    )
);
CREATE INDEX coding_publications_pending ON forge_agent.coding_publications (owner_id, status, created_at);
REVOKE ALL ON forge_agent.coding_plan_decisions, forge_agent.coding_publications FROM PUBLIC;

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
             AND pinned_recipe->>'resource_profile' IN ('forge-coding-v1', 'forge-coding-v2', 'forge-coding-v3', 'forge-coding-v4')
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
          AND (t.pinned_recipe->>'resource_profile') IN ('forge-coding-v1', 'forge-coding-v2', 'forge-coding-v3', 'forge-coding-v4')
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
            lease_expires_at, deadline_at, started_at, updated_at
        )
        VALUES (
            v_task.owner_id, v_task.project_alias, v_task.id, v_attempt_id, v_attempt_no,
            v_attempt_token, v_task.agent_profile, v_task.pinned_recipe, v_task.recipe_digest,
            session_user, p_worker_id, p_incarnation,
            v_unit_name, 'starting', true, v_resume_digest, v_task.plan_decision_id,
            v_lease_expires, v_deadline, v_now, v_now
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
            'resume_attempt_id', CASE WHEN v_resume_digest IS NOT NULL THEN v_task.latest_attempt_id ELSE NULL END
        );
    END LOOP;

    RETURN NULL;
END;
$fn$;

-- CREATE OR REPLACE preserves the existing scoped EXECUTE grants.
REVOKE ALL ON FUNCTION forge_coding_worker_api.claim(text, uuid, text[]) FROM PUBLIC;

-- Read only the approved immutable Plan belonging to the currently admitted
-- attempt. Agent children receive a bounded file, not this private DB port.
CREATE FUNCTION forge_coding_worker_api.read_plan(p_attempt_id uuid, p_attempt_token uuid)
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
    IF a.plan_decision_id IS NULL THEN RETURN NULL; END IF;
    SELECT * INTO d FROM forge_agent.coding_plan_decisions x
    WHERE x.owner_id=a.owner_id AND x.project_alias=a.project_alias AND x.task_id=a.task_id AND x.decision_id=a.plan_decision_id;
    IF NOT FOUND OR d.decision NOT IN ('resume','revise') THEN RAISE EXCEPTION 'Invalid Plan decision' USING ERRCODE='P0009'; END IF;
    SELECT * INTO source FROM forge_agent.coding_attempts x WHERE x.owner_id=a.owner_id AND x.project_alias=a.project_alias
       AND x.task_id=a.task_id AND x.attempt_id=d.plan_attempt_id;
    IF source.pinned_recipe->>'mode' IS DISTINCT FROM 'plan' OR source.phase<>'stopped'
       OR source.published_at IS NULL OR source.result_sha256 IS DISTINCT FROM d.plan_sha256
       OR source.result_byte_count NOT BETWEEN 1 AND 16777216 THEN
        RAISE EXCEPTION 'Plan artifact no longer matches' USING ERRCODE='P0006';
    END IF;
    SELECT string_agg(c.content,'' ORDER BY c.chunk_index) INTO content FROM forge_agent.coding_result_chunks c
    WHERE c.owner_id=a.owner_id AND c.project_alias=a.project_alias AND c.task_id=a.task_id AND c.attempt_id=d.plan_attempt_id;
    IF content IS NULL OR octet_length(content)<>source.result_byte_count
       OR encode(sha256(convert_to(content,'UTF8')),'hex')<>d.plan_sha256 THEN
        RAISE EXCEPTION 'Plan artifact no longer matches' USING ERRCODE='P0006';
    END IF;
    RETURN jsonb_build_object('decision_id',d.decision_id,'decision',d.decision,'plan_attempt_id',d.plan_attempt_id,
                             'plan_sha256',d.plan_sha256,'text',content,'note',d.note);
END; $fn$;

CREATE FUNCTION forge_coding_worker_api.queue_publication(p_attempt_id uuid,p_attempt_token uuid,p_receipt_id uuid)
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
    ON CONFLICT(owner_id,project_alias,task_id,attempt_id) DO NOTHING;
    SELECT publication_id INTO id FROM forge_agent.coding_publications x WHERE x.owner_id=a.owner_id
       AND x.project_alias=a.project_alias AND x.task_id=a.task_id AND x.attempt_id=a.attempt_id;
    PERFORM pg_notify('forge_coding_jobs','');
    RETURN id;
END; $fn$;

-- Trusted publication uses the existing reconciler connection and owner
-- binding. Expired preflight work can be reclaimed; uncertain remote writes
-- become failed and require review rather than automatic Git replay.
CREATE FUNCTION forge_coding_reconciler_api.publication_claim(p_worker_id text)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE j forge_agent.coding_publications%ROWTYPE; now_at timestamptz:=clock_timestamp();
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
    WHERE a.owner_id=current_setting('forge.coding_owner',true) AND d.decision='resume'
      AND t.plan_decision_id=d.decision_id AND t.mode='implement' AND a.pinned_recipe->>'mode'='implement'
      AND t.status IN ('awaiting_review','completed') AND a.phase='stopped' AND NOT a.reservation_active
      AND a.lease_expires_at IS NULL AND a.published_at IS NOT NULL AND a.result_sha256 IS NOT NULL
    ON CONFLICT(owner_id,project_alias,task_id,attempt_id) DO NOTHING;
    -- A superseded/revoked task cannot publish, and must not retain an active
    -- outbox state that permanently blocks later explicit task controls.
    UPDATE forge_agent.coding_publications p SET status='failed',lease_token=NULL,lease_expires_at=NULL,
      summary='Publication source is no longer current; review retained remote metadata before retrying.',updated_at=now_at
    WHERE p.owner_id=current_setting('forge.coding_owner',true) AND p.status IN ('pending','checking','publishing')
      AND NOT EXISTS (SELECT 1 FROM forge_agent.coding_tasks t
        WHERE (t.owner_id,t.project_alias,t.id,t.latest_attempt_id,t.plan_decision_id)=
              (p.owner_id,p.project_alias,p.task_id,p.attempt_id,p.plan_decision_id)
          AND t.mode='implement' AND t.status IN ('awaiting_review','completed'));
    UPDATE forge_agent.coding_publications SET status='failed',lease_token=NULL,lease_expires_at=NULL,
      summary='Publication outcome is uncertain after its lease expired; review remote state before retrying.',updated_at=now_at
    WHERE owner_id=current_setting('forge.coding_owner',true) AND status='publishing' AND lease_expires_at<=now_at;
    SELECT p.* INTO j FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t
      ON (t.owner_id,t.project_alias,t.id,t.latest_attempt_id,t.plan_decision_id)=
         (p.owner_id,p.project_alias,p.task_id,p.attempt_id,p.plan_decision_id)
    WHERE p.owner_id=current_setting('forge.coding_owner',true)
      AND (p.status='pending' OR (p.status='checking' AND p.lease_expires_at<=now_at))
      AND t.mode='implement' AND t.status IN ('awaiting_review','completed')
    ORDER BY p.created_at,p.publication_id FOR UPDATE OF p SKIP LOCKED LIMIT 1;
    IF NOT FOUND THEN RETURN NULL; END IF;
    UPDATE forge_agent.coding_publications SET status='checking',worker_id=p_worker_id,lease_token=gen_random_uuid(),
       lease_expires_at=clock_timestamp()+interval '60 seconds',updated_at=clock_timestamp()
    WHERE publication_id=j.publication_id RETURNING * INTO j;
    RETURN jsonb_build_object('publication_id',j.publication_id,'task_id',j.task_id,'attempt_id',j.attempt_id,
      'project_alias',j.project_alias,'plan_decision_id',j.plan_decision_id,'base_revision',j.base_revision,
      'workspace_digest',j.workspace_digest,'worker_id',j.worker_id,'lease_token',j.lease_token,
      'lease_expires_at',to_char(j.lease_expires_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
      'status',j.status,'branch',j.branch,'commit_sha',j.commit_sha,'pr_url',j.pr_url);
END; $fn$;

CREATE FUNCTION forge_coding_reconciler_api.publication_renew(p_publication_id uuid,p_lease_token uuid)
RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
    UPDATE forge_agent.coding_publications p SET lease_expires_at=clock_timestamp()+interval '60 seconds',updated_at=clock_timestamp()
    FROM forge_agent.coding_tasks t
    WHERE p.publication_id=p_publication_id AND p.lease_token=p_lease_token
      AND p.owner_id=current_setting('forge.coding_owner',true) AND p.status IN ('checking','publishing')
      AND p.lease_expires_at>clock_timestamp() AND (t.owner_id,t.project_alias,t.id,t.latest_attempt_id,t.plan_decision_id)=
          (p.owner_id,p.project_alias,p.task_id,p.attempt_id,p.plan_decision_id)
      AND t.mode='implement' AND t.status IN ('awaiting_review','completed');
    RETURN FOUND;
END; $fn$;

CREATE FUNCTION forge_coding_reconciler_api.publication_finish(
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
    receipt:=jsonb_strip_nulls(jsonb_build_object('publication_id',j.publication_id,'status',p_status,'branch',coalesce(p_branch,j.branch),
       'commit_sha',coalesce(p_commit_sha,j.commit_sha),'pr_url',coalesce(p_pr_url,j.pr_url),'summary',p_summary));
    IF j.finish_token=p_lease_token AND j.finish_receipt=receipt THEN RETURN receipt; END IF;
    IF j.lease_token IS DISTINCT FROM p_lease_token OR j.status NOT IN ('checking','publishing')
       OR j.lease_expires_at<=clock_timestamp() OR NOT EXISTS(SELECT 1 FROM forge_agent.coding_tasks t
         WHERE (t.owner_id,t.project_alias,t.id,t.latest_attempt_id,t.plan_decision_id)=
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
REVOKE ALL ON FUNCTION forge_coding_worker_api.read_plan(uuid,uuid),
    forge_coding_worker_api.queue_publication(uuid,uuid,uuid),
    forge_coding_reconciler_api.publication_claim(text),forge_coding_reconciler_api.publication_renew(uuid,uuid),
    forge_coding_reconciler_api.publication_finish(uuid,uuid,text,text,text,text,text) FROM PUBLIC;
