-- M43 (phase 0d): stop re-hashing delivery plans and repair intents in SQL.
--
-- delivery_release_checkpoint and delivery_repair_checkpoint recomputed
-- SHA-256 over forge_agent.delivery_canonical(jsonb) and compared it with
-- the plan_sha256 / intent_digest that Python computed over json.dumps.
-- Any serializer difference refused a valid frozen plan or repair intent
-- as "invalid". The digest fields keep their 64-hex format check; the
-- structural checks (exact key sets, identity containment, field formats)
-- and the jsonb conflict check against the stored request are unchanged.
-- delivery_canonical itself stays: it still derives retry evidence keys,
-- which are identifiers and move to UUIDs in a later step.
--
-- Additive and safe to apply after M42. CREATE OR REPLACE keeps owner and
-- ACLs; bodies are the live M41 definitions with only the compare changed.

CREATE OR REPLACE FUNCTION forge_coding_reconciler_api.delivery_release_checkpoint(p_id uuid, p_generation integer, p_token uuid, p_plan jsonb)
 RETURNS boolean
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE a forge_agent.delivery_operations%ROWTYPE;d forge_agent.delivery_ledger%ROWTYPE;identity jsonb;keys text[]:=ARRAY['version','policy','release_id','verification_id','owner_id','project_alias','commit_sha','tree_sha','ci_action_id','ci_receipt_sha256','gate_manifest_sha256','artifact_manifest_sha256','plan_sha256'];
BEGIN
 a:=forge_agent.delivery_operation_lock(p_id);PERFORM forge_agent.delivery_operation_renew(p_id,p_generation,p_token);
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=a.verification_id;
 IF a.kind<>'release' OR d.verification_status<>'passed' OR((a.cancel_requested OR a.deadline_at<=clock_timestamp() OR NOT forge_agent.delivery_capable(a.project_alias,'release')) AND NOT(a.actor_started AND a.action_role=session_user)) THEN RAISE EXCEPTION 'Release start denied' USING ERRCODE='42501';END IF;
 identity:=jsonb_build_object('version',1,'policy','forge-portal-web-only-v1','release_id',p_id,'verification_id',d.verification_id,'owner_id',d.owner_id,'project_alias',d.project_alias,'commit_sha',d.commit_sha,'tree_sha',d.tree_sha,'ci_action_id',d.verification_receipt->>'action_id','ci_receipt_sha256',d.verification_receipt->>'check_receipt_sha256','gate_manifest_sha256',d.verification_receipt->>'gate_manifest_sha256');
 IF jsonb_typeof(p_plan) IS DISTINCT FROM 'object' OR p_plan-keys<>'{}'::jsonb OR NOT p_plan?&keys OR NOT p_plan@>identity
 OR(p_plan->>'artifact_manifest_sha256'~'^[0-9a-f]{64}$') IS DISTINCT FROM true
 OR(p_plan->>'plan_sha256'~'^[0-9a-f]{64}$') IS DISTINCT FROM true
 THEN RAISE EXCEPTION 'Invalid frozen release plan' USING ERRCODE='22023';END IF;
 IF a.request IS NOT NULL AND a.request IS DISTINCT FROM p_plan THEN RAISE EXCEPTION 'Release plan conflict' USING ERRCODE='P0009';END IF;
 UPDATE forge_agent.delivery_operations SET request=p_plan,updated_at=clock_timestamp() WHERE operation_id=p_id;RETURN true;
END;$function$;

CREATE OR REPLACE FUNCTION forge_agent.delivery_repair_checkpoint(p_id uuid, p_generation integer, p_token uuid, p_intent jsonb)
 RETURNS boolean
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
DECLARE a forge_agent.delivery_operations%ROWTYPE;d forge_agent.delivery_ledger%ROWTYPE;l forge_agent.delivery_lineages%ROWTYPE;c jsonb;event jsonb;keys text[]:=ARRAY['project_alias','base_revision','mode','agent_profile','prompt','acceptance_criteria','idempotency_key','auto_repair','workflow_policy_digest'];
BEGIN
 a:=forge_agent.delivery_operation_lock(p_id);PERFORM forge_agent.delivery_operation_renew(p_id,p_generation,p_token);
 IF a.kind<>'repair' OR a.cancel_requested OR NOT forge_agent.delivery_capable(a.project_alias,'repair') THEN RAISE EXCEPTION 'Repair admission capability denied' USING ERRCODE='42501';END IF;
 IF EXISTS(SELECT FROM forge_agent.delivery_ledger ledger_row WHERE ledger_row.verification_id=a.verification_id AND forge_agent._delivery_lineage_blocked(a.owner_id,a.project_alias,ledger_row.lineage_id)) THEN
  PERFORM forge_agent.delivery_operation_finish(a.operation_id,a.generation,a.lease_token,'needs_attention',jsonb_build_object('reason','delivery_lineage_conflict'),
   'Confirmed repair lineage conflicts with existing observed work; retained counters and receipts require reconciliation.');RETURN false;END IF;
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
 OR(p_intent->>'intent_digest'~'^[0-9a-f]{64}$') IS DISTINCT FROM true
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
END;$function$;
