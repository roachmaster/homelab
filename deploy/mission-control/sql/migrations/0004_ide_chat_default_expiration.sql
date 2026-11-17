-- Keep concurrent commits and retention changes out of the revision calculation.
LOCK TABLE forge_agent.ide_chats IN EXCLUSIVE MODE;

WITH previously_default AS (
    SELECT chat.owner_id, chat.id
    FROM forge_agent.ide_chats AS chat
    CROSS JOIN LATERAL (
        SELECT count(*) AS completed_turns, min(run.ide_chat_revision) AS first_revision
        FROM forge_agent.runs AS run
        WHERE (run.ide_chat_owner_id, run.ide_chat_id) = (chat.owner_id, chat.id)
            AND run.status = 'completed' AND run.ide_chat_revision IS NOT NULL
    ) AS history
    WHERE chat.removed_at IS NULL AND chat.title IS NOT NULL AND chat.expires_at IS NULL
        -- A new linked chat starts on revision 1 with its first completed run.
        -- An imported chat has one revision before any linked run. In either
        -- case, additional revisions on an indefinite chat mean an explicit Keep.
        AND chat.revision = history.completed_turns +
            CASE WHEN history.first_revision = 1 THEN 0 ELSE 1 END
), deadline AS (
    SELECT (statement_timestamp() AT TIME ZONE 'UTC')::date + 30 AS keep_through
)
UPDATE forge_agent.ide_chats AS chat
SET expires_on = deadline.keep_through,
    time_zone = 'UTC',
    expires_at = (deadline.keep_through + 1)::timestamp AT TIME ZONE 'UTC',
    revision = chat.revision + 1
FROM previously_default, deadline
WHERE (chat.owner_id, chat.id) = (previously_default.owner_id, previously_default.id);
