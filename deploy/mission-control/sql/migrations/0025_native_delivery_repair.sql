-- M25: source-bound native delivery repair and one bounded branch-first replay.
-- Preserve every previous policy, migration, approval and finish receipt.
SET LOCAL lock_timeout='5s';
SET LOCAL statement_timeout='60s';

CREATE FUNCTION forge_coding_reconciler_api.native_merge_failure(
 p_merge uuid,p_token uuid,p_failure jsonb
) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.coding_workflow_actions%ROWTYPE;w forge_agent.coding_workflows%ROWTYPE;
 p forge_agent.coding_publications%ROWTYPE;q forge_agent.coding_reviews%ROWTYPE;
 t forge_agent.coding_tasks%ROWTYPE;seed uuid;feedback uuid;child uuid;receipt jsonb;checks jsonb;
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
 seed:=(p_failure->>'source_review_id')::uuid;
 INSERT INTO forge_agent.coding_reviews(review_id,owner_id,project_alias,task_id,attempt_id,plan_decision_id,scope_plan_decision_id,
  reconciliation_receipt_id,base_revision,workspace_digest,parent_result_sha256,source_revision,review_no,round,stagnant_rounds,
  infrastructure_retries,previous_review_id,status,candidate_commit,gate_manifest_sha256,findings,verification,summary,reason,phase,owner_decision,
  target_publication_id,source_publication_id)
 VALUES(seed,q.owner_id,q.project_alias,q.task_id,q.attempt_id,q.plan_decision_id,q.scope_plan_decision_id,q.reconciliation_receipt_id,
  p_failure->>'integration_base',q.workspace_digest,q.parent_result_sha256,t.revision,
  (SELECT max(review_no)+1 FROM forge_agent.coding_reviews WHERE owner_id=q.owner_id AND project_alias=q.project_alias AND task_id=q.task_id),
  q.round,q.stagnant_rounds,q.infrastructure_retries,q.review_id,'repair',p_failure->>'candidate_commit',p_failure->>'gate_manifest_sha256',
  p_failure->'findings',jsonb_build_object('source_import',true,'checks_passed',false,'cleanup_confirmed',true,'checks','[]'::jsonb,
   'candidate_workspace_digest',p_failure->>'candidate_workspace_digest','repair_requires_new_workspace',true,
   'integration_base',p_failure->>'integration_base','gate_source_sha256',p_failure->>'gate_source_sha256','delivery_failure',p_failure->'delivery_failure'),
  p_failure->>'summary',p_failure->>'reason','source',jsonb_build_object('decision','repair_source','closed_by','native_merge_failure',
   'merge_id',a.action_id,'tested_review_id',q.review_id),NULL,p.publication_id);
 feedback:=forge_agent.coding_make_feedback(seed,t.id,'repair',p_failure->>'summary',p_failure->'findings');
 UPDATE forge_agent.coding_reviews SET feedback_id=feedback WHERE review_id=seed;
 child:=forge_agent.coding_schedule_repair(seed,feedback);
 -- Supersede delivery, while retaining the old review's approval and receipt.
 UPDATE forge_agent.coding_reviews SET repair_task_id=child WHERE review_id=q.review_id;
 receipt:=jsonb_build_object('policy','forge-native-delivery-repair-v1','status','repair','merge_id',a.action_id,
  'workflow_id',w.workflow_id,'source_review_id',seed,'feedback_id',feedback,'repair_task_id',child,'failure',p_failure);
 UPDATE forge_agent.coding_workflow_actions SET status='handoff',finish_token=p_token,finish_receipt=receipt,
  lease_token=NULL,lease_expires_at=NULL,updated_at=clock_timestamp() WHERE action_id=a.action_id;
 UPDATE forge_agent.coding_workflows SET phase='repair',updated_at=clock_timestamp() WHERE workflow_id=w.workflow_id;
 INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
 SELECT t.owner_id,t.project_alias,t.id,coalesce(max(sequence),0)+1,'checkpoint',receipt
 FROM forge_agent.coding_events WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND task_id=t.id;
 PERFORM pg_notify('forge_coding_jobs','');RETURN receipt;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_coding_reconciler_api.native_merge_failure(uuid,uuid,jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_coding_reconciler_api.native_merge_failure(uuid,uuid,jsonb) TO forge_coding_reconciler;

CREATE FUNCTION forge_coding_reconciler_api.native_branch_first_replay(p_review uuid,p_revision bigint)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
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
 SELECT t.owner_id,t.project_alias,t.id,coalesce(max(sequence),0)+1,'checkpoint',receipt FROM forge_agent.coding_events
 WHERE owner_id=t.owner_id AND project_alias=t.project_alias AND task_id=t.id;
 PERFORM pg_notify('forge_coding_jobs','');RETURN receipt;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_coding_reconciler_api.native_branch_first_replay(uuid,bigint) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_coding_reconciler_api.native_branch_first_replay(uuid,bigint) TO forge_coding_reconciler;

CREATE FUNCTION forge_coding_worker_api.read_native_delivery_context(p_attempt uuid,p_token uuid)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE validated jsonb;value jsonb;
BEGIN
 -- Reuse exact attempt-role, lease, immutable-feedback and source admission.
 validated:=forge_coding_worker_api.read_feedback(p_attempt,p_token);
 IF validated IS NULL OR validated->'feedback' IS NULL OR validated->'feedback'='null'::jsonb THEN RETURN NULL;END IF;
 SELECT jsonb_build_object('feedback_id',f.feedback_id,'feedback_sha256',f.feedback_sha256,'review_id',q.review_id,
  'source_task_id',q.task_id,'source_attempt_id',q.attempt_id,'candidate_commit',q.candidate_commit,
  'gate_manifest_sha256',q.gate_manifest_sha256,'delivery_failure',q.verification->'delivery_failure') INTO value
 FROM forge_agent.coding_feedback f JOIN forge_agent.coding_reviews q ON q.review_id=f.review_id
 WHERE f.feedback_id=(validated->'feedback'->>'feedback_id')::uuid
  AND(f.owner_id,f.project_alias)=(q.owner_id,q.project_alias)
  AND f.owner_id=current_setting('forge.coding_owner',true)
  AND f.feedback_sha256=encode(sha256(convert_to(f.feedback_json,'UTF8')),'hex')
  AND jsonb_typeof(q.verification->'delivery_failure')='object';
 RETURN value;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_coding_worker_api.read_native_delivery_context(uuid,uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_coding_worker_api.read_native_delivery_context(uuid,uuid) TO forge_coding_worker;

CREATE FUNCTION forge_coding_reconciler_api.native_branch_published(p_review uuid,p_token uuid,p_receipt jsonb)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
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
 v_payload:=p_receipt||jsonb_build_object('stage','branch_pushed','review_id',p_review);
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
$fn$;
REVOKE ALL ON FUNCTION forge_coding_reconciler_api.native_branch_published(uuid,uuid,jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_coding_reconciler_api.native_branch_published(uuid,uuid,jsonb) TO forge_coding_reconciler;
