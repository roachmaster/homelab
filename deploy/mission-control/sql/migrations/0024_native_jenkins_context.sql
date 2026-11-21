-- M24: narrow trusted-publisher identities and durable CI cancellation reads.
-- Historical recipes, snapshots, counters, source refs and receipts stay frozen.
SET LOCAL lock_timeout='5s';
SET LOCAL statement_timeout='60s';

CREATE FUNCTION forge_coding_reconciler_api.native_ci_status(
 p_review uuid,p_project text,p_task uuid,p_attempt uuid
) RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER
SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE bound record;state text;
BEGIN
 IF p_review IS NULL OR p_task IS NULL OR p_attempt IS NULL OR p_project IS NULL
  OR p_project!~'^[A-Za-z0-9_.-]{1,64}$' THEN
  RAISE EXCEPTION 'Invalid native CI identity' USING ERRCODE='22023';
 END IF;
 SELECT q.review_id,q.task_id,q.attempt_id,q.status AS review_status,q.candidate_commit,
  q.lease_expires_at AS review_lease_expires_at,
  t.owner_id,t.project_alias,t.workflow_id,t.correlation_id,t.latest_attempt_id,t.status AS task_status,
  t.cancelled_at,t.accepted_at,w.enabled,w.phase AS workflow_phase,
  a.phase AS attempt_phase,a.reservation_active,a.lease_expires_at,a.published_at,a.outcome,
  a.result_sha256,q.parent_result_sha256,
  (r.owner_id,r.project_alias,r.task_id,r.attempt_id,r.workspace_digest)
   IS NOT DISTINCT FROM(q.owner_id,q.project_alias,q.task_id,q.attempt_id,q.workspace_digest) AS reconciled
 INTO bound FROM forge_agent.coding_reviews q
 JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id)
 JOIN forge_agent.coding_attempts a ON(a.owner_id,a.project_alias,a.task_id,a.attempt_id)=(q.owner_id,q.project_alias,q.task_id,q.attempt_id)
 JOIN forge_agent.coding_workflows w ON(w.owner_id,w.project_alias,w.workflow_id)=(t.owner_id,t.project_alias,t.workflow_id)
 JOIN forge_agent.coding_reconciliation_receipts r ON r.receipt_id=q.reconciliation_receipt_id
 WHERE q.review_id=p_review AND q.project_alias=p_project AND q.task_id=p_task AND q.attempt_id=p_attempt
  AND q.owner_id=current_setting('forge.coding_owner',true) AND q.phase='local';
 IF NOT FOUND THEN RAISE EXCEPTION 'Native CI source denied' USING ERRCODE='42501';END IF;
 state:=CASE
  WHEN bound.workflow_phase='cancelled' OR bound.task_status='cancelled' OR bound.cancelled_at IS NOT NULL THEN 'cancelled'
  WHEN bound.latest_attempt_id IS DISTINCT FROM p_attempt THEN 'superseded'
  WHEN bound.workflow_phase='completed' OR bound.task_status='completed' OR bound.accepted_at IS NOT NULL THEN 'completed'
  WHEN NOT bound.enabled OR bound.workflow_phase='escalated'
   OR bound.task_status<>'awaiting_review'
   OR bound.review_status NOT IN('checking','awaiting_review','approved')
   OR(bound.review_status='checking' AND(bound.review_lease_expires_at IS NULL
     OR bound.review_lease_expires_at<=clock_timestamp()))
   OR bound.attempt_phase<>'stopped' OR bound.reservation_active OR bound.lease_expires_at IS NOT NULL
   OR bound.published_at IS NULL OR bound.outcome IS DISTINCT FROM 'awaiting_review'
   OR bound.result_sha256 IS DISTINCT FROM bound.parent_result_sha256 OR NOT bound.reconciled THEN 'inactive'
  ELSE 'active' END;
 RETURN jsonb_build_object('version',1,'owner_id',bound.owner_id,'project_alias',bound.project_alias,
  'task_id',bound.task_id,'attempt_id',bound.attempt_id,'review_id',bound.review_id,
  'workflow_id',bound.workflow_id,'correlation_id',bound.correlation_id,
  'candidate_commit',bound.candidate_commit,'active',state='active','state',state);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_coding_reconciler_api.native_ci_status(uuid,text,uuid,uuid) FROM PUBLIC;

CREATE FUNCTION forge_coding_reconciler_api.native_ci_context(
 p_review uuid,p_token uuid,p_project text,p_task uuid,p_attempt uuid
) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE value jsonb;
BEGIN
 IF p_token IS NULL THEN RAISE EXCEPTION 'Native CI lease denied' USING ERRCODE='42501';END IF;
 -- Match native task-control lock ordering before taking source row locks.
 PERFORM pg_advisory_xact_lock(hashtextextended(
  current_setting('forge.coding_owner',true)||E'\n'||p_project,2026111901));
 -- Lock the current source and workflow before admitting a fresh CI action.
 PERFORM 1 FROM forge_agent.coding_reviews q
 JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id)
 JOIN forge_agent.coding_attempts a ON(a.owner_id,a.project_alias,a.task_id,a.attempt_id)=(q.owner_id,q.project_alias,q.task_id,q.attempt_id)
 JOIN forge_agent.coding_workflows w ON(w.owner_id,w.project_alias,w.workflow_id)=(t.owner_id,t.project_alias,t.workflow_id)
 WHERE q.review_id=p_review AND q.project_alias=p_project AND q.task_id=p_task AND q.attempt_id=p_attempt
  AND q.owner_id=current_setting('forge.coding_owner',true) AND q.phase='local'
  AND q.status='checking' AND q.lease_token=p_token AND q.lease_expires_at>clock_timestamp()
 FOR SHARE OF q,t,a,w;
 IF NOT FOUND THEN RAISE EXCEPTION 'Native CI lease denied' USING ERRCODE='42501';END IF;
 value:=forge_coding_reconciler_api.native_ci_status(p_review,p_project,p_task,p_attempt);
 IF value->'active' IS DISTINCT FROM 'true'::jsonb THEN
  RAISE EXCEPTION 'Native CI source no longer active' USING ERRCODE='42501';
 END IF;
 RETURN value;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_coding_reconciler_api.native_ci_context(uuid,uuid,text,uuid,uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_coding_reconciler_api.native_ci_status(uuid,text,uuid,uuid),
 forge_coding_reconciler_api.native_ci_context(uuid,uuid,text,uuid,uuid) TO forge_coding_reconciler;
