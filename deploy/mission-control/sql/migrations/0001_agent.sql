CREATE TABLE forge_agent.conversations (
    id uuid PRIMARY KEY,
    owner_id text NOT NULL CHECK (btrim(owner_id) <> ''),
    title text,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX conversations_by_owner
    ON forge_agent.conversations (owner_id, created_at DESC);

CREATE TABLE forge_agent.runs (
    id uuid PRIMARY KEY,
    conversation_id uuid NOT NULL
        REFERENCES forge_agent.conversations (id) ON DELETE CASCADE,
    request_id text NOT NULL CHECK (btrim(request_id) <> ''),
    model_id text NOT NULL CHECK (btrim(model_id) <> ''),
    status text NOT NULL DEFAULT 'queued'
        CHECK (status IN (
            'queued', 'running', 'waiting_for_tool', 'completed',
            'failed', 'cancelled', 'interrupted'
        )),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (conversation_id, request_id),
    UNIQUE (conversation_id, id)
);

CREATE INDEX runs_by_status
    ON forge_agent.runs (status, created_at);

CREATE TABLE forge_agent.tool_calls (
    run_id uuid NOT NULL REFERENCES forge_agent.runs (id) ON DELETE CASCADE,
    call_id text NOT NULL CHECK (btrim(call_id) <> ''),
    tool_name text NOT NULL CHECK (btrim(tool_name) <> ''),
    arguments jsonb NOT NULL CHECK (jsonb_typeof(arguments) = 'object'),
    status text NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'finished', 'cancelled', 'outcome_unknown')),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, call_id)
);

CREATE TABLE forge_agent.tool_results (
    run_id uuid NOT NULL,
    call_id text NOT NULL,
    outcome text NOT NULL
        CHECK (outcome IN ('exited', 'timed_out', 'cancelled', 'execution_error')),
    exit_code integer,
    stdout text NOT NULL DEFAULT '',
    stderr text NOT NULL DEFAULT '',
    output_truncated boolean NOT NULL DEFAULT false,
    recorded_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, call_id),
    FOREIGN KEY (run_id, call_id)
        REFERENCES forge_agent.tool_calls (run_id, call_id) ON DELETE CASCADE,
    CHECK (
        (outcome = 'exited' AND exit_code IS NOT NULL AND exit_code BETWEEN 0 AND 255)
        OR (outcome <> 'exited' AND exit_code IS NULL)
    )
);

CREATE TABLE forge_agent.messages (
    conversation_id uuid NOT NULL,
    position bigint NOT NULL CHECK (position > 0),
    run_id uuid NOT NULL,
    role text NOT NULL CHECK (role IN ('system', 'user', 'assistant', 'tool')),
    content text,
    tool_call_id text,
    tool_result_call_id text,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (conversation_id, position),
    FOREIGN KEY (conversation_id, run_id)
        REFERENCES forge_agent.runs (conversation_id, id) ON DELETE CASCADE,
    FOREIGN KEY (run_id, tool_call_id)
        REFERENCES forge_agent.tool_calls (run_id, call_id),
    FOREIGN KEY (run_id, tool_result_call_id)
        REFERENCES forge_agent.tool_results (run_id, call_id),
    UNIQUE (run_id, tool_call_id),
    UNIQUE (run_id, tool_result_call_id),
    CHECK (
        (role IN ('system', 'user') AND content IS NOT NULL
            AND tool_call_id IS NULL AND tool_result_call_id IS NULL)
        OR (role = 'assistant' AND (content IS NOT NULL OR tool_call_id IS NOT NULL)
            AND tool_result_call_id IS NULL)
        OR (role = 'tool' AND content IS NULL
            AND tool_call_id IS NULL AND tool_result_call_id IS NOT NULL)
    )
);

CREATE TABLE forge_agent.run_events (
    run_id uuid NOT NULL REFERENCES forge_agent.runs (id) ON DELETE CASCADE,
    sequence bigint NOT NULL CHECK (sequence > 0),
    event_type text NOT NULL CHECK (btrim(event_type) <> ''),
    data jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(data) = 'object'),
    recorded_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, sequence)
);
