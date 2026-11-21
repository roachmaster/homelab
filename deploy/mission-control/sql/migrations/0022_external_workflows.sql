-- M22: external author / native independent reviewer / trusted delivery process manager.
-- Earlier migrations and native P10 recipes remain immutable.
CREATE SCHEMA forge_external_workflow AUTHORIZATION forge_agent_owner;
CREATE SCHEMA forge_external_workflow_api AUTHORIZATION forge_agent_owner;
CREATE SCHEMA forge_external_workflow_worker_api AUTHORIZATION forge_agent_owner;
CREATE SCHEMA forge_external_workflow_delivery_api AUTHORIZATION forge_agent_owner;
REVOKE ALL ON SCHEMA forge_external_workflow,forge_external_workflow_api,forge_external_workflow_worker_api,forge_external_workflow_delivery_api FROM PUBLIC;
CREATE TABLE forge_external_workflow.workflows(
 workflow_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),owner_id text NOT NULL,project_alias text NOT NULL,
 root_task_id uuid NOT NULL,active_task_id uuid NOT NULL,author_profile text NOT NULL,reviewer_profile text NOT NULL,
 policy_digest text NOT NULL CHECK(policy_digest~'^[0-9a-f]{64}$'),snapshot jsonb NOT NULL CHECK(jsonb_typeof(snapshot)='object'),
 phase text NOT NULL CHECK(phase IN('plan','plan_review','plan_revision','implement','checks','code_review','repair','merging','completed','cancelled','escalated')),
 revision bigint NOT NULL DEFAULT 1 CHECK(revision BETWEEN 1 AND 9007199254740991),
 plan_revisions integer NOT NULL DEFAULT 0 CHECK(plan_revisions BETWEEN 0 AND 3),code_revisions integer NOT NULL DEFAULT 0 CHECK(code_revisions BETWEEN 0 AND 3),
 enabled boolean NOT NULL DEFAULT true,state jsonb NOT NULL DEFAULT '{}' CHECK(jsonb_typeof(state)='object'),
 lease_token uuid,lease_until timestamptz,created_at timestamptz NOT NULL DEFAULT clock_timestamp(),updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 UNIQUE(owner_id,project_alias,root_task_id),UNIQUE(owner_id,project_alias,workflow_id),
 FOREIGN KEY(owner_id,project_alias,root_task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id),
 FOREIGN KEY(owner_id,project_alias,active_task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id),
 CHECK((lease_token IS NULL)=(lease_until IS NULL))
);
CREATE TABLE forge_external_workflow.tasks(
 owner_id text NOT NULL,project_alias text NOT NULL,task_id uuid NOT NULL,workflow_id uuid NOT NULL,
 role text NOT NULL CHECK(role IN('root','author','reviewer')),PRIMARY KEY(owner_id,project_alias,task_id),
 FOREIGN KEY(owner_id,project_alias,task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id),
 FOREIGN KEY(owner_id,project_alias,workflow_id) REFERENCES forge_external_workflow.workflows(owner_id,project_alias,workflow_id)
);
CREATE TABLE forge_external_workflow.contexts(
 assignment_id uuid PRIMARY KEY REFERENCES forge_external_agent.assignments(assignment_id),
 content text NOT NULL CHECK(octet_length(content) BETWEEN 1 AND 1048576),
 sha256 text NOT NULL CHECK(sha256~'^[0-9a-f]{64}$')
);
CREATE TABLE forge_external_workflow.reviews(
 review_id uuid PRIMARY KEY,workflow_id uuid NOT NULL REFERENCES forge_external_workflow.workflows,
 owner_id text NOT NULL,project_alias text NOT NULL,source_task_id uuid NOT NULL,source_assignment_id uuid NOT NULL REFERENCES forge_external_agent.assignments,
 source_sha256 text NOT NULL CHECK(source_sha256~'^[0-9a-f]{64}$'),target_kind text NOT NULL CHECK(target_kind IN('plan','code')),
 reviewer_task_id uuid NOT NULL,target_json text NOT NULL CHECK(octet_length(target_json) BETWEEN 1 AND 1048576),
 target_sha256 text NOT NULL CHECK(target_sha256~'^[0-9a-f]{64}$'),
 status text NOT NULL DEFAULT 'reviewing' CHECK(status IN('reviewing','approved','revise','escalated','retry')),
 reviewer_attempt_id uuid,reviewer_result_sha256 text,verdict jsonb,receipt jsonb,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 FOREIGN KEY(owner_id,project_alias,source_task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id),
 FOREIGN KEY(owner_id,project_alias,reviewer_task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id),
 UNIQUE(reviewer_task_id)
);
CREATE TABLE forge_external_workflow.actions(
 action_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),workflow_id uuid NOT NULL REFERENCES forge_external_workflow.workflows,
 owner_id text NOT NULL,project_alias text NOT NULL,workflow_revision bigint NOT NULL,
 kind text NOT NULL CHECK(kind IN('source','prepare','verify','merge','cancel')),source_key text NOT NULL,
 intent jsonb NOT NULL CHECK(jsonb_typeof(intent)='object' AND octet_length(intent::text)<=65536),
 status text NOT NULL DEFAULT 'queued' CHECK(status IN('queued','leased','completed','failed')),
 result jsonb,checkpoint_digest text,checkpoint_token uuid,checkpoint_generation integer,checkpoint_role name,
 generation integer NOT NULL DEFAULT 0 CHECK(generation BETWEEN 0 AND 10000),lease_token uuid,lease_role name,lease_until timestamptz,
 poll_after timestamptz NOT NULL DEFAULT clock_timestamp(),deadline_at timestamptz NOT NULL DEFAULT(clock_timestamp()+interval '24 hours'),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 UNIQUE(workflow_id,kind,source_key),
 CHECK((status='leased' AND lease_token IS NOT NULL AND lease_role IS NOT NULL AND lease_until IS NOT NULL) OR(status<>'leased' AND lease_token IS NULL AND lease_role IS NULL AND lease_until IS NULL))
);
CREATE INDEX external_delivery_pending ON forge_external_workflow.actions(status,poll_after,created_at);
REVOKE ALL ON ALL TABLES IN SCHEMA forge_external_workflow FROM PUBLIC;
GRANT USAGE ON SCHEMA forge_external_workflow TO forge_agent;
GRANT SELECT,INSERT,UPDATE ON ALL TABLES IN SCHEMA forge_external_workflow TO forge_agent;
CREATE TABLE forge_external_workflow.delivery_grants(
 worker_role name NOT NULL,owner_id text NOT NULL,project_alias text NOT NULL,enabled boolean NOT NULL DEFAULT false,
 PRIMARY KEY(worker_role,owner_id,project_alias)
);
REVOKE ALL ON forge_external_workflow.delivery_grants FROM PUBLIC;
GRANT SELECT ON forge_external_workflow.delivery_grants TO forge_agent;

