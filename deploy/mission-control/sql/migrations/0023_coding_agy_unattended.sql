-- M23: opt-in unattended Antigravity policy, separate from immutable native P10.
-- No existing task, attempt, receipt, role, grant or policy identity is changed.
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '60s';

-- The historical task recipe accepts resource identifiers generally. Bind only
-- the new identifier to its exact v2 AGY policy rather than widening other backends.
ALTER TABLE forge_agent.coding_tasks
 ADD CONSTRAINT coding_tasks_agy_unattended_recipe_check CHECK (
  pinned_recipe->>'resource_profile' IS DISTINCT FROM 'forge-coding-agy-unattended-v1'
  OR (
   jsonb_typeof(pinned_recipe)='object'
   AND pinned_recipe - ARRAY['version','backend','model','resource_profile','mode','generation_token_limit','policy_digest']='{}'::jsonb
   AND pinned_recipe ?& ARRAY['version','backend','model','resource_profile','mode','generation_token_limit','policy_digest']
   AND pinned_recipe->'version'='2'::jsonb
   AND pinned_recipe->>'backend'='antigravity'
   AND jsonb_typeof(pinned_recipe->'model')='string'
   AND octet_length(pinned_recipe->>'model') BETWEEN 1 AND 128
   AND btrim(pinned_recipe->>'model')<>''
   AND (pinned_recipe->>'model')~'^[ -~]+$'
   AND pinned_recipe->>'mode' IN('plan','review','implement')
   AND jsonb_typeof(pinned_recipe->'generation_token_limit')='null'
   AND pinned_recipe->>'policy_digest'='223cba6fa54a1d6c809ed8ab84d7b96b140962e5dbe026bc28cc3a42a29b3130'
  ) IS TRUE
 );

DO $upgrade_attempt$
DECLARE definition text;
BEGIN
 SELECT pg_get_constraintdef(oid) INTO definition FROM pg_constraint
 WHERE conrelid='forge_agent.coding_attempts'::regclass
  AND conname='coding_attempts_pinned_recipe_check' AND contype='c' AND convalidated;
 IF encode(sha256(convert_to(definition,'UTF8')),'hex') IS DISTINCT FROM
    '3983c883dbdf742473bc218d1e5281aa32db4e7d900a039a0570ed0d9701babe' THEN
  RAISE EXCEPTION 'M23 attempt recipe predecessor changed';
 END IF;
 definition:=replace(definition,'''forge-coding-v10''::text',
  '''forge-coding-v10''::text, ''forge-coding-agy-unattended-v1''::text');
 ALTER TABLE forge_agent.coding_attempts DROP CONSTRAINT coding_attempts_pinned_recipe_check;
 EXECUTE 'ALTER TABLE forge_agent.coding_attempts ADD CONSTRAINT coding_attempts_pinned_recipe_check '||definition;
END;
$upgrade_attempt$;

-- A second exact guard makes the single new allowlist entry AGY-only even for
-- direct database admissions. Native workers still need a matching live digest.
ALTER TABLE forge_agent.coding_attempts
 ADD CONSTRAINT coding_attempts_agy_unattended_recipe_check CHECK (
  pinned_recipe->>'resource_profile' IS DISTINCT FROM 'forge-coding-agy-unattended-v1'
  OR (
   jsonb_typeof(pinned_recipe)='object'
   AND pinned_recipe - ARRAY['version','backend','model','resource_profile','mode','generation_token_limit','policy_digest']='{}'::jsonb
   AND pinned_recipe ?& ARRAY['version','backend','model','resource_profile','mode','generation_token_limit','policy_digest']
   AND pinned_recipe->'version'='2'::jsonb
   AND pinned_recipe->>'backend'='antigravity'
   AND jsonb_typeof(pinned_recipe->'model')='string'
   AND octet_length(pinned_recipe->>'model') BETWEEN 1 AND 128
   AND btrim(pinned_recipe->>'model')<>''
   AND (pinned_recipe->>'model')~'^[ -~]+$'
   AND pinned_recipe->>'mode' IN('plan','review','implement')
   AND jsonb_typeof(pinned_recipe->'generation_token_limit')='null'
   AND pinned_recipe->>'policy_digest'='223cba6fa54a1d6c809ed8ab84d7b96b140962e5dbe026bc28cc3a42a29b3130'
  ) IS TRUE
 );

-- Preserve each complete frozen predecessor and extend only its native-policy
-- capability list. CREATE OR REPLACE retains owners and existing function ACLs.
DO $upgrade_functions$
DECLARE target record;previous text;definition text;
BEGIN
 FOR target IN SELECT * FROM(VALUES
  ('forge_coding_worker_api.claim(text,uuid,text[])','d98e5c06e11b5d5b8f7dd357b4e8809637e67a45b8ca0b78773b0e7fd9dc7344'),
  ('forge_coding_reconciler_api.review_claim(text)','d38650a9b15aea08cc8ed148997004758c74b43f111af944ea5e649fc07724f0'),
  ('forge_coding_reconciler_api.publication_claim(text)','35b2db844b7f873c8e43cc611c48ea77d6ec5d986fe90b7e3f36f689bc87dc44'),
  ('forge_agent.coding_make_feedback(uuid,uuid,text,text,jsonb)','92e8a1e43b3abf7f560ff91f4a8543b7acb0afbc31981ada55eb0d0ac052609e')
 )x(signature,sha256) LOOP
  previous:=pg_get_functiondef(target.signature::regprocedure);
  IF encode(sha256(convert_to(previous,'UTF8')),'hex') IS DISTINCT FROM target.sha256 THEN
   RAISE EXCEPTION 'M23 native policy predecessor changed: %',target.signature;
  END IF;
  definition:=replace(previous,'''forge-coding-v10'')',
   '''forge-coding-v10'', ''forge-coding-agy-unattended-v1'')');
  IF definition=previous THEN RAISE EXCEPTION 'M23 native policy predicate missing: %',target.signature;END IF;
  IF target.signature='forge_coding_worker_api.claim(text,uuid,text[])' THEN
   definition:=replace(definition,
    $old$AND (t.pinned_recipe->>'policy_digest') = r.policy_digest$old$,
    $new$AND (t.pinned_recipe->>'policy_digest') = r.policy_digest
          AND (t.pinned_recipe->>'resource_profile'<>'forge-coding-agy-unattended-v1' OR EXISTS (
              SELECT 1 FROM jsonb_array_elements(r.profiles) AS configured(profile)
              WHERE profile->>'alias'=t.agent_profile
                AND profile->>'backend'='antigravity'
                AND profile->>'model'=t.pinned_recipe->>'model'
                AND profile->'modes' ? t.mode
          ))$new$);
  END IF;
  EXECUTE definition;
 END LOOP;
END;
$upgrade_functions$;
