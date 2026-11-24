CREATE FUNCTION forge_agent.coding_review_supersession_guard() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $guard$
DECLARE parent forge_agent.coding_workflow_reviews%ROWTYPE;
BEGIN
 IF TG_OP='UPDATE' THEN
  IF(NEW.review_generation,NEW.supersedes_review_id) IS DISTINCT FROM(OLD.review_generation,OLD.supersedes_review_id)
  THEN RAISE EXCEPTION 'Immutable independent review lineage' USING ERRCODE='P0009';END IF;
  RETURN NEW;
 END IF;
 IF NEW.supersedes_review_id IS NULL THEN RETURN NEW;END IF;
 SELECT * INTO parent FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=NEW.supersedes_review_id FOR SHARE;
 IF NOT FOUND OR parent.status<>'escalated' OR parent.review_generation+1<>NEW.review_generation
 OR(NEW.workflow_id,NEW.owner_id,NEW.project_alias,NEW.target_kind,NEW.source_task_id,NEW.source_attempt_id,NEW.source_result_sha256,NEW.source_revision,NEW.source_review_id,NEW.candidate_commit,NEW.gate_manifest_sha256)
  IS DISTINCT FROM(parent.workflow_id,parent.owner_id,parent.project_alias,parent.target_kind,parent.source_task_id,parent.source_attempt_id,parent.source_result_sha256,parent.source_revision,parent.source_review_id,parent.candidate_commit,parent.gate_manifest_sha256)
 OR NEW.status<>'pending' OR NEW.target_json IS NOT NULL OR NEW.target_sha256 IS NOT NULL
 OR NEW.reviewer_task_id IS NOT NULL OR NEW.reviewer_attempt_id IS NOT NULL OR NEW.reviewer_result_sha256 IS NOT NULL
 OR NEW.verdict_json IS NOT NULL OR NEW.verdict_sha256 IS NOT NULL
 THEN RAISE EXCEPTION 'Superseding review source or generation changed' USING ERRCODE='P0009';END IF;
 RETURN NEW;
END;$guard$;
REVOKE ALL ON FUNCTION forge_agent.coding_review_supersession_guard() FROM PUBLIC;
CREATE TRIGGER coding_workflow_reviews_supersession_guard BEFORE INSERT OR UPDATE ON forge_agent.coding_workflow_reviews
 FOR EACH ROW EXECUTE FUNCTION forge_agent.coding_review_supersession_guard();
