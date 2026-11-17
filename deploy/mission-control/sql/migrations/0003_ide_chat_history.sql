ALTER TABLE forge_agent.ide_workspace_history
    ADD COLUMN chat_history_active boolean NOT NULL DEFAULT false;

ALTER TABLE forge_agent.ide_workspace_tabs
    ADD COLUMN chat_history_enabled boolean NOT NULL DEFAULT false;

CREATE TABLE forge_agent.ide_chats (
    owner_id text NOT NULL REFERENCES forge_agent.ide_workspace_history (owner_id),
    id uuid NOT NULL,
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    title text,
    messages jsonb NOT NULL DEFAULT '[]'::jsonb CHECK (
        jsonb_typeof(messages) = 'array' AND octet_length(messages::text) <= 131072
    ),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    expires_on date,
    time_zone text,
    expires_at timestamptz,
    removed_at timestamptz,
    removal_reason text CHECK (removal_reason IN ('deleted', 'expired')),
    PRIMARY KEY (owner_id, id),
    CHECK ((expires_on IS NULL AND time_zone IS NULL AND expires_at IS NULL)
        OR (expires_on IS NOT NULL AND time_zone IS NOT NULL AND expires_at IS NOT NULL)),
    CHECK ((title IS NULL) = (jsonb_array_length(messages) = 0)),
    CHECK ((removed_at IS NULL AND removal_reason IS NULL)
        OR (removed_at IS NOT NULL AND removal_reason IS NOT NULL AND title IS NULL AND expires_at IS NULL))
);

CREATE INDEX ide_chats_recent
    ON forge_agent.ide_chats (owner_id, updated_at DESC, id DESC)
    WHERE removed_at IS NULL AND title IS NOT NULL;

CREATE INDEX ide_chats_due
    ON forge_agent.ide_chats (owner_id, expires_at, id)
    WHERE removed_at IS NULL AND expires_at IS NOT NULL;

ALTER TABLE forge_agent.runs
    ADD COLUMN ide_chat_owner_id text,
    ADD COLUMN ide_chat_id uuid,
    ADD COLUMN ide_chat_revision bigint CHECK (ide_chat_revision > 0),
    ADD FOREIGN KEY (ide_chat_owner_id, ide_chat_id)
        REFERENCES forge_agent.ide_chats (owner_id, id) MATCH FULL,
    ADD CHECK (ide_chat_revision IS NULL OR ide_chat_id IS NOT NULL);

CREATE INDEX runs_by_ide_chat
    ON forge_agent.runs (ide_chat_owner_id, ide_chat_id)
    WHERE ide_chat_id IS NOT NULL;
