-- M31 delivery retries belong to unresolved failure events; totals remain history.
ALTER TABLE forge_agent.delivery_lineages DROP CONSTRAINT delivery_lineages_rounds_check;
ALTER TABLE forge_agent.delivery_lineages ADD CONSTRAINT delivery_lineages_rounds_check CHECK(rounds >= 0);
ALTER TABLE forge_agent.delivery_task_lineages DROP CONSTRAINT delivery_task_lineages_round_check;
ALTER TABLE forge_agent.delivery_task_lineages ADD CONSTRAINT delivery_task_lineages_round_check CHECK(round >= 0);
ALTER TABLE forge_agent.delivery_operations DROP CONSTRAINT delivery_operations_round_check;
ALTER TABLE forge_agent.delivery_operations ADD CONSTRAINT delivery_operations_round_check CHECK(round >= 0);

CREATE TABLE forge_agent.delivery_retry_event_links(
 verification_id uuid PRIMARY KEY REFERENCES forge_agent.delivery_ledger,
 event_id uuid NOT NULL REFERENCES forge_agent.coding_retry_events(event_id)
);
REVOKE ALL ON forge_agent.delivery_retry_event_links FROM PUBLIC;

CREATE FUNCTION forge_agent.delivery_retry_observe(p_verification uuid) RETURNS jsonb
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE d forge_agent.delivery_ledger%ROWTYPE;kind text;passed text[]:=ARRAY[]::text[];
 phase text:='';passed_phases text[]:=ARRAY[]::text[];
 evidence text;event jsonb;linked uuid;all_kinds text[]:=ARRAY['compilation','tests','browser','lint','formatting','verification','infrastructure'];
BEGIN
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=p_verification;
 IF FOUND AND d.verification_status NOT IN('passed','failed') THEN RETURN NULL;END IF;
 IF NOT FOUND OR d.owner_id IS DISTINCT FROM current_setting('forge.coding_owner',true) THEN
  RAISE EXCEPTION 'Retry event delivery scope denied' USING ERRCODE='42501';END IF;
 evidence:=encode(sha256(convert_to(forge_agent.delivery_canonical(d.verification_receipt),'UTF8')),'hex');
 IF d.verification_status='passed' THEN
  FOREACH kind IN ARRAY all_kinds LOOP
   PERFORM forge_agent.retry_event_resolve(d.project_alias,'delivery',d.lineage_id,kind,d.verification_id::text,evidence);
  END LOOP;
  RETURN NULL;
 END IF;
 SELECT event_id INTO linked FROM forge_agent.delivery_retry_event_links WHERE verification_id=d.verification_id;
 IF FOUND THEN RETURN forge_agent.retry_event_get(linked);END IF;
 -- New receipts have fixed, directly executed phase witnesses. Legacy receipts
 -- retain an explicit verification event; diagnostics never mint a new budget.
 IF d.verification_receipt->>'verification_profile'='forge-sdlc-jenkins-v3'
 AND d.verification_receipt->'result_version'='3'::jsonb
 AND d.verification_receipt->>'failure_kind'=ANY(all_kinds)
 AND jsonb_typeof(d.verification_receipt->'passed_kinds')='array' THEN
  kind:=d.verification_receipt->>'failure_kind';
  SELECT coalesce(array_agg(value ORDER BY value),ARRAY[]::text[]) INTO passed
   FROM jsonb_array_elements_text(d.verification_receipt->'passed_kinds') value;
  IF EXISTS(SELECT FROM unnest(passed) value WHERE NOT value=ANY(all_kinds))
  OR kind=ANY(passed) OR cardinality(passed)<>(SELECT count(DISTINCT value) FROM unnest(passed) value) THEN
   RAISE EXCEPTION 'Invalid trusted phase witness' USING ERRCODE='P0009';END IF;
  SELECT item->>'phase_id' INTO phase
   FROM jsonb_array_elements(d.verification_receipt->'phase_results') WITH ORDINALITY phase_rows(item,ordinality)
   WHERE (item->>'returncode')::integer<>0 ORDER BY ordinality LIMIT 1;
  IF d.verification_receipt->>'failure' IS DISTINCT FROM 'source_changed_during_checks' THEN
   SELECT coalesce(array_agg(item->>'phase_id' ORDER BY ordinality),ARRAY[]::text[]) INTO passed_phases
    FROM jsonb_array_elements(d.verification_receipt->'phase_results') WITH ORDINALITY phase_rows(item,ordinality)
    WHERE (item->>'returncode')::integer=0;
  END IF;
 ELSE
  kind:='verification';
 END IF;
 event:=forge_agent.retry_event_observe(d.project_alias,'delivery',d.lineage_id,kind,d.verification_id::text,evidence,passed,phase,passed_phases);
 INSERT INTO forge_agent.delivery_retry_event_links(verification_id,event_id) VALUES(d.verification_id,(event->>'event_id')::uuid);
 RETURN event;
