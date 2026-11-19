ALTER TABLE forge_agent.mail_threads
    ADD COLUMN archive_reason text CHECK (archive_reason IN ('context_limit', 'manual')),
    ADD COLUMN archive_base_revision bigint CHECK (archive_base_revision > 0 AND archive_base_revision <= 9007199254740991);

UPDATE forge_agent.mail_threads
SET archive_reason = 'context_limit'
WHERE archived_at IS NOT NULL;

ALTER TABLE forge_agent.mail_threads
    ADD CONSTRAINT mail_archive_reason_check CHECK (
        ((archived_at IS NULL AND archive_reason IS NULL AND archive_base_revision IS NULL)
        OR (archived_at IS NOT NULL AND archive_reason = 'context_limit' AND archive_base_revision IS NULL)
        OR (archived_at IS NOT NULL AND archive_reason = 'manual' AND archive_base_revision IS NOT NULL AND archive_base_revision = revision - 1)) IS TRUE
    );
