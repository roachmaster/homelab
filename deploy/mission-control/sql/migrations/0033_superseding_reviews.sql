-- New independent attempts supersede malformed verdicts without changing evidence.
ALTER TABLE forge_agent.coding_workflow_reviews
 ADD COLUMN review_generation integer NOT NULL DEFAULT 0,
 ADD COLUMN supersedes_review_id uuid,
 ADD CONSTRAINT coding_workflow_reviews_review_generation_check CHECK(review_generation BETWEEN 0 AND 20),
 ADD CONSTRAINT coding_workflow_reviews_supersession_check CHECK((review_generation=0)=(supersedes_review_id IS NULL)),
 ADD CONSTRAINT coding_workflow_reviews_supersedes_review_id_fkey FOREIGN KEY(supersedes_review_id) REFERENCES forge_agent.coding_workflow_reviews(workflow_review_id),
 ADD CONSTRAINT coding_workflow_reviews_supersedes_review_id_key UNIQUE(supersedes_review_id);
ALTER TABLE forge_agent.coding_workflow_reviews
 DROP CONSTRAINT coding_workflow_reviews_workflow_id_target_kind_source_atte_key,
 ADD CONSTRAINT coding_workflow_reviews_workflow_id_target_kind_source_atte_key
 UNIQUE NULLS NOT DISTINCT(workflow_id,target_kind,source_attempt_id,source_review_id,review_generation);
CREATE UNIQUE INDEX coding_workflow_reviews_one_live_source ON forge_agent.coding_workflow_reviews
 (workflow_id,target_kind,source_attempt_id,source_review_id) NULLS NOT DISTINCT WHERE status IN('pending','reviewing');
\ir ../fragments/0033_superseding_reviews.sql
