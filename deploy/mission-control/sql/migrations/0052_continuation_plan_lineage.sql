-- M52: separate the historical budget root from the approved Plan owner.
-- Only source-review-bound linked repair ancestry can widen Plan ownership.
DO $m52$
DECLARE source text;
BEGIN
 SELECT prosrc INTO source FROM pg_proc WHERE oid='forge_coding_worker_api.read_plan(uuid,uuid)'::regprocedure;
 IF encode(sha256(convert_to(source,'UTF8')),'hex')<>'1b108a060c2717971cd1ad21cedad7d231f392e64ed49f6cafdabec3a6a674ce'
 OR (length(source)-length(replace(source,$old$(x.task_id=t.id OR x.task_id=coalesce(t.repair_root_task_id,t.id))$old$,'')))/length($old$(x.task_id=t.id OR x.task_id=coalesce(t.repair_root_task_id,t.id))$old$)<>1
 THEN RAISE EXCEPTION 'M52 read_plan predecessor drift';END IF;
 EXECUTE replace(pg_get_functiondef('forge_coding_worker_api.read_plan(uuid,uuid)'::regprocedure),$old$(x.task_id=t.id OR x.task_id=coalesce(t.repair_root_task_id,t.id))$old$,$new$(x.task_id=t.id OR x.task_id=coalesce(t.repair_root_task_id,t.id) OR EXISTS (
        WITH RECURSIVE lineage(id,seen) AS (
            SELECT t.id,ARRAY[t.id]
            UNION ALL
            SELECT parent.id,lineage.seen||parent.id
            FROM lineage
            JOIN forge_agent.coding_tasks child ON child.owner_id=t.owner_id AND child.project_alias=t.project_alias AND child.id=lineage.id
            JOIN forge_agent.coding_reviews review ON review.owner_id=child.owner_id AND review.project_alias=child.project_alias
              AND review.review_id=child.repair_source_review_id AND review.task_id=child.parent_task_id
              AND review.repair_task_id=child.id AND review.status='repair'
              AND review.candidate_commit=child.repair_candidate_commit AND review.candidate_commit=child.base_revision
              AND coalesce(review.plan_decision_id,review.scope_plan_decision_id)=x.decision_id
            JOIN forge_agent.coding_tasks parent ON parent.owner_id=child.owner_id AND parent.project_alias=child.project_alias
              AND parent.id=review.task_id AND parent.workflow_id=t.workflow_id
              AND coalesce(parent.plan_decision_id,parent.scope_plan_decision_id)=x.decision_id
            JOIN forge_agent.coding_feedback feedback ON feedback.owner_id=child.owner_id AND feedback.project_alias=child.project_alias
              AND feedback.feedback_id=child.feedback_id AND feedback.feedback_sha256=child.feedback_sha256
              AND feedback.review_id=review.review_id AND feedback.task_id=child.id AND feedback.parent_attempt_id=review.attempt_id
            WHERE child.workflow_id=t.workflow_id AND t.workflow_id IS NOT NULL AND x.decision='resume'
              AND child.scope_plan_decision_id=x.decision_id AND child.plan_decision_id IS NULL
              AND child.mode='implement' AND parent.mode='implement'
              AND cardinality(lineage.seen)<32 AND NOT parent.id=ANY(lineage.seen)
        ) SELECT 1 FROM lineage WHERE lineage.id=x.task_id AND cardinality(lineage.seen)>1
    ))$new$);
END
$m52$;
