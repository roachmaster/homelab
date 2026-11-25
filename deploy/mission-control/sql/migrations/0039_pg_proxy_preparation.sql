-- M39: Postgres proxy preparation (additive; changes no existing behavior).
-- Creates the audit log and entry points, grants the
-- writer principal the mutating external EXECUTE, grants
-- each caller role SELECT on its allowlisted read tables
-- (plus schema USAGE), and resolves the
-- writer identity to the muse_events grant in _scope/_locked/finish.
-- The forge_pg_proxy and muse_proxy_writer roles are created by
-- provision.sql (run before apply.sql; forge_agent_owner has no
-- CREATEROLE, so the migration cannot create them itself).

-- Proxy schemas. forge_pg_proxy holds the audit log;
-- forge_pg_proxy_api holds the entry points. Both owned by
-- forge_agent_owner, like the other API objects.
CREATE SCHEMA forge_pg_proxy AUTHORIZATION forge_agent_owner;
CREATE SCHEMA forge_pg_proxy_api AUTHORIZATION forge_agent_owner;
REVOKE ALL ON SCHEMA forge_pg_proxy, forge_pg_proxy_api FROM PUBLIC;
GRANT USAGE ON SCHEMA forge_pg_proxy_api TO forge_agent, forge_coding_worker, forge_coding_reconciler, forge_gpu_worker, muse_proxy_writer;

CREATE TABLE forge_pg_proxy.audit(
 audit_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
 recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 db_role name NOT NULL,
 actor_kind text NOT NULL,
 actor_id text NOT NULL,
 operation text NOT NULL,
 target jsonb NOT NULL
);
REVOKE ALL ON forge_pg_proxy.audit FROM PUBLIC;

