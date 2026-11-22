-- M20: scoped external DB transport. No native execution or reconciliation.
CREATE SCHEMA forge_external_agent AUTHORIZATION forge_agent_owner;
CREATE SCHEMA forge_external_agent_api AUTHORIZATION forge_agent_owner;
REVOKE ALL ON SCHEMA forge_external_agent,forge_external_agent_api FROM PUBLIC;
CREATE TABLE forge_external_agent.scope_grants (
 external_role name NOT NULL, service_role name NOT NULL DEFAULT 'forge_agent',
 owner_id text NOT NULL CHECK(octet_length(owner_id) BETWEEN 1 AND 128 AND btrim(owner_id)<>''),
 project_alias text NOT NULL CHECK(project_alias~'^[A-Za-z0-9_.-]{1,64}$'),
 profile_alias text NOT NULL CHECK(profile_alias~'^[A-Za-z0-9_.-]{1,64}$'), enabled boolean NOT NULL DEFAULT false,
 PRIMARY KEY(external_role,owner_id,project_alias,profile_alias)
);
CREATE TABLE forge_external_agent.readiness (
 external_role name NOT NULL,owner_id text NOT NULL,project_alias text NOT NULL,profile_alias text NOT NULL,
 runner_id text NOT NULL CHECK(runner_id~'^[A-Za-z0-9_.-]{1,64}$'),observed_at timestamptz NOT NULL,
 PRIMARY KEY(external_role,owner_id,project_alias,profile_alias),
 FOREIGN KEY(external_role,owner_id,project_alias,profile_alias) REFERENCES forge_external_agent.scope_grants
);
CREATE TABLE forge_external_agent.assignments (
 assignment_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
 external_role name NOT NULL,owner_id text NOT NULL,project_alias text NOT NULL,profile_alias text NOT NULL,task_id uuid NOT NULL,
 intent jsonb NOT NULL,intent_digest text NOT NULL CHECK(intent_digest~'^[0-9a-f]{64}$'),
 status text NOT NULL DEFAULT 'queued' CHECK(status IN('queued','leased','submitted','failed','cancelled')),
 task_revision bigint NOT NULL CHECK(task_revision BETWEEN 1 AND 9007199254740991),
 generation integer NOT NULL DEFAULT 0 CHECK(generation BETWEEN 0 AND 3),
 lease_token uuid,lease_until timestamptz,deadline_at timestamptz NOT NULL DEFAULT(clock_timestamp()+interval '24 hours'),
 finish_token uuid,finish_intent text,receipt jsonb,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 UNIQUE(owner_id,project_alias,task_id),
 FOREIGN KEY(owner_id,project_alias,task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id),
 FOREIGN KEY(external_role,owner_id,project_alias,profile_alias) REFERENCES forge_external_agent.scope_grants,
 CHECK((status='leased' AND lease_token IS NOT NULL AND lease_until IS NOT NULL) OR(status<>'leased' AND lease_token IS NULL AND lease_until IS NULL)),
 CHECK((status='submitted' AND finish_token IS NOT NULL AND finish_intent IS NOT NULL AND receipt IS NOT NULL) OR(status<>'submitted' AND finish_token IS NULL AND finish_intent IS NULL AND receipt IS NULL))
);
CREATE INDEX external_assignments_pending ON forge_external_agent.assignments(external_role,project_alias,profile_alias,status,created_at);
CREATE TABLE forge_external_agent.artifacts (
 assignment_id uuid PRIMARY KEY REFERENCES forge_external_agent.assignments(assignment_id),
 provider text NOT NULL DEFAULT 'muse-db' CHECK(provider='muse-db'),
 sha256 text NOT NULL CHECK(sha256~'^[0-9a-f]{64}$'),content text NOT NULL CHECK(octet_length(content) BETWEEN 1 AND 262144),
 summary text NOT NULL CHECK(octet_length(summary) BETWEEN 1 AND 4096),submitted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 principal name NOT NULL,generation integer NOT NULL CHECK(generation BETWEEN 1 AND 3),recipe_digest text NOT NULL
);
REVOKE ALL ON ALL TABLES IN SCHEMA forge_external_agent FROM PUBLIC;

