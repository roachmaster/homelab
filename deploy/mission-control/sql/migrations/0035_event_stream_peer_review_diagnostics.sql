-- Event-stream reliability fix 1: peer-review diagnostics and superseding-review correction.
--
-- coding_workflow_reviews gains a diagnostics column (set only when a peer-wire
-- failure escalates the workflow; carries the failing check id plus expected /
-- actual digest detail, never finding or source text) and a supersedes pointer
-- for the append-only correction path. A superseded row keeps its target and
-- verdict bytes untouched; only its status changes, which the evidence-guard
-- trigger does not guard. M33's generation-aware source-binding constraint
-- and one-live-source index remain authoritative for both correction paths.
-- A replacement must advance the canonical generation rather than reuse an
-- unversioned source binding.
ALTER TABLE forge_agent.coding_workflow_reviews
    ADD COLUMN diagnostics jsonb CHECK (diagnostics IS NULL OR octet_length(diagnostics::text) <= 65536),
    ADD COLUMN supersedes_workflow_review_id uuid REFERENCES forge_agent.coding_workflow_reviews(workflow_review_id);
ALTER TABLE forge_agent.coding_workflow_reviews
    DROP CONSTRAINT coding_workflow_reviews_status_check,
    ADD CONSTRAINT coding_workflow_reviews_status_check
        CHECK (status IN ('pending', 'reviewing', 'decided', 'escalated', 'cancelled', 'superseded'));
