\set ON_ERROR_STOP on
BEGIN;

SELECT NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'forge_studio_api') AS create_api \gset
\if :create_api
CREATE ROLE forge_studio_api NOLOGIN;
\endif
SELECT NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'forge_studio_worker') AS create_worker \gset
\if :create_worker
CREATE ROLE forge_studio_worker NOLOGIN;
\endif

GRANT USAGE ON SCHEMA forge_game TO forge_studio_api, forge_studio_worker;
GRANT SELECT ON forge_game.builds, forge_game.build_events TO forge_studio_api, forge_studio_worker;
GRANT INSERT (build_id, node_alias, project_alias, revision, profile, source_recipe, intent_key, state)
ON forge_game.builds TO forge_studio_api;
GRANT UPDATE (state, attempt_token, lease_until) ON forge_game.builds TO forge_studio_api;
GRANT UPDATE (state, attempts, attempt_token, lease_until, artifact_digest, artifact_bytes)
ON forge_game.builds TO forge_studio_worker;
GRANT INSERT ON forge_game.build_events TO forge_studio_api, forge_studio_worker;

COMMIT;
