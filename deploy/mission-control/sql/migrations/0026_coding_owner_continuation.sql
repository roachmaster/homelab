-- Explicit owner continuation is one preserved writer, never a fresh repair
-- allowance. No historical row, policy, credential or default ACL is changed.
SET LOCAL lock_timeout='5s';
SET LOCAL statement_timeout='60s';

CREATE FUNCTION forge_agent._coding_owner_continuation_fenced(p_task uuid)
RETURNS boolean LANGUAGE plpgsql STABLE SECURITY DEFINER
SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE t forge_agent.coding_tasks%ROWTYPE;w forge_agent.coding_workflows%ROWTYPE;
 a forge_agent.coding_workflow_actions%ROWTYPE;q forge_agent.coding_reviews%ROWTYPE;
 r forge_agent.coding_workflow_reviews%ROWTYPE;s forge_agent.coding_attempts%ROWTYPE;
 p forge_agent.coding_plan_decisions%ROWTYPE;rec forge_agent.coding_reconciliation_receipts%ROWTYPE;checked_at timestamptz;
BEGIN
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE id=p_task
  AND owner_id=current_setting('forge.coding_owner',true);
 IF NOT FOUND THEN RAISE EXCEPTION 'Owner continuation scope denied' USING ERRCODE='42501';END IF;
 IF t.workflow_id IS NULL THEN RETURN false;END IF;
 SELECT * INTO w FROM forge_agent.coding_workflows WHERE workflow_id=t.workflow_id
  AND(owner_id,project_alias)=(t.owner_id,t.project_alias);
 SELECT * INTO a FROM forge_agent.coding_workflow_actions WHERE workflow_id=w.workflow_id
  AND(owner_id,project_alias)=(t.owner_id,t.project_alias) AND kind='decide'
  AND source_key='owner-code-resume:'||w.workflow_id::text;
 IF NOT FOUND THEN RETURN false;END IF;
 -- A partial or malformed canonical marker never removes the fence.
 IF(a.status<>'completed' OR jsonb_typeof(a.intent)<>'object'
  OR a.intent->>'policy' IS DISTINCT FROM 'forge-owner-code-resume-v1'
  OR a.intent->>'workflow_id' IS DISTINCT FROM w.workflow_id::text
  OR a.intent->>'policy_digest' IS DISTINCT FROM w.policy_digest
  OR jsonb_typeof(a.intent->'request')<>'object'
  OR a.intent->'request'->>'project_alias' IS DISTINCT FROM t.project_alias
  OR jsonb_typeof(a.finish_receipt)<>'object'
  OR a.finish_receipt->>'status' IS DISTINCT FROM 'owner_continued'
  OR jsonb_typeof(a.finish_receipt->'receipt')<>'object'
 ) IS DISTINCT FROM false THEN RAISE EXCEPTION 'Invalid owner continuation marker' USING ERRCODE='P0009';END IF;
 SELECT x.* INTO q FROM forge_agent.coding_reviews x
 JOIN forge_agent.coding_tasks y ON(y.owner_id,y.project_alias,y.id)=(x.owner_id,x.project_alias,x.task_id)
 WHERE x.review_id::text=a.intent->>'review_id' AND x.task_id::text=a.intent->'request'->>'task_id'
  AND(x.owner_id,x.project_alias)=(t.owner_id,t.project_alias) AND y.workflow_id=w.workflow_id
  AND coalesce(y.repair_root_task_id,y.id)=w.root_task_id;
 IF NOT FOUND THEN RAISE EXCEPTION 'Owner continuation source denied' USING ERRCODE='P0009';END IF;
 SELECT * INTO s FROM forge_agent.coding_attempts WHERE attempt_id=q.attempt_id
  AND(owner_id,project_alias,task_id)=(q.owner_id,q.project_alias,q.task_id);
 SELECT * INTO rec FROM forge_agent.coding_reconciliation_receipts
  WHERE receipt_id::text=a.intent->'request'->>'reconciliation_receipt_id';
 SELECT * INTO r FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id::text=a.intent->>'peer_review_id'
  AND(owner_id,project_alias,workflow_id)=(t.owner_id,t.project_alias,w.workflow_id)
  AND(source_task_id,source_attempt_id,source_review_id)=(q.task_id,q.attempt_id,q.review_id);
 SELECT * INTO p FROM forge_agent.coding_plan_decisions WHERE decision_id=coalesce(q.plan_decision_id,q.scope_plan_decision_id)
  AND(owner_id,project_alias)=(t.owner_id,t.project_alias);
 BEGIN checked_at:=(rec.stop_proof->>'checked_at')::timestamptz;
 EXCEPTION WHEN invalid_datetime_format OR datetime_field_overflow THEN
  RAISE EXCEPTION 'Owner continuation stop proof malformed' USING ERRCODE='P0009';END;
 IF(s.attempt_id IS NULL OR rec.receipt_id IS NULL OR r.workflow_review_id IS NULL OR p.decision_id IS NULL
  OR s.phase<>'stopped' OR s.reservation_active OR s.lease_expires_at IS NOT NULL
  OR s.result_sha256 IS DISTINCT FROM q.parent_result_sha256
  OR a.intent->>'source_attempt_id' IS DISTINCT FROM s.attempt_id::text
  OR a.intent->>'source_result_sha256' IS DISTINCT FROM s.result_sha256
  OR a.intent->>'source_workspace_digest' IS DISTINCT FROM q.workspace_digest
  OR(rec.owner_id,rec.project_alias,rec.task_id,rec.attempt_id,rec.workspace_digest)
   IS DISTINCT FROM(q.owner_id,q.project_alias,q.task_id,q.attempt_id,q.workspace_digest)
  OR rec.receipt_id=q.reconciliation_receipt_id OR rec.task_revision<>q.source_revision
  OR rec.created_at<r.updated_at OR rec.created_at>a.created_at
  OR checked_at IS NULL OR checked_at<r.updated_at OR checked_at>clock_timestamp()
  OR rec.stop_proof->'scope_stopped' IS DISTINCT FROM 'true'::jsonb
  OR rec.stop_proof->>'source' IS DISTINCT FROM 'systemd'
  OR rec.stop_proof->>'unit_name' IS DISTINCT FROM s.unit_name
  OR a.intent->'request'->>'reconciliation_receipt_id' IS DISTINCT FROM rec.receipt_id::text
  OR a.intent->'request'->'expected_revision' IS DISTINCT FROM to_jsonb(q.source_revision)
  OR a.intent->>'candidate_commit' IS DISTINCT FROM q.candidate_commit
  OR a.intent->>'gate_manifest_sha256' IS DISTINCT FROM q.gate_manifest_sha256
  OR a.intent->>'recipe_digest' IS DISTINCT FROM s.recipe_digest
  OR r.status<>'decided' OR r.target_kind<>'code'
  OR r.source_result_sha256 IS DISTINCT FROM s.result_sha256
  OR r.source_revision<>q.source_revision OR r.candidate_commit IS DISTINCT FROM q.candidate_commit
  OR r.gate_manifest_sha256 IS DISTINCT FROM q.gate_manifest_sha256
  OR r.verdict_json::jsonb->>'decision' IS DISTINCT FROM 'revise'
  OR r.verdict_sha256 IS DISTINCT FROM encode(sha256(convert_to(r.verdict_json,'UTF8')),'hex')
  OR a.intent->>'target_sha256' IS DISTINCT FROM r.target_sha256
  OR a.intent->>'verdict_sha256' IS DISTINCT FROM r.verdict_sha256
  OR a.intent->>'reviewer_attempt_id' IS DISTINCT FROM r.reviewer_attempt_id::text
  OR a.intent->>'reviewer_result_sha256' IS DISTINCT FROM r.reviewer_result_sha256
  OR p.decision<>'resume' OR a.intent->>'plan_decision_id' IS DISTINCT FROM p.decision_id::text
  OR a.intent->>'plan_sha256' IS DISTINCT FROM p.plan_sha256
  OR a.intent->'review_round' IS DISTINCT FROM to_jsonb(q.round)
  OR a.intent->'review_stagnant_rounds' IS DISTINCT FROM to_jsonb(q.stagnant_rounds)
  OR a.intent->'infrastructure_retries' IS DISTINCT FROM to_jsonb(q.infrastructure_retries)
  OR a.intent->'plan_revisions' IS DISTINCT FROM to_jsonb(w.plan_revisions)
  OR a.finish_receipt->'receipt'->>'project_alias' IS DISTINCT FROM t.project_alias
  OR a.finish_receipt->'receipt'->>'task_id' IS DISTINCT FROM q.task_id::text
  OR a.finish_receipt->'receipt'->'base_revision' IS DISTINCT FROM to_jsonb(q.source_revision)
  OR a.finish_receipt->'receipt'->'revision' IS DISTINCT FROM to_jsonb(q.source_revision+1)
  OR a.finish_receipt->'receipt'->>'status' IS DISTINCT FROM 'queued'
  OR a.finish_receipt->'receipt'->>'recipe_digest' IS DISTINCT FROM s.recipe_digest
  OR NOT EXISTS(SELECT FROM forge_agent.coding_control_receipts c
   WHERE(c.owner_id,c.project_alias,c.task_id)=(q.owner_id,q.project_alias,q.task_id)
    AND c.action='resume' AND c.idempotency_key=a.intent->'request'->>'idempotency_key'
    AND c.base_revision=q.source_revision AND c.result_revision=q.source_revision+1
    AND c.receipt_json=a.finish_receipt->'receipt')
 ) IS DISTINCT FROM false THEN RAISE EXCEPTION 'Owner continuation evidence changed' USING ERRCODE='P0009';END IF;
 RETURN true;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_agent._coding_owner_continuation_fenced(uuid) FROM PUBLIC;

