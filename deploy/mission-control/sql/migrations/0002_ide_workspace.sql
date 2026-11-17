CREATE TABLE forge_agent.ide_workspace_history (
    owner_id text PRIMARY KEY CHECK (octet_length(owner_id) BETWEEN 1 AND 128),
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    transcripts jsonb NOT NULL DEFAULT '[]'::jsonb CHECK (
        jsonb_typeof(transcripts) = 'array' AND octet_length(transcripts::text) <= 2097152
    )
);

CREATE TABLE forge_agent.ide_workspace_tabs (
    owner_id text NOT NULL REFERENCES forge_agent.ide_workspace_history (owner_id),
    tab_id text NOT NULL CHECK (tab_id ~ '^[0-9a-f]{32}$'),
    lease text NOT NULL CHECK (lease ~ '^[0-9a-f]{64}$'),
    revision bigint NOT NULL DEFAULT 0 CHECK (revision BETWEEN 0 AND 4294967295),
    base_history_revision bigint NOT NULL CHECK (base_history_revision >= 0),
    base_history_fingerprint text NOT NULL CHECK (base_history_fingerprint ~ '^[0-9a-f]{64}$'),
    -- Retain the exact draft representation, including control characters escaped in JSON.
    snapshot json CHECK (json_typeof(snapshot) = 'object' AND octet_length(snapshot::text) <= 2097152),
    touched_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (owner_id, tab_id)
);