CREATE FUNCTION forge_external_agent._immutable() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 IF TG_TABLE_NAME='artifacts' THEN RAISE EXCEPTION 'Immutable external artifact' USING ERRCODE='P0009';END IF;
 IF(NEW.external_role,NEW.owner_id,NEW.project_alias,NEW.profile_alias,NEW.task_id,NEW.intent,NEW.intent_digest,NEW.deadline_at,NEW.created_at)
 IS DISTINCT FROM(OLD.external_role,OLD.owner_id,OLD.project_alias,OLD.profile_alias,OLD.task_id,OLD.intent,OLD.intent_digest,OLD.deadline_at,OLD.created_at) THEN RAISE EXCEPTION 'Immutable external assignment binding' USING ERRCODE='P0009';END IF;
 RETURN NEW;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent._immutable() FROM PUBLIC;

CREATE TRIGGER external_artifact_immutable BEFORE UPDATE OR DELETE ON forge_external_agent.artifacts FOR EACH ROW EXECUTE FUNCTION forge_external_agent._immutable();
CREATE TRIGGER external_assignment_binding BEFORE UPDATE ON forge_external_agent.assignments FOR EACH ROW EXECUTE FUNCTION forge_external_agent._immutable();

CREATE FUNCTION forge_external_agent._intent(p_t forge_agent.coding_tasks) RETURNS jsonb LANGUAGE sql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
SELECT jsonb_build_object('owner_id',p_t.owner_id,'project_alias',p_t.project_alias,'task_id',p_t.id,'agent_profile',p_t.agent_profile,'base_revision',p_t.base_revision,'mode',p_t.mode,'prompt',p_t.prompt,'acceptance_criteria',p_t.acceptance_criteria,'pinned_recipe',p_t.pinned_recipe,'recipe_digest',p_t.recipe_digest)
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent._intent(forge_agent.coding_tasks) FROM PUBLIC;

CREATE FUNCTION forge_external_agent._event(p_a forge_external_agent.assignments,p_kind text,p_payload jsonb) RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
 SELECT p_a.owner_id,p_a.project_alias,p_a.task_id,coalesce(max(sequence),0)+1,p_kind,p_payload FROM forge_agent.coding_events
 WHERE(owner_id,project_alias,task_id)=(p_a.owner_id,p_a.project_alias,p_a.task_id);
 PERFORM pg_notify('forge_coding_jobs','');
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent._event(forge_external_agent.assignments,text,jsonb) FROM PUBLIC;

CREATE FUNCTION forge_external_agent._scope(p_project text,p_profile text) RETURNS forge_external_agent.scope_grants LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE g forge_external_agent.scope_grants%ROWTYPE;n bigint;
BEGIN
 SELECT count(*) INTO n FROM forge_external_agent.scope_grants WHERE external_role=session_user AND project_alias=p_project AND profile_alias=p_profile AND enabled;
 IF n<>1 THEN RAISE EXCEPTION 'External scope denied' USING ERRCODE='42501';END IF;
 SELECT * INTO g FROM forge_external_agent.scope_grants WHERE external_role=session_user AND project_alias=p_project AND profile_alias=p_profile AND enabled FOR SHARE;
 IF NOT FOUND THEN RAISE EXCEPTION 'External scope denied' USING ERRCODE='42501';END IF;RETURN g;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent._scope(text,text) FROM PUBLIC;

CREATE FUNCTION forge_external_agent._locked(p_id uuid) RETURNS forge_external_agent.assignments LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_external_agent.assignments%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;
BEGIN
 SELECT x.* INTO a FROM forge_external_agent.assignments x JOIN forge_external_agent.scope_grants g
 ON(g.external_role,g.owner_id,g.project_alias,g.profile_alias)=(x.external_role,x.owner_id,x.project_alias,x.profile_alias)
 WHERE x.assignment_id=p_id AND g.external_role=session_user AND g.enabled FOR SHARE OF g;
 IF NOT FOUND THEN RAISE EXCEPTION 'External assignment denied' USING ERRCODE='42501';END IF;
 SELECT x.* INTO a FROM forge_external_agent.assignments x JOIN forge_agent.coding_tasks q ON(q.owner_id,q.project_alias,q.id)=(x.owner_id,x.project_alias,x.task_id)
 WHERE x.assignment_id=p_id FOR UPDATE OF q,x;
 IF NOT FOUND THEN RAISE EXCEPTION 'External assignment denied' USING ERRCODE='42501';END IF;
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id);
 IF forge_external_agent._intent(t) IS DISTINCT FROM a.intent OR t.latest_attempt_id IS NOT NULL THEN RAISE EXCEPTION 'External task binding changed' USING ERRCODE='P0009';END IF;
 RETURN a;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent._locked(uuid) FROM PUBLIC;

