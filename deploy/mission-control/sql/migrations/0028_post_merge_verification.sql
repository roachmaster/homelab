-- M28: verification after a confirmed merge. This grants no model/review/merge
-- authority, changes no historical receipts, and never backfills completed work.
CREATE TABLE forge_agent.post_merge_verification_grants(
 worker_role name NOT NULL,owner_id text NOT NULL,project_alias text NOT NULL,
 enabled boolean NOT NULL DEFAULT false,PRIMARY KEY(worker_role,owner_id,project_alias)
);
CREATE TABLE forge_agent.post_merge_verifications(
 post_merge_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
 owner_id text NOT NULL,project_alias text NOT NULL,source_kind text NOT NULL CHECK(source_kind IN('native','external')),
 workflow_id uuid NOT NULL,source_action_id uuid NOT NULL,root_task_id uuid NOT NULL,
 merge_commit text NOT NULL CHECK(merge_commit~'^[0-9a-f]{40}$'),merge_tree text NOT NULL CHECK(merge_tree~'^[0-9a-f]{40}$'),
 merge_receipt jsonb NOT NULL CHECK(jsonb_typeof(merge_receipt)='object'),
 correlation_id uuid NOT NULL DEFAULT gen_random_uuid(),
 status text NOT NULL DEFAULT 'pending' CHECK(status IN('pending','leased','passed','failed','superseded')),
 generation integer NOT NULL DEFAULT 0 CHECK(generation BETWEEN 0 AND 100000),
 worker_id text,action_role name,lease_token uuid,lease_role name,lease_until timestamptz,
 request jsonb,receipt jsonb,cancel_requested boolean NOT NULL DEFAULT false,
 summary text NOT NULL DEFAULT '' CHECK(octet_length(summary)<=4096),
 next_poll_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 deadline_at timestamptz NOT NULL DEFAULT(clock_timestamp()+interval '90 minutes'),
 finish_generation integer,finish_token uuid,finish_role name,finish_value jsonb,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 FOREIGN KEY(owner_id,project_alias,root_task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id),
 UNIQUE(owner_id,project_alias,source_kind,workflow_id,merge_commit),
 CHECK((status='leased' AND lease_token IS NOT NULL AND lease_role IS NOT NULL AND lease_until IS NOT NULL)
 OR(status<>'leased' AND lease_token IS NULL AND lease_role IS NULL AND lease_until IS NULL)),
 CHECK(request IS NULL OR(jsonb_typeof(request)='object' AND octet_length(request::text)<=65536)),
 CHECK(receipt IS NULL OR(jsonb_typeof(receipt)='object' AND octet_length(receipt::text)<=262144))
);
CREATE INDEX post_merge_verifications_pending ON forge_agent.post_merge_verifications(owner_id,status,next_poll_at,created_at);
-- A failure is independently durable follow-up evidence, never a reopened task
-- or an implicit admission of new model work. Explicit policy can consume it.
CREATE TABLE forge_agent.post_merge_remediations(
 post_merge_id uuid PRIMARY KEY REFERENCES forge_agent.post_merge_verifications(post_merge_id),
 owner_id text NOT NULL,project_alias text NOT NULL,workflow_id uuid NOT NULL,root_task_id uuid NOT NULL,
 merge_commit text NOT NULL,summary text NOT NULL CHECK(octet_length(summary)<=4096),
 receipt jsonb NOT NULL CHECK(jsonb_typeof(receipt)='object' AND octet_length(receipt::text)<=262144),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
REVOKE ALL ON forge_agent.post_merge_verification_grants,forge_agent.post_merge_verifications,forge_agent.post_merge_remediations FROM PUBLIC;

CREATE FUNCTION forge_agent.post_merge_native_signal() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE p forge_agent.coding_publications%ROWTYPE;w forge_agent.coding_workflows%ROWTYPE;
BEGIN
 IF OLD.status<>'leased' OR NEW.status<>'completed' OR NEW.kind<>'merge' OR NEW.finish_receipt->>'status'<>'merged' THEN RETURN NEW;END IF;
 SELECT * INTO p FROM forge_agent.coding_publications WHERE publication_id=NEW.source_publication_id;
 SELECT * INTO w FROM forge_agent.coding_workflows WHERE workflow_id=NEW.workflow_id;
 -- merge_finish already checks independent approval before this AFTER trigger.
 -- Bind to its accepted lease, publication and immutable prepared merge intent.
 IF NEW.finish_token IS DISTINCT FROM OLD.lease_token OR OLD.lease_expires_at<=clock_timestamp()
 OR NEW.intent IS NULL OR NEW.finish_receipt->'receipt'->>'candidate_commit' IS DISTINCT FROM p.commit_sha
 OR NEW.finish_receipt->'receipt'->>'merge_commit' IS DISTINCT FROM NEW.intent->>'merge_commit'
 OR NEW.intent->>'candidate_tree'!~'^[0-9a-f]{40}$'
 OR(w.owner_id,w.project_alias) IS DISTINCT FROM(NEW.owner_id,NEW.project_alias)
 THEN RAISE EXCEPTION 'Invalid confirmed native merge signal' USING ERRCODE='P0009';END IF;
 INSERT INTO forge_agent.post_merge_verifications(owner_id,project_alias,source_kind,workflow_id,source_action_id,root_task_id,merge_commit,merge_tree,merge_receipt)
 VALUES(NEW.owner_id,NEW.project_alias,'native',NEW.workflow_id,NEW.action_id,w.root_task_id,
 NEW.intent->>'merge_commit',NEW.intent->>'candidate_tree',NEW.finish_receipt)
 ON CONFLICT(owner_id,project_alias,source_kind,workflow_id,merge_commit) DO NOTHING;
 PERFORM pg_notify('forge_coding_jobs','');RETURN NEW;
END;$fn$;
CREATE TRIGGER post_merge_native_confirmed AFTER UPDATE ON forge_agent.coding_workflow_actions FOR EACH ROW EXECUTE FUNCTION forge_agent.post_merge_native_signal();

CREATE FUNCTION forge_agent.post_merge_external_signal() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_external_workflow.actions%ROWTYPE;c jsonb;r jsonb;
BEGIN
 IF OLD.phase='completed' OR NEW.phase<>'completed' THEN RETURN NEW;END IF;
 c:=NEW.state->'candidate';r:=NEW.state->'merge_receipt';
 SELECT * INTO a FROM forge_external_workflow.actions WHERE workflow_id=NEW.workflow_id AND kind='merge' AND source_key=c->>'candidate_commit';
 IF OLD.phase<>'merging' OR a.action_id IS NULL OR a.status<>'completed' OR a.result->>'status'<>'merged'
 OR a.result->'receipt' IS DISTINCT FROM r OR r->>'merge_commit' IS DISTINCT FROM c->>'candidate_commit'
 OR r->>'candidate_tree' IS DISTINCT FROM c->>'candidate_tree' OR r->>'expected_main' IS DISTINCT FROM c->>'integration_base'
 OR NEW.state->'verification'->>'result' IS DISTINCT FROM 'SUCCESS'
 OR NEW.state->'verification'->>'candidate_commit' IS DISTINCT FROM c->>'candidate_commit'
 OR NEW.state->'verification'->>'candidate_tree' IS DISTINCT FROM c->>'candidate_tree'
 OR(a.owner_id,a.project_alias) IS DISTINCT FROM(NEW.owner_id,NEW.project_alias)
 THEN RAISE EXCEPTION 'Invalid confirmed external merge signal' USING ERRCODE='P0009';END IF;
 INSERT INTO forge_agent.post_merge_verifications(owner_id,project_alias,source_kind,workflow_id,source_action_id,root_task_id,merge_commit,merge_tree,merge_receipt)
 VALUES(NEW.owner_id,NEW.project_alias,'external',NEW.workflow_id,a.action_id,NEW.root_task_id,
 r->>'merge_commit',r->>'candidate_tree',a.result)
 ON CONFLICT(owner_id,project_alias,source_kind,workflow_id,merge_commit) DO NOTHING;
 PERFORM pg_notify('forge_coding_jobs','');RETURN NEW;
END;$fn$;
CREATE TRIGGER post_merge_external_confirmed AFTER UPDATE ON forge_external_workflow.workflows FOR EACH ROW EXECUTE FUNCTION forge_agent.post_merge_external_signal();

CREATE FUNCTION forge_agent.post_merge_lock(p_id uuid) RETURNS forge_agent.post_merge_verifications LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.post_merge_verifications%ROWTYPE;
BEGIN
 SELECT * INTO a FROM forge_agent.post_merge_verifications WHERE post_merge_id=p_id AND owner_id=current_setting('forge.coding_owner',true) FOR UPDATE;
 IF NOT FOUND THEN RAISE EXCEPTION 'Post merge scope denied' USING ERRCODE='42501';END IF;
 IF a.action_role IS NOT NULL AND a.action_role<>session_user THEN RAISE EXCEPTION 'Post merge action role denied' USING ERRCODE='42501';END IF;
 PERFORM 1 FROM forge_agent.post_merge_verification_grants WHERE worker_role=session_user AND(owner_id,project_alias)=(a.owner_id,a.project_alias) AND enabled FOR SHARE;
 -- A prepared action retains only its own frozen role's cleanup/recovery
 -- capability after revocation. New starts/checkpoint/pass remain forbidden.
 IF NOT FOUND AND NOT(a.request IS NOT NULL AND a.action_role=session_user) THEN RAISE EXCEPTION 'Post merge worker denied' USING ERRCODE='42501';END IF;
 RETURN a;
END;$fn$;
CREATE FUNCTION forge_coding_reconciler_api.post_merge_available(p_owner text) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE signature text;relation text;
BEGIN
 IF p_owner IS DISTINCT FROM current_setting('forge.coding_owner',true) THEN RETURN false;END IF;
 FOREACH signature IN ARRAY ARRAY['forge_coding_reconciler_api.post_merge_claim(text)',
 'forge_coding_reconciler_api.post_merge_renew(uuid,integer,uuid)',
 'forge_coding_reconciler_api.post_merge_checkpoint(uuid,integer,uuid,jsonb)',
 'forge_coding_reconciler_api.post_merge_finish(uuid,integer,uuid,text,jsonb,text)',
 'forge_coding_reconciler_api.post_merge_start_allowed(uuid,integer,uuid)'] LOOP
  IF to_regprocedure(signature) IS NULL OR NOT has_function_privilege(session_user,signature,'EXECUTE') THEN RETURN false;END IF;
 END LOOP;
 FOREACH relation IN ARRAY ARRAY['post_merge_verification_grants','post_merge_verifications','post_merge_remediations'] LOOP
  IF to_regclass('forge_agent.'||relation) IS NULL OR has_table_privilege(session_user,'forge_agent.'||relation,'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER')
  OR has_any_column_privilege(session_user,'forge_agent.'||relation,'SELECT,INSERT,UPDATE,REFERENCES') THEN RETURN false;END IF;
 END LOOP;
 RETURN EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=28 AND name='post_merge_verification')
 AND(EXISTS(SELECT FROM forge_agent.post_merge_verification_grants WHERE worker_role=session_user AND owner_id=p_owner AND enabled)
 OR EXISTS(SELECT FROM forge_agent.post_merge_verifications WHERE owner_id=p_owner AND action_role=session_user AND request IS NOT NULL AND status IN('pending','leased')));