CREATE FUNCTION forge_external_workflow.immutable_binding() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'Immutable external workflow binding' USING ERRCODE='P0009';END IF;
 CASE TG_TABLE_NAME
 WHEN 'tasks','contexts' THEN RAISE EXCEPTION 'Immutable external workflow binding' USING ERRCODE='P0009';
 WHEN 'workflows' THEN
  IF(NEW.owner_id,NEW.project_alias,NEW.root_task_id,NEW.author_profile,NEW.reviewer_profile,NEW.policy_digest,NEW.snapshot,NEW.created_at) IS DISTINCT FROM(OLD.owner_id,OLD.project_alias,OLD.root_task_id,OLD.author_profile,OLD.reviewer_profile,OLD.policy_digest,OLD.snapshot,OLD.created_at) THEN RAISE EXCEPTION 'Immutable external workflow policy' USING ERRCODE='P0009';END IF;
 WHEN 'reviews' THEN
  IF(NEW.workflow_id,NEW.owner_id,NEW.project_alias,NEW.source_task_id,NEW.source_assignment_id,NEW.source_sha256,NEW.target_kind,NEW.reviewer_task_id,NEW.target_json,NEW.target_sha256) IS DISTINCT FROM(OLD.workflow_id,OLD.owner_id,OLD.project_alias,OLD.source_task_id,OLD.source_assignment_id,OLD.source_sha256,OLD.target_kind,OLD.reviewer_task_id,OLD.target_json,OLD.target_sha256) THEN RAISE EXCEPTION 'Immutable external review target' USING ERRCODE='P0009';END IF;
 WHEN 'actions' THEN
  IF(NEW.workflow_id,NEW.owner_id,NEW.project_alias,NEW.workflow_revision,NEW.kind,NEW.source_key,NEW.intent,NEW.deadline_at) IS DISTINCT FROM(OLD.workflow_id,OLD.owner_id,OLD.project_alias,OLD.workflow_revision,OLD.kind,OLD.source_key,OLD.intent,OLD.deadline_at) THEN RAISE EXCEPTION 'Immutable external delivery intent' USING ERRCODE='P0009';END IF;
 ELSE RAISE EXCEPTION 'Unknown external immutable binding';END CASE;
 RETURN NEW;END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_workflow.immutable_binding() FROM PUBLIC;