CREATE FUNCTION forge_agent.owner_continuation_capability()
RETURNS jsonb LANGUAGE plpgsql STABLE SECURITY DEFINER
SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=26 AND name='coding_owner_continuation') THEN
  RAISE EXCEPTION 'Owner continuation capability unavailable' USING ERRCODE='P0002';END IF;
 RETURN jsonb_build_object('policy','forge-owner-code-resume-v1','schema_version',26,
  'migration','coding_owner_continuation','same_task',true,'repair_fenced',true);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_agent.owner_continuation_capability() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_agent.owner_continuation_capability() TO forge_agent;

CREATE FUNCTION forge_coding_reconciler_api.owner_continuation_capability()
RETURNS jsonb LANGUAGE sql STABLE SECURITY DEFINER
SET search_path=pg_catalog,pg_temp AS $fn$
 SELECT forge_agent.owner_continuation_capability();
$fn$;
REVOKE ALL ON FUNCTION forge_coding_reconciler_api.owner_continuation_capability() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_coding_reconciler_api.owner_continuation_capability() TO forge_coding_reconciler;

CREATE FUNCTION forge_coding_reconciler_api.owner_continuation_fenced(p_scope jsonb)
RETURNS boolean LANGUAGE plpgsql STABLE SECURITY DEFINER
SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE task uuid;
BEGIN
 IF(jsonb_typeof(p_scope)<>'object' OR octet_length(p_scope::text)>1024
  OR p_scope-ARRAY['project_alias','task_id','attempt_id','workflow_id']<>'{}'::jsonb
  OR NOT p_scope?&ARRAY['project_alias','task_id','attempt_id','workflow_id']
 ) IS DISTINCT FROM false THEN RAISE EXCEPTION 'Invalid owner continuation scope' USING ERRCODE='22023';END IF;
 SELECT t.id INTO task FROM forge_agent.coding_tasks t JOIN forge_agent.coding_attempts a
  ON(a.owner_id,a.project_alias,a.task_id)=(t.owner_id,t.project_alias,t.id)
 WHERE t.owner_id=current_setting('forge.coding_owner',true) AND t.project_alias=p_scope->>'project_alias'
  AND t.id::text=p_scope->>'task_id' AND t.workflow_id::text=p_scope->>'workflow_id'
  AND a.attempt_id::text=p_scope->>'attempt_id';
 IF NOT FOUND THEN RAISE EXCEPTION 'Owner continuation scope denied' USING ERRCODE='42501';END IF;
 RETURN forge_agent._coding_owner_continuation_fenced(task);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_coding_reconciler_api.owner_continuation_fenced(jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_coding_reconciler_api.owner_continuation_fenced(jsonb) TO forge_coding_reconciler;

-- Replace only exact, verified M25 predecessors, preserving owner and ACL.
DO $owner_patches$
DECLARE expected record;identity text;definition text;body text;patch jsonb;
BEGIN
 FOR expected IN SELECT * FROM(VALUES
 ('forge_agent.coding_schedule_repair(uuid,uuid)',$identities$["9158dd45316c1521aa8d76200e2136a66d3ba26ad1917ce1264aba4b09dcf2de", "f60557353fea873fe57c22ed0dc6ed067801b1efae8d070d9f344f8d51abe40c"]$identities$::jsonb,$patches$[[" root_id:=coalesce(t.repair_root_task_id,t.id);", " root_id:=coalesce(t.repair_root_task_id,t.id);\n IF forge_agent._coding_owner_continuation_fenced(t.id) THEN\n  RAISE EXCEPTION 'Explicit owner continuation forbids another automatic writer' USING ERRCODE='P0009';END IF;"]]$patches$::jsonb),
 ('forge_coding_reconciler_api.review_finish(uuid,uuid,jsonb)',$identities$["bb96b71d726c1a587d5b4ee67b422ee115c6ebae87848737c5845ae1b3129f57", "6b981695bde7a1f1c905d072f0b678775a99cc7ac08039cbe26e28b27f4705a8"]$identities$::jsonb,$patches$[["v_candidate text;v_gate text;", "v_candidate text;v_gate text;owner_fenced boolean;"], [" SELECT * INTO p FROM forge_agent.coding_review_policy WHERE owner_id=t.owner_id AND project_alias=t.project_alias;", " owner_fenced:=forge_agent._coding_owner_continuation_fenced(t.id);\n IF owner_fenced AND disposition IN('repair','retry_infrastructure') THEN disposition:='handoff';END IF;\n SELECT * INTO p FROM forge_agent.coding_review_policy WHERE owner_id=t.owner_id AND project_alias=t.project_alias;"], ["reason=coalesce(p_outcome->>'reason',''),", "reason=CASE WHEN owner_fenced AND p_outcome->>'disposition' IN('repair','retry_infrastructure') THEN 'owner_continuation_fenced' ELSE coalesce(p_outcome->>'reason','') END,"], [" UPDATE forge_agent.coding_reviews SET finish_receipt=receipt WHERE review_id=q.review_id;", " IF owner_fenced AND p_outcome->>'disposition' IN('repair','retry_infrastructure') THEN receipt:=receipt||jsonb_build_object('reason','owner_continuation_fenced');END IF;\n UPDATE forge_agent.coding_reviews SET finish_receipt=receipt WHERE review_id=q.review_id;"]]$patches$::jsonb),
 ('forge_coding_reconciler_api.native_merge_failure(uuid,uuid,jsonb)',$identities$["1893187612aecee06b87a277f38be8d4b54dd48a93843516bf2b5e76cfbec231"]$identities$::jsonb,$patches$[["checks jsonb;", "checks jsonb;owner_fenced boolean;"], [" seed:=(p_failure->>'source_review_id')::uuid;", " owner_fenced:=forge_agent._coding_owner_continuation_fenced(t.id);\n seed:=(p_failure->>'source_review_id')::uuid;"], ["q.review_id,'repair',p_failure->>'candidate_commit'", "q.review_id,CASE WHEN owner_fenced THEN 'handoff' ELSE 'repair' END,p_failure->>'candidate_commit'"], ["p_failure->>'summary',p_failure->>'reason','source'", "p_failure->>'summary',CASE WHEN owner_fenced THEN 'owner_continuation_fenced' ELSE p_failure->>'reason' END,'source'"], [" child:=forge_agent.coding_schedule_repair(seed,feedback);", " IF NOT owner_fenced THEN child:=forge_agent.coding_schedule_repair(seed,feedback);END IF;"], ["'status','repair','merge_id'", "'status',CASE WHEN owner_fenced THEN 'handoff' ELSE 'repair' END,'merge_id'"], [" UPDATE forge_agent.coding_workflow_actions SET status='handoff',finish_token=p_token,finish_receipt=receipt,", " IF owner_fenced THEN receipt:=receipt||jsonb_build_object('reason','owner_continuation_fenced');END IF;\n UPDATE forge_agent.coding_workflow_actions SET status='handoff',finish_token=p_token,finish_receipt=receipt,"], [" UPDATE forge_agent.coding_workflows SET phase='repair',updated_at=clock_timestamp() WHERE workflow_id=w.workflow_id;", " IF owner_fenced THEN\n  UPDATE forge_agent.coding_workflows SET enabled=false,phase='escalated',escalation_reason='Owner continuation failed delivery; no further automatic writer is authorized.',updated_at=clock_timestamp() WHERE workflow_id=w.workflow_id;\n ELSE\n  UPDATE forge_agent.coding_workflows SET phase='repair',updated_at=clock_timestamp() WHERE workflow_id=w.workflow_id;\n END IF;"]]$patches$::jsonb),
 ('forge_coding_reconciler_api.native_branch_first_replay(uuid,bigint)',$identities$["132e5f4a859c181b90b9fa4f1792f42c1c6603e804258f4084a720222952c6a6"]$identities$::jsonb,$patches$[[" SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p_review FOR UPDATE;", " IF forge_agent._coding_owner_continuation_fenced(t.id) THEN\n  RAISE EXCEPTION 'Owner continuation cannot admit another automatic replay' USING ERRCODE='P0009';END IF;\n SELECT * INTO q FROM forge_agent.coding_reviews WHERE review_id=p_review FOR UPDATE;"]]$patches$::jsonb)
 )x(signature,sha256,patches) LOOP
  SELECT pg_get_functiondef(p.oid),p.prosrc,
   encode(sha256(convert_to(jsonb_build_array(pg_get_functiondef(p.oid),pg_get_userbyid(p.proowner),p.proacl)::text,'UTF8')),'hex')
  INTO definition,body,identity FROM pg_proc p WHERE p.oid=expected.signature::regprocedure;
  IF NOT(expected.sha256 ? identity) THEN RAISE EXCEPTION 'Owner continuation predecessor drift: %',expected.signature;END IF;
  FOR patch IN SELECT value FROM jsonb_array_elements(expected.patches) LOOP
   IF position(patch->>0 IN body)=0 THEN RAISE EXCEPTION 'Owner continuation patch anchor missing';END IF;
   body:=replace(body,patch->>0,patch->>1);
  END LOOP;
  EXECUTE replace(definition,(SELECT p.prosrc FROM pg_proc p WHERE p.oid=expected.signature::regprocedure),body);
 END LOOP;
END;
$owner_patches$;
