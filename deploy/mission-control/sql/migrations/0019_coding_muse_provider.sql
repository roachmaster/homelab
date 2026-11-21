-- M19: fourth native backend with immutable P10 recipes. No historical row changes.
-- Retain every preceding policy, receipt, lease and role boundary.
ALTER TABLE forge_agent.coding_tasks DROP CONSTRAINT coding_tasks_pinned_recipe_check,
 ADD CONSTRAINT coding_tasks_pinned_recipe_check CHECK (
        (
            jsonb_typeof(pinned_recipe) = 'object'
            AND pinned_recipe ? 'version'
            AND pinned_recipe ? 'backend'
            AND jsonb_typeof(pinned_recipe->'backend') = 'string'
            AND pinned_recipe->>'backend' IN ('antigravity', 'codex', 'grok', 'muse')
            AND pinned_recipe ? 'model'
            AND jsonb_typeof(pinned_recipe->'model') = 'string'
            AND octet_length(pinned_recipe->>'model') >= 1
            AND octet_length(pinned_recipe->>'model') <= 128
            AND btrim(pinned_recipe->>'model') <> ''
            AND (pinned_recipe->>'model') ~ '^[ -~]+$'
            AND pinned_recipe ? 'resource_profile'
            AND jsonb_typeof(pinned_recipe->'resource_profile') = 'string'
            AND (pinned_recipe->>'resource_profile') ~ '^[A-Za-z0-9_.-]{1,64}$'
            AND pinned_recipe ? 'mode'
            AND jsonb_typeof(pinned_recipe->'mode') = 'string'
            AND pinned_recipe->>'mode' IN ('plan', 'review', 'implement')
            AND pinned_recipe ? 'generation_token_limit'
            AND jsonb_typeof(pinned_recipe->'generation_token_limit') = 'null'
            AND (
                (
                    pinned_recipe->'version' = '1'::jsonb
                    AND pinned_recipe - ARRAY[
                        'version', 'backend', 'model', 'resource_profile', 'mode', 'generation_token_limit'
                    ] = '{}'::jsonb
                )
                OR (
                    pinned_recipe->'version' = '2'::jsonb
                    AND pinned_recipe - ARRAY[
                        'version', 'backend', 'model', 'resource_profile', 'mode',
                        'generation_token_limit', 'policy_digest'
                    ] = '{}'::jsonb
                    AND pinned_recipe ? 'policy_digest'
                    AND jsonb_typeof(pinned_recipe->'policy_digest') = 'string'
                    AND (pinned_recipe->>'policy_digest') ~ '^[0-9a-f]{64}$'
                )
            )
        ) IS TRUE);
ALTER TABLE forge_agent.coding_attempts DROP CONSTRAINT coding_attempts_pinned_recipe_check,
 ADD CONSTRAINT coding_attempts_pinned_recipe_check CHECK (
            (jsonb_typeof(pinned_recipe) = 'object'
             AND pinned_recipe - ARRAY[
                 'version', 'backend', 'model', 'resource_profile', 'mode',
                 'generation_token_limit', 'policy_digest'
             ] = '{}'::jsonb
             AND pinned_recipe ?& ARRAY[
                 'version', 'backend', 'model', 'resource_profile', 'mode',
                 'generation_token_limit', 'policy_digest'
             ]
             AND pinned_recipe->'version' = '2'::jsonb
             AND pinned_recipe->>'backend' IN ('antigravity', 'codex', 'grok', 'muse')
             AND jsonb_typeof(pinned_recipe->'model') = 'string'
             AND octet_length(pinned_recipe->>'model') BETWEEN 1 AND 128
             AND pinned_recipe->>'resource_profile' IN ('forge-coding-v1', 'forge-coding-v2', 'forge-coding-v3', 'forge-coding-v4', 'forge-coding-v5', 'forge-coding-v6', 'forge-coding-v7', 'forge-coding-v8', 'forge-coding-v9', 'forge-coding-v10')
             AND pinned_recipe->>'mode' IN ('plan', 'review', 'implement')
             AND jsonb_typeof(pinned_recipe->'generation_token_limit') = 'null'
             AND (pinned_recipe->>'policy_digest') ~ '^[0-9a-f]{64}$') IS TRUE);

-- Extend only the exact reviewed capability predicates; abort on unexpected
-- predecessor definitions rather than silently replacing unrelated SQL.
DO $upgrade$
DECLARE target text; definition text; previous text;
BEGIN
 FOREACH target IN ARRAY ARRAY[
  'forge_coding_worker_api.advertise(text,uuid,jsonb,jsonb,text)',
  'forge_coding_worker_api._validate_usage_json(jsonb)',
  'forge_coding_worker_api.claim(text,uuid,text[])',
  'forge_coding_reconciler_api.review_claim(text)',
  'forge_coding_reconciler_api.publication_claim(text)',
  'forge_agent.coding_make_feedback(uuid,uuid,text,text,jsonb)'
 ] LOOP
  previous := pg_get_functiondef(target::regprocedure);
  definition := replace(previous, '(''antigravity'', ''codex'', ''grok'')', '(''antigravity'', ''codex'', ''grok'', ''muse'')');
  definition := replace(definition, '(''codex-exec-turn-v1'', ''agy-result-v1'', ''grok-headless-end-v1'')', '(''codex-exec-turn-v1'', ''agy-result-v1'', ''grok-headless-end-v1'', ''muse-run-usage-v1'')');
  definition := replace(definition, '''forge-coding-v9'')', '''forge-coding-v9'', ''forge-coding-v10'')');
  IF target='forge_coding_worker_api.claim(text,uuid,text[])' THEN
   definition := replace(definition,
    $old$AND (t.pinned_recipe->>'policy_digest') = r.policy_digest$old$,
    $new$AND (t.pinned_recipe->>'policy_digest') = r.policy_digest
          AND (t.pinned_recipe->>'backend'<>'muse' OR EXISTS (
              SELECT 1 FROM jsonb_array_elements(r.profiles) AS configured(profile)
              WHERE profile->>'alias'=t.agent_profile
                AND profile->>'backend'='muse'
                AND profile->>'model'=t.pinned_recipe->>'model'
                AND profile->'modes' ? t.mode
          ))$new$);
  END IF;
  IF definition=previous THEN RAISE EXCEPTION 'M19 predecessor capability missing: %',target;END IF;
  EXECUTE definition;
 END LOOP;
END;
$upgrade$;
-- CREATE OR REPLACE retains existing ownership and function ACLs. Internal
-- helpers remain ungranted; no new database role or arbitrary command port.
