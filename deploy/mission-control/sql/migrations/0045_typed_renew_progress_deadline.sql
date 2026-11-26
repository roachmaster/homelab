-- M45 (phase 0e): typed lease renewal and progress-aware attempt deadlines.
--
-- renew(uuid,uuid) returns false for an expired lease, a cancelled task, a
-- disabled workflow and a vanished attempt alike, so the worker reported every
-- cancellation as "lease lost". renew_status() returns one of
--   renewed | lease_expired | deadline_exceeded | task_cancelled | task_gone
-- and, when the worker reports recent progress, extends a nearly due attempt
-- deadline in bounded steps up to a hard outer bound, so a healthy full-gate
-- run is not killed by the fixed 30-minute wall clock while a stuck attempt
-- (no progress) still dies at its deadline. Every extension is recorded.
--
-- Additive and safe to apply after M44: renew() is unchanged; one table and
-- one function are added.

CREATE TABLE forge_agent.coding_attempt_deadline_extensions(
 owner_id text NOT NULL,
 project_alias text NOT NULL,
 task_id uuid NOT NULL,
 attempt_id uuid NOT NULL,
 extension_no integer NOT NULL CHECK(extension_no BETWEEN 1 AND 64),
 previous_deadline timestamptz NOT NULL,
 new_deadline timestamptz NOT NULL CHECK(new_deadline>previous_deadline),
 signal text NOT NULL CHECK(signal IN('progress_report','checkpoint','output')),
 extended_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(attempt_id,extension_no),
 FOREIGN KEY(owner_id,project_alias,task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id)
);

CREATE FUNCTION forge_coding_worker_api.renew_status(p_attempt_id uuid,p_attempt_token uuid,p_progress_signal text)
 RETURNS text LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE v_state record;v_now timestamptz;v_bound timestamptz;v_next timestamptz;v_count integer;
BEGIN
 IF p_progress_signal IS NOT NULL AND p_progress_signal NOT IN('progress_report','checkpoint','output') THEN
  RAISE EXCEPTION 'Invalid progress signal' USING ERRCODE='22023';
 END IF;
 IF forge_coding_worker_api.renew(p_attempt_id,p_attempt_token) THEN
  v_now:=clock_timestamp();
  SELECT a.owner_id,a.project_alias,a.task_id,a.attempt_id,a.deadline_at,a.started_at INTO v_state
   FROM forge_agent.coding_attempts a WHERE a.attempt_id=p_attempt_id AND a.attempt_token=p_attempt_token;
  -- Extend only near the deadline, only on reported progress, 10 minutes at a
  -- time, never past started_at + 4 hours.
  IF p_progress_signal IS NOT NULL AND v_state.deadline_at-v_now<=interval '5 minutes' THEN
   v_bound:=v_state.started_at+interval '4 hours';
   v_next:=least(v_state.deadline_at+interval '10 minutes',v_bound);
   SELECT count(*) INTO v_count FROM forge_agent.coding_attempt_deadline_extensions e WHERE e.attempt_id=v_state.attempt_id;
   IF v_next>v_state.deadline_at AND v_count<64 THEN
    UPDATE forge_agent.coding_attempts SET deadline_at=v_next,lease_expires_at=least(v_next,v_now+interval '60 seconds'),updated_at=v_now
     WHERE attempt_id=v_state.attempt_id;
    INSERT INTO forge_agent.coding_attempt_deadline_extensions(owner_id,project_alias,task_id,attempt_id,extension_no,previous_deadline,new_deadline,signal)
     VALUES(v_state.owner_id,v_state.project_alias,v_state.task_id,v_state.attempt_id,v_count+1,v_state.deadline_at,v_next,p_progress_signal);
   END IF;
  END IF;
  RETURN 'renewed';
 END IF;
 SELECT t.status,a.published_at,a.deadline_at,w.enabled AS workflow_enabled,w.phase AS workflow_phase,a.workflow_id
  INTO v_state
  FROM forge_agent.coding_attempts a
  JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id)
  LEFT JOIN forge_agent.coding_workflows w ON w.workflow_id=a.workflow_id
  WHERE a.attempt_id=p_attempt_id AND a.attempt_token=p_attempt_token AND a.worker_role=session_user
    AND a.owner_id=current_setting('forge.coding_owner',true);
 IF NOT FOUND THEN RETURN 'task_gone';END IF;
 IF v_state.status IN('cancelled','cancel_requested')
    OR(v_state.workflow_id IS NOT NULL AND(NOT v_state.workflow_enabled OR v_state.workflow_phase IN('cancelled','escalated'))) THEN
  RETURN 'task_cancelled';
 END IF;
 IF v_state.status NOT IN('running') OR v_state.published_at IS NOT NULL THEN RETURN 'task_gone';END IF;
 IF v_state.deadline_at<=clock_timestamp() THEN RETURN 'deadline_exceeded';END IF;
 RETURN 'lease_expired';
END;
$fn$;
REVOKE ALL ON FUNCTION forge_coding_worker_api.renew_status(uuid,uuid,text) FROM PUBLIC;
REVOKE ALL ON forge_agent.coding_attempt_deadline_extensions FROM PUBLIC;
DO $grant$
BEGIN
 IF EXISTS(SELECT 1 FROM pg_roles WHERE rolname='muse_events') THEN
  EXECUTE 'REVOKE ALL ON forge_agent.coding_attempt_deadline_extensions FROM muse_events';
 END IF;
END
$grant$;