CREATE FUNCTION forge_external_agent._active(p_id uuid,p_gen integer,p_token uuid) RETURNS forge_external_agent.assignments LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_external_agent.assignments%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;
BEGIN
 a:=forge_external_agent._locked(p_id);
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id);
 IF p_gen IS NULL OR p_token IS NULL OR a.status<>'leased' OR a.generation<>p_gen OR a.lease_token<>p_token
 OR a.lease_until<=clock_timestamp() OR a.deadline_at<=clock_timestamp() OR t.status<>'running'
 OR t.revision<>a.task_revision OR t.cancelled_at IS NOT NULL OR t.accepted_at IS NOT NULL THEN
 RAISE EXCEPTION 'External lease or task state changed' USING ERRCODE='P0009';END IF;
 RETURN a;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent._active(uuid,integer,uuid) FROM PUBLIC;

CREATE FUNCTION forge_external_agent_api.capabilities() RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 RETURN coalesce((SELECT jsonb_agg(jsonb_build_object('protocol','forge-external-db-v1','project_alias',project_alias,'profile_alias',profile_alias,'modes',jsonb_build_array('plan','review')) ORDER BY project_alias,profile_alias)
 FROM forge_external_agent.scope_grants WHERE external_role=session_user AND enabled),'[]'::jsonb);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent_api.capabilities() FROM PUBLIC;

