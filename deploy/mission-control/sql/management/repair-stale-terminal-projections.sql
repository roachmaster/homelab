-- Repair procedure for stale terminal workflow projections and open retry events.
-- Run as forge_agent_owner (or database superuser) after verifying the preflight query below.
-- Single atomic transaction, idempotent on re-run, never touches live workflows or exhausted (20/20) retries.
--
-- Read-only preflight preview (run BEFORE executing this script):
--
--   WITH external_candidates AS (
--     SELECT 'external_workflow'::text AS scope_kind, w.owner_id, w.project_alias, w.workflow_id, w.root_task_id
--     FROM forge_external_workflow.workflows w
--     JOIN forge_agent.coding_tasks r
--       ON (r.owner_id, r.project_alias, r.id) = (w.owner_id, w.project_alias, w.root_task_id)
--     WHERE w.phase = 'escalated'
--       AND r.status = 'cancelled'
--       AND (
--         EXISTS (
--           SELECT 1 FROM forge_agent.coding_control_receipts cr
--           WHERE (cr.owner_id, cr.project_alias, cr.task_id) = (w.owner_id, w.project_alias, w.root_task_id)
--             AND (cr.action = 'cancel'
--                  OR (cr.action IN ('plan_decide', 'review_decide') AND cr.receipt_json->>'status' = 'cancelled'))
--         )
--         OR EXISTS (
--           SELECT 1 FROM forge_agent.coding_events ce
--           WHERE (ce.owner_id, ce.project_alias, ce.task_id) = (w.owner_id, w.project_alias, w.root_task_id)
--             AND ce.event_kind IN ('cancel_requested', 'cancelled')
--             AND ce.payload->'actor'->>'kind' = 'operator'
--             AND coalesce(ce.payload->>'cause', 'operator_request') <> 'workflow_cancelled'
--         )
--         OR EXISTS (
--           SELECT 1 FROM forge_agent.coding_events ce
--           WHERE (ce.owner_id, ce.project_alias, ce.task_id) = (w.owner_id, w.project_alias, w.root_task_id)
--             AND ce.event_kind IN ('cancel_requested', 'cancelled')
--             AND ce.payload->'actor' IS NULL
--             AND NOT (ce.payload ? 'workflow_id')
--             AND NOT (ce.payload ? 'attempt_id')
--             AND NOT (ce.payload ? 'assignment_id')
--             AND coalesce(ce.payload->>'reason', '') NOT IN ('stopped_without_publication', 'external_lease_reconciled')
--         )
--       )
--   ),
--   native_candidates AS (
--     SELECT 'native_workflow'::text AS scope_kind, w.owner_id, w.project_alias, w.workflow_id, w.root_task_id
--     FROM forge_agent.coding_workflows w
--     JOIN forge_agent.coding_tasks r
--       ON (r.owner_id, r.project_alias, r.id) = (w.owner_id, w.project_alias, w.root_task_id)
--     WHERE w.phase = 'escalated'
--       AND r.status = 'cancelled'
--       AND (
--         EXISTS (
--           SELECT 1 FROM forge_agent.coding_control_receipts cr
--           WHERE (cr.owner_id, cr.project_alias, cr.task_id) = (w.owner_id, w.project_alias, w.root_task_id)
--             AND (cr.action = 'cancel'
--                  OR (cr.action IN ('plan_decide', 'review_decide') AND cr.receipt_json->>'status' = 'cancelled'))
--         )
--         OR EXISTS (
--           SELECT 1 FROM forge_agent.coding_events ce
--           WHERE (ce.owner_id, ce.project_alias, ce.task_id) = (w.owner_id, w.project_alias, w.root_task_id)
--             AND ce.event_kind IN ('cancel_requested', 'cancelled')
--             AND ce.payload->'actor'->>'kind' = 'operator'
--             AND coalesce(ce.payload->>'cause', 'operator_request') <> 'workflow_cancelled'
--         )
--         OR EXISTS (
--           SELECT 1 FROM forge_agent.coding_events ce
--           WHERE (ce.owner_id, ce.project_alias, ce.task_id) = (w.owner_id, w.project_alias, w.root_task_id)
--             AND ce.event_kind IN ('cancel_requested', 'cancelled')
--             AND ce.payload->'actor' IS NULL
--             AND NOT (ce.payload ? 'workflow_id')
--             AND NOT (ce.payload ? 'attempt_id')
--             AND NOT (ce.payload ? 'assignment_id')
--             AND coalesce(ce.payload->>'reason', '') NOT IN ('stopped_without_publication', 'external_lease_reconciled')
--         )
--       )
--   )
--   SELECT * FROM external_candidates
--   UNION ALL
--   SELECT * FROM native_candidates
--   ORDER BY scope_kind, workflow_id;
--
--   SELECT e.event_id, e.owner_id, e.project_alias, e.scope_kind, e.scope_id, e.kind, e.used, e.attempt_limit
--   FROM forge_agent.coding_retry_events e
--   WHERE e.status = 'open'
--     AND (
--       (e.scope_kind = 'native_workflow' AND EXISTS (
--         SELECT 1 FROM forge_agent.coding_workflows w
--         WHERE (w.owner_id, w.project_alias, w.workflow_id) = (e.owner_id, e.project_alias, e.scope_id)
--           AND w.phase IN ('escalated', 'cancelled', 'completed')))
--       OR (e.scope_kind = 'external_workflow' AND EXISTS (
--         SELECT 1 FROM forge_external_workflow.workflows w
--         WHERE (w.owner_id, w.project_alias, w.workflow_id) = (e.owner_id, e.project_alias, e.scope_id)
--           AND w.phase IN ('escalated', 'cancelled', 'completed')))
--       OR (e.scope_kind = 'manual_task' AND EXISTS (
--         SELECT 1 FROM forge_agent.coding_tasks t
--         WHERE (t.owner_id, t.project_alias, t.id) = (e.owner_id, e.project_alias, e.scope_id)
--           AND t.workflow_id IS NULL AND t.status IN ('cancelled', 'completed')))
--     )
--   ORDER BY e.scope_kind, e.scope_id, e.event_id;