CREATE TRIGGER external_workflow_binding BEFORE UPDATE OR DELETE ON forge_external_workflow.workflows FOR EACH ROW EXECUTE FUNCTION forge_external_workflow.immutable_binding();
CREATE TRIGGER external_review_binding BEFORE UPDATE OR DELETE ON forge_external_workflow.reviews FOR EACH ROW EXECUTE FUNCTION forge_external_workflow.immutable_binding();
CREATE TRIGGER external_action_binding BEFORE UPDATE OR DELETE ON forge_external_workflow.actions FOR EACH ROW EXECUTE FUNCTION forge_external_workflow.immutable_binding();
CREATE TRIGGER external_context_binding BEFORE UPDATE OR DELETE ON forge_external_workflow.contexts FOR EACH ROW EXECUTE FUNCTION forge_external_workflow.immutable_binding();
CREATE TRIGGER external_task_binding BEFORE UPDATE OR DELETE ON forge_external_workflow.tasks FOR EACH ROW EXECUTE FUNCTION forge_external_workflow.immutable_binding();

CREATE FUNCTION forge_external_workflow.claim_legacy(p_project text,p_profile text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE g forge_external_agent.scope_grants%ROWTYPE;a forge_external_agent.assignments%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;
BEGIN
 g:=forge_external_agent._scope(p_project,p_profile);
 IF NOT EXISTS(SELECT FROM forge_external_agent.readiness WHERE(external_role,owner_id,project_alias,profile_alias)=(g.external_role,g.owner_id,p_project,p_profile) AND observed_at>clock_timestamp()-interval '90 seconds') THEN RAISE EXCEPTION 'External runner not advertised' USING ERRCODE='P0009';END IF;
 PERFORM forge_external_agent._housekeep(g,64);
 SELECT x.* INTO a FROM forge_external_agent.assignments x JOIN forge_agent.coding_tasks q ON(q.owner_id,q.project_alias,q.id)=(x.owner_id,x.project_alias,x.task_id)
 WHERE(x.external_role,x.owner_id,x.project_alias,x.profile_alias)=(g.external_role,g.owner_id,p_project,p_profile)
 AND x.status IN('queued','leased') AND(x.status='queued' OR x.lease_until<=clock_timestamp()) AND x.generation<3 AND x.deadline_at>clock_timestamp()
 AND q.status IN('queued','running') AND q.latest_attempt_id IS NULL
 ORDER BY x.created_at,x.assignment_id FOR UPDATE OF q,x SKIP LOCKED LIMIT 1;
 IF NOT FOUND THEN RETURN NULL;END IF;
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id);
 IF forge_external_agent._intent(t) IS DISTINCT FROM a.intent OR t.revision<>a.task_revision THEN RAISE EXCEPTION 'External task binding changed' USING ERRCODE='P0009';END IF;
 UPDATE forge_agent.coding_tasks SET status='running',revision=revision+1,updated_at=clock_timestamp() WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id) RETURNING revision INTO a.task_revision;
 UPDATE forge_external_agent.assignments SET status='leased',task_revision=a.task_revision,generation=generation+1,lease_token=gen_random_uuid(),lease_until=least(clock_timestamp()+interval '5 minutes',deadline_at),updated_at=clock_timestamp() WHERE assignment_id=a.assignment_id RETURNING * INTO a;
 PERFORM forge_external_agent._event(a,'started',jsonb_build_object('provider','muse-db','assignment_id',a.assignment_id,'generation',a.generation));
 RETURN a.intent||jsonb_build_object('assignment_id',a.assignment_id,'provider','muse-db','generation',a.generation,'token',a.lease_token,'lease_until',to_char(a.lease_until AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),'deadline_at',to_char(a.deadline_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),'revision',a.task_revision);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_workflow.claim_legacy(text,text) FROM PUBLIC;