CREATE FUNCTION forge_external_agent_api.advertise(p_project text,p_profile text,p_runner text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE g forge_external_agent.scope_grants%ROWTYPE;seen timestamptz:=clock_timestamp();
BEGIN
 g:=forge_external_agent._scope(p_project,p_profile);
 IF p_runner IS NULL OR p_runner!~'^[A-Za-z0-9_.-]{1,64}$' THEN RAISE EXCEPTION 'Invalid runner identity' USING ERRCODE='22023';END IF;
 INSERT INTO forge_external_agent.readiness(external_role,owner_id,project_alias,profile_alias,runner_id,observed_at)
 VALUES(g.external_role,g.owner_id,g.project_alias,g.profile_alias,p_runner,seen)
 ON CONFLICT(external_role,owner_id,project_alias,profile_alias) DO UPDATE SET runner_id=excluded.runner_id,observed_at=excluded.observed_at;
 RETURN jsonb_build_object('status','advertised','observed_at',to_char(seen AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'));
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent_api.advertise(text,text,text) FROM PUBLIC;

CREATE FUNCTION forge_external_agent_api.enqueue(p_owner text,p_project text,p_task uuid) RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE t forge_agent.coding_tasks%ROWTYPE;g forge_external_agent.scope_grants%ROWTYPE;a forge_external_agent.assignments%ROWTYPE;i jsonb;n bigint;
BEGIN
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(p_owner,p_project,p_task) FOR UPDATE;
 IF NOT FOUND THEN RAISE EXCEPTION 'External task missing' USING ERRCODE='42501';END IF;
 SELECT count(*) INTO n FROM forge_external_agent.scope_grants WHERE service_role=session_user AND owner_id=p_owner AND project_alias=p_project AND profile_alias=t.agent_profile AND enabled;
 IF n<>1 THEN RAISE EXCEPTION 'External service scope denied' USING ERRCODE='42501';END IF;
 SELECT * INTO g FROM forge_external_agent.scope_grants WHERE service_role=session_user AND owner_id=p_owner AND project_alias=p_project AND profile_alias=t.agent_profile AND enabled FOR SHARE;
 IF NOT FOUND THEN RAISE EXCEPTION 'External service scope denied' USING ERRCODE='42501';END IF;
 IF (t.mode IN('plan','review') AND t.pinned_recipe=jsonb_build_object('version',2,'backend','muse','model','external-runner','resource_profile','forge-external-db-v1','mode',t.mode,'generation_token_limit',NULL,'policy_digest','413cc03151f4de66b715b6b3d2da4ebcc46cc1f09756edc0ae2ddf22a3c4f584')
 AND t.latest_attempt_id IS NULL AND NOT t.auto_repair AND NOT t.workflow_required AND NOT t.review_required) IS NOT TRUE THEN RAISE EXCEPTION 'Invalid external recipe' USING ERRCODE='22023';END IF;
 i:=forge_external_agent._intent(t);
 SELECT * INTO a FROM forge_external_agent.assignments WHERE(owner_id,project_alias,task_id)=(p_owner,p_project,p_task);
 IF FOUND THEN
 IF a.intent IS DISTINCT FROM i OR a.external_role<>g.external_role THEN RAISE EXCEPTION 'External enqueue conflict' USING ERRCODE='P0009';END IF;
 RETURN a.assignment_id;END IF;
 IF t.status<>'queued' OR t.cancelled_at IS NOT NULL OR t.accepted_at IS NOT NULL THEN RAISE EXCEPTION 'External task not queued' USING ERRCODE='P0009';END IF;
 INSERT INTO forge_external_agent.assignments(external_role,owner_id,project_alias,profile_alias,task_id,intent,intent_digest,task_revision)
 VALUES(g.external_role,p_owner,p_project,t.agent_profile,p_task,i,encode(sha256(convert_to(i::text,'UTF8')),'hex'),t.revision) RETURNING assignment_id INTO a.assignment_id;
 PERFORM pg_notify('forge_external_agent_jobs','');RETURN a.assignment_id;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent_api.enqueue(text,text,uuid) FROM PUBLIC;

CREATE FUNCTION forge_external_agent_api.catalogue(p_owner text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 RETURN coalesce((SELECT jsonb_agg(jsonb_build_object('project_alias',g.project_alias,'profile_alias',g.profile_alias,'runner_id',r.runner_id,
 'observed_at',to_char(r.observed_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),'is_fresh',coalesce(r.observed_at>clock_timestamp()-interval '90 seconds',false)) ORDER BY g.project_alias,g.profile_alias)
 FROM forge_external_agent.scope_grants g JOIN forge_external_agent.readiness r USING(external_role,owner_id,project_alias,profile_alias)
 WHERE g.service_role=session_user AND g.owner_id=p_owner AND g.enabled),'[]'::jsonb);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent_api.catalogue(text) FROM PUBLIC;

CREATE FUNCTION forge_external_agent._housekeep(p_g forge_external_agent.scope_grants,p_budget integer) RETURNS integer LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_external_agent.assignments%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;s text;ev text;n integer:=0;
BEGIN
 FOR a IN SELECT x.* FROM forge_external_agent.assignments x JOIN forge_agent.coding_tasks q ON(q.owner_id,q.project_alias,q.id)=(x.owner_id,x.project_alias,x.task_id)
 WHERE(x.external_role,x.owner_id,x.project_alias,x.profile_alias)=(p_g.external_role,p_g.owner_id,p_g.project_alias,p_g.profile_alias)
 AND x.status IN('queued','leased') AND q.latest_attempt_id IS NULL AND q.status IN('queued','running','cancel_requested','cancelled')
 AND(NOT p_g.enabled OR q.status IN('cancelled','cancel_requested') OR x.deadline_at<=clock_timestamp() OR(x.status='leased' AND x.lease_until<=clock_timestamp())) ORDER BY x.created_at,x.assignment_id FOR UPDATE OF q,x SKIP LOCKED LIMIT least(64,greatest(0,p_budget)) LOOP
 n:=n+1;
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id);
 s:=CASE WHEN t.status IN('cancelled','cancel_requested') THEN 'cancelled' WHEN NOT p_g.enabled THEN 'blocked_permission' WHEN a.deadline_at<=clock_timestamp() OR a.generation>=3 OR forge_external_agent._intent(t) IS DISTINCT FROM a.intent THEN 'failed' ELSE 'queued' END;
 UPDATE forge_agent.coding_tasks SET status=s,cancelled_at=CASE WHEN s='cancelled' THEN coalesce(cancelled_at,clock_timestamp()) ELSE NULL END,revision=revision+1,updated_at=clock_timestamp() WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id) RETURNING revision INTO a.task_revision;
 UPDATE forge_external_agent.assignments SET status=CASE WHEN s='blocked_permission' THEN 'failed' ELSE s END,task_revision=a.task_revision,lease_token=NULL,lease_until=NULL,updated_at=clock_timestamp() WHERE assignment_id=a.assignment_id;
 ev:=CASE WHEN s='queued' THEN 'resumed' ELSE s END;
 PERFORM forge_external_agent._event(a,ev,jsonb_build_object('provider','muse-db','assignment_id',a.assignment_id,'reason','external_lease_reconciled'));
 END LOOP;
 IF n>0 THEN PERFORM pg_notify('forge_external_agent_jobs','');END IF;RETURN n;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent._housekeep(forge_external_agent.scope_grants,integer) FROM PUBLIC;

CREATE FUNCTION forge_external_agent_api.reconcile(p_owner text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE g forge_external_agent.scope_grants%ROWTYPE;remaining integer:=64;
BEGIN
 FOR g IN SELECT * FROM forge_external_agent.scope_grants WHERE service_role=session_user AND owner_id=p_owner ORDER BY project_alias,profile_alias LIMIT 64 LOOP
 EXIT WHEN remaining<=0;remaining:=remaining-forge_external_agent._housekeep(g,remaining);END LOOP;
 RETURN jsonb_build_object('status','reconciled','count',64-remaining);
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent_api.reconcile(text) FROM PUBLIC;

CREATE FUNCTION forge_external_agent_api.claim(p_project text,p_profile text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
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
REVOKE ALL ON FUNCTION forge_external_agent_api.claim(text,text) FROM PUBLIC;

CREATE FUNCTION forge_external_agent_api.renew(p_id uuid,p_gen integer,p_token uuid) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_external_agent.assignments%ROWTYPE;
BEGIN a:=forge_external_agent._active(p_id,p_gen,p_token);
 UPDATE forge_external_agent.assignments SET lease_until=least(clock_timestamp()+interval '5 minutes',deadline_at),updated_at=clock_timestamp() WHERE assignment_id=p_id RETURNING * INTO a;
 RETURN jsonb_build_object('status','renewed','lease_until',to_char(a.lease_until AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'));END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent_api.renew(uuid,integer,uuid) FROM PUBLIC;

CREATE FUNCTION forge_external_agent_api.progress(p_id uuid,p_gen integer,p_token uuid,p_summary text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_external_agent.assignments%ROWTYPE;
BEGIN a:=forge_external_agent._active(p_id,p_gen,p_token);
 IF p_summary IS NULL OR octet_length(p_summary) NOT BETWEEN 1 AND 4096 OR btrim(p_summary)='' OR translate(p_summary,E'\n\r\t','')~'[[:cntrl:]]' THEN RAISE EXCEPTION 'Invalid external summary' USING ERRCODE='22023';END IF;
 PERFORM forge_external_agent._event(a,'checkpoint',jsonb_build_object('provider','muse-db','assignment_id',p_id,'generation',p_gen,'summary',p_summary));
 RETURN jsonb_build_object('status','checkpoint');END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent_api.progress(uuid,integer,uuid,text) FROM PUBLIC;

CREATE FUNCTION forge_external_agent_api.finish(p_id uuid,p_gen integer,p_token uuid,p_content text,p_summary text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_external_agent.assignments%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;digest text;intent_hash text;r jsonb;stamp timestamptz:=clock_timestamp();
BEGIN
 a:=forge_external_agent._locked(p_id);
 IF p_gen IS NULL OR p_token IS NULL OR p_content IS NULL OR octet_length(p_content) NOT BETWEEN 1 AND 262144 OR btrim(p_content)='' OR translate(p_content,E'\n\r\t','')~'[[:cntrl:]]'
 OR p_summary IS NULL OR octet_length(p_summary) NOT BETWEEN 1 AND 4096 OR btrim(p_summary)='' OR translate(p_summary,E'\n\r\t','')~'[[:cntrl:]]' THEN RAISE EXCEPTION 'Invalid external result' USING ERRCODE='22023';END IF;
 digest:=encode(sha256(convert_to(p_content,'UTF8')),'hex');
 intent_hash:=encode(sha256(convert_to(jsonb_build_object('assignment',p_id,'scope',a.intent_digest,'principal',session_user,'generation',p_gen,'sha256',digest,'summary',p_summary)::text,'UTF8')),'hex');
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id);
 IF t.status IN('cancelled','cancel_requested') THEN RAISE EXCEPTION 'External task cancelled' USING ERRCODE='P0009';END IF;
 IF a.status='submitted' THEN
 IF a.generation<>p_gen OR a.finish_token IS DISTINCT FROM p_token OR a.finish_intent IS DISTINCT FROM intent_hash THEN RAISE EXCEPTION 'External finish conflict' USING ERRCODE='P0009';END IF;
 RETURN a.receipt;END IF;
 a:=forge_external_agent._active(p_id,p_gen,p_token);
 r:=jsonb_build_object('assignment_id',p_id,'task_id',a.task_id,'project_alias',a.project_alias,'provider','muse-db','status','awaiting_review','sha256',digest,'generation',p_gen,'submitted_at',to_char(stamp AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'));
 INSERT INTO forge_external_agent.artifacts(assignment_id,sha256,content,summary,submitted_at,principal,generation,recipe_digest) VALUES(p_id,digest,p_content,p_summary,stamp,session_user,p_gen,a.intent->>'recipe_digest');
 UPDATE forge_agent.coding_tasks SET status='awaiting_review',revision=revision+1,updated_at=stamp WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id);
 UPDATE forge_external_agent.assignments SET status='submitted',lease_token=NULL,lease_until=NULL,finish_token=p_token,finish_intent=intent_hash,receipt=r,updated_at=stamp WHERE assignment_id=p_id;
 PERFORM forge_external_agent._event(a,'awaiting_review',r);RETURN r;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent_api.finish(uuid,integer,uuid,text,text) FROM PUBLIC;

CREATE FUNCTION forge_external_agent_api.release(p_id uuid,p_gen integer,p_token uuid) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_external_agent.assignments%ROWTYPE;s text;
BEGIN a:=forge_external_agent._active(p_id,p_gen,p_token);s:=CASE WHEN a.generation>=3 THEN 'failed' ELSE 'queued' END;
 UPDATE forge_agent.coding_tasks SET status=s,revision=revision+1,updated_at=clock_timestamp() WHERE(owner_id,project_alias,id)=(a.owner_id,a.project_alias,a.task_id) RETURNING revision INTO a.task_revision;
 UPDATE forge_external_agent.assignments SET status=s,task_revision=a.task_revision,lease_token=NULL,lease_until=NULL,updated_at=clock_timestamp() WHERE assignment_id=p_id;
 PERFORM forge_external_agent._event(a,CASE WHEN s='queued' THEN 'resumed' ELSE 'failed' END,jsonb_build_object('provider','muse-db','assignment_id',p_id,'generation',p_gen));
 PERFORM pg_notify('forge_external_agent_jobs','');RETURN jsonb_build_object('status',s);END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent_api.release(uuid,integer,uuid) FROM PUBLIC;

CREATE FUNCTION forge_external_agent._artifact(p_id uuid) RETURNS jsonb LANGUAGE sql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
SELECT jsonb_build_object('assignment_id',assignment_id,'provider',provider,'sha256',sha256,'content',content,'submitted_at',to_char(submitted_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),'summary',summary) FROM forge_external_agent.artifacts WHERE assignment_id=p_id
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent._artifact(uuid) FROM PUBLIC;

CREATE FUNCTION forge_external_agent_api.artifact(p_id uuid) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_external_agent.assignments%ROWTYPE;
BEGIN a:=forge_external_agent._locked(p_id);RETURN forge_external_agent._artifact(p_id);END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent_api.artifact(uuid) FROM PUBLIC;

CREATE FUNCTION forge_external_agent_api.artifact_by_task(p_owner text,p_project text,p_task uuid) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_external_agent.assignments%ROWTYPE;
BEGIN
 SELECT * INTO a FROM forge_external_agent.assignments WHERE(owner_id,project_alias,task_id)=(p_owner,p_project,p_task);
 IF NOT FOUND THEN RETURN NULL;END IF;
 PERFORM 1 FROM forge_external_agent.scope_grants g WHERE g.service_role=session_user AND(g.external_role,g.owner_id,g.project_alias,g.profile_alias)=(a.external_role,a.owner_id,a.project_alias,a.profile_alias) AND g.enabled FOR SHARE;
 IF NOT FOUND THEN RAISE EXCEPTION 'External service scope denied' USING ERRCODE='42501';END IF;
 RETURN forge_external_agent._artifact(a.assignment_id);END;
$fn$;
REVOKE ALL ON FUNCTION forge_external_agent_api.artifact_by_task(text,text,uuid) FROM PUBLIC;
