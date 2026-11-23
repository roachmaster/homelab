-- Retry allowance belongs to one unresolved cause, never a task lifetime.
CREATE TABLE forge_agent.coding_retry_events(
 event_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),owner_id text NOT NULL,project_alias text NOT NULL,
 scope_kind text NOT NULL CHECK(scope_kind IN('native_workflow','external_workflow','delivery','manual_task')),scope_id uuid NOT NULL,
 kind text NOT NULL CHECK(kind IN('compilation','tests','browser','lint','formatting','planning','review','merge','infrastructure','verification','feedback_delivery','reviewer_correction')),
 phase_id text NOT NULL CHECK(phase_id~'^[A-Za-z0-9_.:-]{1,128}$'),
 used integer NOT NULL DEFAULT 0 CHECK(used BETWEEN 0 AND 20),attempt_limit integer NOT NULL DEFAULT 20 CHECK(attempt_limit=20),
 status text NOT NULL DEFAULT 'open' CHECK(status IN('open','exhausted','resolved')),
 source_id text NOT NULL CHECK(octet_length(source_id) BETWEEN 1 AND 256),source_digest text NOT NULL CHECK(source_digest~'^[0-9a-f]{64}$'),
 legacy_used integer,legacy_source_id text,legacy_source_digest text,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),resolved_at timestamptz,
 UNIQUE(owner_id,project_alias,event_id),CHECK((status='resolved')=(resolved_at IS NOT NULL)),CHECK(status<>'exhausted' OR used=20),
 CHECK(legacy_used IS NULL OR legacy_used BETWEEN 0 AND 20)
);
CREATE UNIQUE INDEX coding_retry_unresolved_scope ON forge_agent.coding_retry_events(owner_id,project_alias,scope_kind,scope_id,kind,phase_id) WHERE status IN('open','exhausted');
CREATE TABLE forge_agent.coding_retry_event_admissions(
 event_id uuid NOT NULL REFERENCES forge_agent.coding_retry_events,event_key text NOT NULL CHECK(octet_length(event_key) BETWEEN 1 AND 256),
 ordinal integer NOT NULL CHECK(ordinal BETWEEN 1 AND 20),receipt jsonb NOT NULL,created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(event_id,event_key),UNIQUE(event_id,ordinal)
);
CREATE TABLE forge_agent.coding_retry_event_observations(
 owner_id text NOT NULL,project_alias text NOT NULL,scope_kind text NOT NULL,scope_id uuid NOT NULL,kind text NOT NULL,phase_id text NOT NULL,
 source_id text NOT NULL,source_digest text NOT NULL,event_id uuid REFERENCES forge_agent.coding_retry_events,
 observation_kind text NOT NULL CHECK(observation_kind IN('failure','passed')),created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(owner_id,project_alias,scope_kind,scope_id,kind,phase_id,source_id,source_digest,observation_kind)
);
CREATE TABLE forge_agent.coding_retry_review_events(review_id uuid PRIMARY KEY REFERENCES forge_agent.coding_reviews,event_id uuid NOT NULL REFERENCES forge_agent.coding_retry_events,reservation_key text);
REVOKE ALL ON forge_agent.coding_retry_events,forge_agent.coding_retry_event_admissions,forge_agent.coding_retry_event_observations,forge_agent.coding_retry_review_events FROM PUBLIC;