END;$fn$;
REVOKE ALL ON FUNCTION forge_agent.delivery_retry_observe(uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION forge_agent.delivery_enqueue_successors(p_verification uuid) RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE d forge_agent.delivery_ledger%ROWTYPE;head forge_agent.delivery_project_main_heads%ROWTYPE;
BEGIN
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=p_verification;
 SELECT * INTO head FROM forge_agent.delivery_project_main_heads WHERE(owner_id,project_alias)=(d.owner_id,d.project_alias);
 IF head.commit_sha IS NOT NULL AND head.commit_sha<>d.commit_sha THEN RETURN;END IF;
 PERFORM forge_agent.delivery_retry_observe(d.verification_id);
 IF d.verification_status='failed' THEN
  INSERT INTO forge_agent.delivery_operations(verification_id,owner_id,project_alias,kind,occurrence) VALUES(d.verification_id,d.owner_id,d.project_alias,'repair',coalesce(head.generation,0)) ON CONFLICT(verification_id,kind,occurrence) DO NOTHING;
 ELSIF d.verification_status='passed' THEN
  INSERT INTO forge_agent.delivery_operations(verification_id,owner_id,project_alias,kind,occurrence) VALUES(d.verification_id,d.owner_id,d.project_alias,'release',coalesce(head.generation,0)) ON CONFLICT(verification_id,kind,occurrence) DO NOTHING;
 END IF;
 PERFORM pg_notify('forge_coding_jobs','');
END;$fn$;

CREATE OR REPLACE FUNCTION forge_agent.delivery_repair_claim(p_worker text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE job jsonb;a forge_agent.delivery_operations%ROWTYPE;d forge_agent.delivery_ledger%ROWTYPE;l forge_agent.delivery_lineages%ROWTYPE;h forge_agent.delivery_project_main_heads%ROWTYPE;existing jsonb;original jsonb;event jsonb;
BEGIN
 job:=forge_agent.delivery_operation_claim('repair',p_worker);IF job IS NULL THEN RETURN NULL;END IF;
 SELECT * INTO a FROM forge_agent.delivery_operations WHERE operation_id=(job->>'operation_id')::uuid;
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=a.verification_id;
 SELECT * INTO l FROM forge_agent.delivery_lineages WHERE lineage_id=d.lineage_id FOR UPDATE;
 SELECT * INTO h FROM forge_agent.delivery_project_main_heads WHERE(owner_id,project_alias)=(d.owner_id,d.project_alias);
 IF a.request IS NULL AND h.commit_sha IS NOT NULL AND h.commit_sha<>d.commit_sha THEN
  PERFORM forge_agent.delivery_operation_finish(a.operation_id,a.generation,a.lease_token,'admitted',jsonb_build_object('reason','source_superseded'),'A newer main source superseded this repair before any model admission.');RETURN NULL;END IF;
 event:=forge_agent.delivery_retry_observe(d.verification_id);
 IF a.request IS NULL AND (event IS NULL OR (event->>'used')::integer>=20) THEN
  IF event IS NOT NULL THEN PERFORM forge_agent.retry_event_reserve((event->>'event_id')::uuid,'delivery-repair.'||a.operation_id::text);END IF;
  UPDATE forge_agent.delivery_operations SET round=l.rounds WHERE operation_id=a.operation_id;
  PERFORM forge_agent.delivery_operation_finish(a.operation_id,a.generation,a.lease_token,'needs_attention',
   jsonb_build_object('reason','retry_event_exhausted','retry_event',event),
   'This failure event exhausted its twenty repair attempts; other event budgets are independent.');RETURN NULL;END IF;
 IF a.request IS NOT NULL THEN
  SELECT e.payload INTO existing FROM forge_agent.coding_tasks t JOIN forge_agent.coding_events e ON(e.owner_id,e.project_alias,e.task_id)=(t.owner_id,t.project_alias,t.id) AND e.sequence=1
  WHERE(t.owner_id,t.project_alias,t.idempotency_key)=(a.owner_id,a.project_alias,a.request->'command'->>'idempotency_key') AND t.intent_digest=a.request->>'intent_digest';
 END IF;
 SELECT jsonb_build_object('prompt',left(prompt,1500),'acceptance_criteria',left(acceptance_criteria,512)) INTO original FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(d.owner_id,d.project_alias,d.root_task_id);
 RETURN job||jsonb_build_object('round',CASE WHEN a.round=0 THEN l.rounds+1 ELSE a.round END,'max_rounds',20,'retry_event',event,'base_revision',coalesce(h.commit_sha,d.commit_sha),'existing_receipt',existing,'original',original);
END;$fn$;

CREATE OR REPLACE FUNCTION forge_agent.delivery_repair_checkpoint(p_id uuid,p_generation integer,p_token uuid,p_intent jsonb) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.delivery_operations%ROWTYPE;d forge_agent.delivery_ledger%ROWTYPE;l forge_agent.delivery_lineages%ROWTYPE;c jsonb;event jsonb;keys text[]:=ARRAY['project_alias','base_revision','mode','agent_profile','prompt','acceptance_criteria','idempotency_key','auto_repair','workflow_policy_digest'];
BEGIN
 a:=forge_agent.delivery_operation_lock(p_id);PERFORM forge_agent.delivery_operation_renew(p_id,p_generation,p_token);
 IF a.kind<>'repair' OR a.cancel_requested OR NOT forge_agent.delivery_capable(a.project_alias,'repair') THEN RAISE EXCEPTION 'Repair admission capability denied' USING ERRCODE='42501';END IF;
 IF a.request IS NOT NULL THEN IF a.request IS DISTINCT FROM p_intent THEN RAISE EXCEPTION 'Repair intent conflict' USING ERRCODE='P0009';END IF;RETURN true;END IF;
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=a.verification_id;
 SELECT * INTO l FROM forge_agent.delivery_lineages WHERE lineage_id=d.lineage_id FOR UPDATE;
 c:=p_intent->'command';
 IF jsonb_typeof(p_intent) IS DISTINCT FROM 'object' OR p_intent-ARRAY['command','intent_digest']<>'{}'::jsonb OR NOT p_intent?&ARRAY['command','intent_digest']
 OR jsonb_typeof(c) IS DISTINCT FROM 'object' OR c-keys<>'{}'::jsonb OR NOT c?&keys OR c->>'project_alias' IS DISTINCT FROM a.project_alias
 OR c->>'mode' IS DISTINCT FROM 'plan' OR c->'auto_repair' IS DISTINCT FROM 'false'::jsonb
 OR c->>'idempotency_key' IS DISTINCT FROM 'delivery-repair.'||p_id::text OR(c->>'workflow_policy_digest'~'^[0-9a-f]{64}$') IS DISTINCT FROM true
 OR(c->>'base_revision'~'^[0-9a-f]{40}$') IS DISTINCT FROM true
 OR jsonb_typeof(c->'prompt') IS DISTINCT FROM 'string' OR octet_length(c->>'prompt') NOT BETWEEN 1 AND 8192
 OR jsonb_typeof(c->'acceptance_criteria') IS DISTINCT FROM 'string' OR octet_length(c->>'acceptance_criteria')>2048
 OR p_intent->>'intent_digest' IS DISTINCT FROM encode(sha256(convert_to(forge_agent.delivery_canonical(c-ARRAY['idempotency_key','auto_repair']),'UTF8')),'hex')
 THEN RAISE EXCEPTION 'Invalid frozen normal repair admission' USING ERRCODE='22023';END IF;
 event:=forge_agent.delivery_retry_observe(d.verification_id);
 IF event IS NULL THEN RAISE EXCEPTION 'Verified failure event required' USING ERRCODE='P0009';END IF;
 event:=forge_agent.retry_event_reserve((event->>'event_id')::uuid,'delivery-repair.'||p_id::text);
 IF event->>'status'='exhausted' THEN
  PERFORM forge_agent.delivery_operation_finish(p_id,p_generation,p_token,'needs_attention',
   jsonb_build_object('reason','retry_event_exhausted','retry_event',event),
   'This failure event exhausted its twenty repair attempts; other event budgets are independent.');
  RETURN false;
 END IF;
 UPDATE forge_agent.delivery_lineages SET rounds=rounds+1 WHERE lineage_id=d.lineage_id RETURNING * INTO l;
 UPDATE forge_agent.delivery_operations SET request=p_intent,round=l.rounds,updated_at=clock_timestamp() WHERE operation_id=p_id;RETURN true;
END;$fn$;

CREATE FUNCTION forge_agent.delivery_retry_projection(p_project text,p_task uuid DEFAULT NULL) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE d forge_agent.delivery_ledger%ROWTYPE;r forge_agent.delivery_operations%ROWTYPE;s forge_agent.delivery_operations%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;lineage uuid;repair_status text;release_status text;verification_status text;retry_event jsonb;
BEGIN
 IF p_task IS NULL THEN
  SELECT l.* INTO d FROM forge_agent.delivery_project_main_heads h JOIN forge_agent.delivery_ledger l USING(verification_id) WHERE h.owner_id=current_setting('forge.coding_owner',true) AND h.project_alias=p_project;
  IF NOT FOUND THEN SELECT * INTO d FROM forge_agent.delivery_ledger WHERE owner_id=current_setting('forge.coding_owner',true) AND project_alias=p_project ORDER BY created_at DESC,verification_id DESC LIMIT 1;END IF;
 ELSE
  PERFORM 1 FROM forge_agent.coding_tasks WHERE owner_id=current_setting('forge.coding_owner',true) AND project_alias=p_project AND id=p_task;
  IF NOT FOUND THEN RETURN NULL;END IF;
  SELECT lineage_id INTO lineage FROM forge_agent.delivery_task_lineages WHERE owner_id=current_setting('forge.coding_owner',true) AND project_alias=p_project AND task_id=p_task;
  IF lineage IS NULL THEN RETURN NULL;END IF;
  SELECT * INTO d FROM forge_agent.delivery_ledger WHERE owner_id=current_setting('forge.coding_owner',true) AND project_alias=p_project AND lineage_id=lineage ORDER BY created_at DESC,verification_id DESC LIMIT 1;
  IF d.source_kind='observed' AND NOT EXISTS(SELECT FROM forge_agent.delivery_post_merge_links WHERE(owner_id,project_alias,root_task_id,verification_id)=(d.owner_id,d.project_alias,p_task,d.verification_id)) THEN RETURN NULL;END IF;
 END IF;
 IF d.verification_id IS NULL THEN RETURN NULL;END IF;
 SELECT forge_agent.retry_event_get(x.event_id) INTO retry_event FROM forge_agent.delivery_retry_event_links x WHERE x.verification_id=d.verification_id;
 SELECT * INTO r FROM forge_agent.delivery_operations WHERE verification_id=d.verification_id AND kind='repair' ORDER BY created_at DESC,operation_id DESC LIMIT 1;
 SELECT * INTO s FROM forge_agent.delivery_operations WHERE verification_id=d.verification_id AND kind='release' ORDER BY created_at DESC,operation_id DESC LIMIT 1;
 IF r.task_id IS NOT NULL THEN SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(d.owner_id,d.project_alias,r.task_id);END IF;
 repair_status:=CASE WHEN r.operation_id IS NULL THEN 'none' WHEN r.status='needs_attention' THEN 'needs_attention'
 WHEN r.status='superseded' THEN 'completed' WHEN r.status='admitted' AND r.task_id IS NULL THEN 'completed' WHEN r.status='admitted' AND t.status='completed' THEN 'completed'
 WHEN r.status='admitted' AND t.status='queued' THEN 'queued' WHEN r.status='admitted' AND t.status IN('running','awaiting_review','cancel_requested') THEN 'running'
 WHEN r.status='admitted' THEN 'needs_attention' WHEN r.status='leased' THEN 'running' ELSE 'queued' END;
 release_status:=CASE WHEN s.receipt->'operator_required'='true'::jsonb THEN 'failed' WHEN s.operation_id IS NULL THEN 'none' WHEN s.status IN('deployed','failed','superseded') THEN s.status
 WHEN s.request IS NOT NULL THEN 'deploying' WHEN s.status='leased' THEN 'building' ELSE 'queued' END;
 verification_status:=CASE WHEN d.superseded_by IS NOT NULL THEN 'superseded' WHEN d.verification_status='leased' THEN 'running' WHEN d.verification_status='pending' AND d.verification_receipt->'build_number' IS NOT NULL THEN 'running' WHEN d.verification_status='pending' THEN 'queued' ELSE d.verification_status END;
 RETURN jsonb_build_object('verification_id',d.verification_id,'source_kind',d.source_kind,'commit_sha',d.commit_sha,'tree_sha',d.tree_sha,
 'updated_at',forge_agent.delivery_timestamp(greatest(d.updated_at,coalesce(r.updated_at,d.updated_at),coalesce(s.updated_at,d.updated_at),coalesce(t.updated_at,d.updated_at))),
 'verification',jsonb_strip_nulls(jsonb_build_object('status',verification_status,'updated_at',forge_agent.delivery_timestamp(d.updated_at),'summary',left(coalesce(nullif(d.verification_summary,''),'Main verification is queued.'),512),'build_number',d.verification_receipt->'build_number')),
 'repair',jsonb_strip_nulls(jsonb_build_object('status',repair_status,'round',coalesce(r.round,0),'max_rounds',20,'retry_event',retry_event,'task_id',r.task_id,'updated_at',forge_agent.delivery_timestamp(coalesce(r.updated_at,d.updated_at)),'summary',left(coalesce(nullif(r.summary,''),'No repair assignment is required.'),512))),
 'release',jsonb_strip_nulls(jsonb_build_object('status',release_status,'release_id',s.operation_id,'receipt_sha256',s.receipt->>'activation_receipt_sha256','updated_at',forge_agent.delivery_timestamp(coalesce(s.updated_at,d.updated_at)),'summary',left(CASE WHEN s.receipt->'operator_required'='true'::jsonb THEN 'Portal recovery requires operator attention; cleanup remains unconfirmed.' ELSE coalesce(nullif(s.summary,''),'No release is queued.') END,512))));
END;$fn$;
REVOKE ALL ON FUNCTION forge_agent.delivery_retry_projection(text,uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_agent.delivery_retry_projection(text,uuid) TO forge_agent;
