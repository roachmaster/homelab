-- M46 (phase 0e): failed events carry a machine-readable cause.
--
-- publish() records a 'failed' event with no cause, so the portal and the
-- event stream could not say why an attempt failed (workflow 3824445d). The
-- wrapper below publishes exactly as before and then attaches the producer's
-- cause code and a bounded summary to the failed event written in the same
-- transaction. A publish replay writes no new event and changes nothing;
-- historical events are never rewritten. publish() itself is unchanged.

CREATE FUNCTION forge_coding_worker_api.publish_with_cause(p_attempt_id uuid,p_attempt_token uuid,p_outcome text,p_manifest jsonb,
  p_usage jsonb,p_summary text,p_stop_proof jsonb,p_cause text)
 RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE v_receipt jsonb;v_attempt record;
BEGIN
 IF (p_outcome='failed')<>(p_cause IS NOT NULL) OR (p_cause IS NOT NULL AND p_cause NOT IN
   ('malformed_output','policy_unavailable','context_unavailable','resource_bound','agent_input','agent_failed')) THEN
  RAISE EXCEPTION 'Invalid failure cause' USING ERRCODE='22023';
 END IF;
 v_receipt:=forge_coding_worker_api.publish(p_attempt_id,p_attempt_token,p_outcome,p_manifest,p_usage,p_summary,p_stop_proof);
 IF p_cause IS NOT NULL THEN
  SELECT a.owner_id,a.project_alias,a.task_id INTO v_attempt FROM forge_agent.coding_attempts a
   WHERE a.attempt_id=p_attempt_id AND a.attempt_token=p_attempt_token;
  UPDATE forge_agent.coding_events e
     SET payload=e.payload||jsonb_build_object('cause',p_cause,'cause_summary',
          convert_from(substring(convert_to(p_summary,'UTF8') FROM 1 FOR 512),'UTF8'))
   WHERE (e.owner_id,e.project_alias,e.task_id)=(v_attempt.owner_id,v_attempt.project_alias,v_attempt.task_id)
     AND e.event_kind='failed' AND e.payload->>'attempt_id'=p_attempt_id::text AND NOT e.payload ? 'cause';
 END IF;
 RETURN v_receipt;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_coding_worker_api.publish_with_cause(uuid,uuid,text,jsonb,jsonb,text,jsonb,text) FROM PUBLIC;
