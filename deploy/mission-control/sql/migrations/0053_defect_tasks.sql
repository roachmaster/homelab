-- M53: structured defects remain normal coding tasks and retain their identity
-- throughout SQL-created Plan revisions and repair descendants.
CREATE FUNCTION forge_agent.valid_defect(p jsonb) RETURNS boolean
LANGUAGE plpgsql IMMUTABLE SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE k text; cap integer; v text;
BEGIN
 IF p IS NULL OR jsonb_typeof(p)<>'object' THEN RETURN false;END IF;
 IF NOT p ?& ARRAY['summary','location','observed_behavior','expected_behavior','severity','evidence']
 OR p-ARRAY['summary','location','observed_behavior','expected_behavior','severity','evidence','discovering_task_id','discovering_workflow_id']<>'{}'::jsonb
 THEN RETURN false;END IF;
 FOREACH k IN ARRAY ARRAY['summary','observed_behavior','expected_behavior','evidence'] LOOP
  cap:=CASE k WHEN 'summary' THEN 512 WHEN 'evidence' THEN 8192 ELSE 4096 END;
  v:=p->>k;
  IF jsonb_typeof(p->k)<>'string' OR v IS NULL OR btrim(v,E' \t\r\n'||chr(160)||chr(5760)||chr(8192)||chr(8193)||chr(8194)||chr(8195)||chr(8196)||chr(8197)||chr(8198)||chr(8199)||chr(8200)||chr(8201)||chr(8202)||chr(8232)||chr(8233)||chr(8239)||chr(8287)||chr(12288))='' OR octet_length(v)>cap
    OR translate(v,E'\t\n','')~'[[:cntrl:]]' OR v~('['||chr(127)||'-'||chr(159)||']') THEN RETURN false;END IF;
 END LOOP;
 IF jsonb_typeof(p->'severity')<>'string' OR p->>'severity' NOT IN('low','medium','high','critical') THEN RETURN false;END IF;
 IF jsonb_typeof(p->'location')<>'object' OR NOT (p->'location') ?& ARRAY['repo_path','function','commit']
 OR (p->'location')-ARRAY['repo_path','function','commit']<>'{}'::jsonb THEN RETURN false;END IF;
 FOREACH k IN ARRAY ARRAY['repo_path','function','commit'] LOOP
  v:=p->'location'->>k;
  cap:=CASE k WHEN 'repo_path' THEN 1024 WHEN 'function' THEN 512 ELSE 40 END;
  IF jsonb_typeof(p->'location'->k)<>'string' OR v IS NULL OR btrim(v,E' \t\r\n'||chr(160)||chr(5760)||chr(8192)||chr(8193)||chr(8194)||chr(8195)||chr(8196)||chr(8197)||chr(8198)||chr(8199)||chr(8200)||chr(8201)||chr(8202)||chr(8232)||chr(8233)||chr(8239)||chr(8287)||chr(12288))='' OR octet_length(v)>cap
    OR v~'[[:cntrl:]]' OR v~('['||chr(127)||'-'||chr(159)||']') THEN RETURN false;END IF;
 END LOOP;
 IF p->'location'->>'commit'!~'^[0-9a-f]{40}$'
 OR p->'location'->>'repo_path'~'(^/|//|/$|(^|/)\.\.?(/|$))' OR position(chr(92) in p->'location'->>'repo_path')>0 THEN RETURN false;END IF;
 FOREACH k IN ARRAY ARRAY['discovering_task_id','discovering_workflow_id'] LOOP
  IF p ? k AND (jsonb_typeof(p->k)<>'string' OR p->>k!~'^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$') THEN RETURN false;END IF;
 END LOOP;
 RETURN true;
END
$fn$;
REVOKE ALL ON FUNCTION forge_agent.valid_defect(jsonb) FROM PUBLIC;

ALTER TABLE forge_agent.coding_tasks
 ADD COLUMN task_kind text NOT NULL DEFAULT 'feature',
 ADD COLUMN defect jsonb,
 ADD CONSTRAINT coding_tasks_defect_check CHECK (
   (task_kind='feature' AND defect IS NULL) OR
   (task_kind='defect' AND defect IS NOT NULL AND forge_agent.valid_defect(defect))
 );
CREATE INDEX coding_tasks_defect_filter ON forge_agent.coding_tasks(owner_id,project_alias,task_kind,created_at DESC);

CREATE FUNCTION forge_agent.inherit_task_defect() RETURNS trigger
LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE parent forge_agent.coding_tasks%ROWTYPE;
BEGIN
 IF NEW.parent_task_id IS NOT NULL THEN
  SELECT * INTO parent FROM forge_agent.coding_tasks
   WHERE (owner_id,project_alias,id)=(NEW.owner_id,NEW.project_alias,NEW.parent_task_id);
  IF FOUND THEN NEW.task_kind:=parent.task_kind;NEW.defect:=parent.defect;END IF;
 END IF;
 RETURN NEW;
END
$fn$;
REVOKE ALL ON FUNCTION forge_agent.inherit_task_defect() FROM PUBLIC;
CREATE TRIGGER coding_tasks_inherit_defect BEFORE INSERT ON forge_agent.coding_tasks
 FOR EACH ROW EXECUTE FUNCTION forge_agent.inherit_task_defect();

-- Active worker leases receive only their own task's metadata; no new table
-- privileges or ambient project-wide read authority are introduced.
CREATE FUNCTION forge_coding_worker_api.read_defect(p_attempt_id uuid,p_attempt_token uuid)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.coding_attempts%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;
BEGIN
 a:=forge_coding_worker_api._lock_task_and_attempt(p_attempt_id,p_attempt_token);
 IF a.attempt_id IS NULL THEN RAISE EXCEPTION 'Invalid attempt ownership' USING ERRCODE='42501';END IF;
 SELECT * INTO t FROM forge_agent.coding_tasks
  WHERE (owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id);
 IF a.lease_expires_at IS NULL OR a.lease_expires_at<=clock_timestamp() OR a.deadline_at<=clock_timestamp()
 OR t.latest_attempt_id IS DISTINCT FROM a.attempt_id OR t.status IS DISTINCT FROM 'running' THEN
  RAISE EXCEPTION 'Attempt lease is not active' USING ERRCODE='P0008';END IF;
 RETURN jsonb_build_object('task_kind',t.task_kind,'defect',t.defect);
END
$fn$;
REVOKE ALL ON FUNCTION forge_coding_worker_api.read_defect(uuid,uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_coding_worker_api.read_defect(uuid,uuid) TO forge_coding_worker;

GRANT EXECUTE ON FUNCTION forge_agent.valid_defect(jsonb) TO forge_agent;
