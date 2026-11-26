-- M47 (phase 0e): deadline extensions are visible to the worker and in the
-- task event stream.
--
-- renew_status() (M45) extends a nearly due attempt deadline on a progress
-- signal but returns only a status, and the extension lives only in
-- coding_attempt_deadline_extensions. renew_deadline() wraps it unchanged and
-- returns {status, deadline_at, extended}; when this call extended the
-- deadline it appends one 'checkpoint' event with stage 'deadline_extended'
-- (attempt, extension number, previous/new deadline, signal, worker actor).
-- The worker uses deadline_at to move its own local wall bound, so a healthy
-- Codex full-gate run that keeps producing output is no longer killed at the
-- fixed 30-minute mark. Additive: renew_status() is unchanged.

CREATE FUNCTION forge_coding_worker_api.renew_deadline(p_attempt_id uuid,p_attempt_token uuid,p_progress_signal text)
 RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE v_before integer;v_status text;v_ext forge_agent.coding_attempt_deadline_extensions%ROWTYPE;v_attempt record;v_extended boolean:=false;
BEGIN
 SELECT count(*) INTO v_before FROM forge_agent.coding_attempt_deadline_extensions e WHERE e.attempt_id=p_attempt_id;
 v_status:=forge_coding_worker_api.renew_status(p_attempt_id,p_attempt_token,p_progress_signal);
 IF v_status<>'renewed' THEN
  RETURN jsonb_build_object('status',v_status,'deadline_at',NULL,'extended',false);
 END IF;
 SELECT a.owner_id,a.project_alias,a.task_id,a.deadline_at INTO v_attempt FROM forge_agent.coding_attempts a
  WHERE a.attempt_id=p_attempt_id AND a.attempt_token=p_attempt_token;
 SELECT * INTO v_ext FROM forge_agent.coding_attempt_deadline_extensions e WHERE e.attempt_id=p_attempt_id AND e.extension_no=v_before+1;
 IF FOUND THEN
  v_extended:=true;
  INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
  SELECT v_attempt.owner_id,v_attempt.project_alias,v_attempt.task_id,coalesce(max(sequence),0)+1,'checkpoint',
   jsonb_build_object('stage','deadline_extended','attempt_id',p_attempt_id::text,'extension_no',v_ext.extension_no,
    'previous_deadline',to_char(v_ext.previous_deadline AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS"Z"'),
    'new_deadline',to_char(v_ext.new_deadline AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS"Z"'),
    'signal',v_ext.signal,'actor',jsonb_build_object('kind','worker','id',session_user::text))
  FROM forge_agent.coding_events WHERE owner_id=v_attempt.owner_id AND project_alias=v_attempt.project_alias AND task_id=v_attempt.task_id;
 END IF;
 RETURN jsonb_build_object('status',v_status,
  'deadline_at',to_char(v_attempt.deadline_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),'extended',v_extended);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_coding_worker_api.renew_deadline(uuid,uuid,text) FROM PUBLIC;