CREATE FUNCTION forge_agent._retry_scope(p_project text,p_scope_kind text,p_scope_id uuid) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $f$
BEGIN
 IF current_user<>'forge_agent_owner' OR(session_user NOT IN('forge_agent','forge_coding_reconciler') AND NOT EXISTS(SELECT FROM pg_roles WHERE rolname=session_user AND rolsuper)) OR coalesce(current_setting('forge.coding_owner',true),'')='' THEN RAISE EXCEPTION 'Trusted scoped retry service required' USING ERRCODE='42501';END IF;
 IF NOT (CASE p_scope_kind
 WHEN 'native_workflow' THEN EXISTS(SELECT FROM forge_agent.coding_workflows WHERE owner_id=current_setting('forge.coding_owner',true) AND project_alias=p_project AND workflow_id=p_scope_id)
 WHEN 'external_workflow' THEN EXISTS(SELECT FROM forge_external_workflow.workflows WHERE owner_id=current_setting('forge.coding_owner',true) AND project_alias=p_project AND workflow_id=p_scope_id)
 WHEN 'delivery' THEN EXISTS(SELECT FROM forge_agent.delivery_lineages WHERE owner_id=current_setting('forge.coding_owner',true) AND project_alias=p_project AND lineage_id=p_scope_id)
 WHEN 'manual_task' THEN EXISTS(SELECT FROM forge_agent.coding_tasks WHERE owner_id=current_setting('forge.coding_owner',true) AND project_alias=p_project AND id=p_scope_id AND workflow_id IS NULL)
 ELSE false END) THEN RAISE EXCEPTION 'Retry event scope mismatch' USING ERRCODE='42501';END IF;
END;$f$;
CREATE FUNCTION forge_agent._retry_event_dto(p forge_agent.coding_retry_events) RETURNS jsonb LANGUAGE sql IMMUTABLE SET search_path=pg_catalog,pg_temp AS $f$
 SELECT jsonb_build_object('event_id',p.event_id,'kind',p.kind,'used',p.used,'limit',p.attempt_limit,'status',p.status)
$f$;
CREATE FUNCTION forge_agent.retry_event_get(p_event uuid) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $f$
DECLARE e forge_agent.coding_retry_events%ROWTYPE;
BEGIN
 SELECT * INTO e FROM forge_agent.coding_retry_events WHERE event_id=p_event;
 IF NOT FOUND THEN RETURN NULL;END IF;PERFORM forge_agent._retry_scope(e.project_alias,e.scope_kind,e.scope_id);
 RETURN forge_agent._retry_event_dto(e);
END;$f$;
CREATE FUNCTION forge_agent.retry_event_project(p_project text,p_scope_kind text,p_scope_id uuid) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $f$
DECLARE e forge_agent.coding_retry_events%ROWTYPE;
BEGIN
 PERFORM forge_agent._retry_scope(p_project,p_scope_kind,p_scope_id);
 SELECT * INTO e FROM forge_agent.coding_retry_events WHERE owner_id=current_setting('forge.coding_owner',true) AND project_alias=p_project AND scope_kind=p_scope_kind AND scope_id=p_scope_id
 ORDER BY(status<>'resolved') DESC,updated_at DESC,event_id DESC LIMIT 1;
 IF NOT FOUND THEN RETURN NULL;END IF;RETURN forge_agent._retry_event_dto(e);
