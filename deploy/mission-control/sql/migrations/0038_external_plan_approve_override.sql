-- M38: Immutable audit record for authorized external plan approve-overrides.
-- Preserves all M22/M31 tables, functions, reviews, artifacts and retry history.
CREATE TABLE forge_external_workflow.plan_approve_overrides(
 override_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
 owner_id text NOT NULL,
 project_alias text NOT NULL,
 workflow_id uuid NOT NULL UNIQUE,
 root_task_id uuid NOT NULL,
 actor_kind text NOT NULL CHECK(actor_kind='operator'),
 actor_id text NOT NULL CHECK(octet_length(actor_id) BETWEEN 1 AND 128 AND btrim(actor_id)<>'' AND actor_id!~'[[:cntrl:]]'),
 reason text NOT NULL CHECK(octet_length(reason) BETWEEN 1 AND 2048 AND btrim(reason)<>'' AND reason!~'[[:cntrl:]]'),
 acceptance_ref text NOT NULL CHECK(octet_length(acceptance_ref) BETWEEN 1 AND 256 AND btrim(acceptance_ref)<>'' AND acceptance_ref!~'[[:cntrl:]]'),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 plan_task_id uuid NOT NULL,
 plan_assignment_id uuid NOT NULL REFERENCES forge_external_agent.assignments(assignment_id),
 plan_sha256 text NOT NULL CHECK(plan_sha256~'^[0-9a-f]{64}$'),
 prior_workflow_revision bigint NOT NULL CHECK(prior_workflow_revision BETWEEN 1 AND 9007199254740991),
 prior_root_revision bigint NOT NULL CHECK(prior_root_revision BETWEEN 1 AND 9007199254740991),
 prior_phase text NOT NULL CHECK(prior_phase='escalated'),
 prior_plan_revisions integer NOT NULL CHECK(prior_plan_revisions>=0),
 prior_review_id uuid NOT NULL REFERENCES forge_external_workflow.reviews(review_id),
 prior_escalation_reason text NOT NULL CHECK(octet_length(prior_escalation_reason) BETWEEN 1 AND 2048 AND btrim(prior_escalation_reason)<>''),
 retry_event_id uuid REFERENCES forge_agent.coding_retry_events(event_id),
 idempotency_key text NOT NULL CHECK(idempotency_key~'^[A-Za-z0-9_.-]{1,128}$'),
 intent_digest text NOT NULL CHECK(intent_digest~'^[0-9a-f]{64}$'),
 implement_task_id uuid NOT NULL,
 implement_assignment_id uuid NOT NULL REFERENCES forge_external_agent.assignments(assignment_id),
 receipt jsonb NOT NULL CHECK(jsonb_typeof(receipt)='object'),
 FOREIGN KEY(owner_id,project_alias,workflow_id) REFERENCES forge_external_workflow.workflows(owner_id,project_alias,workflow_id),
 FOREIGN KEY(owner_id,project_alias,root_task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id),
 FOREIGN KEY(owner_id,project_alias,plan_task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id),
 FOREIGN KEY(owner_id,project_alias,implement_task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id)
);
REVOKE ALL ON forge_external_workflow.plan_approve_overrides FROM PUBLIC;
GRANT SELECT,INSERT ON forge_external_workflow.plan_approve_overrides TO forge_agent;

CREATE FUNCTION forge_external_workflow.immutable_plan_approve_override() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 RAISE EXCEPTION 'Immutable external plan approve override' USING ERRCODE='P0009';
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_workflow.immutable_plan_approve_override() FROM PUBLIC;
CREATE TRIGGER external_plan_approve_override_immutable
 BEFORE UPDATE OR DELETE ON forge_external_workflow.plan_approve_overrides
 FOR EACH ROW EXECUTE FUNCTION forge_external_workflow.immutable_plan_approve_override();