\set ON_ERROR_STOP on

BEGIN;

CREATE TEMP TABLE _repaired_workflows (
 scope_kind text NOT NULL,
 owner_id text NOT NULL,
 project_alias text NOT NULL,
 workflow_id uuid NOT NULL,
 root_task_id uuid NOT NULL
) ON COMMIT DROP;

CREATE TEMP TABLE _resolved_retry_events (
 event_id uuid NOT NULL,
 owner_id text NOT NULL,
 project_alias text NOT NULL,
 scope_kind text NOT NULL,
 scope_id uuid NOT NULL,
 kind text NOT NULL,
 used integer NOT NULL,
 attempt_limit integer NOT NULL
) ON COMMIT DROP;

-- Step 1: Move stuck escalated workflows with independently cancelled roots to 'cancelled'.
-- Unified strict predicate on root_task_id itself (never uses escalation_reason,
-- coordinator cleanup events, worker attempt stops, external lease reconciliations,
-- or non-root sibling cancellations as evidence of independent root cancellation).
DO $repair_workflows$
DECLARE
 rec record;
 next_seq bigint;
BEGIN
 FOR rec IN
  SELECT w.owner_id, w.project_alias, w.workflow_id, w.root_task_id
  FROM forge_external_workflow.workflows w
  JOIN forge_agent.coding_tasks r
    ON (r.owner_id, r.project_alias, r.id) = (w.owner_id, w.project_alias, w.root_task_id)
  WHERE w.phase = 'escalated'
    AND r.status = 'cancelled'
    AND (
      EXISTS (
        SELECT 1 FROM forge_agent.coding_control_receipts cr
        WHERE (cr.owner_id, cr.project_alias, cr.task_id) = (w.owner_id, w.project_alias, w.root_task_id)
          AND (cr.action = 'cancel'
               OR (cr.action IN ('plan_decide', 'review_decide') AND cr.receipt_json->>'status' = 'cancelled'))
      )
      OR EXISTS (
        SELECT 1 FROM forge_agent.coding_events ce
        WHERE (ce.owner_id, ce.project_alias, ce.task_id) = (w.owner_id, w.project_alias, w.root_task_id)
          AND ce.event_kind IN ('cancel_requested', 'cancelled')
          AND ce.payload->'actor'->>'kind' = 'operator'
          AND coalesce(ce.payload->>'cause', 'operator_request') <> 'workflow_cancelled'
      )
      OR EXISTS (
        SELECT 1 FROM forge_agent.coding_events ce
        WHERE (ce.owner_id, ce.project_alias, ce.task_id) = (w.owner_id, w.project_alias, w.root_task_id)
          AND ce.event_kind IN ('cancel_requested', 'cancelled')
          AND ce.payload->'actor' IS NULL
          AND NOT (ce.payload ? 'workflow_id')
          AND NOT (ce.payload ? 'attempt_id')
          AND NOT (ce.payload ? 'assignment_id')
          AND coalesce(ce.payload->>'reason', '') NOT IN ('stopped_without_publication', 'external_lease_reconciled')
      )
    )
  ORDER BY w.owner_id, w.project_alias, w.workflow_id
  FOR UPDATE OF w, r
 LOOP
  PERFORM pg_advisory_xact_lock(hashtextextended(rec.owner_id || E'\n' || rec.project_alias, 2026111901));
  UPDATE forge_external_workflow.workflows
  SET phase = 'cancelled',
      enabled = false,
      revision = revision + 1,
      updated_at = clock_timestamp()
  WHERE workflow_id = rec.workflow_id;

  SELECT coalesce(max(sequence), 0) + 1 INTO next_seq
  FROM forge_agent.coding_events
  WHERE (owner_id, project_alias, task_id) = (rec.owner_id, rec.project_alias, rec.root_task_id);

  INSERT INTO forge_agent.coding_events(owner_id, project_alias, task_id, sequence, event_kind, payload)
  VALUES (
    rec.owner_id,
    rec.project_alias,
    rec.root_task_id,
    next_seq,
    'checkpoint',
    jsonb_build_object(
      'workflow_id', rec.workflow_id::text,
      'phase', 'cancelled',
      'summary', 'Repaired stale escalated external workflow phase after verified independent root cancellation.',
      'actor', jsonb_build_object('kind', 'operator', 'id', 'terminal-projection-repair')
    )
  );

  INSERT INTO _repaired_workflows(scope_kind, owner_id, project_alias, workflow_id, root_task_id)
  VALUES ('external_workflow', rec.owner_id, rec.project_alias, rec.workflow_id, rec.root_task_id);
 END LOOP;

 FOR rec IN
  SELECT w.owner_id, w.project_alias, w.workflow_id, w.root_task_id
  FROM forge_agent.coding_workflows w
  JOIN forge_agent.coding_tasks r
    ON (r.owner_id, r.project_alias, r.id) = (w.owner_id, w.project_alias, w.root_task_id)
  WHERE w.phase = 'escalated'
    AND r.status = 'cancelled'
    AND (
      EXISTS (
        SELECT 1 FROM forge_agent.coding_control_receipts cr
        WHERE (cr.owner_id, cr.project_alias, cr.task_id) = (w.owner_id, w.project_alias, w.root_task_id)
          AND (cr.action = 'cancel'
               OR (cr.action IN ('plan_decide', 'review_decide') AND cr.receipt_json->>'status' = 'cancelled'))
      )
      OR EXISTS (
        SELECT 1 FROM forge_agent.coding_events ce
        WHERE (ce.owner_id, ce.project_alias, ce.task_id) = (w.owner_id, w.project_alias, w.root_task_id)
          AND ce.event_kind IN ('cancel_requested', 'cancelled')
          AND ce.payload->'actor'->>'kind' = 'operator'
          AND coalesce(ce.payload->>'cause', 'operator_request') <> 'workflow_cancelled'
      )
      OR EXISTS (
        SELECT 1 FROM forge_agent.coding_events ce
        WHERE (ce.owner_id, ce.project_alias, ce.task_id) = (w.owner_id, w.project_alias, w.root_task_id)
          AND ce.event_kind IN ('cancel_requested', 'cancelled')
          AND ce.payload->'actor' IS NULL
          AND NOT (ce.payload ? 'workflow_id')
          AND NOT (ce.payload ? 'attempt_id')
          AND NOT (ce.payload ? 'assignment_id')
          AND coalesce(ce.payload->>'reason', '') NOT IN ('stopped_without_publication', 'external_lease_reconciled')
      )
    )
  ORDER BY w.owner_id, w.project_alias, w.workflow_id
  FOR UPDATE OF w, r
 LOOP
  PERFORM pg_advisory_xact_lock(hashtextextended(rec.owner_id || E'\n' || rec.project_alias, 2026111901));
  UPDATE forge_agent.coding_workflows
  SET phase = 'cancelled',
      enabled = false,
      escalation_reason = '',
      updated_at = clock_timestamp()
  WHERE workflow_id = rec.workflow_id;

  SELECT coalesce(max(sequence), 0) + 1 INTO next_seq
  FROM forge_agent.coding_events
  WHERE (owner_id, project_alias, task_id) = (rec.owner_id, rec.project_alias, rec.root_task_id);

  INSERT INTO forge_agent.coding_events(owner_id, project_alias, task_id, sequence, event_kind, payload)
  VALUES (
    rec.owner_id,
    rec.project_alias,
    rec.root_task_id,
    next_seq,
    'checkpoint',
    jsonb_build_object(
      'workflow_id', rec.workflow_id::text,
      'phase', 'cancelled',
      'summary', 'Repaired stale escalated native workflow phase after verified independent root cancellation.',
      'actor', jsonb_build_object('kind', 'operator', 'id', 'terminal-projection-repair')
    )
  );

  INSERT INTO _repaired_workflows(scope_kind, owner_id, project_alias, workflow_id, root_task_id)
  VALUES ('native_workflow', rec.owner_id, rec.project_alias, rec.workflow_id, rec.root_task_id);
 END LOOP;