END;$f$;
CREATE FUNCTION forge_agent.retry_event_resolve(p_project text,p_scope_kind text,p_scope_id uuid,p_kind text,p_source_id text,p_digest text,p_phase_id text DEFAULT NULL) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $f$
DECLARE e forge_agent.coding_retry_events%ROWTYPE;packet jsonb;witness_phase text:=coalesce(p_phase_id,'*');inserted integer;prior_event uuid;
BEGIN
 PERFORM forge_agent._retry_scope(p_project,p_scope_kind,p_scope_id);
 IF p_kind NOT IN('compilation','tests','browser','lint','formatting','planning','review','merge','infrastructure','verification','feedback_delivery','reviewer_correction') OR p_source_id IS NULL OR octet_length(p_source_id) NOT BETWEEN 1 AND 256 OR p_source_id~'[[:cntrl:]]' OR p_digest IS NULL OR p_digest!~'^[0-9a-f]{64}$' OR(p_phase_id IS NOT NULL AND p_phase_id!~'^[A-Za-z0-9_.:-]{1,128}$') THEN RAISE EXCEPTION 'Invalid trusted stage evidence' USING ERRCODE='22023';END IF;
 PERFORM pg_advisory_xact_lock(hashtextextended(current_setting('forge.coding_owner',true)||E'\n'||p_project,2026111901));
 INSERT INTO forge_agent.coding_retry_event_observations(owner_id,project_alias,scope_kind,scope_id,kind,phase_id,source_id,source_digest,event_id,observation_kind)
 VALUES(current_setting('forge.coding_owner',true),p_project,p_scope_kind,p_scope_id,p_kind,witness_phase,p_source_id,p_digest,NULL,'passed') ON CONFLICT DO NOTHING;
 GET DIAGNOSTICS inserted=ROW_COUNT;
 IF inserted=0 THEN
  SELECT event_id INTO prior_event FROM forge_agent.coding_retry_event_observations WHERE(owner_id,project_alias,scope_kind,scope_id,kind,phase_id,source_id,source_digest,observation_kind)=(current_setting('forge.coding_owner',true),p_project,p_scope_kind,p_scope_id,p_kind,witness_phase,p_source_id,p_digest,'passed');
  IF prior_event IS NULL THEN RETURN NULL;END IF;RETURN forge_agent.retry_event_get(prior_event);
 END IF;
 FOR e IN SELECT * FROM forge_agent.coding_retry_events WHERE owner_id=current_setting('forge.coding_owner',true) AND project_alias=p_project AND scope_kind=p_scope_kind AND scope_id=p_scope_id AND kind=p_kind AND status IN('open','exhausted') AND(p_phase_id IS NULL OR phase_id=p_phase_id) FOR UPDATE LOOP
  INSERT INTO forge_agent.coding_retry_event_observations VALUES(e.owner_id,e.project_alias,e.scope_kind,e.scope_id,e.kind,e.phase_id,p_source_id,p_digest,e.event_id,'passed',clock_timestamp()) ON CONFLICT DO NOTHING;
  UPDATE forge_agent.coding_retry_events SET status='resolved',resolved_at=clock_timestamp(),updated_at=clock_timestamp() WHERE event_id=e.event_id RETURNING * INTO e;
  packet:=forge_agent._retry_event_dto(e);
 END LOOP;
 UPDATE forge_agent.coding_retry_event_observations SET event_id=e.event_id WHERE(owner_id,project_alias,scope_kind,scope_id,kind,phase_id,source_id,source_digest,observation_kind)=(current_setting('forge.coding_owner',true),p_project,p_scope_kind,p_scope_id,p_kind,witness_phase,p_source_id,p_digest,'passed');
 RETURN packet;
