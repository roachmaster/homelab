-- M38: immutable audit table for authorized external plan approve-overrides.
-- Narrow one-shot operator override for workflows stranded in plan-revision
-- exhaustion; preserves all prior reviews, retry events, and assignments.
SET LOCAL lock_timeout='5s';
SET LOCAL statement_timeout='60s';

CREATE TABLE forge_external_workflow.plan_approve_overrides(
 override_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
 owner_id text NOT NULL,
 project_alias text NOT NULL,
 workflow_id uuid NOT NULL UNIQUE,
 root_task_id uuid NOT NULL,
 actor_kind text NOT NULL CHECK(actor_kind='operator'),
 actor_id text NOT NULL CHECK(octet_length(actor_id) BETWEEN 1 AND 128 AND btrim(actor_id)<>''),
 reason text NOT NULL CHECK(octet_length(reason) BETWEEN 1 AND 2048 AND btrim(reason)<>''),
 acceptance_ref text NOT NULL CHECK(octet_length(acceptance_ref) BETWEEN 1 AND 256 AND btrim(acceptance_ref)<>''),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 plan_task_id uuid NOT NULL,
 plan_assignment_id uuid NOT NULL REFERENCES forge_external_agent.assignments(assignment_id),
 plan_sha256 text NOT NULL CHECK(plan_sha256~'^[0-9a-f]{64}$'),
 prior_workflow_revision bigint NOT NULL CHECK(prior_workflow_revision BETWEEN 1 AND 9007199254740991),
 prior_root_revision bigint NOT NULL CHECK(prior_root_revision BETWEEN 1 AND 9007199254740991),
 prior_phase text NOT NULL CHECK(prior_phase='escalated'),
 prior_plan_revisions integer NOT NULL CHECK(prior_plan_revisions>=0),
 prior_review_id uuid NOT NULL REFERENCES forge_external_workflow.reviews(review_id),
 prior_escalation_reason text NOT NULL CHECK(octet_length(prior_escalation_reason) BETWEEN 1 AND 2048),
 retry_event_id uuid REFERENCES forge_agent.coding_retry_events(event_id),
 idempotency_key text NOT NULL CHECK(octet_length(idempotency_key) BETWEEN 1 AND 128 AND idempotency_key~'^[A-Za-z0-9_.-]{1,128}$'),
 intent_digest text NOT NULL CHECK(intent_digest~'^[0-9a-f]{64}$'),
 implement_task_id uuid NOT NULL,
 implement_assignment_id uuid NOT NULL REFERENCES forge_external_agent.assignments(assignment_id),
 receipt jsonb NOT NULL CHECK(jsonb_typeof(receipt)='object' AND octet_length(receipt::text)<=65536),
 CHECK(plan_task_id<>implement_task_id AND plan_assignment_id<>implement_assignment_id),
 FOREIGN KEY(owner_id,project_alias,workflow_id) REFERENCES forge_external_workflow.workflows(owner_id,project_alias,workflow_id),
 FOREIGN KEY(owner_id,project_alias,root_task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id),
 FOREIGN KEY(owner_id,project_alias,plan_task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id),
 FOREIGN KEY(owner_id,project_alias,implement_task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id)
);

CREATE FUNCTION forge_external_workflow.immutable_plan_approve_override() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 RAISE EXCEPTION 'Immutable external plan approve override' USING ERRCODE='P0009';
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_workflow.immutable_plan_approve_override() FROM PUBLIC;
CREATE TRIGGER external_plan_approve_override_immutable BEFORE UPDATE OR DELETE ON forge_external_workflow.plan_approve_overrides FOR EACH ROW EXECUTE FUNCTION forge_external_workflow.immutable_plan_approve_override();

REVOKE ALL ON forge_external_workflow.plan_approve_overrides FROM PUBLIC;
GRANT SELECT,INSERT ON forge_external_workflow.plan_approve_overrides TO forge_agent;