END;$fn$;
CREATE FUNCTION forge_coding_reconciler_api.post_merge_claim(p_worker text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.post_merge_verifications%ROWTYPE;
BEGIN
 IF p_worker IS NULL OR p_worker!~'^[A-Za-z0-9_.-]{1,64}$' THEN RAISE EXCEPTION 'Invalid post merge worker' USING ERRCODE='22023';END IF;
 SELECT x.* INTO a FROM forge_agent.post_merge_verifications x
 WHERE x.owner_id=current_setting('forge.coding_owner',true) AND(x.status='pending' OR(x.status='leased' AND x.lease_until<=clock_timestamp()))
 AND x.next_poll_at<=clock_timestamp() AND x.generation<100000
 AND(x.action_role IS NULL OR x.action_role=session_user)
 AND(EXISTS(SELECT FROM forge_agent.post_merge_verification_grants g WHERE g.worker_role=session_user AND(g.owner_id,g.project_alias)=(x.owner_id,x.project_alias) AND g.enabled)
 OR(x.request IS NOT NULL AND x.action_role=session_user))
 ORDER BY x.created_at,x.post_merge_id FOR UPDATE OF x SKIP LOCKED LIMIT 1;
 IF NOT FOUND THEN RETURN NULL;END IF;
 PERFORM forge_agent.post_merge_lock(a.post_merge_id);
 UPDATE forge_agent.post_merge_verifications SET status='leased',generation=generation+1,worker_id=p_worker,action_role=coalesce(action_role,session_user),
 cancel_requested=cancel_requested OR NOT EXISTS(SELECT FROM forge_agent.post_merge_verification_grants g WHERE g.worker_role=session_user AND(g.owner_id,g.project_alias)=(a.owner_id,a.project_alias) AND g.enabled),
 lease_token=gen_random_uuid(),lease_role=session_user,lease_until=clock_timestamp()+interval '90 seconds',updated_at=clock_timestamp()
 WHERE post_merge_id=a.post_merge_id RETURNING * INTO a;
 RETURN to_jsonb(a)||jsonb_build_object('deadline_expired',a.deadline_at<=clock_timestamp());
END;$fn$;
CREATE FUNCTION forge_coding_reconciler_api.post_merge_renew(p_id uuid,p_generation integer,p_token uuid) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.post_merge_verifications%ROWTYPE;
BEGIN
 a:=forge_agent.post_merge_lock(p_id);
 IF a.status<>'leased' OR a.generation IS DISTINCT FROM p_generation OR a.lease_token IS DISTINCT FROM p_token OR a.lease_role<>session_user OR a.lease_until<=clock_timestamp() THEN RAISE EXCEPTION 'Invalid post merge lease' USING ERRCODE='P0008';END IF;
 UPDATE forge_agent.post_merge_verifications SET lease_until=clock_timestamp()+interval '90 seconds',updated_at=clock_timestamp() WHERE post_merge_id=p_id;RETURN true;
END;$fn$;
CREATE FUNCTION forge_coding_reconciler_api.post_merge_checkpoint(p_id uuid,p_generation integer,p_token uuid,p_request jsonb) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.post_merge_verifications%ROWTYPE;keys text[]:=ARRAY['action_id','prepare_action_id','workflow_id','project_alias','candidate_commit','candidate_tree','integration_base','gate_manifest_sha256','branch_scope','source_branch','verification_profile','correlation_id'];
BEGIN
 a:=forge_agent.post_merge_lock(p_id);
 PERFORM forge_coding_reconciler_api.post_merge_renew(p_id,p_generation,p_token);
 IF NOT forge_coding_reconciler_api.post_merge_start_allowed(p_id,p_generation,p_token) THEN RAISE EXCEPTION 'Post merge start permission elapsed' USING ERRCODE='P0008';END IF;
 IF jsonb_typeof(p_request) IS DISTINCT FROM 'object' OR p_request-keys<>'{}'::jsonb OR NOT p_request?&keys
 OR octet_length(p_request::text)>65536 OR EXISTS(SELECT FROM jsonb_each(p_request) x WHERE jsonb_typeof(x.value)<>'string')
 OR p_request->>'action_id' IS DISTINCT FROM a.post_merge_id::text OR p_request->>'workflow_id' IS DISTINCT FROM a.workflow_id::text
 OR p_request->>'project_alias' IS DISTINCT FROM a.project_alias OR p_request->>'correlation_id' IS DISTINCT FROM a.correlation_id::text
 OR p_request->>'candidate_commit' IS DISTINCT FROM a.merge_commit OR p_request->>'integration_base' IS DISTINCT FROM a.merge_commit
 OR p_request->>'candidate_tree' IS DISTINCT FROM a.merge_tree OR p_request->>'branch_scope' IS DISTINCT FROM 'main'
 OR p_request->>'source_branch' IS DISTINCT FROM 'main' OR p_request->>'verification_profile' IS DISTINCT FROM 'forge-sdlc-jenkins-v2'
 OR p_request->>'gate_manifest_sha256'!~'^[0-9a-f]{64}$' OR p_request->>'prepare_action_id'!~'^[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'
 THEN RAISE EXCEPTION 'Invalid exact main verification request' USING ERRCODE='22023';END IF;
 IF a.request IS NOT NULL AND a.request IS DISTINCT FROM p_request THEN RAISE EXCEPTION 'Post merge request conflict' USING ERRCODE='P0009';END IF;
 UPDATE forge_agent.post_merge_verifications SET request=p_request,updated_at=clock_timestamp() WHERE post_merge_id=p_id;RETURN true;
END;$fn$;
CREATE FUNCTION forge_coding_reconciler_api.post_merge_finish(p_id uuid,p_generation integer,p_token uuid,p_status text,p_receipt jsonb,p_summary text) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.post_merge_verifications%ROWTYPE;value jsonb;identity jsonb;own_verification boolean;own_cancel boolean;
BEGIN
 a:=forge_agent.post_merge_lock(p_id);value:=jsonb_build_object('status',p_status,'receipt',p_receipt,'summary',p_summary);
 IF a.finish_token=p_token AND a.finish_generation=p_generation AND a.finish_role=session_user THEN
  IF a.finish_value IS DISTINCT FROM value THEN RAISE EXCEPTION 'Post merge outcome conflict' USING ERRCODE='P0009';END IF;RETURN true;END IF;
 PERFORM forge_coding_reconciler_api.post_merge_renew(p_id,p_generation,p_token);
 IF p_status IS NULL OR p_status NOT IN('pending','passed','failed','superseded') OR jsonb_typeof(p_receipt) IS DISTINCT FROM 'object'
 OR octet_length(p_receipt::text)>262144 OR p_summary IS NULL OR octet_length(p_summary)>4096
 THEN RAISE EXCEPTION 'Invalid post merge result' USING ERRCODE='22023';END IF;
 IF p_status<>'pending' AND p_receipt->'cleanup_confirmed' IS DISTINCT FROM 'true'::jsonb AND p_receipt->'process_started' IS DISTINCT FROM 'false'::jsonb THEN
  RAISE EXCEPTION 'Post merge cleanup unconfirmed' USING ERRCODE='P0009';END IF;
 identity:=(a.request-ARRAY['prepare_action_id','integration_base','source_branch'])||jsonb_build_object('version',2,'expected_main',a.merge_commit);
 own_verification:=a.request IS NOT NULL AND p_receipt@>identity AND p_receipt->>'provider'='jenkins'
 AND p_receipt->'cleanup_confirmed'='true'::jsonb AND p_receipt->>'check_receipt_sha256'~'^[0-9a-f]{64}$';
 own_cancel:=a.request IS NOT NULL AND p_receipt->>'provider'='jenkins' AND p_receipt->>'target_action_id'=a.post_merge_id::text
 AND(p_receipt->'process_started'='false'::jsonb OR(p_receipt->'cleanup_confirmed'='true'::jsonb AND p_receipt->>'check_receipt_sha256'~'^[0-9a-f]{64}$'));
 IF p_status IN('failed','superseded') AND a.request IS NOT NULL AND own_verification IS DISTINCT FROM true AND own_cancel IS DISTINCT FROM true THEN
  RAISE EXCEPTION 'Foreign or unproven post merge cleanup' USING ERRCODE='P0009';END IF;
 IF p_status='passed' THEN
  IF a.cancel_requested THEN RAISE EXCEPTION 'Cancelled post merge action cannot pass' USING ERRCODE='P0009';END IF;
  IF NOT EXISTS(SELECT FROM forge_agent.post_merge_verification_grants WHERE worker_role=session_user AND(owner_id,project_alias)=(a.owner_id,a.project_alias) AND enabled) THEN RAISE EXCEPTION 'Post merge pass grant revoked' USING ERRCODE='42501';END IF;
  identity:=a.request-ARRAY['prepare_action_id','integration_base','source_branch'];
  identity:=identity||jsonb_build_object('version',2,'expected_main',a.merge_commit);
  IF a.request IS NULL OR NOT p_receipt@>identity OR p_receipt->>'result' IS DISTINCT FROM 'SUCCESS'
  OR p_receipt->'cleanup_confirmed' IS DISTINCT FROM 'true'::jsonb OR p_receipt->'result_version' IS DISTINCT FROM '2'::jsonb
  OR p_receipt->>'provider' IS DISTINCT FROM 'jenkins' OR (p_receipt->>'bundle_sha256'~'^[0-9a-f]{64}$') IS DISTINCT FROM true
  OR (p_receipt->>'check_receipt_sha256'~'^[0-9a-f]{64}$') IS DISTINCT FROM true OR (p_receipt->>'log_sha256'~'^[0-9a-f]{64}$') IS DISTINCT FROM true
  OR p_receipt->'coverage' IS DISTINCT FROM '{"scope":"main","browser":"required","database":"required_zero_skips","backend_suites":"all_four"}'::jsonb
  OR p_receipt->'checks' IS DISTINCT FROM '[{"index":0,"argv":["/usr/bin/make","ci-verified"],"returncode":0},{"index":1,"argv":["/usr/bin/make","ui-qa"],"returncode":0},{"index":2,"argv":["/usr/bin/python3","-B","deploy/mission-control/backend-suites.py","--scope","main"],"returncode":0}]'::jsonb
  THEN RAISE EXCEPTION 'Unverified main result' USING ERRCODE='P0009';END IF;
 END IF;
 UPDATE forge_agent.post_merge_verifications SET status=p_status,receipt=p_receipt,summary=p_summary,
 cancel_requested=cancel_requested OR(p_receipt->'cancellation_required'='true'::jsonb) IS TRUE,
 next_poll_at=CASE WHEN p_status='pending' THEN clock_timestamp()+interval '5 seconds' ELSE next_poll_at END,
 lease_token=NULL,lease_role=NULL,lease_until=NULL,finish_token=p_token,finish_generation=p_generation,finish_role=session_user,finish_value=value,updated_at=clock_timestamp() WHERE post_merge_id=p_id;
 IF p_status='failed' THEN
  INSERT INTO forge_agent.post_merge_remediations(post_merge_id,owner_id,project_alias,workflow_id,root_task_id,merge_commit,summary,receipt)
  VALUES(a.post_merge_id,a.owner_id,a.project_alias,a.workflow_id,a.root_task_id,a.merge_commit,p_summary,p_receipt);
 END IF;
 PERFORM pg_notify('forge_coding_jobs','');RETURN true;
END;$fn$;
CREATE FUNCTION forge_coding_reconciler_api.post_merge_start_allowed(p_id uuid,p_generation integer,p_token uuid) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.post_merge_verifications%ROWTYPE;
BEGIN
 a:=forge_agent.post_merge_lock(p_id);
 PERFORM forge_coding_reconciler_api.post_merge_renew(p_id,p_generation,p_token);
 RETURN a.deadline_at>clock_timestamp() AND NOT a.cancel_requested AND EXISTS(SELECT FROM forge_agent.post_merge_verification_grants WHERE worker_role=session_user AND(owner_id,project_alias)=(a.owner_id,a.project_alias) AND enabled);
END;$fn$;
REVOKE ALL ON FUNCTION forge_agent.post_merge_native_signal(),forge_agent.post_merge_external_signal(),forge_agent.post_merge_lock(uuid),
 forge_coding_reconciler_api.post_merge_available(text),forge_coding_reconciler_api.post_merge_claim(text),
 forge_coding_reconciler_api.post_merge_renew(uuid,integer,uuid),forge_coding_reconciler_api.post_merge_checkpoint(uuid,integer,uuid,jsonb),
 forge_coding_reconciler_api.post_merge_finish(uuid,integer,uuid,text,jsonb,text),
 forge_coding_reconciler_api.post_merge_start_allowed(uuid,integer,uuid) FROM PUBLIC;