END;$f$;
CREATE FUNCTION forge_agent.retry_event_observe(p_project text,p_scope_kind text,p_scope_id uuid,p_kind text,p_source_id text,p_digest text,p_passed_kinds text[] DEFAULT ARRAY[]::text[],p_phase_id text DEFAULT '',p_passed_phase_ids text[] DEFAULT ARRAY[]::text[]) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $f$
DECLARE e forge_agent.coding_retry_events%ROWTYPE;k text;phase text:=coalesce(nullif(p_phase_id,''),p_kind);passed_phase text;
BEGIN
 PERFORM forge_agent._retry_scope(p_project,p_scope_kind,p_scope_id);
 IF p_kind NOT IN('compilation','tests','browser','lint','formatting','planning','review','merge','infrastructure','verification','feedback_delivery','reviewer_correction') OR p_source_id IS NULL OR octet_length(p_source_id) NOT BETWEEN 1 AND 256 OR p_source_id~'[[:cntrl:]]' OR p_digest IS NULL OR p_digest!~'^[0-9a-f]{64}$' OR phase IS NULL OR phase!~'^[A-Za-z0-9_.:-]{1,128}$' OR p_passed_phase_ids IS NULL OR cardinality(p_passed_phase_ids)>128 OR EXISTS(SELECT FROM unnest(p_passed_phase_ids)x WHERE x!~'^[A-Za-z0-9_.:-]{1,128}$') OR p_passed_kinds IS NULL OR cardinality(p_passed_kinds)>12 OR p_kind=ANY(p_passed_kinds) OR cardinality(p_passed_kinds)<>(SELECT count(DISTINCT x) FROM unnest(p_passed_kinds)x) THEN RAISE EXCEPTION 'Invalid trusted failure evidence' USING ERRCODE='22023';END IF;
 PERFORM pg_advisory_xact_lock(hashtextextended(current_setting('forge.coding_owner',true)||E'\n'||p_project,2026111901));
 SELECT t.* INTO e FROM forge_agent.coding_retry_event_observations o JOIN forge_agent.coding_retry_events t USING(event_id)
 WHERE(o.owner_id,o.project_alias,o.scope_kind,o.scope_id,o.kind,o.phase_id,o.source_id,o.source_digest,o.observation_kind)=(current_setting('forge.coding_owner',true),p_project,p_scope_kind,p_scope_id,p_kind,phase,p_source_id,p_digest,'failure');
 IF FOUND THEN RETURN forge_agent._retry_event_dto(e);END IF;
 FOREACH k IN ARRAY p_passed_kinds LOOP PERFORM forge_agent.retry_event_resolve(p_project,p_scope_kind,p_scope_id,k,p_source_id,p_digest);END LOOP;
 FOREACH passed_phase IN ARRAY p_passed_phase_ids LOOP
  FOR k IN SELECT DISTINCT kind FROM forge_agent.coding_retry_events WHERE owner_id=current_setting('forge.coding_owner',true) AND project_alias=p_project AND scope_kind=p_scope_kind AND scope_id=p_scope_id AND phase_id=passed_phase AND status IN('open','exhausted') LOOP
   PERFORM forge_agent.retry_event_resolve(p_project,p_scope_kind,p_scope_id,k,p_source_id,p_digest,passed_phase);
  END LOOP;
 END LOOP;
 SELECT * INTO e FROM forge_agent.coding_retry_events WHERE owner_id=current_setting('forge.coding_owner',true) AND project_alias=p_project AND scope_kind=p_scope_kind AND scope_id=p_scope_id AND kind=p_kind AND phase_id=phase AND status IN('open','exhausted') FOR UPDATE;
 IF NOT FOUND THEN
  INSERT INTO forge_agent.coding_retry_events(owner_id,project_alias,scope_kind,scope_id,kind,phase_id,source_id,source_digest)
  VALUES(current_setting('forge.coding_owner',true),p_project,p_scope_kind,p_scope_id,p_kind,phase,p_source_id,p_digest) RETURNING * INTO e;
 ELSE
  UPDATE forge_agent.coding_retry_events SET source_id=p_source_id,source_digest=p_digest,status=CASE WHEN used>=attempt_limit THEN 'exhausted' ELSE status END,updated_at=clock_timestamp() WHERE event_id=e.event_id RETURNING * INTO e;
 END IF;
 INSERT INTO forge_agent.coding_retry_event_observations VALUES(e.owner_id,e.project_alias,e.scope_kind,e.scope_id,e.kind,e.phase_id,p_source_id,p_digest,e.event_id,'failure',clock_timestamp());
 RETURN forge_agent._retry_event_dto(e);