CREATE OR REPLACE FUNCTION forge_external_agent_api.claim(p_project text,p_profile text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE value jsonb;c forge_external_workflow.contexts%ROWTYPE;
BEGIN
 value:=forge_external_workflow.claim_legacy(p_project,p_profile);
 IF value IS NULL THEN RETURN NULL;END IF;
 SELECT * INTO c FROM forge_external_workflow.contexts WHERE assignment_id=(value->>'assignment_id')::uuid;
 IF FOUND THEN RETURN value||jsonb_build_object('external_context',c.content::jsonb,'external_context_json',c.content,'external_context_sha256',c.sha256);END IF;
 RETURN value;
END;
$fn$;
CREATE FUNCTION forge_external_workflow_api.enqueue_v2(p_owner text,p_project text,p_task uuid) RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE t forge_agent.coding_tasks%ROWTYPE;g forge_external_agent.scope_grants%ROWTYPE;a forge_external_agent.assignments%ROWTYPE;i jsonb;
BEGIN
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(p_owner,p_project,p_task) FOR UPDATE;
 IF NOT FOUND THEN RAISE EXCEPTION 'External task missing' USING ERRCODE='42501';END IF;
 SELECT * INTO g FROM forge_external_agent.scope_grants WHERE service_role=session_user AND owner_id=p_owner AND project_alias=p_project AND profile_alias=t.agent_profile AND enabled FOR SHARE;
 IF NOT FOUND THEN RAISE EXCEPTION 'External service scope denied' USING ERRCODE='42501';END IF;
 IF(t.mode IN('plan','implement') AND t.pinned_recipe=jsonb_build_object('version',2,'backend','muse','model','external-runner','resource_profile','forge-external-db-v2','mode',t.mode,'generation_token_limit',NULL,'policy_digest','b4dc030f454026f6c0e4058fd2fecfa0f8e0176cd3f220c7c890896302a3b60f') AND t.latest_attempt_id IS NULL AND NOT t.auto_repair AND NOT t.workflow_required AND NOT t.review_required) IS NOT TRUE THEN RAISE EXCEPTION 'Invalid external-v2 recipe' USING ERRCODE='22023';END IF;
 IF t.mode='implement' AND NOT EXISTS(SELECT FROM forge_external_workflow.tasks x JOIN forge_external_workflow.workflows w USING(workflow_id) WHERE(x.owner_id,x.project_alias,x.task_id)=(p_owner,p_project,p_task) AND w.enabled AND w.phase IN('implement','repair') AND w.active_task_id=t.id AND w.state?'approved_plan') THEN RAISE EXCEPTION 'Unapproved external implementation' USING ERRCODE='42501';END IF;
 i:=forge_external_agent._intent(t);
 SELECT * INTO a FROM forge_external_agent.assignments WHERE(owner_id,project_alias,task_id)=(p_owner,p_project,p_task);
 IF FOUND THEN IF a.intent IS DISTINCT FROM i OR a.external_role<>g.external_role THEN RAISE EXCEPTION 'External enqueue conflict' USING ERRCODE='P0009';END IF;RETURN a.assignment_id;END IF;
 IF t.status<>'queued' OR t.cancelled_at IS NOT NULL OR t.accepted_at IS NOT NULL THEN RAISE EXCEPTION 'External task not queued' USING ERRCODE='P0009';END IF;
 INSERT INTO forge_external_agent.assignments(external_role,owner_id,project_alias,profile_alias,task_id,intent,intent_digest,task_revision) VALUES(g.external_role,p_owner,p_project,t.agent_profile,p_task,i,encode(sha256(convert_to(i::text,'UTF8')),'hex'),t.revision) RETURNING assignment_id INTO a.assignment_id;
 PERFORM pg_notify('forge_external_agent_jobs','');RETURN a.assignment_id;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_workflow_api.enqueue_v2(text,text,uuid) FROM PUBLIC;
GRANT USAGE ON SCHEMA forge_external_workflow_api TO forge_agent;
GRANT EXECUTE ON FUNCTION forge_external_workflow_api.enqueue_v2(text,text,uuid) TO forge_agent;

CREATE FUNCTION forge_external_workflow_worker_api.read_context(p_attempt uuid,p_token uuid) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE r record;
BEGIN
 SELECT v.target_json,v.target_sha256 INTO r FROM forge_external_workflow.reviews v
 JOIN forge_external_workflow.workflows w USING(workflow_id)
 JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(v.owner_id,v.project_alias,v.reviewer_task_id)
 JOIN forge_agent.coding_attempts a ON(a.owner_id,a.project_alias,a.task_id,a.attempt_id)=(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)
 JOIN forge_agent.coding_worker_grants g ON(g.owner_id,g.project_alias,g.agent_profile,g.mode,g.worker_role)=(t.owner_id,t.project_alias,t.agent_profile,t.mode,session_user)
 JOIN forge_external_agent.scope_grants e ON(e.owner_id,e.project_alias,e.profile_alias)=(w.owner_id,w.project_alias,w.author_profile)
 WHERE a.attempt_id=p_attempt AND a.attempt_token=p_token AND a.worker_role=session_user
 AND a.lease_expires_at>clock_timestamp() AND a.deadline_at>clock_timestamp() AND a.reservation_active AND a.phase<>'stopped'
 AND t.status='running' AND t.cancelled_at IS NULL AND t.accepted_at IS NULL AND t.mode='review'
 AND t.owner_id=current_setting('forge.coding_owner',true) AND v.status='reviewing' AND w.enabled AND e.enabled
 AND w.phase IN('plan_review','code_review') FOR SHARE OF g,e,w,v;
 IF NOT FOUND THEN
  IF EXISTS(SELECT FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id) JOIN forge_external_workflow.reviews v ON(v.owner_id,v.project_alias,v.reviewer_task_id)=(t.owner_id,t.project_alias,t.id) WHERE a.attempt_id=p_attempt) THEN RAISE EXCEPTION 'External reviewer lease denied' USING ERRCODE='42501';END IF;
  RETURN NULL;
 END IF;
 RETURN jsonb_build_object('target_json',r.target_json,'target_sha256',r.target_sha256);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_workflow_worker_api.read_context(uuid,uuid) FROM PUBLIC;