CREATE FUNCTION forge_pg_proxy_api.audit(p_actor_kind text, p_actor_id text, p_operation text, p_target jsonb)
RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE id uuid;
BEGIN
 INSERT INTO forge_pg_proxy.audit (db_role, actor_kind, actor_id, operation, target)
   VALUES (session_user, p_actor_kind, p_actor_id, p_operation, p_target)
   RETURNING audit_id INTO id;
 RETURN id;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.audit(text, text, text, jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.audit(text, text, text, jsonb) TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.audit(text, text, text, jsonb) TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.audit(text, text, text, jsonb) TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.audit(text, text, text, jsonb) TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.audit(text, text, text, jsonb) TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_task_insert(p_owner_id text, p_project_alias text, p_id uuid, p_idempotency_key text, p_intent_digest text, p_base_revision text, p_mode text, p_agent_profile text, p_prompt text, p_acceptance_criteria text, p_pinned_recipe jsonb, p_recipe_digest text, p_title text, p_auto_repair boolean, p_review_required boolean, p_workflow_required boolean, p_workflow_id uuid, p_workflow_review_id uuid, p_review_target_sha256 text, p_parent_task_id uuid, p_repair_source_review_id uuid, p_repair_candidate_commit text, p_scope_plan_decision_id uuid, p_repair_root_task_id uuid)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.coding_tasks%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.coding_tasks (owner_id, project_alias, id, idempotency_key, intent_digest, base_revision, mode, agent_profile, prompt, acceptance_criteria, pinned_recipe, recipe_digest, title, auto_repair, review_required, workflow_required, workflow_id, workflow_review_id, review_target_sha256, parent_task_id, repair_source_review_id, repair_candidate_commit, scope_plan_decision_id, repair_root_task_id)
    VALUES (p_owner_id, p_project_alias, p_id, p_idempotency_key, p_intent_digest, p_base_revision, p_mode, p_agent_profile, p_prompt, p_acceptance_criteria, p_pinned_recipe, p_recipe_digest, p_title, COALESCE(p_auto_repair, false), COALESCE(p_review_required, true), COALESCE(p_workflow_required, false), p_workflow_id, p_workflow_review_id, p_review_target_sha256, p_parent_task_id, p_repair_source_review_id, p_repair_candidate_commit, p_scope_plan_decision_id, p_repair_root_task_id)
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_task_insert(text, text, uuid, text, text, text, text, text, text, text, jsonb, text, text, boolean, boolean, boolean, uuid, uuid, text, uuid, uuid, text, uuid, uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_task_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_task_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_task_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_task_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_task_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_workflow_insert(p_owner_id text, p_project_alias text, p_root_task_id uuid, p_policy_digest text, p_snapshot jsonb)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.coding_workflows%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.coding_workflows (owner_id, project_alias, root_task_id, policy_digest, snapshot)
    VALUES (p_owner_id, p_project_alias, p_root_task_id, p_policy_digest, p_snapshot)
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_workflow_insert(text, text, uuid, text, jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_event_insert(p_owner_id text, p_project_alias text, p_task_id uuid, p_event_kind text, p_payload jsonb, p_progress_attempt_id uuid, p_progress_sequence integer, p_correlation_id uuid, p_first boolean)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.coding_events%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.coding_events (owner_id, project_alias, task_id, event_kind, payload, sequence, progress_attempt_id, progress_sequence, correlation_id)
    VALUES (p_owner_id, p_project_alias, p_task_id, p_event_kind, p_payload, CASE WHEN COALESCE(p_first, false) THEN 1 ELSE (SELECT COALESCE(MAX(sequence), 0) + 1 FROM forge_agent.coding_events WHERE owner_id = p_owner_id AND project_alias = p_project_alias AND task_id = p_task_id) END, p_progress_attempt_id, p_progress_sequence, COALESCE(p_correlation_id, gen_random_uuid()))
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_event_insert(text, text, uuid, text, jsonb, uuid, integer, uuid, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_event_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_event_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_event_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_event_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_event_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_control_receipt_insert(p_owner_id text, p_project_alias text, p_task_id uuid, p_action text, p_idempotency_key text, p_intent_digest text, p_base_revision bigint, p_result_revision bigint, p_receipt_json jsonb)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.coding_control_receipts%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.coding_control_receipts (owner_id, project_alias, task_id, action, idempotency_key, intent_digest, base_revision, result_revision, receipt_json)
    VALUES (p_owner_id, p_project_alias, p_task_id, p_action, p_idempotency_key, p_intent_digest, p_base_revision, p_result_revision, p_receipt_json)
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_control_receipt_insert(text, text, uuid, text, text, text, bigint, bigint, jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_control_receipt_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_control_receipt_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_control_receipt_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_control_receipt_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_control_receipt_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_review_insert(p_owner_id text, p_project_alias text, p_task_id uuid, p_attempt_id uuid, p_plan_decision_id uuid, p_scope_plan_decision_id uuid, p_reconciliation_receipt_id uuid, p_base_revision text, p_workspace_digest text, p_parent_result_sha256 text, p_source_revision bigint, p_round integer, p_previous_review_id uuid, p_candidate_commit text, p_summary text, p_phase text, p_owner_decision jsonb, p_findings jsonb, p_verification jsonb, p_target_publication_id uuid, p_source_publication_id uuid, p_gate_manifest_sha256 text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.coding_reviews%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.coding_reviews (owner_id, project_alias, task_id, attempt_id, reconciliation_receipt_id, base_revision, workspace_digest, parent_result_sha256, source_revision, review_no, round, plan_decision_id, scope_plan_decision_id, previous_review_id, candidate_commit, summary, phase, owner_decision, findings, verification, target_publication_id, source_publication_id, gate_manifest_sha256)
    VALUES (p_owner_id, p_project_alias, p_task_id, p_attempt_id, p_reconciliation_receipt_id, p_base_revision, p_workspace_digest, p_parent_result_sha256, p_source_revision, (SELECT COALESCE(MAX(review_no), 0) + 1 FROM forge_agent.coding_reviews WHERE owner_id = p_owner_id AND project_alias = p_project_alias AND task_id = p_task_id), COALESCE(p_round, (SELECT b.repair_rounds FROM forge_agent.coding_review_budget b JOIN forge_agent.coding_tasks t ON (t.owner_id, t.project_alias, COALESCE(t.repair_root_task_id, t.id)) = (b.owner_id, b.project_alias, b.root_task_id) WHERE t.owner_id = p_owner_id AND t.project_alias = p_project_alias AND t.id = p_task_id), 0), p_plan_decision_id, p_scope_plan_decision_id, p_previous_review_id, p_candidate_commit, COALESCE(p_summary, ''::text), COALESCE(p_phase, 'local'::text), p_owner_decision, COALESCE(p_findings, '[]'::jsonb), COALESCE(p_verification, '{}'::jsonb), p_target_publication_id, p_source_publication_id, p_gate_manifest_sha256)
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_review_insert(text, text, uuid, uuid, uuid, uuid, uuid, text, text, text, bigint, integer, uuid, text, text, text, jsonb, jsonb, jsonb, uuid, uuid, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_plan_decision_insert(p_decision_id uuid, p_owner_id text, p_project_alias text, p_task_id uuid, p_plan_attempt_id uuid, p_plan_sha256 text, p_source_revision bigint, p_base_revision text, p_decision text, p_agent_profile text, p_note text, p_idempotency_key text, p_receipt_json jsonb)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.coding_plan_decisions%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.coding_plan_decisions (decision_id, owner_id, project_alias, task_id, plan_attempt_id, plan_sha256, source_revision, base_revision, decision, agent_profile, note, idempotency_key, receipt_json)
    VALUES (p_decision_id, p_owner_id, p_project_alias, p_task_id, p_plan_attempt_id, p_plan_sha256, p_source_revision, p_base_revision, p_decision, p_agent_profile, p_note, p_idempotency_key, p_receipt_json)
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_plan_decision_insert(uuid, text, text, uuid, uuid, text, bigint, text, text, text, text, text, jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_plan_decision_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_plan_decision_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_plan_decision_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_plan_decision_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_plan_decision_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_review_policy_upsert(p_owner_id text, p_project_alias text, p_version text, p_max_rounds integer, p_max_stagnant_rounds integer, p_max_infrastructure_retries integer, p_required_hosted_checks text[])
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.coding_review_policy%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.coding_review_policy (owner_id, project_alias, version, max_rounds, max_stagnant_rounds, max_infrastructure_retries, required_hosted_checks)
    VALUES (p_owner_id, p_project_alias, p_version, p_max_rounds, p_max_stagnant_rounds, p_max_infrastructure_retries, p_required_hosted_checks)
  ON CONFLICT (owner_id, project_alias) DO UPDATE SET version = EXCLUDED.version, max_rounds = EXCLUDED.max_rounds, max_stagnant_rounds = EXCLUDED.max_stagnant_rounds, max_infrastructure_retries = EXCLUDED.max_infrastructure_retries, required_hosted_checks = EXCLUDED.required_hosted_checks
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_review_policy_upsert(text, text, text, integer, integer, integer, text[]) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_policy_upsert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_policy_upsert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_policy_upsert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_policy_upsert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_policy_upsert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_review_budget_insert(p_owner_id text, p_project_alias text, p_root_task_id uuid, p_policy jsonb)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.coding_review_budget%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.coding_review_budget (owner_id, project_alias, root_task_id, policy)
    VALUES (p_owner_id, p_project_alias, p_root_task_id, p_policy)
  ON CONFLICT DO NOTHING
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_review_budget_insert(text, text, uuid, jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_budget_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_budget_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_budget_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_budget_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_budget_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_worker_grant_insert(p_worker_role text, p_owner_id text, p_project_alias text, p_agent_profile text, p_mode text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.coding_worker_grants%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.coding_worker_grants (worker_role, owner_id, project_alias, agent_profile, mode)
    VALUES (p_worker_role, p_owner_id, p_project_alias, p_agent_profile, p_mode)
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_worker_grant_insert(text, text, text, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_worker_grant_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_worker_grant_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_worker_grant_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_worker_grant_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_worker_grant_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_usage_refresh_request_insert(p_owner_id text, p_profile_alias text, p_project_alias text, p_status text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.coding_usage_refresh_requests%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.coding_usage_refresh_requests (owner_id, profile_alias, project_alias, status, refresh_id)
    VALUES (p_owner_id, p_profile_alias, p_project_alias, p_status, gen_random_uuid())
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_usage_refresh_request_insert(text, text, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_usage_refresh_request_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_usage_refresh_request_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_usage_refresh_request_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_usage_refresh_request_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_usage_refresh_request_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ide_conversation_insert(p_id uuid, p_owner_id text, p_title text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.conversations%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.conversations (id, owner_id, title)
    VALUES (p_id, p_owner_id, p_title)
  ON CONFLICT (id) DO NOTHING
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ide_conversation_insert(uuid, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_conversation_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_conversation_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_conversation_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_conversation_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_conversation_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ide_run_insert(p_id uuid, p_conversation_id uuid, p_request_id text, p_model_id text, p_status text, p_ide_chat_owner_id text, p_ide_chat_id uuid, p_ide_chat_revision bigint, p_distributed boolean, p_run_deadline text, p_next_attempt_at text, p_mail_owner_id text, p_mail_thread_id uuid, p_mail_source_run_id uuid)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.runs%ROWTYPE;
BEGIN
  IF p_ide_chat_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  IF NOT EXISTS (SELECT 1 FROM forge_agent.conversations
    WHERE id = p_conversation_id
      AND owner_id = current_setting('forge.coding_owner', true)) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.runs (id, conversation_id, request_id, model_id, status, ide_chat_owner_id, ide_chat_id, ide_chat_revision, distributed, run_deadline, next_attempt_at, mail_owner_id, mail_thread_id, mail_source_run_id)
    VALUES (p_id, p_conversation_id, p_request_id, p_model_id, COALESCE(p_status, 'queued'::text), p_ide_chat_owner_id, p_ide_chat_id, p_ide_chat_revision, COALESCE(p_distributed, false), p_run_deadline, p_next_attempt_at, p_mail_owner_id, p_mail_thread_id, p_mail_source_run_id)
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ide_run_insert(uuid, uuid, text, text, text, text, uuid, bigint, boolean, text, text, text, uuid, uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ide_message_insert(p_conversation_id uuid, p_position bigint, p_run_id uuid, p_role text, p_content text, p_tool_call_id text, p_tool_result_call_id text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.messages%ROWTYPE;
BEGIN
  IF NOT EXISTS (SELECT 1 FROM forge_agent.conversations
    WHERE id = p_conversation_id
      AND owner_id = current_setting('forge.coding_owner', true)) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.messages (conversation_id, run_id, role, content, position, tool_call_id, tool_result_call_id)
    VALUES (p_conversation_id, p_run_id, p_role, p_content, COALESCE(p_position, (SELECT COALESCE(MAX(position), 0) + 1 FROM forge_agent.messages WHERE conversation_id = p_conversation_id)), p_tool_call_id, p_tool_result_call_id)
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ide_message_insert(uuid, bigint, uuid, text, text, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_message_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_message_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_message_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_message_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_message_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ide_tool_call_insert(p_run_id uuid, p_call_id text, p_tool_name text, p_arguments jsonb, p_status text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.tool_calls%ROWTYPE;
BEGIN
  IF NOT EXISTS (SELECT 1 FROM forge_agent.runs r
    JOIN forge_agent.conversations c ON c.id = r.conversation_id
    WHERE r.id = p_run_id
      AND c.owner_id = current_setting('forge.coding_owner', true)) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.tool_calls (run_id, call_id, tool_name, arguments, status)
    VALUES (p_run_id, p_call_id, p_tool_name, p_arguments, COALESCE(p_status, 'pending'::text))
  ON CONFLICT (run_id, call_id) DO NOTHING
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ide_tool_call_insert(uuid, text, text, jsonb, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_tool_call_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_tool_call_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_tool_call_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_tool_call_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_tool_call_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ide_tool_result_insert(p_run_id uuid, p_call_id text, p_outcome text, p_exit_code integer, p_stdout text, p_stderr text, p_output_truncated boolean)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.tool_results%ROWTYPE;
BEGIN
  IF NOT EXISTS (SELECT 1 FROM forge_agent.runs r
    JOIN forge_agent.conversations c ON c.id = r.conversation_id
    WHERE r.id = p_run_id
      AND c.owner_id = current_setting('forge.coding_owner', true)) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.tool_results (run_id, call_id, outcome, exit_code, stdout, stderr, output_truncated)
    VALUES (p_run_id, p_call_id, p_outcome, p_exit_code, COALESCE(p_stdout, ''::text), COALESCE(p_stderr, ''::text), COALESCE(p_output_truncated, false))
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ide_tool_result_insert(uuid, text, text, integer, text, text, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_tool_result_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_tool_result_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_tool_result_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_tool_result_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_tool_result_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ide_run_event_insert(p_run_id uuid, p_event_type text, p_data jsonb)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.run_events%ROWTYPE;
BEGIN
  IF NOT EXISTS (SELECT 1 FROM forge_agent.runs r
    JOIN forge_agent.conversations c ON c.id = r.conversation_id
    WHERE r.id = p_run_id
      AND c.owner_id = current_setting('forge.coding_owner', true)) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.run_events (run_id, event_type, data, sequence)
    VALUES (p_run_id, p_event_type, p_data, (SELECT COALESCE(MAX(sequence), 0) + 1 FROM forge_agent.run_events WHERE run_id = p_run_id))
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ide_run_event_insert(uuid, text, jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_event_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_event_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_event_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_event_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_event_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ide_workspace_tab_insert(p_owner_id text, p_tab_id text, p_lease text, p_revision bigint, p_base_history_revision bigint, p_base_history_fingerprint text, p_snapshot jsonb, p_chat_history_enabled boolean)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.ide_workspace_tabs%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.ide_workspace_tabs (owner_id, tab_id, lease, base_history_revision, base_history_fingerprint, revision, snapshot, chat_history_enabled)
    VALUES (p_owner_id, p_tab_id, p_lease, p_base_history_revision, p_base_history_fingerprint, COALESCE(p_revision, 0), p_snapshot, COALESCE(p_chat_history_enabled, false))
  ON CONFLICT (owner_id, tab_id) DO UPDATE SET lease = EXCLUDED.lease, revision = 0, base_history_revision = EXCLUDED.base_history_revision, base_history_fingerprint = EXCLUDED.base_history_fingerprint, snapshot = EXCLUDED.snapshot, chat_history_enabled = EXCLUDED.chat_history_enabled, touched_at = clock_timestamp()
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ide_workspace_tab_insert(text, text, text, bigint, bigint, text, jsonb, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_tab_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_tab_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_tab_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_tab_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_tab_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ide_workspace_history_insert(p_owner_id text, p_revision bigint, p_transcripts jsonb, p_chat_history_active boolean)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.ide_workspace_history%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.ide_workspace_history (owner_id, revision, transcripts, chat_history_active)
    VALUES (p_owner_id, COALESCE(p_revision, 0), COALESCE(p_transcripts, '[]'::jsonb), COALESCE(p_chat_history_active, false))
  ON CONFLICT (owner_id) DO NOTHING
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ide_workspace_history_insert(text, bigint, jsonb, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_history_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_history_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_history_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_history_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_history_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ide_chat_insert(p_owner_id text, p_id uuid, p_title text, p_messages jsonb, p_revision bigint, p_expires_on text, p_time_zone text, p_expires_at text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.ide_chats%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.ide_chats (owner_id, id, title, messages, revision, expires_on, time_zone, expires_at)
    VALUES (p_owner_id, p_id, p_title, COALESCE(p_messages, '[]'::jsonb), COALESCE(p_revision, 0), p_expires_on, p_time_zone, p_expires_at)
  ON CONFLICT (owner_id, id) DO NOTHING
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ide_chat_insert(text, uuid, text, jsonb, bigint, text, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_chat_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_chat_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_chat_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_chat_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_chat_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.mail_thread_insert(p_owner_id text, p_id uuid, p_subject text, p_profile_id text, p_context_remaining_tokens integer)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.mail_threads%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.mail_threads (owner_id, id, subject, profile_id, context_remaining_tokens)
    VALUES (p_owner_id, p_id, p_subject, p_profile_id, p_context_remaining_tokens)
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.mail_thread_insert(text, uuid, text, text, integer) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.mail_thread_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.mail_thread_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.mail_thread_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.mail_thread_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.mail_thread_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.mail_message_insert(p_owner_id text, p_thread_id uuid, p_run_id uuid, p_role text, p_content text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.mail_messages%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.mail_messages (owner_id, thread_id, run_id, role, content, sequence)
    VALUES (p_owner_id, p_thread_id, p_run_id, p_role, p_content, (SELECT COALESCE(MAX(sequence), 0) + 1 FROM forge_agent.mail_messages WHERE owner_id = p_owner_id AND thread_id = p_thread_id))
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.mail_message_insert(text, uuid, uuid, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.mail_message_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.mail_message_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.mail_message_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.mail_message_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.mail_message_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.jenkins_event_insert(p_owner_id text, p_project_alias text, p_sequence bigint, p_jenkins_instance text, p_job_name text, p_delivery_id text, p_event_kind text, p_build_number integer, p_queue_id bigint, p_commit_sha text, p_branch text, p_build_result text, p_stage_name text, p_stage_status text, p_duration_ms integer, p_build_url text, p_summary text, p_correlation_id text, p_occurred_at text, p_payload_sha256 text, p_actor text, p_backfilled boolean, p_reconciled boolean, p_completion_time_estimated boolean, p_started_at text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.jenkins_events%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.jenkins_events (owner_id, project_alias, sequence, jenkins_instance, job_name, delivery_id, event_kind, summary, occurred_at, payload_sha256, build_number, queue_id, commit_sha, branch, build_result, stage_name, stage_status, duration_ms, build_url, correlation_id, actor, backfilled, reconciled, completion_time_estimated, started_at)
    VALUES (p_owner_id, p_project_alias, p_sequence, p_jenkins_instance, p_job_name, p_delivery_id, p_event_kind, p_summary, p_occurred_at, p_payload_sha256, p_build_number, p_queue_id, p_commit_sha, p_branch, p_build_result, p_stage_name, p_stage_status, p_duration_ms, p_build_url, p_correlation_id, p_actor, COALESCE(p_backfilled, false), COALESCE(p_reconciled, false), COALESCE(p_completion_time_estimated, false), p_started_at)
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.jenkins_event_insert(text, text, bigint, text, text, text, text, integer, bigint, text, text, text, text, text, integer, text, text, text, text, text, text, boolean, boolean, boolean, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_event_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_event_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_event_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_event_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_event_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.jenkins_event_delivery_insert(p_event_id uuid, p_owner_id text, p_project_alias text, p_event_sequence bigint, p_consumer_name text, p_status text, p_max_attempts integer)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.jenkins_event_deliveries%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.jenkins_event_deliveries (event_id, owner_id, project_alias, event_sequence, consumer_name, status, max_attempts)
    VALUES (p_event_id, p_owner_id, p_project_alias, p_event_sequence, p_consumer_name, COALESCE(p_status, 'pending'::text), COALESCE(p_max_attempts, 5))
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.jenkins_event_delivery_insert(uuid, text, text, bigint, text, text, integer) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_event_delivery_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_event_delivery_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_event_delivery_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_event_delivery_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_event_delivery_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ext_workflow_insert(p_owner_id text, p_project_alias text, p_root_task_id uuid, p_active_task_id uuid, p_author_profile text, p_reviewer_profile text, p_policy_digest text, p_snapshot jsonb, p_phase text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_external_workflow.workflows%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_external_workflow.workflows (owner_id, project_alias, root_task_id, author_profile, reviewer_profile, policy_digest, snapshot, active_task_id, phase)
    VALUES (p_owner_id, p_project_alias, p_root_task_id, p_author_profile, p_reviewer_profile, p_policy_digest, p_snapshot, COALESCE(p_active_task_id, p_root_task_id), COALESCE(p_phase, 'plan'))
  ON CONFLICT (owner_id, project_alias, root_task_id) DO NOTHING
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ext_workflow_insert(text, text, uuid, uuid, text, text, text, jsonb, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_workflow_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_workflow_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_workflow_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_workflow_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_workflow_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ext_task_insert(p_owner_id text, p_project_alias text, p_task_id uuid, p_workflow_id uuid, p_role text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_external_workflow.tasks%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_external_workflow.tasks (owner_id, project_alias, task_id, workflow_id, role)
    VALUES (p_owner_id, p_project_alias, p_task_id, p_workflow_id, p_role)
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ext_task_insert(text, text, uuid, uuid, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_task_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_task_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_task_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_task_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_task_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ext_context_insert(p_assignment_id uuid, p_content text, p_sha256 text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_external_workflow.contexts%ROWTYPE;
BEGIN
  IF NOT EXISTS (SELECT 1 FROM forge_external_agent.assignments
    WHERE assignment_id = p_assignment_id
      AND owner_id = current_setting('forge.coding_owner', true)) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_external_workflow.contexts (assignment_id, content, sha256)
    VALUES (p_assignment_id, p_content, p_sha256)
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ext_context_insert(uuid, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_context_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_context_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_context_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_context_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_context_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ext_review_insert(p_review_id uuid, p_workflow_id uuid, p_owner_id text, p_project_alias text, p_source_task_id uuid, p_source_assignment_id uuid, p_source_sha256 text, p_target_kind text, p_reviewer_task_id uuid, p_target_json text, p_target_sha256 text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_external_workflow.reviews%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_external_workflow.reviews (review_id, workflow_id, owner_id, project_alias, source_task_id, source_assignment_id, source_sha256, target_kind, reviewer_task_id, target_json, target_sha256)
    VALUES (p_review_id, p_workflow_id, p_owner_id, p_project_alias, p_source_task_id, p_source_assignment_id, p_source_sha256, p_target_kind, p_reviewer_task_id, p_target_json, p_target_sha256)
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ext_review_insert(uuid, uuid, text, text, uuid, uuid, text, text, uuid, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_review_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_review_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_review_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_review_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_review_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ext_action_insert(p_action_id uuid, p_workflow_id uuid, p_owner_id text, p_project_alias text, p_workflow_revision bigint, p_kind text, p_source_key text, p_intent jsonb)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_external_workflow.actions%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_external_workflow.actions (workflow_id, owner_id, project_alias, kind, source_key, action_id, workflow_revision, intent)
    VALUES (p_workflow_id, p_owner_id, p_project_alias, p_kind, p_source_key, COALESCE(p_action_id, gen_random_uuid()), p_workflow_revision, p_intent)
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ext_action_insert(uuid, uuid, text, text, bigint, text, text, jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_action_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_action_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_action_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_action_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_action_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ext_plan_approve_override_insert(p_owner_id text, p_project_alias text, p_workflow_id uuid, p_root_task_id uuid, p_actor_kind text, p_actor_id text, p_reason text, p_acceptance_ref text, p_plan_task_id uuid, p_plan_assignment_id uuid, p_plan_sha256 text, p_prior_workflow_revision bigint, p_prior_root_revision bigint, p_prior_phase text, p_prior_plan_revisions integer, p_prior_review_id uuid, p_prior_escalation_reason text, p_retry_event_id uuid, p_idempotency_key text, p_intent_digest text, p_implement_task_id uuid, p_implement_assignment_id uuid, p_receipt jsonb)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_external_workflow.plan_approve_overrides%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_external_workflow.plan_approve_overrides (owner_id, project_alias, workflow_id, root_task_id, actor_kind, actor_id, reason, acceptance_ref, plan_task_id, plan_assignment_id, plan_sha256, prior_workflow_revision, prior_root_revision, prior_phase, prior_plan_revisions, prior_review_id, prior_escalation_reason, idempotency_key, intent_digest, implement_task_id, implement_assignment_id, receipt, retry_event_id)
    VALUES (p_owner_id, p_project_alias, p_workflow_id, p_root_task_id, p_actor_kind, p_actor_id, p_reason, p_acceptance_ref, p_plan_task_id, p_plan_assignment_id, p_plan_sha256, p_prior_workflow_revision, p_prior_root_revision, p_prior_phase, p_prior_plan_revisions, p_prior_review_id, p_prior_escalation_reason, p_idempotency_key, p_intent_digest, p_implement_task_id, p_implement_assignment_id, p_receipt, p_retry_event_id)
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ext_plan_approve_override_insert(text, text, uuid, uuid, text, text, text, text, uuid, uuid, text, bigint, bigint, text, integer, uuid, text, uuid, text, text, uuid, uuid, jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_plan_approve_override_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_plan_approve_override_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_plan_approve_override_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_plan_approve_override_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_plan_approve_override_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_task_update(p_owner_id text, p_project_alias text, p_id uuid, p_status text, p_mode text, p_revision bigint, p_agent_profile text, p_pinned_recipe jsonb, p_recipe_digest text, p_cancelled_at text, p_set_cancelled_at_now boolean, p_clear_cancelled_at boolean, p_accepted_at text, p_set_accepted_at_now boolean, p_touch_accepted_at boolean, p_clear_accepted_at boolean, p_latest_attempt_id uuid, p_resume_workspace_digest text, p_plan_decision_id uuid, p_feedback_id uuid, p_feedback_sha256 text, p_legacy_resume_note text, p_auto_repair boolean, p_review_required boolean, p_workflow_required boolean, p_workflow_id uuid, p_workflow_review_id uuid, p_workflow_feedback_review_id uuid, p_clear_workflow_feedback_review_id boolean, p_workflow_feedback_sha256 text, p_clear_workflow_feedback_sha256 boolean, p_title text, p_review_note text, p_bump_revision boolean, p_cancel_transition boolean, p_cancel_or_request_transition boolean)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.coding_tasks%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  UPDATE forge_agent.coding_tasks AS t SET
    status = CASE WHEN COALESCE(p_cancel_transition, false) THEN CASE WHEN status = 'queued' THEN 'cancelled' ELSE 'cancel_requested' END WHEN COALESCE(p_cancel_or_request_transition, false) THEN CASE WHEN status IN ('running', 'cancel_requested') THEN 'cancel_requested' ELSE 'cancelled' END WHEN p_status IS NOT NULL THEN p_status ELSE status END,
    cancelled_at = CASE WHEN COALESCE(p_cancel_transition, false) THEN CASE WHEN status = 'queued' THEN clock_timestamp() ELSE NULL END WHEN COALESCE(p_cancel_or_request_transition, false) THEN CASE WHEN status IN ('running', 'cancel_requested') THEN NULL ELSE clock_timestamp() END WHEN COALESCE(p_clear_cancelled_at, false) THEN NULL WHEN COALESCE(p_set_cancelled_at_now, false) THEN clock_timestamp() WHEN p_cancelled_at IS NOT NULL THEN p_cancelled_at::timestamptz ELSE cancelled_at END,
    accepted_at = CASE WHEN COALESCE(p_clear_accepted_at, false) THEN NULL WHEN COALESCE(p_set_accepted_at_now, false) THEN clock_timestamp() WHEN COALESCE(p_touch_accepted_at, false) THEN COALESCE(accepted_at, clock_timestamp()) WHEN p_accepted_at IS NOT NULL THEN p_accepted_at::timestamptz ELSE accepted_at END,
    revision = CASE WHEN COALESCE(p_cancel_transition, false) OR COALESCE(p_cancel_or_request_transition, false) OR COALESCE(p_bump_revision, false) THEN revision + 1 WHEN p_revision IS NOT NULL THEN p_revision ELSE revision END,
    mode = COALESCE(p_mode, mode),
    agent_profile = COALESCE(p_agent_profile, agent_profile),
    pinned_recipe = COALESCE(p_pinned_recipe, pinned_recipe),
    recipe_digest = COALESCE(p_recipe_digest, recipe_digest),
    latest_attempt_id = COALESCE(p_latest_attempt_id, latest_attempt_id),
    resume_workspace_digest = COALESCE(p_resume_workspace_digest, resume_workspace_digest),
    plan_decision_id = COALESCE(p_plan_decision_id, plan_decision_id),
    feedback_id = COALESCE(p_feedback_id, feedback_id),
    feedback_sha256 = COALESCE(p_feedback_sha256, feedback_sha256),
    legacy_resume_note = COALESCE(p_legacy_resume_note, legacy_resume_note),
    auto_repair = COALESCE(p_auto_repair, auto_repair),
    review_required = COALESCE(p_review_required, review_required),
    workflow_required = COALESCE(p_workflow_required, workflow_required),
    workflow_id = COALESCE(p_workflow_id, workflow_id),
    workflow_review_id = COALESCE(p_workflow_review_id, workflow_review_id),
    workflow_feedback_review_id = CASE WHEN p_clear_workflow_feedback_review_id THEN NULL ELSE COALESCE(p_workflow_feedback_review_id, workflow_feedback_review_id) END,
    workflow_feedback_sha256 = CASE WHEN p_clear_workflow_feedback_sha256 THEN NULL ELSE COALESCE(p_workflow_feedback_sha256, workflow_feedback_sha256) END,
    title = COALESCE(p_title, title),
    review_note = COALESCE(p_review_note, review_note),
    updated_at = clock_timestamp()
  WHERE (t.owner_id, t.project_alias, t.id) = (p_owner_id, p_project_alias, p_id)
  RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_task_update(text, text, uuid, text, text, bigint, text, jsonb, text, text, boolean, boolean, text, boolean, boolean, boolean, uuid, text, uuid, uuid, text, text, boolean, boolean, boolean, uuid, uuid, uuid, boolean, text, boolean, text, text, boolean, boolean, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_task_update TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_task_update TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_task_update TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_task_update TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_task_update TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_workflow_update(p_workflow_id uuid, p_phase text, p_enabled boolean, p_escalation_reason text, p_plan_revisions_delta integer, p_snapshot jsonb)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.coding_workflows%ROWTYPE;
BEGIN
  UPDATE forge_agent.coding_workflows AS t SET
    phase = COALESCE(p_phase, phase),
    enabled = COALESCE(p_enabled, enabled),
    escalation_reason = COALESCE(p_escalation_reason, escalation_reason),
    plan_revisions = plan_revisions + COALESCE(p_plan_revisions_delta, 0),
    snapshot = COALESCE(p_snapshot, snapshot),
    updated_at = clock_timestamp()
  WHERE t.workflow_id = p_workflow_id AND t.owner_id = current_setting('forge.coding_owner', true)
  RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_workflow_update(uuid, text, boolean, text, integer, jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_update TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_update TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_update TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_update TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_update TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_workflow_review_update(p_workflow_review_id uuid, p_status text, p_diagnostics jsonb, p_target_json text, p_target_sha256 text, p_reviewer_task_id uuid, p_reviewer_attempt_id uuid, p_reviewer_result_sha256 text, p_verdict_json text, p_verdict_sha256 text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.coding_workflow_reviews%ROWTYPE;
BEGIN
  UPDATE forge_agent.coding_workflow_reviews AS t SET
    status = COALESCE(p_status, status),
    diagnostics = COALESCE(p_diagnostics, diagnostics),
    target_json = COALESCE(p_target_json, target_json),
    target_sha256 = COALESCE(p_target_sha256, target_sha256),
    reviewer_task_id = COALESCE(p_reviewer_task_id, reviewer_task_id),
    reviewer_attempt_id = COALESCE(p_reviewer_attempt_id, reviewer_attempt_id),
    reviewer_result_sha256 = COALESCE(p_reviewer_result_sha256, reviewer_result_sha256),
    verdict_json = COALESCE(p_verdict_json, verdict_json),
    verdict_sha256 = COALESCE(p_verdict_sha256, verdict_sha256),
    updated_at = clock_timestamp()
  WHERE t.workflow_review_id = p_workflow_review_id AND t.owner_id = current_setting('forge.coding_owner', true)
  RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_workflow_review_update(uuid, text, jsonb, text, text, uuid, uuid, text, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_review_update TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_review_update TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_review_update TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_review_update TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_review_update TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_workflow_action_update(p_action_id uuid, p_status text, p_worker_id text, p_clear_worker_id boolean, p_lease_token uuid, p_clear_lease_token boolean, p_lease_expires_at text, p_clear_lease_expires_at boolean, p_lease_60s boolean, p_intent jsonb, p_finish_receipt jsonb, p_finish_token uuid, p_clear_finish_token boolean)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.coding_workflow_actions%ROWTYPE;
BEGIN
  UPDATE forge_agent.coding_workflow_actions AS t SET
    status = COALESCE(p_status, status),
    worker_id = CASE WHEN p_clear_worker_id THEN NULL ELSE COALESCE(p_worker_id, worker_id) END,
    lease_token = CASE WHEN p_clear_lease_token THEN NULL ELSE COALESCE(p_lease_token, lease_token) END,
    lease_expires_at = CASE WHEN COALESCE(p_clear_lease_expires_at, false) THEN NULL WHEN COALESCE(p_lease_60s, false) THEN clock_timestamp() + interval '60 seconds' WHEN p_lease_expires_at IS NOT NULL THEN p_lease_expires_at::timestamptz ELSE lease_expires_at END,
    intent = COALESCE(p_intent, intent),
    finish_receipt = COALESCE(p_finish_receipt, finish_receipt),
    finish_token = CASE WHEN p_clear_finish_token THEN NULL ELSE COALESCE(p_finish_token, finish_token) END,
    updated_at = clock_timestamp()
  WHERE t.action_id = p_action_id AND t.owner_id = current_setting('forge.coding_owner', true)
  RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_workflow_action_update(uuid, text, text, boolean, uuid, boolean, text, boolean, boolean, jsonb, jsonb, uuid, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_action_update TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_action_update TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_action_update TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_action_update TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_action_update TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_review_update(p_review_id uuid, p_status text, p_owner_decision jsonb, p_clear_owner_decision boolean, p_findings jsonb, p_summary text, p_reason text, p_feedback_id uuid, p_candidate_commit text, p_clear_candidate_commit boolean, p_gate_manifest_sha256 text, p_clear_gate_manifest_sha256 boolean, p_verification jsonb)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.coding_reviews%ROWTYPE;
BEGIN
  UPDATE forge_agent.coding_reviews AS t SET
    status = COALESCE(p_status, status),
    owner_decision = CASE WHEN p_clear_owner_decision THEN NULL ELSE COALESCE(p_owner_decision, owner_decision) END,
    findings = COALESCE(p_findings, findings),
    summary = COALESCE(p_summary, summary),
    reason = COALESCE(p_reason, reason),
    feedback_id = COALESCE(p_feedback_id, feedback_id),
    candidate_commit = CASE WHEN p_clear_candidate_commit THEN NULL ELSE COALESCE(p_candidate_commit, candidate_commit) END,
    gate_manifest_sha256 = CASE WHEN p_clear_gate_manifest_sha256 THEN NULL ELSE COALESCE(p_gate_manifest_sha256, gate_manifest_sha256) END,
    verification = COALESCE(p_verification, verification),
    updated_at = clock_timestamp()
  WHERE t.review_id = p_review_id AND t.owner_id = current_setting('forge.coding_owner', true)
  RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_review_update(uuid, text, jsonb, boolean, jsonb, text, text, uuid, text, boolean, text, boolean, jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_update TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_update TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_update TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_update TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_update TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ide_run_update(p_id uuid, p_status text, p_ide_chat_revision bigint, p_lease_owner text, p_clear_lease_owner boolean, p_lease_until text, p_clear_lease_until boolean, p_attempt_token uuid, p_clear_attempt_token boolean, p_attempt_no integer, p_next_attempt_at text, p_accepted_result_hash text, p_clear_accepted_result_hash boolean, p_requeue boolean)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.runs%ROWTYPE;
BEGIN
  UPDATE forge_agent.runs AS t SET
    status = CASE WHEN COALESCE(p_requeue, false) THEN 'queued' WHEN p_status IS NOT NULL THEN p_status ELSE status END,
    ide_chat_revision = COALESCE(p_ide_chat_revision, ide_chat_revision),
    lease_owner = CASE WHEN COALESCE(p_requeue, false) THEN NULL WHEN COALESCE(p_clear_lease_owner, false) THEN NULL ELSE COALESCE(p_lease_owner, lease_owner) END,
    lease_until = CASE WHEN COALESCE(p_requeue, false) THEN NULL WHEN COALESCE(p_clear_lease_until, false) THEN NULL WHEN p_lease_until IS NOT NULL THEN p_lease_until::timestamptz ELSE lease_until END,
    attempt_token = CASE WHEN COALESCE(p_requeue, false) THEN NULL WHEN COALESCE(p_clear_attempt_token, false) THEN NULL ELSE COALESCE(p_attempt_token, attempt_token) END,
    attempt_no = COALESCE(p_attempt_no, attempt_no),
    next_attempt_at = CASE WHEN COALESCE(p_requeue, false) THEN clock_timestamp() + attempt_no * interval '5 seconds' WHEN p_next_attempt_at IS NOT NULL THEN p_next_attempt_at::timestamptz ELSE next_attempt_at END,
    accepted_result_hash = CASE WHEN COALESCE(p_requeue, false) THEN NULL WHEN COALESCE(p_clear_accepted_result_hash, false) THEN NULL WHEN p_accepted_result_hash IS NOT NULL THEN decode(p_accepted_result_hash, 'hex') ELSE accepted_result_hash END
  WHERE t.id = p_id AND NOT COALESCE(p_requeue, false) OR (attempt_no < 3 AND run_deadline > clock_timestamp() + attempt_no * interval '5 seconds') AND EXISTS (SELECT 1 FROM forge_agent.runs r
  JOIN forge_agent.conversations c ON c.id = r.conversation_id
  WHERE r.id = t.run_id
    AND c.owner_id = current_setting('forge.coding_owner', true))
  RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ide_run_update(uuid, text, bigint, text, boolean, text, boolean, uuid, boolean, integer, text, text, boolean, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_update TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_update TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_update TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_update TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_update TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ide_tool_call_update(p_run_id uuid, p_call_id text, p_status text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.tool_calls%ROWTYPE;
BEGIN
  UPDATE forge_agent.tool_calls AS t SET
    status = p_status
  WHERE t.run_id = p_run_id AND t.call_id = p_call_id AND t.status = 'running' AND EXISTS (SELECT 1 FROM forge_agent.runs r
  JOIN forge_agent.conversations c ON c.id = r.conversation_id
  WHERE r.id = t.run_id
    AND c.owner_id = current_setting('forge.coding_owner', true))
  RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ide_tool_call_update(uuid, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_tool_call_update TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_tool_call_update TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_tool_call_update TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_tool_call_update TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_tool_call_update TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ide_workspace_tab_update(p_owner_id text, p_tab_id text, p_revision bigint, p_base_history_revision bigint, p_base_history_fingerprint text, p_snapshot jsonb, p_clear_snapshot boolean)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.ide_workspace_tabs%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  UPDATE forge_agent.ide_workspace_tabs AS t SET
    revision = COALESCE(p_revision, revision),
    base_history_revision = COALESCE(p_base_history_revision, base_history_revision),
    base_history_fingerprint = COALESCE(p_base_history_fingerprint, base_history_fingerprint),
    snapshot = CASE WHEN p_clear_snapshot THEN NULL ELSE COALESCE(p_snapshot, snapshot) END,
    touched_at = clock_timestamp()
  WHERE t.owner_id = p_owner_id AND t.tab_id = p_tab_id
  RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ide_workspace_tab_update(text, text, bigint, bigint, text, jsonb, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_tab_update TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_tab_update TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_tab_update TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_tab_update TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_tab_update TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ide_workspace_history_update(p_owner_id text, p_revision bigint, p_bump_revision boolean, p_transcripts jsonb, p_chat_history_active boolean)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.ide_workspace_history%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  UPDATE forge_agent.ide_workspace_history AS t SET
    revision = CASE WHEN COALESCE(p_bump_revision, false) THEN revision + 1 WHEN p_revision IS NOT NULL THEN p_revision ELSE revision END,
    transcripts = COALESCE(p_transcripts, transcripts),
    chat_history_active = COALESCE(p_chat_history_active, chat_history_active)
  WHERE t.owner_id = p_owner_id
  RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ide_workspace_history_update(text, bigint, boolean, jsonb, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_history_update TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_history_update TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_history_update TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_history_update TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_history_update TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ide_chat_update(p_owner_id text, p_id uuid, p_title text, p_clear_title boolean, p_messages jsonb, p_expires_on text, p_clear_expires_on boolean, p_time_zone text, p_clear_time_zone boolean, p_expires_at text, p_clear_expires_at boolean, p_bump_revision boolean, p_set_removed_at boolean, p_removal_reason text, p_set_retention_if_fresh boolean, p_require_unexpired boolean)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.ide_chats%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  UPDATE forge_agent.ide_chats AS t SET
    title = CASE WHEN p_clear_title THEN NULL ELSE COALESCE(p_title, title) END,
    messages = COALESCE(p_messages, messages),
    expires_on = CASE WHEN COALESCE(p_clear_expires_on, false) THEN NULL WHEN COALESCE(p_set_retention_if_fresh, false) AND revision = 0 THEN p_expires_on::date WHEN p_expires_on IS NOT NULL THEN p_expires_on::date ELSE expires_on END,
    time_zone = CASE WHEN COALESCE(p_clear_time_zone, false) THEN NULL WHEN COALESCE(p_set_retention_if_fresh, false) AND revision = 0 THEN p_time_zone WHEN p_time_zone IS NOT NULL THEN p_time_zone ELSE time_zone END,
    expires_at = CASE WHEN COALESCE(p_clear_expires_at, false) THEN NULL WHEN COALESCE(p_set_retention_if_fresh, false) AND revision = 0 THEN p_expires_at::timestamptz WHEN p_expires_at IS NOT NULL THEN p_expires_at::timestamptz ELSE expires_at END,
    revision = CASE WHEN COALESCE(p_bump_revision, false) THEN revision + 1 ELSE revision END,
    removed_at = CASE WHEN COALESCE(p_set_removed_at, false) THEN clock_timestamp() ELSE removed_at END,
    removal_reason = COALESCE(p_removal_reason, removal_reason),
    updated_at = clock_timestamp()
  WHERE t.owner_id = p_owner_id AND t.id = p_id AND NOT COALESCE(p_require_unexpired, false) OR expires_at IS NULL OR expires_at > clock_timestamp()
  RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ide_chat_update(text, uuid, text, boolean, jsonb, text, boolean, text, boolean, text, boolean, boolean, boolean, text, boolean, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_chat_update TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_chat_update TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_chat_update TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_chat_update TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_chat_update TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.mail_thread_update(p_owner_id text, p_id uuid, p_latest_run_id uuid, p_context_remaining_tokens integer, p_archive boolean, p_archive_base_revision bigint, p_manual_archive boolean, p_require_not_manually_archived boolean)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.mail_threads%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  IF COALESCE(p_manual_archive, false) AND p_archive_base_revision IS NULL THEN
    RAISE EXCEPTION 'manual_archive requires archive_base_revision' USING ERRCODE = '22023';
  END IF;
  UPDATE forge_agent.mail_threads AS t SET
    latest_run_id = COALESCE(p_latest_run_id, latest_run_id),
    context_remaining_tokens = COALESCE(p_context_remaining_tokens, context_remaining_tokens),
    archived_at = CASE WHEN COALESCE(p_manual_archive, false) THEN clock_timestamp() WHEN COALESCE(p_archive, false) AND archived_at IS NULL THEN clock_timestamp() ELSE archived_at END,
    archive_reason = CASE WHEN COALESCE(p_manual_archive, false) THEN 'manual' WHEN COALESCE(p_archive, false) AND archive_reason IS NULL THEN 'context_limit' ELSE archive_reason END,
    archive_base_revision = CASE WHEN COALESCE(p_manual_archive, false) THEN p_archive_base_revision ELSE archive_base_revision END,
    revision = revision + 1,
    updated_at = clock_timestamp()
  WHERE t.owner_id = p_owner_id AND t.id = p_id AND NOT COALESCE(p_require_not_manually_archived, false) OR archive_reason IS DISTINCT FROM 'manual'
  RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.mail_thread_update(text, uuid, uuid, integer, boolean, bigint, boolean, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.mail_thread_update TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.mail_thread_update TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.mail_thread_update TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.mail_thread_update TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.mail_thread_update TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.jenkins_event_delivery_update(p_delivery_row_id uuid, p_status text, p_worker_id text, p_clear_worker_id boolean, p_lease_token uuid, p_clear_lease_token boolean, p_lease_expires_at text, p_clear_lease_expires_at boolean, p_lease_expires_in_secs integer, p_next_attempt_delay_secs integer, p_last_error text, p_default_last_error text, p_receipt_digest text, p_increment_attempt_count boolean, p_set_consumed_at_now boolean)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.jenkins_event_deliveries%ROWTYPE;
BEGIN
  UPDATE forge_agent.jenkins_event_deliveries AS t SET
    status = COALESCE(p_status, status),
    worker_id = CASE WHEN p_clear_worker_id THEN NULL ELSE COALESCE(p_worker_id, worker_id) END,
    lease_token = CASE WHEN p_clear_lease_token THEN NULL ELSE COALESCE(p_lease_token, lease_token) END,
    lease_expires_at = CASE WHEN COALESCE(p_clear_lease_expires_at, false) THEN NULL WHEN p_lease_expires_in_secs IS NOT NULL THEN clock_timestamp() + make_interval(secs => p_lease_expires_in_secs) WHEN p_lease_expires_at IS NOT NULL THEN p_lease_expires_at::timestamptz ELSE lease_expires_at END,
    next_attempt_at = CASE WHEN p_next_attempt_delay_secs IS NOT NULL THEN clock_timestamp() + make_interval(secs => p_next_attempt_delay_secs) ELSE next_attempt_at END,
    last_error = CASE WHEN p_last_error IS NOT NULL THEN p_last_error WHEN p_default_last_error IS NOT NULL THEN COALESCE(last_error, p_default_last_error) ELSE last_error END,
    receipt_digest = COALESCE(p_receipt_digest, receipt_digest),
    attempt_count = CASE WHEN COALESCE(p_increment_attempt_count, false) THEN attempt_count + 1 ELSE attempt_count END,
    consumed_at = CASE WHEN COALESCE(p_set_consumed_at_now, false) THEN clock_timestamp() ELSE consumed_at END,
    updated_at = clock_timestamp()
  WHERE t.delivery_row_id = p_delivery_row_id AND t.owner_id = current_setting('forge.coding_owner', true)
  RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.jenkins_event_delivery_update(uuid, text, text, boolean, uuid, boolean, text, boolean, integer, integer, text, text, text, boolean, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_event_delivery_update TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_event_delivery_update TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_event_delivery_update TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_event_delivery_update TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_event_delivery_update TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ext_workflow_update(p_workflow_id uuid, p_active_task_id uuid, p_phase text, p_plan_revisions integer, p_code_revisions integer, p_enabled boolean, p_state jsonb, p_bump_revision boolean)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_external_workflow.workflows%ROWTYPE;
BEGIN
  UPDATE forge_external_workflow.workflows AS t SET
    active_task_id = COALESCE(p_active_task_id, active_task_id),
    phase = COALESCE(p_phase, phase),
    plan_revisions = COALESCE(p_plan_revisions, plan_revisions),
    code_revisions = COALESCE(p_code_revisions, code_revisions),
    enabled = COALESCE(p_enabled, enabled),
    state = COALESCE(p_state, state),
    revision = CASE WHEN COALESCE(p_bump_revision, false) THEN revision + 1 ELSE revision END,
    updated_at = clock_timestamp()
  WHERE t.workflow_id = p_workflow_id AND t.owner_id = current_setting('forge.coding_owner', true)
  RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ext_workflow_update(uuid, uuid, text, integer, integer, boolean, jsonb, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_workflow_update TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_workflow_update TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_workflow_update TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_workflow_update TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_workflow_update TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ext_review_update(p_review_id uuid, p_status text, p_reviewer_attempt_id uuid, p_reviewer_result_sha256 text, p_verdict jsonb, p_receipt jsonb)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_external_workflow.reviews%ROWTYPE;
BEGIN
  UPDATE forge_external_workflow.reviews AS t SET
    status = COALESCE(p_status, status),
    reviewer_attempt_id = COALESCE(p_reviewer_attempt_id, reviewer_attempt_id),
    reviewer_result_sha256 = COALESCE(p_reviewer_result_sha256, reviewer_result_sha256),
    verdict = COALESCE(p_verdict, verdict),
    receipt = COALESCE(p_receipt, receipt),
    updated_at = clock_timestamp()
  WHERE t.review_id = p_review_id AND t.owner_id = current_setting('forge.coding_owner', true)
  RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ext_review_update(uuid, text, uuid, text, jsonb, jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_review_update TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_review_update TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_review_update TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_review_update TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_review_update TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ext_action_update(p_action_id uuid, p_status text, p_lease_token uuid, p_clear_lease_token boolean, p_lease_role text, p_clear_lease_role boolean, p_lease_until text, p_clear_lease_until boolean, p_result jsonb)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_external_workflow.actions%ROWTYPE;
BEGIN
  UPDATE forge_external_workflow.actions AS t SET
    status = COALESCE(p_status, status),
    lease_token = CASE WHEN p_clear_lease_token THEN NULL ELSE COALESCE(p_lease_token, lease_token) END,
    lease_role = CASE WHEN p_clear_lease_role THEN NULL ELSE COALESCE(p_lease_role, lease_role) END,
    lease_until = CASE WHEN p_clear_lease_until THEN NULL ELSE COALESCE(p_lease_until, lease_until) END,
    result = COALESCE(p_result, result),
    updated_at = clock_timestamp()
  WHERE t.action_id = p_action_id AND t.owner_id = current_setting('forge.coding_owner', true)
  RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ext_action_update(uuid, text, uuid, boolean, text, boolean, text, boolean, jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_action_update TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_action_update TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_action_update TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_action_update TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_action_update TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ide_conversation_delete(p_owner_id text, p_ids text[])
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE deleted jsonb;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  WITH d AS (
    DELETE FROM forge_agent.conversations AS c WHERE c.owner_id = p_owner_id AND c.id = ANY(p_ids) AND NOT EXISTS (SELECT 1 FROM forge_agent.runs r WHERE r.conversation_id = c.id) RETURNING *
  ) SELECT COALESCE(jsonb_agg(to_jsonb(d)), '[]'::jsonb) INTO deleted FROM d;
  RETURN jsonb_build_object('deleted', deleted);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ide_conversation_delete(text, text[]) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_conversation_delete TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_conversation_delete TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_conversation_delete TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_conversation_delete TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_conversation_delete TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ide_run_delete(p_ide_chat_owner_id text, p_ide_chat_id uuid)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE deleted jsonb;
BEGIN
  IF p_ide_chat_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  WITH d AS (
    DELETE FROM forge_agent.runs WHERE ide_chat_owner_id = p_ide_chat_owner_id AND ide_chat_id = p_ide_chat_id RETURNING *
  ) SELECT COALESCE(jsonb_agg(to_jsonb(d)), '[]'::jsonb) INTO deleted FROM d;
  RETURN jsonb_build_object('deleted', deleted);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ide_run_delete(text, uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_delete TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_delete TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_delete TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_delete TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_run_delete TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ide_workspace_tab_delete(p_owner_id text, p_tab_ids text[])
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE deleted jsonb;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  WITH d AS (
    DELETE FROM forge_agent.ide_workspace_tabs WHERE owner_id = p_owner_id AND tab_id = ANY(p_tab_ids) RETURNING *
  ) SELECT COALESCE(jsonb_agg(to_jsonb(d)), '[]'::jsonb) INTO deleted FROM d;
  RETURN jsonb_build_object('deleted', deleted);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ide_workspace_tab_delete(text, text[]) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_tab_delete TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_tab_delete TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_tab_delete TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_tab_delete TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ide_workspace_tab_delete TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.inference_result_delete(p_run_id uuid)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE deleted jsonb;
BEGIN
  WITH d AS (
    DELETE FROM forge_agent.inference_results WHERE run_id = p_run_id AND EXISTS (SELECT 1 FROM forge_agent.runs r
  JOIN forge_agent.conversations c ON c.id = r.conversation_id
  WHERE r.id = inference_results.run_id
    AND c.owner_id = current_setting('forge.coding_owner', true)) RETURNING *
  ) SELECT COALESCE(jsonb_agg(to_jsonb(d)), '[]'::jsonb) INTO deleted FROM d;
  RETURN jsonb_build_object('deleted', deleted);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.inference_result_delete(uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.inference_result_delete TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.inference_result_delete TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.inference_result_delete TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.inference_result_delete TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.inference_result_delete TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_workflow_review_insert(p_workflow_id uuid, p_owner_id text, p_project_alias text, p_target_kind text, p_source_task_id uuid, p_source_attempt_id uuid, p_source_result_sha256 text, p_source_revision bigint, p_source_review_id uuid, p_candidate_commit text, p_gate_manifest_sha256 text, p_supersedes_workflow_review_id uuid, p_review_generation integer, p_supersedes_review_id uuid, p_workflow_review_id uuid, p_on_conflict_do_nothing boolean)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.coding_workflow_reviews%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  IF COALESCE(p_on_conflict_do_nothing, false) THEN
    INSERT INTO forge_agent.coding_workflow_reviews (workflow_id, owner_id, project_alias, target_kind, source_task_id, source_attempt_id, source_result_sha256, source_revision, source_review_id, candidate_commit, gate_manifest_sha256, supersedes_workflow_review_id, review_generation, supersedes_review_id, workflow_review_id)
      VALUES (p_workflow_id, p_owner_id, p_project_alias, p_target_kind, p_source_task_id, p_source_attempt_id, p_source_result_sha256, p_source_revision, p_source_review_id, p_candidate_commit, p_gate_manifest_sha256, p_supersedes_workflow_review_id, COALESCE(p_review_generation, 0), p_supersedes_review_id, COALESCE(p_workflow_review_id, gen_random_uuid())) ON CONFLICT DO NOTHING RETURNING * INTO r;
  ELSE
    INSERT INTO forge_agent.coding_workflow_reviews (workflow_id, owner_id, project_alias, target_kind, source_task_id, source_attempt_id, source_result_sha256, source_revision, source_review_id, candidate_commit, gate_manifest_sha256, supersedes_workflow_review_id, review_generation, supersedes_review_id, workflow_review_id)
      VALUES (p_workflow_id, p_owner_id, p_project_alias, p_target_kind, p_source_task_id, p_source_attempt_id, p_source_result_sha256, p_source_revision, p_source_review_id, p_candidate_commit, p_gate_manifest_sha256, p_supersedes_workflow_review_id, COALESCE(p_review_generation, 0), p_supersedes_review_id, COALESCE(p_workflow_review_id, gen_random_uuid())) RETURNING * INTO r;
  END IF;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_workflow_review_insert(uuid, text, text, text, uuid, uuid, text, bigint, uuid, text, text, uuid, integer, uuid, uuid, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_review_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_review_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_review_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_review_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_review_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_workflow_action_insert(p_workflow_id uuid, p_owner_id text, p_project_alias text, p_kind text, p_source_key text, p_workflow_review_id uuid, p_source_publication_id uuid, p_status text, p_intent jsonb, p_finish_receipt jsonb, p_only_if_reviewing boolean, p_on_conflict_do_nothing boolean)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.coding_workflow_actions%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  IF COALESCE(p_only_if_reviewing, false) THEN
    INSERT INTO forge_agent.coding_workflow_actions (workflow_id, owner_id, project_alias, workflow_review_id, kind, source_key)
      SELECT workflow_id, owner_id, project_alias, workflow_review_id, 'decide', p_source_key
      FROM forge_agent.coding_workflow_reviews
      WHERE workflow_review_id = p_workflow_review_id AND status = 'reviewing'
      ON CONFLICT DO NOTHING RETURNING * INTO r;
  ELSIF COALESCE(p_on_conflict_do_nothing, false) THEN
    INSERT INTO forge_agent.coding_workflow_actions (workflow_id, owner_id, project_alias, workflow_review_id, kind, source_key, source_publication_id, status, intent, finish_receipt)
      VALUES (p_workflow_id, p_owner_id, p_project_alias, p_workflow_review_id, p_kind, p_source_key, p_source_publication_id, COALESCE(p_status, 'pending'), p_intent, p_finish_receipt) ON CONFLICT DO NOTHING RETURNING * INTO r;
  ELSE
    INSERT INTO forge_agent.coding_workflow_actions (workflow_id, owner_id, project_alias, workflow_review_id, kind, source_key, source_publication_id, status, intent, finish_receipt)
      VALUES (p_workflow_id, p_owner_id, p_project_alias, p_workflow_review_id, p_kind, p_source_key, p_source_publication_id, COALESCE(p_status, 'pending'), p_intent, p_finish_receipt) RETURNING * INTO r;
  END IF;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_workflow_action_insert(uuid, text, text, text, text, uuid, uuid, text, jsonb, jsonb, boolean, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_action_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_action_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_action_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_action_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_action_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_review_insert_copy(p_source_review_id uuid, p_reconciliation_receipt_id uuid, p_summary text, p_source_revision bigint, p_increment_infrastructure_retries boolean, p_strip_verification_keys boolean)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.coding_reviews%ROWTYPE;
BEGIN
  INSERT INTO forge_agent.coding_reviews (owner_id, project_alias, task_id, attempt_id, plan_decision_id, scope_plan_decision_id, reconciliation_receipt_id, base_revision, workspace_digest, parent_result_sha256, source_revision, review_no, round, stagnant_rounds, infrastructure_retries, previous_review_id, candidate_commit, summary, phase, owner_decision, findings, verified_finding_ids, verification, target_publication_id, source_publication_id, gate_manifest_sha256)
    SELECT s.owner_id, s.project_alias, s.task_id, s.attempt_id, s.plan_decision_id, s.scope_plan_decision_id, COALESCE(p_reconciliation_receipt_id, s.reconciliation_receipt_id), s.base_revision, s.workspace_digest, s.parent_result_sha256, COALESCE(p_source_revision, s.source_revision), (SELECT COALESCE(MAX(review_no), 0) + 1 FROM forge_agent.coding_reviews WHERE owner_id = s.owner_id AND project_alias = s.project_alias AND task_id = s.task_id), s.round, s.stagnant_rounds, CASE WHEN COALESCE(p_increment_infrastructure_retries, false) THEN s.infrastructure_retries + 1 ELSE s.infrastructure_retries END, s.review_id, s.candidate_commit, COALESCE(p_summary, s.summary), s.phase, s.owner_decision, s.findings, s.verified_finding_ids, CASE WHEN COALESCE(p_strip_verification_keys, false) THEN s.verification - 'failure_stage' - 'exception_type' ELSE s.verification END, s.target_publication_id, s.source_publication_id, s.gate_manifest_sha256
    FROM forge_agent.coding_reviews s WHERE s.review_id = p_source_review_id
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_review_insert_copy(uuid, uuid, text, bigint, boolean, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_insert_copy TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_insert_copy TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_insert_copy TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_insert_copy TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_review_insert_copy TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.jenkins_observation_insert(p_owner_id text, p_project_alias text, p_jenkins_instance text, p_job_name text, p_observation_key text, p_build_number integer, p_queue_id bigint, p_last_event_id uuid, p_last_event_sequence bigint, p_last_event_kind text, p_commit_sha text, p_branch text, p_build_result text, p_stage_name text, p_stage_status text, p_duration_ms integer, p_build_url text, p_summary text, p_occurred_at text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE r forge_agent.jenkins_observations%ROWTYPE;
BEGIN
  IF p_owner_id IS DISTINCT FROM current_setting('forge.coding_owner', true) THEN
    RAISE EXCEPTION 'Owner scope mismatch' USING ERRCODE = '42501';
  END IF;
  INSERT INTO forge_agent.jenkins_observations (owner_id, project_alias, jenkins_instance, job_name, observation_key, build_number, queue_id, last_event_id, last_event_sequence, last_event_kind, commit_sha, branch, build_result, stage_name, stage_status, duration_ms, build_url, summary, occurred_at, updated_at)
    VALUES (p_owner_id, p_project_alias, p_jenkins_instance, p_job_name, p_observation_key, p_build_number, p_queue_id, p_last_event_id, p_last_event_sequence, p_last_event_kind, p_commit_sha, p_branch, p_build_result, p_stage_name, p_stage_status, p_duration_ms, p_build_url, p_summary, p_occurred_at::timestamptz, clock_timestamp())
    ON CONFLICT (owner_id, project_alias, jenkins_instance, job_name, observation_key)
    DO UPDATE SET build_number = COALESCE(EXCLUDED.build_number, forge_agent.jenkins_observations.build_number), queue_id = COALESCE(EXCLUDED.queue_id, forge_agent.jenkins_observations.queue_id), last_event_id = EXCLUDED.last_event_id, last_event_sequence = EXCLUDED.last_event_sequence, last_event_kind = EXCLUDED.last_event_kind, commit_sha = COALESCE(EXCLUDED.commit_sha, forge_agent.jenkins_observations.commit_sha), branch = COALESCE(EXCLUDED.branch, forge_agent.jenkins_observations.branch), build_result = COALESCE(EXCLUDED.build_result, forge_agent.jenkins_observations.build_result), stage_name = EXCLUDED.stage_name, stage_status = EXCLUDED.stage_status, duration_ms = COALESCE(EXCLUDED.duration_ms, forge_agent.jenkins_observations.duration_ms), build_url = COALESCE(EXCLUDED.build_url, forge_agent.jenkins_observations.build_url), summary = EXCLUDED.summary, occurred_at = EXCLUDED.occurred_at, updated_at = clock_timestamp()
    WHERE EXCLUDED.last_event_sequence > forge_agent.jenkins_observations.last_event_sequence
    RETURNING * INTO r;
  IF NOT FOUND THEN RETURN NULL; END IF;
  RETURN to_jsonb(r);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.jenkins_observation_insert(text, text, text, text, text, integer, bigint, uuid, bigint, text, text, text, text, text, text, integer, text, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_observation_insert TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_observation_insert TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_observation_insert TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_observation_insert TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.jenkins_observation_insert TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.workflow_action_lease(p_worker_id text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE a forge_agent.coding_workflow_actions%ROWTYPE;
BEGIN
  SELECT a.* INTO a FROM forge_agent.coding_workflow_actions a
  JOIN forge_agent.coding_workflows w ON w.workflow_id = a.workflow_id
  WHERE a.owner_id = current_setting('forge.coding_owner', true)
    AND a.kind IN ('review', 'decide') AND w.enabled
    AND w.phase NOT IN ('completed', 'cancelled', 'escalated')
    AND (a.status = 'pending' OR (a.status = 'leased' AND a.lease_expires_at <= clock_timestamp()))
  ORDER BY a.created_at
  FOR UPDATE OF a SKIP LOCKED LIMIT 1;
  IF NOT FOUND THEN RETURN NULL; END IF;
  UPDATE forge_agent.coding_workflow_actions
    SET status = 'leased', worker_id = p_worker_id,
        lease_token = gen_random_uuid(),
        lease_expires_at = clock_timestamp() + interval '60 seconds',
        updated_at = clock_timestamp()
    WHERE action_id = a.action_id RETURNING * INTO a;
  RETURN to_jsonb(a);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.workflow_action_lease(text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.workflow_action_lease TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.workflow_action_lease TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.workflow_action_lease TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.workflow_action_lease TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.workflow_action_lease TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.ext_workflow_cancel(p_project_alias text, p_task_id uuid, p_expected_revision bigint, p_idempotency_key text, p_actor_kind text, p_actor_id text, p_cause text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE
  w forge_external_workflow.workflows%ROWTYPE;
  t forge_agent.coding_tasks%ROWTYPE;
  prior RECORD;
  digest text;
  actor jsonb;
  child RECORD;
  child_action RECORD;
  active boolean;
  child_status text;
  receipt jsonb;
  row_count int;
  new_action_id uuid;
BEGIN
  -- intent digest (sha256_canonical of the cancel intent)

  digest := encode(sha256(convert_to(
    format('{"action":"cancel","expected_revision":%s,"project_alias":%s,"task_id":%s}',
           p_expected_revision, to_json(p_project_alias), to_json(p_task_id::text)),
    'UTF8')), 'hex');

  -- Lock the workflow row via the external task link.
  SELECT w.* INTO w FROM forge_external_workflow.workflows w
  JOIN forge_external_workflow.tasks x USING (workflow_id)
  WHERE (x.owner_id, x.project_alias, x.task_id) =
        (current_setting('forge.coding_owner', true), p_project_alias, p_task_id)
  FOR UPDATE OF w;
  IF NOT FOUND THEN RETURN NULL; END IF;
  -- service_scope: the grant must exist (raises if it does not).
  PERFORM forge_external_workflow_api.service_scope(
    current_setting('forge.coding_owner', true), p_project_alias, w.author_profile, false);
  IF p_task_id <> w.root_task_id THEN
    RETURN jsonb_build_object('error', 'not_cancellable');
  END IF;
  -- Lock the root task.
  SELECT * INTO t FROM forge_agent.coding_tasks
  WHERE (owner_id, project_alias, id) =
        (current_setting('forge.coding_owner', true), p_project_alias, p_task_id)
  FOR UPDATE;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'External workflow task disappeared' USING ERRCODE = 'P0002';
  END IF;
  -- Idempotency: a prior receipt for this intent key (any action).
  SELECT action, intent_digest, receipt_json INTO prior
    FROM forge_agent.coding_control_receipts
    WHERE (owner_id, project_alias, task_id, idempotency_key) =
          (current_setting('forge.coding_owner', true), p_project_alias, p_task_id,
           p_idempotency_key);
  IF FOUND THEN
    IF prior.action <> 'cancel' OR prior.intent_digest <> digest THEN
      RETURN jsonb_build_object('error', 'idempotency_conflict');
    END IF;
    RETURN prior.receipt_json;
  END IF;
  IF t.revision <> p_expected_revision THEN
    RETURN jsonb_build_object('error', 'revision_conflict');
  END IF;
  IF w.phase = 'merging' OR w.phase IN ('completed', 'cancelled', 'escalated') THEN
    RETURN jsonb_build_object('error', 'not_cancellable');
  END IF;
  -- Actor/cause validation (Python validates here, after the checks).

  IF p_actor_kind NOT IN ('operator', 'coordinator', 'worker', 'agent', 'service', 'system') THEN
    RAISE EXCEPTION 'invalid event actor' USING ERRCODE = '22023';
  END IF;
  IF p_actor_id IS NULL OR length(p_actor_id) = 0 OR length(p_actor_id) > 128 THEN
    RAISE EXCEPTION 'invalid event actor' USING ERRCODE = '22023';
  END IF;
  IF p_cause NOT IN ('operator_request', 'workflow_cancelled', 'budget_exhausted', 'superseded') THEN
    RAISE EXCEPTION 'invalid_request' USING ERRCODE = '22023';
  END IF;

  actor := jsonb_build_object('kind', p_actor_kind, 'id', p_actor_id);
  -- _cancel_children: cancel each child task in a cancellable status.
  FOR child IN
    SELECT t2.* FROM forge_agent.coding_tasks t2
    JOIN forge_external_workflow.tasks x
      ON (x.owner_id, x.project_alias, x.task_id) =
         (t2.owner_id, t2.project_alias, t2.id)
    WHERE x.workflow_id = w.workflow_id
      AND t2.status IN ('queued', 'running', 'awaiting_review', 'needs_reconcile',
                        'blocked_auth', 'blocked_permission', 'quota_blocked')
    FOR UPDATE OF t2
  LOOP
    SELECT EXISTS(SELECT 1 FROM forge_agent.coding_attempts a
      WHERE (a.owner_id, a.project_alias, a.task_id) =
            (current_setting('forge.coding_owner', true), p_project_alias, child.id)
        AND (a.phase <> 'stopped' OR a.reservation_active
             OR a.lease_expires_at IS NOT NULL)) INTO active;
    child_status := CASE WHEN active THEN 'cancel_requested' ELSE 'cancelled' END;
    UPDATE forge_agent.coding_tasks
      SET status = child_status,
          cancelled_at = CASE WHEN child_status = 'cancelled'
                              THEN clock_timestamp() ELSE NULL END,
          revision = revision + 1, updated_at = clock_timestamp()
      WHERE (owner_id, project_alias, id) =
            (current_setting('forge.coding_owner', true), p_project_alias, child.id);
    INSERT INTO forge_agent.coding_events
      (owner_id, project_alias, task_id, sequence, event_kind, payload)
      SELECT current_setting('forge.coding_owner', true), p_project_alias, child.id,
             COALESCE(MAX(sequence), 0) + 1, child_status,
             jsonb_build_object('base_revision', child.revision,
                                'revision', child.revision + 1,
                                'workflow_id', w.workflow_id::text,
                                'actor', actor, 'cause', p_cause)
      FROM forge_agent.coding_events
      WHERE owner_id = current_setting('forge.coding_owner', true)
        AND project_alias = p_project_alias AND task_id = child.id;
  END LOOP;
  -- Fail queued/leased verify actions.
  UPDATE forge_external_workflow.actions
    SET status = 'failed',
        result = jsonb_build_object('status', 'failed', 'receipt', jsonb_build_object(),
                  'summary', 'Workflow admission stopped before delivery completed.'),
        lease_token = NULL, lease_role = NULL, lease_until = NULL,
        updated_at = clock_timestamp()
    WHERE workflow_id = w.workflow_id AND status IN ('queued', 'leased');
  -- Queue a cancel action per verify action being cleaned up
  -- (_cleanup_action -> _action for kind='cancel').
  FOR child_action IN
    SELECT * FROM forge_external_workflow.actions
    WHERE workflow_id = w.workflow_id AND kind = 'verify'
      AND status IN ('queued', 'leased', 'failed')
  LOOP
    SELECT * INTO prior FROM forge_external_workflow.actions
    WHERE workflow_id = w.workflow_id AND kind = 'cancel'
      AND source_key = child_action.action_id::text;
    IF FOUND THEN
      -- _action expiry-fail: only queued/leased actions past their
      -- deadline (or past the generation cap) are failed.
      UPDATE forge_external_workflow.actions
        SET status = 'failed',
            result = jsonb_build_object('status', 'failed', 'receipt', jsonb_build_object(),
                      'summary', 'The bounded trusted delivery retry deadline was exhausted.'),
            lease_token = NULL, lease_role = NULL, lease_until = NULL,
            updated_at = clock_timestamp()
        WHERE action_id = prior.action_id AND status IN ('queued', 'leased')
          AND (deadline_at <= clock_timestamp() OR generation >= 10000);
    ELSE
      new_action_id := gen_random_uuid();
      INSERT INTO forge_external_workflow.actions
        (action_id, workflow_id, owner_id, project_alias, workflow_revision,
         kind, source_key, intent)
        VALUES (new_action_id, w.workflow_id,
                current_setting('forge.coding_owner', true),
                w.project_alias, w.revision, 'cancel',
                child_action.action_id::text,
                jsonb_build_object('target_action_id', child_action.action_id::text,
                  'target_kind', child_action.kind,
                  'target_request', child_action.intent,
                  'target_receipt', COALESCE(child_action.result -> 'receipt', '{}'::jsonb),
                  'workflow_id', w.workflow_id::text,
                  'project_alias', w.project_alias,
                  'correlation_id', t.correlation_id::text,
                  'action_id', new_action_id::text));
      -- _action queues the action: 'Trusted cancel action queued.'
      -- checkpoint event on the root task + two notifies.
      INSERT INTO forge_agent.coding_events
        (owner_id, project_alias, task_id, sequence, event_kind, payload)
        SELECT current_setting('forge.coding_owner', true), p_project_alias, w.root_task_id,
               COALESCE(MAX(sequence), 0) + 1, 'checkpoint',
               jsonb_build_object('workflow_id', w.workflow_id::text, 'phase', w.phase,
                 'summary', 'Trusted cancel action queued.',
                 'actor', jsonb_build_object('kind', 'coordinator', 'id', 'forge-coordinator'))
        FROM forge_agent.coding_events
        WHERE owner_id = current_setting('forge.coding_owner', true)
          AND project_alias = p_project_alias AND task_id = w.root_task_id;
      PERFORM pg_notify('forge_coding_jobs', '');
      PERFORM pg_notify('forge_external_delivery', '');
    END IF;
  END LOOP;
  -- _save: workflow revision bump (optimistic), phase cancelled,
  -- enabled false; then the workflow checkpoint event + notify.
  UPDATE forge_external_workflow.workflows
    SET revision = revision + 1, phase = 'cancelled', enabled = false,
        updated_at = clock_timestamp()
    WHERE workflow_id = w.workflow_id AND revision = w.revision;
  GET DIAGNOSTICS row_count = ROW_COUNT;
  IF row_count = 0 THEN
    RETURN jsonb_build_object('error', 'result_mismatch');
  END IF;
  INSERT INTO forge_agent.coding_events
    (owner_id, project_alias, task_id, sequence, event_kind, payload)
    SELECT current_setting('forge.coding_owner', true), p_project_alias, w.root_task_id,
           COALESCE(MAX(sequence), 0) + 1, 'checkpoint',
           jsonb_build_object('workflow_id', w.workflow_id::text, 'phase', 'cancelled',
             'summary', left('User cancelled the complete external workflow.', 4096),
             'actor', jsonb_build_object('kind', 'coordinator', 'id', 'forge-coordinator'))
    FROM forge_agent.coding_events
    WHERE owner_id = current_setting('forge.coding_owner', true)
      AND project_alias = p_project_alias AND task_id = w.root_task_id;
  PERFORM pg_notify('forge_coding_jobs', '');
  -- Root task revision set to expected+1.
  UPDATE forge_agent.coding_tasks
    SET revision = p_expected_revision + 1, updated_at = clock_timestamp()
    WHERE (owner_id, project_alias, id) =
          (current_setting('forge.coding_owner', true), p_project_alias, p_task_id);
  -- Receipt (exact Python shape) + control receipt.
  receipt := jsonb_build_object('project_alias', p_project_alias,
    'task_id', p_task_id::text, 'base_revision', p_expected_revision,
    'revision', p_expected_revision + 1, 'status', 'cancelled', 'actor', actor);
  INSERT INTO forge_agent.coding_control_receipts
    (owner_id, project_alias, task_id, action, idempotency_key, intent_digest,
     base_revision, result_revision, receipt_json)
    VALUES (current_setting('forge.coding_owner', true), p_project_alias, p_task_id,
            'cancel', p_idempotency_key, digest,
            p_expected_revision, p_expected_revision + 1, receipt);
  RETURN receipt;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.ext_workflow_cancel(text, uuid, bigint, text, text, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_workflow_cancel TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_workflow_cancel TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_workflow_cancel TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_workflow_cancel TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.ext_workflow_cancel TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_pg_proxy_api.coding_workflow_cancel(p_project_alias text, p_task_id uuid, p_expected_revision bigint, p_idempotency_key text, p_actor_kind text, p_actor_id text, p_cause text)
  RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
  SET search_path = pg_catalog, pg_temp AS $fn$
DECLARE
  t forge_agent.coding_tasks%ROWTYPE;
  scope_rec RECORD;
  prior RECORD;
  digest text;
  actor jsonb;
  wf_phase text;
  affected RECORD;
  affected_target text;
  affected_cause text;
  target text;
  new_revision bigint;
  receipt jsonb;
BEGIN
  IF p_expected_revision < 1 OR p_expected_revision > 9007199254740991 THEN
    RETURN jsonb_build_object('error', 'invalid_request');
  END IF;

  digest := encode(sha256(convert_to(
    format('{"action":"cancel","expected_revision":%s,"project_alias":%s,"task_id":%s}',
           p_expected_revision, to_json(p_project_alias), to_json(p_task_id::text)),
    'UTF8')), 'hex');

  -- Scope: the task's workflow root; lock the root task if it differs.
  SELECT w.root_task_id INTO scope_rec FROM forge_agent.coding_tasks t
  JOIN forge_agent.coding_workflows w ON w.workflow_id = t.workflow_id
  WHERE t.owner_id = current_setting('forge.coding_owner', true)
    AND t.project_alias = p_project_alias AND t.id = p_task_id;
  IF FOUND AND scope_rec.root_task_id <> p_task_id THEN
    PERFORM id FROM forge_agent.coding_tasks
    WHERE owner_id = current_setting('forge.coding_owner', true)
      AND project_alias = p_project_alias AND id = scope_rec.root_task_id
    FOR UPDATE;
  END IF;
  -- Lock the task.
  SELECT * INTO t FROM forge_agent.coding_tasks
  WHERE (owner_id, project_alias, id) =
        (current_setting('forge.coding_owner', true), p_project_alias, p_task_id)
  FOR UPDATE;
  IF NOT FOUND THEN
    RETURN jsonb_build_object('error', 'not_found');
  END IF;
  -- Idempotency: a prior receipt for this intent key (any action).
  SELECT action, intent_digest, receipt_json INTO prior
    FROM forge_agent.coding_control_receipts
    WHERE (owner_id, project_alias, task_id, idempotency_key) =
          (current_setting('forge.coding_owner', true), p_project_alias, p_task_id,
           p_idempotency_key);
  IF FOUND THEN
    IF prior.action <> 'cancel' OR prior.intent_digest <> digest THEN
      RETURN jsonb_build_object('error', 'idempotency_conflict');
    END IF;
    RETURN prior.receipt_json;
  END IF;
  IF t.workflow_required THEN
    IF t.revision <> p_expected_revision THEN
      RETURN jsonb_build_object('error', 'revision_conflict');
    END IF;
    SELECT phase INTO wf_phase FROM forge_agent.coding_workflows
    WHERE workflow_id = t.workflow_id FOR UPDATE;
    IF NOT FOUND OR wf_phase IN ('completed', 'cancelled', 'merging') THEN
      RETURN jsonb_build_object('error', 'not_cancellable');
    END IF;
    -- Actor/cause validation (Python validates here, after the checks).

  IF p_actor_kind NOT IN ('operator', 'coordinator', 'worker', 'agent', 'service', 'system') THEN
    RAISE EXCEPTION 'invalid event actor' USING ERRCODE = '22023';
  END IF;
  IF p_actor_id IS NULL OR length(p_actor_id) = 0 OR length(p_actor_id) > 128 THEN
    RAISE EXCEPTION 'invalid event actor' USING ERRCODE = '22023';
  END IF;
  IF p_cause NOT IN ('operator_request', 'workflow_cancelled', 'budget_exhausted', 'superseded') THEN
    RAISE EXCEPTION 'invalid_request' USING ERRCODE = '22023';
  END IF;

    actor := jsonb_build_object('kind', p_actor_kind, 'id', p_actor_id);
    target := CASE WHEN t.status IN ('running', 'cancel_requested')
                   THEN 'cancel_requested' ELSE 'cancelled' END;
    -- Events first, using the pre-update revisions captured in the
    -- locked rows; the task/workflow/action updates follow. The final
    -- state is identical to the Python order (single transaction).
    FOR affected IN
      SELECT id, project_alias, status, revision FROM forge_agent.coding_tasks
      WHERE workflow_id = t.workflow_id AND status NOT IN ('completed', 'cancelled')
      FOR UPDATE
    LOOP
      affected_target := CASE WHEN affected.status IN ('running', 'cancel_requested')
                              THEN 'cancel_requested' ELSE 'cancelled' END;
      affected_cause := CASE WHEN affected.id = p_task_id
                             THEN p_cause ELSE 'workflow_cancelled' END;
      INSERT INTO forge_agent.coding_events
        (owner_id, project_alias, task_id, sequence, event_kind, payload)
        SELECT current_setting('forge.coding_owner', true), affected.project_alias, affected.id,
               COALESCE(MAX(sequence), 0) + 1, affected_target,
               jsonb_build_object('base_revision', affected.revision,
                                  'revision', affected.revision + 1,
                                  'workflow_id', t.workflow_id::text,
                                  'actor', actor, 'cause', affected_cause)
        FROM forge_agent.coding_events
        WHERE owner_id = current_setting('forge.coding_owner', true)
          AND project_alias = affected.project_alias AND task_id = affected.id;
    END LOOP;
    UPDATE forge_agent.coding_workflows
      SET phase = 'cancelled', enabled = false, updated_at = clock_timestamp()
      WHERE workflow_id = t.workflow_id;
    UPDATE forge_agent.coding_tasks
      SET cancelled_at = CASE WHEN status IN ('running', 'cancel_requested')
                              THEN NULL ELSE clock_timestamp() END,
          status = CASE WHEN status IN ('running', 'cancel_requested')
                        THEN 'cancel_requested' ELSE 'cancelled' END,
          revision = revision + 1, updated_at = clock_timestamp()
      WHERE workflow_id = t.workflow_id AND status NOT IN ('completed', 'cancelled');
    UPDATE forge_agent.coding_workflow_actions
      SET status = 'cancelled', lease_token = NULL, lease_expires_at = NULL,
          updated_at = clock_timestamp()
      WHERE workflow_id = t.workflow_id AND status IN ('pending', 'leased');
    receipt := jsonb_build_object('project_alias', p_project_alias,
      'task_id', p_task_id::text, 'base_revision', p_expected_revision,
      'revision', p_expected_revision + 1, 'status', target, 'actor', actor);
    INSERT INTO forge_agent.coding_control_receipts
      (owner_id, project_alias, task_id, action, idempotency_key, intent_digest,
       base_revision, result_revision, receipt_json)
      VALUES (current_setting('forge.coding_owner', true), p_project_alias, p_task_id,
              'cancel', p_idempotency_key, digest,
              p_expected_revision, p_expected_revision + 1, receipt);
    PERFORM pg_notify('forge_coding_jobs', '');
    RETURN receipt;
  END IF;
  -- Single-task branch.
  IF t.status NOT IN ('queued', 'running') THEN
    RETURN jsonb_build_object('error', 'not_cancellable');
  END IF;
  IF t.revision <> p_expected_revision OR p_expected_revision >= 9007199254740991 THEN
    RETURN jsonb_build_object('error', 'revision_conflict');
  END IF;
  IF t.status = 'queued' THEN
    target := 'cancelled';
    UPDATE forge_agent.coding_tasks
      SET status = 'cancelled', revision = revision + 1,
          cancelled_at = clock_timestamp(), updated_at = clock_timestamp()
      WHERE (owner_id, project_alias, id) =
            (current_setting('forge.coding_owner', true), p_project_alias, p_task_id)
        AND status = 'queued' AND revision = p_expected_revision
      RETURNING revision INTO new_revision;
  ELSE
    target := 'cancel_requested';
    UPDATE forge_agent.coding_tasks
      SET status = 'cancel_requested', revision = revision + 1,
          updated_at = clock_timestamp()
      WHERE (owner_id, project_alias, id) =
            (current_setting('forge.coding_owner', true), p_project_alias, p_task_id)
        AND status = 'running' AND revision = p_expected_revision
      RETURNING revision INTO new_revision;
  END IF;
  IF new_revision IS NULL THEN
    RETURN jsonb_build_object('error', 'revision_conflict');
  END IF;
  -- Actor/cause validation (Python validates here, after the update).

  IF p_actor_kind NOT IN ('operator', 'coordinator', 'worker', 'agent', 'service', 'system') THEN
    RAISE EXCEPTION 'invalid event actor' USING ERRCODE = '22023';
  END IF;
  IF p_actor_id IS NULL OR length(p_actor_id) = 0 OR length(p_actor_id) > 128 THEN
    RAISE EXCEPTION 'invalid event actor' USING ERRCODE = '22023';
  END IF;
  IF p_cause NOT IN ('operator_request', 'workflow_cancelled', 'budget_exhausted', 'superseded') THEN
    RAISE EXCEPTION 'invalid_request' USING ERRCODE = '22023';
  END IF;

  actor := jsonb_build_object('kind', p_actor_kind, 'id', p_actor_id);
  receipt := jsonb_build_object('project_alias', t.project_alias,
    'task_id', p_task_id::text, 'base_revision', p_expected_revision,
    'revision', new_revision, 'status', target, 'actor', actor);
  INSERT INTO forge_agent.coding_events
    (owner_id, project_alias, task_id, sequence, event_kind, payload)
    SELECT current_setting('forge.coding_owner', true), p_project_alias, p_task_id,
           COALESCE(MAX(sequence), 0) + 1, target,
           jsonb_build_object('base_revision', p_expected_revision,
                              'revision', new_revision,
                              'actor', actor, 'cause', p_cause)
    FROM forge_agent.coding_events
    WHERE owner_id = current_setting('forge.coding_owner', true)
      AND project_alias = p_project_alias AND task_id = p_task_id;
  INSERT INTO forge_agent.coding_control_receipts
    (owner_id, project_alias, task_id, action, idempotency_key, intent_digest,
     base_revision, result_revision, receipt_json)
    VALUES (current_setting('forge.coding_owner', true), p_project_alias, p_task_id,
            'cancel', p_idempotency_key, digest,
            p_expected_revision, new_revision, receipt);
  IF target = 'cancel_requested' THEN
    PERFORM pg_notify('forge_coding_jobs', '');
  END IF;
  RETURN receipt;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_pg_proxy_api.coding_workflow_cancel(text, uuid, bigint, text, text, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_cancel TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_cancel TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_cancel TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_cancel TO forge_gpu_worker;
GRANT EXECUTE ON FUNCTION forge_pg_proxy_api.coding_workflow_cancel TO muse_proxy_writer;

CREATE OR REPLACE FUNCTION forge_external_agent._scope(p_project text,p_profile text) RETURNS forge_external_agent.scope_grants LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE g forge_external_agent.scope_grants%ROWTYPE;n bigint;effective name;
BEGIN
 effective:=CASE WHEN session_user='muse_proxy_writer' THEN 'muse_events'::name ELSE session_user END;
 SELECT count(*) INTO n FROM forge_external_agent.scope_grants WHERE external_role=effective AND project_alias=p_project AND profile_alias=p_profile AND enabled;
 IF n<>1 THEN RAISE EXCEPTION 'External scope denied' USING ERRCODE='42501';END IF;
 SELECT * INTO g FROM forge_external_agent.scope_grants WHERE external_role=effective AND project_alias=p_project AND profile_alias=p_profile AND enabled FOR SHARE;
 IF NOT FOUND THEN RAISE EXCEPTION 'External scope denied' USING ERRCODE='42501';END IF;RETURN g;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent._scope(text,text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION forge_external_agent._locked(p_id uuid) RETURNS forge_external_agent.assignments LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_external_agent.assignments%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;effective name;
BEGIN
 effective:=CASE WHEN session_user='muse_proxy_writer' THEN 'muse_events'::name ELSE session_user END;
 SELECT x.* INTO a FROM forge_external_agent.assignments x JOIN forge_external_agent.scope_grants g
 ON(g.external_role,g.owner_id,g.project_alias,g.profile_alias)=(x.external_role,x.owner_id,x.project_alias,x.profile_alias)
 WHERE x.assignment_id=p_id AND g.external_role=effective AND g.enabled FOR SHARE OF g;
 IF NOT FOUND THEN RAISE EXCEPTION 'External assignment denied' USING ERRCODE='42501';END IF;
 SELECT x.* INTO a FROM forge_external_agent.assignments x JOIN forge_agent.coding_tasks q ON(q.owner_id,q.project_alias,q.id)=(x.owner_id,x.project_alias,x.task_id)
 WHERE x.assignment_id=p_id FOR UPDATE OF q,x;
 IF NOT FOUND THEN RAISE EXCEPTION 'External assignment denied' USING ERRCODE='42501';END IF;
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id);
 IF forge_external_agent._intent(t) IS DISTINCT FROM a.intent OR t.latest_attempt_id IS NOT NULL THEN RAISE EXCEPTION 'External task binding changed' USING ERRCODE='P0009';END IF;
 RETURN a;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent._locked(uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION forge_external_agent_api.finish(p_id uuid,p_gen integer,p_token uuid,p_content text,p_summary text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_external_agent.assignments%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;digest text;intent_hash text;r jsonb;stamp timestamptz:=clock_timestamp();effective name;
BEGIN
 effective:=CASE WHEN session_user='muse_proxy_writer' THEN 'muse_events'::name ELSE session_user END;
 a:=forge_external_agent._locked(p_id);
 IF p_gen IS NULL OR p_token IS NULL OR p_content IS NULL OR octet_length(p_content) NOT BETWEEN 1 AND 262144 OR btrim(p_content)='' OR translate(p_content,E'\n\r\t','')~'[[:cntrl:]]'
 OR p_summary IS NULL OR octet_length(p_summary) NOT BETWEEN 1 AND 4096 OR btrim(p_summary)='' OR translate(p_summary,E'\n\r\t','')~'[[:cntrl:]]' THEN RAISE EXCEPTION 'Invalid external result' USING ERRCODE='22023';END IF;
 digest:=encode(sha256(convert_to(p_content,'UTF8')),'hex');
 intent_hash:=encode(sha256(convert_to(jsonb_build_object('assignment',p_id,'scope',a.intent_digest,'principal',effective,'generation',p_gen,'sha256',digest,'summary',p_summary)::text,'UTF8')),'hex');
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id);
 IF t.status IN('cancelled','cancel_requested') THEN RAISE EXCEPTION 'External task cancelled' USING ERRCODE='P0009';END IF;
 IF a.status='submitted' THEN
 IF a.generation<>p_gen OR a.finish_token IS DISTINCT FROM p_token OR a.finish_intent IS DISTINCT FROM intent_hash THEN RAISE EXCEPTION 'External finish conflict' USING ERRCODE='P0009';END IF;
 RETURN a.receipt;END IF;
 a:=forge_external_agent._active(p_id,p_gen,p_token);
 r:=jsonb_build_object('assignment_id',p_id,'task_id',a.task_id,'project_alias',a.project_alias,'provider','muse-db','status','awaiting_review','sha256',digest,'generation',p_gen,'submitted_at',to_char(stamp AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'));
 INSERT INTO forge_external_agent.artifacts(assignment_id,sha256,content,summary,submitted_at,principal,generation,recipe_digest) VALUES(p_id,digest,p_content,p_summary,stamp,effective,p_gen,a.intent->>'recipe_digest');
 UPDATE forge_agent.coding_tasks SET status='awaiting_review',revision=revision+1,updated_at=stamp WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id);
 UPDATE forge_external_agent.assignments SET status='submitted',lease_token=NULL,lease_until=NULL,finish_token=p_token,finish_intent=intent_hash,receipt=r,updated_at=stamp WHERE assignment_id=p_id;
 PERFORM forge_external_agent._event(a,'awaiting_review',r);RETURN r;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent_api.finish(uuid,integer,uuid,text,text) FROM PUBLIC;

-- Writer principal: the proxy's muse identity for mutations.
GRANT USAGE ON SCHEMA forge_external_agent_api TO muse_proxy_writer;
GRANT EXECUTE ON FUNCTION forge_external_agent_api.advertise(text,text,text) TO muse_proxy_writer;
GRANT EXECUTE ON FUNCTION forge_external_agent_api.claim(text,text) TO muse_proxy_writer;
GRANT EXECUTE ON FUNCTION forge_external_agent_api.renew(uuid,integer,uuid) TO muse_proxy_writer;
GRANT EXECUTE ON FUNCTION forge_external_agent_api.progress(uuid,integer,uuid,text) TO muse_proxy_writer;
GRANT EXECUTE ON FUNCTION forge_external_agent_api.finish(uuid,integer,uuid,text,text) TO muse_proxy_writer;
GRANT EXECUTE ON FUNCTION forge_external_agent_api.release(uuid,integer,uuid) TO muse_proxy_writer;

-- Proxy read surface (§8: every inventoried read works
-- through the proxy's read API, as the caller's role).
-- SELECT on each allowlisted table; USAGE on its schema.
GRANT USAGE ON SCHEMA forge_agent TO forge_agent;
GRANT USAGE ON SCHEMA forge_external_agent TO forge_agent;
GRANT USAGE ON SCHEMA forge_external_workflow TO forge_agent;
GRANT SELECT ON forge_agent.coding_attempts TO forge_agent;
GRANT SELECT ON forge_agent.coding_control_receipts TO forge_agent;
GRANT SELECT ON forge_agent.coding_events TO forge_agent;
GRANT SELECT ON forge_agent.coding_feedback TO forge_agent;
GRANT SELECT ON forge_agent.coding_hosted_checks TO forge_agent;
GRANT SELECT ON forge_agent.coding_plan_decisions TO forge_agent;
GRANT SELECT ON forge_agent.coding_provider_usage TO forge_agent;
GRANT SELECT ON forge_agent.coding_publications TO forge_agent;
GRANT SELECT ON forge_agent.coding_reconciliation_receipts TO forge_agent;
GRANT SELECT ON forge_agent.coding_result_chunks TO forge_agent;
GRANT SELECT ON forge_agent.coding_retry_event_admissions TO forge_agent;
GRANT SELECT ON forge_agent.coding_retry_event_observations TO forge_agent;
GRANT SELECT ON forge_agent.coding_retry_events TO forge_agent;
GRANT SELECT ON forge_agent.coding_review_budget TO forge_agent;
GRANT SELECT ON forge_agent.coding_review_policy TO forge_agent;
GRANT SELECT ON forge_agent.coding_reviews TO forge_agent;
GRANT SELECT ON forge_agent.coding_tasks TO forge_agent;
GRANT SELECT ON forge_agent.coding_usage_refresh_requests TO forge_agent;
GRANT SELECT ON forge_agent.coding_worker_grants TO forge_agent;
GRANT SELECT ON forge_agent.coding_worker_readiness TO forge_agent;
GRANT SELECT ON forge_agent.coding_workflow_actions TO forge_agent;
GRANT SELECT ON forge_agent.coding_workflow_reviews TO forge_agent;
GRANT SELECT ON forge_agent.coding_workflows TO forge_agent;
GRANT SELECT ON forge_agent.conversations TO forge_agent;
GRANT SELECT ON forge_agent.gpu_worker_readiness TO forge_agent;
GRANT SELECT ON forge_agent.ide_chats TO forge_agent;
GRANT SELECT ON forge_agent.ide_workspace_history TO forge_agent;
GRANT SELECT ON forge_agent.ide_workspace_tabs TO forge_agent;
GRANT SELECT ON forge_agent.inference_results TO forge_agent;
GRANT SELECT ON forge_agent.jenkins_event_deliveries TO forge_agent;
GRANT SELECT ON forge_agent.jenkins_events TO forge_agent;
GRANT SELECT ON forge_agent.jenkins_observations TO forge_agent;
GRANT SELECT ON forge_agent.mail_messages TO forge_agent;
GRANT SELECT ON forge_agent.mail_threads TO forge_agent;
GRANT SELECT ON forge_agent.messages TO forge_agent;
GRANT SELECT ON forge_agent.run_events TO forge_agent;
GRANT SELECT ON forge_agent.runs TO forge_agent;
GRANT SELECT ON forge_agent.tool_calls TO forge_agent;
GRANT SELECT ON forge_agent.tool_results TO forge_agent;
GRANT SELECT ON forge_external_agent.artifacts TO forge_agent;
GRANT SELECT ON forge_external_agent.assignments TO forge_agent;
GRANT SELECT ON forge_external_agent.readiness TO forge_agent;
GRANT SELECT ON forge_external_agent.scope_grants TO forge_agent;
GRANT SELECT ON forge_external_workflow.actions TO forge_agent;
GRANT SELECT ON forge_external_workflow.contexts TO forge_agent;
GRANT SELECT ON forge_external_workflow.plan_approve_overrides TO forge_agent;
GRANT SELECT ON forge_external_workflow.reviews TO forge_agent;
GRANT SELECT ON forge_external_workflow.tasks TO forge_agent;
GRANT SELECT ON forge_external_workflow.workflows TO forge_agent;
GRANT USAGE ON SCHEMA forge_agent TO forge_coding_reconciler;
GRANT SELECT ON forge_agent.coding_attempts TO forge_coding_reconciler;
GRANT SELECT ON forge_agent.coding_events TO forge_coding_reconciler;
GRANT SELECT ON forge_agent.coding_feedback TO forge_coding_reconciler;
GRANT SELECT ON forge_agent.coding_hosted_checks TO forge_coding_reconciler;
GRANT SELECT ON forge_agent.coding_plan_decisions TO forge_coding_reconciler;
GRANT SELECT ON forge_agent.coding_publications TO forge_coding_reconciler;
GRANT SELECT ON forge_agent.coding_reconciliation_receipts TO forge_coding_reconciler;
GRANT SELECT ON forge_agent.coding_result_chunks TO forge_coding_reconciler;
GRANT SELECT ON forge_agent.coding_review_budget TO forge_coding_reconciler;
GRANT SELECT ON forge_agent.coding_reviews TO forge_coding_reconciler;
GRANT SELECT ON forge_agent.coding_tasks TO forge_coding_reconciler;
GRANT SELECT ON forge_agent.coding_workflow_actions TO forge_coding_reconciler;
GRANT SELECT ON forge_agent.coding_workflow_reviews TO forge_coding_reconciler;
GRANT SELECT ON forge_agent.coding_workflows TO forge_coding_reconciler;
GRANT USAGE ON SCHEMA forge_agent TO forge_coding_worker;
GRANT SELECT ON forge_agent.coding_attempts TO forge_coding_worker;
GRANT SELECT ON forge_agent.coding_events TO forge_coding_worker;
GRANT SELECT ON forge_agent.coding_feedback TO forge_coding_worker;
GRANT SELECT ON forge_agent.coding_hosted_checks TO forge_coding_worker;
GRANT SELECT ON forge_agent.coding_plan_decisions TO forge_coding_worker;
GRANT SELECT ON forge_agent.coding_reconciliation_receipts TO forge_coding_worker;
GRANT SELECT ON forge_agent.coding_result_chunks TO forge_coding_worker;
GRANT SELECT ON forge_agent.coding_retry_events TO forge_coding_worker;
GRANT SELECT ON forge_agent.coding_review_budget TO forge_coding_worker;
GRANT SELECT ON forge_agent.coding_review_policy TO forge_coding_worker;
GRANT SELECT ON forge_agent.coding_reviews TO forge_coding_worker;
GRANT SELECT ON forge_agent.coding_tasks TO forge_coding_worker;
GRANT SELECT ON forge_agent.coding_usage_refresh_requests TO forge_coding_worker;
GRANT SELECT ON forge_agent.coding_worker_grants TO forge_coding_worker;
GRANT SELECT ON forge_agent.coding_workflow_actions TO forge_coding_worker;
GRANT SELECT ON forge_agent.coding_workflow_reviews TO forge_coding_worker;
GRANT SELECT ON forge_agent.coding_workflows TO forge_coding_worker;
GRANT USAGE ON SCHEMA forge_agent TO forge_gpu_worker;
GRANT SELECT ON forge_agent.gpu_worker_readiness TO forge_gpu_worker;
GRANT SELECT ON forge_agent.inference_results TO forge_gpu_worker;
GRANT SELECT ON forge_agent.runs TO forge_gpu_worker;
GRANT USAGE ON SCHEMA forge_agent TO muse_proxy_writer;
GRANT USAGE ON SCHEMA forge_external_agent TO muse_proxy_writer;
GRANT SELECT ON forge_agent.coding_events TO muse_proxy_writer;
GRANT SELECT ON forge_agent.coding_tasks TO muse_proxy_writer;
GRANT SELECT ON forge_external_agent.artifacts TO muse_proxy_writer;
GRANT SELECT ON forge_external_agent.assignments TO muse_proxy_writer;
GRANT SELECT ON forge_external_agent.readiness TO muse_proxy_writer;
GRANT SELECT ON forge_external_agent.scope_grants TO muse_proxy_writer;