END;
$repair_workflows$;

-- Step 2: Resolve stale status = 'open' retry events on terminal scopes.
-- Preserves all live-workflow 'open' events and all 'exhausted' (20/20) events.
DO $repair_retries$
DECLARE
 scope_rec record;
BEGIN
 FOR scope_rec IN
  SELECT DISTINCT e.owner_id, e.project_alias
  FROM forge_agent.coding_retry_events e
  WHERE e.status = 'open'
  ORDER BY e.owner_id, e.project_alias
 LOOP
  PERFORM pg_advisory_xact_lock(hashtextextended(scope_rec.owner_id || E'\n' || scope_rec.project_alias, 2026111901));
 END LOOP;

 WITH resolved AS (
  UPDATE forge_agent.coding_retry_events e
  SET status = 'resolved',
      resolved_at = clock_timestamp(),
      updated_at = clock_timestamp()
  WHERE e.status = 'open'
    AND (
      (e.scope_kind = 'native_workflow' AND EXISTS (
        SELECT 1 FROM forge_agent.coding_workflows w
        WHERE (w.owner_id, w.project_alias, w.workflow_id) = (e.owner_id, e.project_alias, e.scope_id)
          AND w.phase IN ('escalated', 'cancelled', 'completed')))
      OR (e.scope_kind = 'external_workflow' AND EXISTS (
        SELECT 1 FROM forge_external_workflow.workflows w
        WHERE (w.owner_id, w.project_alias, w.workflow_id) = (e.owner_id, e.project_alias, e.scope_id)
          AND w.phase IN ('escalated', 'cancelled', 'completed')))
      OR (e.scope_kind = 'manual_task' AND EXISTS (
        SELECT 1 FROM forge_agent.coding_tasks t
        WHERE (t.owner_id, t.project_alias, t.id) = (e.owner_id, e.project_alias, e.scope_id)
          AND t.workflow_id IS NULL AND t.status IN ('cancelled', 'completed')))
    )
  RETURNING e.event_id, e.owner_id, e.project_alias, e.scope_kind, e.scope_id, e.kind, e.used, e.attempt_limit
 )
 INSERT INTO _resolved_retry_events(event_id, owner_id, project_alias, scope_kind, scope_id, kind, used, attempt_limit)
 SELECT event_id, owner_id, project_alias, scope_kind, scope_id, kind, used, attempt_limit
 FROM resolved;
END;
$repair_retries$;

SELECT scope_kind, owner_id, project_alias, workflow_id, root_task_id
FROM _repaired_workflows
ORDER BY scope_kind, workflow_id;

SELECT event_id, owner_id, project_alias, scope_kind, scope_id, kind, used, attempt_limit
FROM _resolved_retry_events
ORDER BY scope_kind, scope_id, event_id;

SELECT
 (SELECT count(*) FROM _repaired_workflows WHERE scope_kind = 'external_workflow') AS repaired_external_workflows,
 (SELECT count(*) FROM _repaired_workflows WHERE scope_kind = 'native_workflow') AS repaired_native_workflows,
 (SELECT count(*) FROM _resolved_retry_events) AS resolved_retry_events;

COMMIT;