GRANT USAGE ON SCHEMA forge_external_workflow_worker_api TO forge_coding_worker;
GRANT EXECUTE ON FUNCTION forge_external_workflow_worker_api.read_context(uuid,uuid) TO forge_coding_worker;

CREATE FUNCTION forge_external_workflow_delivery_api.claim(p_owner text,p_worker text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_external_workflow.actions%ROWTYPE;
BEGIN
 IF p_worker IS NULL OR p_worker!~'^[A-Za-z0-9_.-]{1,64}$' THEN RAISE EXCEPTION 'Invalid delivery worker' USING ERRCODE='22023';END IF;
 PERFORM 1 FROM forge_external_workflow.delivery_grants WHERE worker_role=session_user AND owner_id=p_owner AND enabled FOR SHARE;
 IF NOT FOUND THEN RAISE EXCEPTION 'Delivery scope denied' USING ERRCODE='42501';END IF;
 SELECT x.* INTO a FROM forge_external_workflow.actions x JOIN forge_external_workflow.workflows w USING(workflow_id)
 WHERE x.owner_id=p_owner AND(x.kind='cancel' OR(w.enabled AND w.phase NOT IN('completed','cancelled','escalated') AND w.revision=x.workflow_revision))
 AND(x.status='queued' OR(x.status='leased' AND x.lease_until<=clock_timestamp())) AND x.poll_after<=clock_timestamp() AND x.deadline_at>clock_timestamp() AND x.generation<10000
 AND EXISTS(SELECT FROM forge_external_workflow.delivery_grants g WHERE g.worker_role=session_user AND(g.owner_id,g.project_alias)=(x.owner_id,x.project_alias) AND g.enabled)
 AND(x.kind='cancel' OR EXISTS(SELECT FROM forge_external_agent.scope_grants g WHERE(g.owner_id,g.project_alias,g.profile_alias)=(w.owner_id,w.project_alias,w.author_profile) AND g.enabled))
 ORDER BY x.created_at,x.action_id FOR UPDATE OF x,w SKIP LOCKED LIMIT 1;
 IF NOT FOUND THEN RETURN NULL;END IF;
 PERFORM forge_external_workflow.delivery_lock(a.action_id);
 UPDATE forge_external_workflow.actions SET status='leased',generation=generation+1,lease_token=gen_random_uuid(),lease_role=session_user,lease_until=least(clock_timestamp()+interval '90 seconds',deadline_at),updated_at=clock_timestamp() WHERE action_id=a.action_id RETURNING * INTO a;
 RETURN jsonb_build_object('action_id',a.action_id,'workflow_id',a.workflow_id,'project_alias',a.project_alias,'kind',a.kind,'generation',a.generation,'token',a.lease_token,'lease_until',to_char(a.lease_until AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),'request',a.intent||jsonb_build_object('previous_receipt',coalesce(a.result->'receipt','{}'::jsonb)));
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_workflow_delivery_api.claim(text,text) FROM PUBLIC;
CREATE FUNCTION forge_external_workflow.delivery_lock(p_id uuid) RETURNS forge_external_workflow.actions LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_external_workflow.actions%ROWTYPE;
BEGIN
 SELECT * INTO a FROM forge_external_workflow.actions WHERE action_id=p_id FOR UPDATE;
 IF NOT FOUND THEN RAISE EXCEPTION 'Delivery action denied' USING ERRCODE='42501';END IF;
 PERFORM 1 FROM forge_external_workflow.delivery_grants g WHERE g.worker_role=session_user AND(g.owner_id,g.project_alias)=(a.owner_id,a.project_alias) AND g.enabled FOR SHARE;
 IF NOT FOUND THEN RAISE EXCEPTION 'Delivery scope denied' USING ERRCODE='42501';END IF;
 IF a.kind<>'cancel' THEN
 PERFORM 1 FROM forge_external_agent.scope_grants g JOIN forge_external_workflow.workflows w ON(g.owner_id,g.project_alias,g.profile_alias)=(w.owner_id,w.project_alias,w.author_profile) WHERE w.workflow_id=a.workflow_id AND g.enabled FOR SHARE OF g,w;
 IF NOT FOUND THEN RAISE EXCEPTION 'External delivery grant denied' USING ERRCODE='42501';END IF;END IF;
 RETURN a;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_workflow.delivery_lock(uuid) FROM PUBLIC;
CREATE FUNCTION forge_external_workflow_delivery_api.renew(p_id uuid,p_generation integer,p_token uuid) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_external_workflow.actions%ROWTYPE;
BEGIN
 a:=forge_external_workflow.delivery_lock(p_id);
 IF p_generation IS NULL OR p_token IS NULL OR a.status<>'leased' OR a.generation<>p_generation OR a.lease_token IS DISTINCT FROM p_token OR a.lease_role<>session_user OR a.lease_until<=clock_timestamp() OR a.deadline_at<=clock_timestamp()
 OR(a.kind<>'cancel' AND NOT EXISTS(SELECT FROM forge_external_workflow.workflows WHERE workflow_id=a.workflow_id AND enabled AND revision=a.workflow_revision AND phase NOT IN('completed','cancelled','escalated'))) THEN RAISE EXCEPTION 'Stale delivery lease' USING ERRCODE='P0009';END IF;
 UPDATE forge_external_workflow.actions SET lease_until=least(clock_timestamp()+interval '90 seconds',deadline_at),updated_at=clock_timestamp() WHERE action_id=p_id;RETURN true;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_workflow_delivery_api.renew(uuid,integer,uuid) FROM PUBLIC;
CREATE FUNCTION forge_external_workflow_delivery_api.checkpoint(p_id uuid,p_generation integer,p_token uuid,p_result jsonb) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_external_workflow.actions%ROWTYPE;d text;s text;
BEGIN
 a:=forge_external_workflow.delivery_lock(p_id);
 IF p_result IS NULL OR jsonb_typeof(p_result)<>'object' OR p_result-ARRAY['status','receipt','summary']<>'{}' OR NOT(p_result?&ARRAY['status','receipt','summary']) OR jsonb_typeof(p_result->'receipt')<>'object' OR jsonb_typeof(p_result->'summary')<>'string' OR octet_length(p_result::text)>65536 OR octet_length(p_result->>'summary')>4096 THEN RAISE EXCEPTION 'Invalid delivery receipt' USING ERRCODE='22023';END IF;
 s:=p_result->>'status';IF s NOT IN('pending','passed','failed','merged','main_changed','cancelled') OR(s='cancelled' AND a.kind<>'cancel') OR(a.kind='cancel' AND s NOT IN('pending','cancelled','failed')) OR(s='merged' AND a.kind<>'merge') OR(s='passed' AND a.kind='merge') OR(s='main_changed' AND a.kind NOT IN('prepare','merge')) THEN RAISE EXCEPTION 'Invalid delivery status' USING ERRCODE='22023';END IF;
 d:=encode(sha256(convert_to(p_result::text,'UTF8')),'hex');
 IF a.checkpoint_generation=p_generation AND a.checkpoint_token=p_token AND a.checkpoint_role=session_user THEN
 IF a.checkpoint_digest IS DISTINCT FROM d THEN RAISE EXCEPTION 'Delivery checkpoint conflict' USING ERRCODE='P0009';END IF;RETURN a.result;END IF;
 PERFORM forge_external_workflow_delivery_api.renew(p_id,p_generation,p_token);
 UPDATE forge_external_workflow.actions SET status=CASE WHEN s='pending' THEN 'queued' WHEN s IN('passed','merged','cancelled') THEN 'completed' ELSE 'failed' END,result=p_result,checkpoint_digest=d,checkpoint_token=p_token,checkpoint_generation=p_generation,checkpoint_role=session_user,lease_token=NULL,lease_role=NULL,lease_until=NULL,poll_after=clock_timestamp()+interval '5 seconds',updated_at=clock_timestamp() WHERE action_id=p_id;
 PERFORM pg_notify('forge_coding_jobs','');RETURN p_result;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_workflow_delivery_api.checkpoint(uuid,integer,uuid,jsonb) FROM PUBLIC;
GRANT USAGE ON SCHEMA forge_external_workflow_delivery_api TO forge_coding_reconciler;
GRANT EXECUTE ON FUNCTION forge_external_workflow_delivery_api.claim(text,text),forge_external_workflow_delivery_api.renew(uuid,integer,uuid),forge_external_workflow_delivery_api.checkpoint(uuid,integer,uuid,jsonb) TO forge_coding_reconciler;

CREATE FUNCTION forge_external_workflow_api.is_internal(p_owner text,p_project text,p_task uuid) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE x record;
BEGIN
 SELECT t.role,w.author_profile INTO x FROM forge_external_workflow.tasks t JOIN forge_external_workflow.workflows w USING(workflow_id) WHERE(t.owner_id,t.project_alias,t.task_id)=(p_owner,p_project,p_task);
 IF NOT FOUND THEN RETURN false;END IF;
 PERFORM 1 FROM forge_external_agent.scope_grants g WHERE g.service_role=session_user AND(g.owner_id,g.project_alias,g.profile_alias)=(p_owner,p_project,x.author_profile);
 IF NOT FOUND THEN RAISE EXCEPTION 'External workflow scope denied' USING ERRCODE='42501';END IF;
 RETURN x.role<>'root';
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_workflow_api.is_internal(text,text,uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_external_workflow_api.is_internal(text,text,uuid) TO forge_agent;
CREATE FUNCTION forge_external_workflow_api.service_scope(p_owner text,p_project text,p_profile text,p_active boolean) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE active boolean;
BEGIN
 IF p_active IS NULL THEN RAISE EXCEPTION 'Invalid external scope mode' USING ERRCODE='22023';END IF;
 IF p_active THEN
 SELECT enabled INTO active FROM forge_external_agent.scope_grants WHERE service_role=session_user AND(owner_id,project_alias,profile_alias)=(p_owner,p_project,p_profile) FOR SHARE;
 ELSE
 SELECT enabled INTO active FROM forge_external_agent.scope_grants WHERE service_role=session_user AND(owner_id,project_alias,profile_alias)=(p_owner,p_project,p_profile);
 END IF;
 IF NOT FOUND THEN RAISE EXCEPTION 'External service scope denied' USING ERRCODE='42501';END IF;RETURN active;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_workflow_api.service_scope(text,text,text,boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION forge_external_workflow_api.service_scope(text,text,text,boolean) TO forge_agent;
CREATE OR REPLACE FUNCTION forge_external_agent_api.capabilities() RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 RETURN coalesce((SELECT jsonb_agg(jsonb_build_object('protocol',CASE WHEN EXISTS(SELECT FROM forge_external_workflow.workflows w WHERE(w.owner_id,w.project_alias,w.author_profile)=(g.owner_id,g.project_alias,g.profile_alias)) THEN 'forge-external-db-v2' ELSE 'forge-external-db-v1' END,
 'project_alias',g.project_alias,'profile_alias',g.profile_alias,'modes',CASE WHEN EXISTS(SELECT FROM forge_external_workflow.workflows w WHERE(w.owner_id,w.project_alias,w.author_profile)=(g.owner_id,g.project_alias,g.profile_alias)) THEN jsonb_build_array('plan','review','implement') ELSE jsonb_build_array('plan','review') END) ORDER BY g.project_alias,g.profile_alias)
 FROM forge_external_agent.scope_grants g WHERE external_role=session_user AND enabled),'[]'::jsonb);
END;
$fn$;