END;$f$;
CREATE FUNCTION forge_agent.retry_event_reserve(p_event uuid,p_action_key text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $f$
DECLARE e forge_agent.coding_retry_events%ROWTYPE;prior jsonb;packet jsonb;
BEGIN
 SELECT * INTO e FROM forge_agent.coding_retry_events WHERE event_id=p_event;
 IF NOT FOUND THEN RAISE EXCEPTION 'Retry event missing' USING ERRCODE='P0002';END IF;
 PERFORM forge_agent._retry_scope(e.project_alias,e.scope_kind,e.scope_id);
 IF p_action_key IS NULL OR octet_length(p_action_key) NOT BETWEEN 1 AND 256 OR p_action_key~'[[:cntrl:]]' THEN RAISE EXCEPTION 'Invalid retry admission key' USING ERRCODE='22023';END IF;
 PERFORM pg_advisory_xact_lock(hashtextextended(e.owner_id||E'\n'||e.project_alias,2026111901));
 SELECT * INTO e FROM forge_agent.coding_retry_events WHERE event_id=p_event FOR UPDATE;
 SELECT receipt INTO prior FROM forge_agent.coding_retry_event_admissions WHERE event_id=p_event AND event_key=p_action_key;
 IF FOUND THEN RETURN prior;END IF;
 IF e.status='resolved' THEN RAISE EXCEPTION 'Resolved retry event cannot admit work' USING ERRCODE='P0009';END IF;
 IF e.used>=e.attempt_limit THEN UPDATE forge_agent.coding_retry_events SET status='exhausted',updated_at=clock_timestamp() WHERE event_id=p_event RETURNING * INTO e;RETURN forge_agent._retry_event_dto(e);END IF;
 UPDATE forge_agent.coding_retry_events SET used=used+1,updated_at=clock_timestamp() WHERE event_id=p_event RETURNING * INTO e;
 packet:=forge_agent._retry_event_dto(e);
 INSERT INTO forge_agent.coding_retry_event_admissions(event_id,event_key,ordinal,receipt) VALUES(p_event,p_action_key,e.used,packet);
 RETURN packet;
END;$f$;
CREATE FUNCTION forge_agent.retry_event_adopt_legacy(p_event uuid,p_used integer,p_source_id text,p_digest text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $f$
DECLARE e forge_agent.coding_retry_events%ROWTYPE;spent integer;
BEGIN
 IF session_user<>'forge_agent' THEN RAISE EXCEPTION 'Private owner continuation required' USING ERRCODE='42501';END IF;
 SELECT * INTO e FROM forge_agent.coding_retry_events WHERE event_id=p_event FOR UPDATE;
 IF NOT FOUND THEN RAISE EXCEPTION 'Retry event missing' USING ERRCODE='P0002';END IF;
 PERFORM forge_agent._retry_scope(e.project_alias,e.scope_kind,e.scope_id);
 IF p_used IS NULL OR p_used NOT BETWEEN 0 AND 20 OR p_source_id IS NULL OR octet_length(p_source_id) NOT BETWEEN 1 AND 256 OR p_digest IS NULL OR p_digest!~'^[0-9a-f]{64}$' THEN RAISE EXCEPTION 'Invalid retained retry evidence' USING ERRCODE='22023';END IF;
 IF e.legacy_used IS NOT NULL THEN
  IF(e.legacy_used,e.legacy_source_id,e.legacy_source_digest) IS DISTINCT FROM(p_used,p_source_id,p_digest) THEN RAISE EXCEPTION 'Retained retry evidence changed' USING ERRCODE='P0006';END IF;
  RETURN forge_agent._retry_event_dto(e);
 END IF;
 IF e.used<>0 OR e.status<>'open' THEN RAISE EXCEPTION 'Retry adoption is not a reset path' USING ERRCODE='P0009';END IF;
 IF e.scope_kind='external_workflow' AND e.kind='planning' THEN
  SELECT plan_revisions INTO spent FROM forge_external_workflow.workflows WHERE(owner_id,project_alias,workflow_id)=(e.owner_id,e.project_alias,e.scope_id);
 ELSIF e.scope_kind='native_workflow' AND e.kind='reviewer_correction' AND e.phase_id~'^reviewer_correction\.[0-9a-f-]{36}$' THEN
  IF NOT EXISTS(SELECT FROM forge_agent.coding_workflow_reviews WHERE(owner_id,project_alias,workflow_id,workflow_review_id)=(e.owner_id,e.project_alias,e.scope_id,split_part(e.phase_id,'.',2)::uuid)) THEN RAISE EXCEPTION 'Retained review scope mismatch' USING ERRCODE='P0006';END IF;
  SELECT count(*) INTO spent FROM forge_agent.coding_workflow_actions WHERE workflow_id=e.scope_id AND kind='decide' AND source_key LIKE 'peer-reviewer-correction:'||split_part(e.phase_id,'.',2)||':%';
 ELSIF e.scope_kind='native_workflow' AND e.kind='feedback_delivery' AND e.phase_id~'^feedback\.[0-9a-f-]{36}$' THEN
  IF NOT EXISTS(SELECT FROM forge_agent.coding_feedback f JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(f.owner_id,f.project_alias,f.task_id) WHERE(f.owner_id,f.project_alias,t.workflow_id,f.feedback_id)=(e.owner_id,e.project_alias,e.scope_id,split_part(e.phase_id,'.',2)::uuid)) THEN RAISE EXCEPTION 'Retained feedback scope mismatch' USING ERRCODE='P0006';END IF;
  SELECT count(*) INTO spent FROM forge_agent.coding_workflow_actions WHERE workflow_id=e.scope_id AND kind='decide' AND status='completed' AND source_key LIKE 'feedback-delivery-retry:%' AND intent->>'feedback_id'=split_part(e.phase_id,'.',2);
 ELSE RAISE EXCEPTION 'Unsupported retained retry evidence' USING ERRCODE='P0009';END IF;
 IF spent IS DISTINCT FROM p_used THEN RAISE EXCEPTION 'Retained planning counter changed' USING ERRCODE='P0006';END IF;
 UPDATE forge_agent.coding_retry_events SET used=p_used,status=CASE WHEN p_used=20 THEN 'exhausted' ELSE 'open' END,legacy_used=p_used,legacy_source_id=p_source_id,legacy_source_digest=p_digest,updated_at=clock_timestamp() WHERE event_id=p_event RETURNING * INTO e;
 RETURN forge_agent._retry_event_dto(e);
END;$f$;
REVOKE ALL ON FUNCTION forge_agent._retry_scope(text,text,uuid),forge_agent._retry_event_dto(forge_agent.coding_retry_events),forge_agent.retry_event_get(uuid),forge_agent.retry_event_project(text,text,uuid),forge_agent.retry_event_resolve(text,text,uuid,text,text,text,text),forge_agent.retry_event_observe(text,text,uuid,text,text,text,text[],text,text[]),forge_agent.retry_event_reserve(uuid,text),forge_agent.retry_event_adopt_legacy(uuid,integer,text,text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_agent.retry_event_get(uuid),forge_agent.retry_event_project(text,text,uuid),forge_agent.retry_event_resolve(text,text,uuid,text,text,text,text),forge_agent.retry_event_observe(text,text,uuid,text,text,text,text[],text,text[]),forge_agent.retry_event_reserve(uuid,text) TO forge_agent,forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_agent.retry_event_adopt_legacy(uuid,integer,text,text) TO forge_agent;
\ir ../fragments/0031_native_retry_events.sql
\ir ../fragments/0031_ci_stage_receipts.sql
\ir ../fragments/0031_delivery_retry_events.sql
-- Preserve an existing Muse reader's role/defaults without extending its
-- historical table allowlist to retry authority sidecars.
DO $retry_reader_boundary$
BEGIN
 IF EXISTS(SELECT FROM pg_roles WHERE rolname='muse_events') THEN
  REVOKE ALL ON forge_agent.coding_retry_events,forge_agent.coding_retry_event_admissions,
   forge_agent.coding_retry_event_observations,forge_agent.coding_retry_review_events,
   forge_agent.delivery_retry_event_links FROM muse_events;
 END IF;
END;$retry_reader_boundary$;
\ir ../fragments/0031_retry_event_fingerprints.sql
