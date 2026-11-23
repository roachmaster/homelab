-- M29: independently observable main verification, bounded normal admission,
-- source observation and guarded releases. M1--28 and their authority stay intact.
CREATE TABLE forge_agent.delivery_projects(
 owner_id text NOT NULL,project_alias text NOT NULL,repository text NOT NULL CHECK(repository~'^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$'),
 PRIMARY KEY(owner_id,project_alias)
);
CREATE TABLE forge_agent.delivery_capability_grants(
 worker_role name NOT NULL,owner_id text NOT NULL,project_alias text NOT NULL,
 capability text NOT NULL CHECK(capability IN('observe','repair','release')),enabled boolean NOT NULL DEFAULT false,
 PRIMARY KEY(worker_role,owner_id,project_alias,capability),
 FOREIGN KEY(owner_id,project_alias) REFERENCES forge_agent.delivery_projects(owner_id,project_alias)
);
CREATE TABLE forge_agent.delivery_lineages(
 lineage_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),owner_id text NOT NULL,project_alias text NOT NULL,
 rounds integer NOT NULL DEFAULT 0 CHECK(rounds BETWEEN 0 AND 20),created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TABLE forge_agent.delivery_ledger(
 verification_id uuid PRIMARY KEY,owner_id text NOT NULL,project_alias text NOT NULL,
 source_kind text NOT NULL CHECK(source_kind IN('native','external','observed')),
 workflow_id uuid,root_task_id uuid,transport_scope_id uuid NOT NULL,correlation_id uuid NOT NULL,
 commit_sha text NOT NULL CHECK(commit_sha~'^[0-9a-f]{40}$'),tree_sha text NOT NULL CHECK(tree_sha~'^[0-9a-f]{40}$'),
 lineage_id uuid NOT NULL REFERENCES forge_agent.delivery_lineages,
 verification_status text NOT NULL DEFAULT 'pending' CHECK(verification_status IN('pending','leased','passed','failed','superseded')),
 superseded_by uuid REFERENCES forge_agent.delivery_ledger(verification_id),
 verification_receipt jsonb,verification_summary text NOT NULL DEFAULT '',
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 UNIQUE(owner_id,project_alias,commit_sha),
 CHECK((source_kind='observed' AND workflow_id IS NULL AND root_task_id IS NULL) OR(source_kind<>'observed' AND workflow_id IS NOT NULL AND root_task_id IS NOT NULL)),
 FOREIGN KEY(owner_id,project_alias,root_task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id)
);
CREATE TABLE forge_agent.delivery_post_merge_links(
 post_merge_id uuid PRIMARY KEY REFERENCES forge_agent.post_merge_verifications,
 verification_id uuid NOT NULL REFERENCES forge_agent.delivery_ledger,
 owner_id text NOT NULL,project_alias text NOT NULL,root_task_id uuid NOT NULL,
 FOREIGN KEY(owner_id,project_alias,root_task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id)
);
CREATE TABLE forge_agent.delivery_task_lineages(
 task_id uuid PRIMARY KEY,owner_id text NOT NULL,project_alias text NOT NULL,lineage_id uuid NOT NULL REFERENCES forge_agent.delivery_lineages,
 round integer NOT NULL CHECK(round BETWEEN 0 AND 20),
 FOREIGN KEY(owner_id,project_alias,task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id)
);
CREATE TABLE forge_agent.delivery_project_main_heads(
 owner_id text NOT NULL,project_alias text NOT NULL,repository text NOT NULL,
 commit_sha text NOT NULL CHECK(commit_sha~'^[0-9a-f]{40}$'),tree_sha text NOT NULL CHECK(tree_sha~'^[0-9a-f]{40}$'),
 verification_id uuid NOT NULL REFERENCES forge_agent.delivery_ledger,generation bigint NOT NULL DEFAULT 1,
 observed_at timestamptz NOT NULL DEFAULT clock_timestamp(),next_poll_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(owner_id,project_alias),FOREIGN KEY(owner_id,project_alias) REFERENCES forge_agent.delivery_projects(owner_id,project_alias)
);
CREATE TABLE forge_agent.delivery_operations(
 operation_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),verification_id uuid NOT NULL REFERENCES forge_agent.delivery_ledger,
 owner_id text NOT NULL,project_alias text NOT NULL,kind text NOT NULL CHECK(kind IN('verify','repair','release')),
 status text NOT NULL DEFAULT 'queued' CHECK(status IN('queued','leased','passed','failed','superseded','admitted','needs_attention','deployed')),
 generation integer NOT NULL DEFAULT 0,worker_id text,action_role name,lease_role name,lease_token uuid,lease_until timestamptz,
 request jsonb,receipt jsonb,summary text NOT NULL DEFAULT '' CHECK(octet_length(summary)<=4096),
 cancel_requested boolean NOT NULL DEFAULT false,actor_started boolean NOT NULL DEFAULT false,next_poll_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 deadline_at timestamptz NOT NULL DEFAULT(clock_timestamp()+interval '4 hours'),
 occurrence bigint NOT NULL DEFAULT 0,round integer NOT NULL DEFAULT 0 CHECK(round BETWEEN 0 AND 20),task_id uuid,
 finish_token uuid,finish_generation integer,finish_role name,finish_value jsonb,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 UNIQUE(verification_id,kind,occurrence),
 CHECK((status='leased' AND lease_token IS NOT NULL AND lease_role IS NOT NULL AND lease_until IS NOT NULL)
 OR(status<>'leased' AND lease_token IS NULL AND lease_role IS NULL AND lease_until IS NULL)),
 CHECK(request IS NULL OR(jsonb_typeof(request)='object' AND octet_length(request::text)<=65536)),
 CHECK(receipt IS NULL OR(jsonb_typeof(receipt)='object' AND octet_length(receipt::text)<=262144)),
 FOREIGN KEY(owner_id,project_alias,task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id)
);
CREATE INDEX delivery_operations_pending ON forge_agent.delivery_operations(owner_id,kind,status,next_poll_at,created_at);
REVOKE ALL ON forge_agent.delivery_projects,forge_agent.delivery_capability_grants,forge_agent.delivery_lineages,
 forge_agent.delivery_ledger,forge_agent.delivery_post_merge_links,forge_agent.delivery_task_lineages,
 forge_agent.delivery_project_main_heads,forge_agent.delivery_operations FROM PUBLIC;

CREATE FUNCTION forge_agent.delivery_canonical(p_value jsonb) RETURNS text LANGUAGE plpgsql IMMUTABLE SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE result text;
BEGIN
 CASE jsonb_typeof(p_value)
 WHEN 'object' THEN SELECT '{'||coalesce(string_agg(to_jsonb(k)::text||':'||forge_agent.delivery_canonical(v),',' ORDER BY k COLLATE "C"),'')||'}' INTO result FROM jsonb_each(p_value) e(k,v);
 WHEN 'array' THEN SELECT '['||coalesce(string_agg(forge_agent.delivery_canonical(v),',' ORDER BY n),'')||']' INTO result FROM jsonb_array_elements(p_value) WITH ORDINALITY e(v,n);
 ELSE result:=p_value::text;END CASE;RETURN result;
END;$fn$;
CREATE FUNCTION forge_agent.delivery_capable(p_project text,p_capability text) RETURNS boolean LANGUAGE sql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
 SELECT EXISTS(SELECT FROM forge_agent.delivery_capability_grants g WHERE g.worker_role=session_user AND g.owner_id=current_setting('forge.coding_owner',true) AND g.project_alias=p_project AND g.capability=p_capability AND g.enabled)
$fn$;
CREATE FUNCTION forge_agent.delivery_verify_granted(p_project text) RETURNS boolean LANGUAGE sql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
 SELECT EXISTS(SELECT FROM forge_agent.post_merge_verification_grants g WHERE g.worker_role=session_user AND g.owner_id=current_setting('forge.coding_owner',true) AND g.project_alias=p_project AND g.enabled)
$fn$;
CREATE FUNCTION forge_agent.delivery_enqueue_successors(p_verification uuid) RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE d forge_agent.delivery_ledger%ROWTYPE;head forge_agent.delivery_project_main_heads%ROWTYPE;
BEGIN
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=p_verification;
 SELECT * INTO head FROM forge_agent.delivery_project_main_heads WHERE(owner_id,project_alias)=(d.owner_id,d.project_alias);
 IF head.commit_sha IS NOT NULL AND head.commit_sha<>d.commit_sha THEN RETURN;END IF;
 IF d.verification_status='failed' THEN
  INSERT INTO forge_agent.delivery_operations(verification_id,owner_id,project_alias,kind,occurrence) VALUES(d.verification_id,d.owner_id,d.project_alias,'repair',coalesce(head.generation,0)) ON CONFLICT(verification_id,kind,occurrence) DO NOTHING;
 ELSIF d.verification_status='passed' THEN
  INSERT INTO forge_agent.delivery_operations(verification_id,owner_id,project_alias,kind,occurrence) VALUES(d.verification_id,d.owner_id,d.project_alias,'release',coalesce(head.generation,0)) ON CONFLICT(verification_id,kind,occurrence) DO NOTHING;
 END IF;
 PERFORM pg_notify('forge_coding_jobs','');
END;$fn$;
CREATE FUNCTION forge_agent.delivery_register_post_merge(p_id uuid) RETURNS uuid LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE r forge_agent.post_merge_verifications%ROWTYPE;d forge_agent.delivery_ledger%ROWTYPE;lineage uuid;repo text;prior text;previous uuid;changed integer;
BEGIN
 SELECT * INTO r FROM forge_agent.post_merge_verifications WHERE post_merge_id=p_id;
 IF NOT FOUND THEN RAISE EXCEPTION 'Unknown confirmed delivery' USING ERRCODE='P0002';END IF;
 PERFORM pg_advisory_xact_lock(hashtextextended(r.owner_id||E'\n'||r.project_alias,2026111901));
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE(owner_id,project_alias,commit_sha)=(r.owner_id,r.project_alias,r.merge_commit) FOR UPDATE;
 IF NOT FOUND THEN
  SELECT lineage_id INTO lineage FROM forge_agent.delivery_task_lineages WHERE(owner_id,project_alias,task_id)=(r.owner_id,r.project_alias,r.root_task_id);
  IF lineage IS NULL THEN INSERT INTO forge_agent.delivery_lineages(owner_id,project_alias) VALUES(r.owner_id,r.project_alias) RETURNING lineage_id INTO lineage;END IF;
  INSERT INTO forge_agent.delivery_ledger(verification_id,owner_id,project_alias,source_kind,workflow_id,root_task_id,transport_scope_id,correlation_id,commit_sha,tree_sha,lineage_id)
  VALUES(r.post_merge_id,r.owner_id,r.project_alias,r.source_kind,r.workflow_id,r.root_task_id,r.workflow_id,r.correlation_id,r.merge_commit,r.merge_tree,lineage) RETURNING * INTO d;
 END IF;
 IF d.tree_sha IS DISTINCT FROM r.merge_tree THEN RAISE EXCEPTION 'Delivery commit tree conflict' USING ERRCODE='P0009';END IF;
 INSERT INTO forge_agent.delivery_post_merge_links(post_merge_id,verification_id,owner_id,project_alias,root_task_id) VALUES(r.post_merge_id,d.verification_id,r.owner_id,r.project_alias,r.root_task_id) ON CONFLICT DO NOTHING;
 INSERT INTO forge_agent.delivery_task_lineages(task_id,owner_id,project_alias,lineage_id,round) VALUES(r.root_task_id,r.owner_id,r.project_alias,d.lineage_id,0) ON CONFLICT DO NOTHING;
 IF d.verification_id=r.post_merge_id THEN
  UPDATE forge_agent.delivery_ledger SET verification_status=r.status,verification_receipt=r.receipt,verification_summary=r.summary,updated_at=r.updated_at WHERE verification_id=d.verification_id;
 END IF;
 -- A confirmed merge can advance a known head only from its exact frozen base.
 SELECT repository INTO repo FROM forge_agent.delivery_projects WHERE(owner_id,project_alias)=(r.owner_id,r.project_alias);
 IF repo IS NOT NULL THEN
  IF r.source_kind='native' THEN SELECT intent->>'expected_main' INTO prior FROM forge_agent.coding_workflow_actions WHERE action_id=r.source_action_id;
  ELSE prior:=r.merge_receipt->'receipt'->>'expected_main';END IF;
  SELECT verification_id INTO previous FROM forge_agent.delivery_project_main_heads WHERE(owner_id,project_alias,commit_sha)=(r.owner_id,r.project_alias,prior);
  INSERT INTO forge_agent.delivery_project_main_heads(owner_id,project_alias,repository,commit_sha,tree_sha,verification_id)
  VALUES(r.owner_id,r.project_alias,repo,r.merge_commit,r.merge_tree,d.verification_id)
  ON CONFLICT(owner_id,project_alias) DO UPDATE SET commit_sha=excluded.commit_sha,tree_sha=excluded.tree_sha,verification_id=excluded.verification_id,generation=forge_agent.delivery_project_main_heads.generation+1,observed_at=clock_timestamp()
  WHERE forge_agent.delivery_project_main_heads.commit_sha=prior AND forge_agent.delivery_project_main_heads.commit_sha<>excluded.commit_sha;
  GET DIAGNOSTICS changed=ROW_COUNT;
  IF changed>0 THEN
   UPDATE forge_agent.delivery_ledger SET superseded_by=d.verification_id WHERE verification_id=previous AND verification_id<>d.verification_id;
   UPDATE forge_agent.delivery_ledger SET superseded_by=NULL WHERE verification_id=d.verification_id;
  END IF;
 END IF;
 PERFORM forge_agent.delivery_enqueue_successors(d.verification_id);RETURN d.verification_id;
END;$fn$;
CREATE FUNCTION forge_agent.delivery_sync_post_merge() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN PERFORM forge_agent.delivery_register_post_merge(NEW.post_merge_id);RETURN NEW;END;$fn$;
CREATE TRIGGER delivery_post_merge_sync AFTER INSERT OR UPDATE ON forge_agent.post_merge_verifications FOR EACH ROW EXECUTE FUNCTION forge_agent.delivery_sync_post_merge();

CREATE FUNCTION forge_agent.delivery_operation_lock(p_id uuid) RETURNS forge_agent.delivery_operations LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.delivery_operations%ROWTYPE;allowed boolean;
BEGIN
 SELECT * INTO a FROM forge_agent.delivery_operations WHERE operation_id=p_id AND owner_id=current_setting('forge.coding_owner',true) FOR UPDATE;
 IF NOT FOUND OR(a.action_role IS NOT NULL AND a.action_role<>session_user) THEN RAISE EXCEPTION 'Delivery action scope denied' USING ERRCODE='42501';END IF;
 allowed:=CASE WHEN a.kind='verify' THEN forge_agent.delivery_verify_granted(a.project_alias) ELSE forge_agent.delivery_capable(a.project_alias,a.kind) END;
 IF NOT allowed AND ((a.request IS NOT NULL OR(a.kind='release' AND a.actor_started)) AND a.action_role=session_user) IS DISTINCT FROM true THEN RAISE EXCEPTION 'Delivery capability denied' USING ERRCODE='42501';END IF;
 RETURN a;
END;$fn$;
CREATE FUNCTION forge_agent.delivery_operation_renew(p_id uuid,p_generation integer,p_token uuid) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.delivery_operations%ROWTYPE;
BEGIN
 a:=forge_agent.delivery_operation_lock(p_id);
 IF a.status<>'leased' OR a.generation IS DISTINCT FROM p_generation OR a.lease_token IS DISTINCT FROM p_token OR a.lease_role<>session_user OR a.lease_until<=clock_timestamp() THEN RAISE EXCEPTION 'Delivery lease expired' USING ERRCODE='P0008';END IF;
 UPDATE forge_agent.delivery_operations SET lease_until=clock_timestamp()+interval '90 seconds',updated_at=clock_timestamp() WHERE operation_id=p_id;RETURN true;
END;$fn$;
CREATE FUNCTION forge_agent.delivery_operation_claim(p_kind text,p_worker text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.delivery_operations%ROWTYPE;d forge_agent.delivery_ledger%ROWTYPE;allowed boolean;value jsonb;
BEGIN
 IF p_kind NOT IN('verify','repair','release') OR p_worker IS NULL OR p_worker!~'^[A-Za-z0-9_.-]{1,64}$' THEN RAISE EXCEPTION 'Invalid delivery claim' USING ERRCODE='22023';END IF;
 SELECT x.* INTO a FROM forge_agent.delivery_operations x
 WHERE x.owner_id=current_setting('forge.coding_owner',true) AND x.kind=p_kind AND(x.status='queued' OR(x.status='leased' AND x.lease_until<=clock_timestamp()))
 AND x.next_poll_at<=clock_timestamp() AND(x.action_role IS NULL OR x.action_role=session_user)
 AND((CASE WHEN p_kind='verify' THEN forge_agent.delivery_verify_granted(x.project_alias) ELSE forge_agent.delivery_capable(x.project_alias,p_kind) END) OR((x.request IS NOT NULL OR(x.kind='release' AND x.actor_started)) AND x.action_role=session_user))
 ORDER BY x.created_at,x.operation_id FOR UPDATE OF x SKIP LOCKED LIMIT 1;
 IF NOT FOUND THEN RETURN NULL;END IF;
 allowed:=CASE WHEN p_kind='verify' THEN forge_agent.delivery_verify_granted(a.project_alias) ELSE forge_agent.delivery_capable(a.project_alias,p_kind) END;
 UPDATE forge_agent.delivery_operations SET status='leased',generation=generation+1,worker_id=p_worker,action_role=coalesce(action_role,session_user),lease_role=session_user,lease_token=gen_random_uuid(),lease_until=clock_timestamp()+interval '90 seconds',cancel_requested=cancel_requested OR NOT allowed,updated_at=clock_timestamp() WHERE operation_id=a.operation_id RETURNING * INTO a;
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=a.verification_id;
 value:=to_jsonb(a)||jsonb_build_object('verification_id',d.verification_id,'post_merge_id',a.operation_id,'source_kind',d.source_kind,'workflow_id',d.workflow_id,'transport_scope_id',d.transport_scope_id,'root_task_id',d.root_task_id,'merge_commit',d.commit_sha,'merge_tree',d.tree_sha,'commit_sha',d.commit_sha,'tree_sha',d.tree_sha,'correlation_id',d.correlation_id,'deadline_expired',a.deadline_at<=clock_timestamp(),'lineage_id',d.lineage_id,'verification_receipt',d.verification_receipt);
 IF p_kind='release' THEN value:=value||jsonb_build_object('release_id',a.operation_id,'ci_action_id',d.verification_receipt->>'action_id','ci_receipt_sha256',d.verification_receipt->>'check_receipt_sha256','gate_manifest_sha256',d.verification_receipt->>'gate_manifest_sha256');END IF;
 RETURN value;
END;$fn$;
CREATE FUNCTION forge_agent.delivery_project_head(p_project text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE h forge_agent.delivery_project_main_heads%ROWTYPE;repo text;
BEGIN
 SELECT * INTO h FROM forge_agent.delivery_project_main_heads WHERE owner_id=current_setting('forge.coding_owner',true) AND project_alias=p_project;
 IF FOUND THEN RETURN to_jsonb(h)||jsonb_build_object('due',h.next_poll_at<=clock_timestamp());END IF;
 SELECT repository INTO repo FROM forge_agent.delivery_projects WHERE owner_id=current_setting('forge.coding_owner',true) AND project_alias=p_project;
 IF repo IS NULL THEN RETURN NULL;END IF;
 RETURN jsonb_build_object('repository',repo,'commit_sha',NULL,'tree_sha',NULL,'verification_id',NULL,'observed_at',NULL,'next_poll_at',NULL,'due',true);
END;$fn$;
CREATE FUNCTION forge_coding_reconciler_api.delivery_project_head(p_project text) RETURNS jsonb LANGUAGE sql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
 SELECT CASE WHEN forge_agent.delivery_capable(p_project,'observe') AND forge_agent.delivery_verify_granted(p_project) THEN forge_agent.delivery_project_head(p_project) ELSE NULL END
$fn$;
CREATE FUNCTION forge_coding_reconciler_api.delivery_observe(p_project text,p_repository text,p_commit text,p_tree text,p_expected text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE owner_scope text:=current_setting('forge.coding_owner',true);h forge_agent.delivery_project_main_heads%ROWTYPE;d forge_agent.delivery_ledger%ROWTYPE;repo text;lineage uuid;id uuid;changed boolean;previous uuid;
BEGIN
 IF NOT forge_agent.delivery_capable(p_project,'observe') OR NOT forge_agent.delivery_verify_granted(p_project) THEN RAISE EXCEPTION 'Source observation denied' USING ERRCODE='42501';END IF;
 SELECT p.repository INTO repo FROM forge_agent.delivery_projects p WHERE p.owner_id=owner_scope AND p.project_alias=p_project FOR SHARE;
 IF repo IS DISTINCT FROM p_repository OR(p_commit~'^[0-9a-f]{40}$') IS DISTINCT FROM true OR(p_tree~'^[0-9a-f]{40}$') IS DISTINCT FROM true OR(p_expected IS NOT NULL AND p_expected!~'^[0-9a-f]{40}$') THEN RAISE EXCEPTION 'Invalid trusted source observation' USING ERRCODE='22023';END IF;
 PERFORM pg_advisory_xact_lock(hashtextextended(owner_scope||E'\n'||p_project,2026111901));
 SELECT * INTO h FROM forge_agent.delivery_project_main_heads p WHERE p.owner_id=owner_scope AND p.project_alias=p_project FOR UPDATE;
 IF h.commit_sha IS DISTINCT FROM p_expected AND h.commit_sha IS DISTINCT FROM p_commit THEN RAISE EXCEPTION 'Source observation compare and swap changed' USING ERRCODE='P0006';END IF;
 changed:=h.commit_sha IS DISTINCT FROM p_commit;previous:=h.verification_id;
 SELECT * INTO d FROM forge_agent.delivery_ledger p WHERE p.owner_id=owner_scope AND p.project_alias=p_project AND p.commit_sha=p_commit FOR UPDATE;
 IF NOT FOUND THEN
  id:=gen_random_uuid();INSERT INTO forge_agent.delivery_lineages(owner_id,project_alias) VALUES(owner_scope,p_project) RETURNING lineage_id INTO lineage;
  INSERT INTO forge_agent.delivery_ledger(verification_id,owner_id,project_alias,source_kind,transport_scope_id,correlation_id,commit_sha,tree_sha,lineage_id)
  VALUES(id,owner_scope,p_project,'observed',id,gen_random_uuid(),p_commit,p_tree,lineage) RETURNING * INTO d;
  INSERT INTO forge_agent.delivery_operations(operation_id,verification_id,owner_id,project_alias,kind,deadline_at) VALUES(id,id,owner_scope,p_project,'verify',clock_timestamp()+interval '90 minutes');
 ELSIF d.tree_sha IS DISTINCT FROM p_tree THEN RAISE EXCEPTION 'Observed commit tree changed' USING ERRCODE='P0009';END IF;
 INSERT INTO forge_agent.delivery_project_main_heads(owner_id,project_alias,repository,commit_sha,tree_sha,verification_id,next_poll_at)
 VALUES(owner_scope,p_project,p_repository,p_commit,p_tree,d.verification_id,clock_timestamp()+interval '60 seconds')
 ON CONFLICT(owner_id,project_alias) DO UPDATE SET commit_sha=excluded.commit_sha,tree_sha=excluded.tree_sha,verification_id=excluded.verification_id,
 generation=forge_agent.delivery_project_main_heads.generation+CASE WHEN changed THEN 1 ELSE 0 END,observed_at=clock_timestamp(),next_poll_at=clock_timestamp()+interval '60 seconds' RETURNING * INTO h;
 IF changed THEN
  UPDATE forge_agent.delivery_ledger SET superseded_by=d.verification_id WHERE verification_id=previous AND verification_id<>d.verification_id;
  UPDATE forge_agent.delivery_ledger SET superseded_by=NULL WHERE verification_id=d.verification_id;
  PERFORM forge_agent.delivery_enqueue_successors(d.verification_id);
 END IF;
 PERFORM pg_notify('forge_coding_jobs','');
 RETURN jsonb_build_object('verification_id',d.verification_id,'changed',changed,'deduplicated',id IS NULL,'commit_sha',p_commit,'tree_sha',p_tree,'next_poll_at',h.next_poll_at);
END;$fn$;
CREATE FUNCTION forge_coding_reconciler_api.delivery_available(p_owner text) RETURNS boolean LANGUAGE sql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
 SELECT p_owner=current_setting('forge.coding_owner',true) AND EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=29 AND name='delivery_automation')
 AND(forge_coding_reconciler_api.post_merge_available(p_owner) OR EXISTS(SELECT FROM forge_agent.delivery_operations WHERE owner_id=p_owner AND action_role=session_user AND(request IS NOT NULL OR(kind='release' AND actor_started)) AND status IN('queued','leased')))
$fn$;
CREATE FUNCTION forge_coding_reconciler_api.delivery_claim(p_worker text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE job jsonb;canonical uuid;iteration integer;
BEGIN
 FOR iteration IN 1..64 LOOP
  job:=forge_coding_reconciler_api.post_merge_claim(p_worker);EXIT WHEN job IS NULL;
  canonical:=forge_agent.delivery_register_post_merge((job->>'post_merge_id')::uuid);
  IF canonical::text<>job->>'post_merge_id' AND job->'request'='null'::jsonb THEN
   PERFORM forge_coding_reconciler_api.post_merge_finish((job->>'post_merge_id')::uuid,(job->>'generation')::integer,(job->>'lease_token')::uuid,'superseded',jsonb_build_object('process_started',false,'canonical_verification_id',canonical),'Exact source is covered by its canonical verification action.');
   CONTINUE;
  END IF;
  RETURN job||jsonb_build_object('verification_id',canonical,'transport_scope_id',job->>'workflow_id');
 END LOOP;
 RETURN forge_agent.delivery_operation_claim('verify',p_worker);
END;$fn$;
CREATE FUNCTION forge_coding_reconciler_api.delivery_renew(p_id uuid,p_generation integer,p_token uuid) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
BEGIN
 IF EXISTS(SELECT FROM forge_agent.post_merge_verifications WHERE post_merge_id=p_id) THEN RETURN forge_coding_reconciler_api.post_merge_renew(p_id,p_generation,p_token);END IF;
 RETURN forge_agent.delivery_operation_renew(p_id,p_generation,p_token);
END;$fn$;
CREATE FUNCTION forge_coding_reconciler_api.delivery_start_allowed(p_id uuid,p_generation integer,p_token uuid) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.delivery_operations%ROWTYPE;
BEGIN
 IF EXISTS(SELECT FROM forge_agent.post_merge_verifications WHERE post_merge_id=p_id) THEN RETURN forge_coding_reconciler_api.post_merge_start_allowed(p_id,p_generation,p_token);END IF;
 a:=forge_agent.delivery_operation_lock(p_id);PERFORM forge_agent.delivery_operation_renew(p_id,p_generation,p_token);
 RETURN a.kind='verify' AND NOT a.cancel_requested AND a.deadline_at>clock_timestamp() AND forge_agent.delivery_verify_granted(a.project_alias);
END;$fn$;
CREATE FUNCTION forge_agent.delivery_request_valid(p_request jsonb,p_id uuid,p_scope uuid,p_project text,p_commit text,p_tree text,p_correlation uuid) RETURNS boolean LANGUAGE sql IMMUTABLE SET search_path=pg_catalog,pg_temp AS $fn$
 SELECT(jsonb_typeof(p_request)='object' AND p_request-ARRAY['action_id','prepare_action_id','workflow_id','project_alias','candidate_commit','candidate_tree','integration_base','gate_manifest_sha256','branch_scope','source_branch','verification_profile','correlation_id']='{}'::jsonb
 AND p_request?&ARRAY['action_id','prepare_action_id','workflow_id','project_alias','candidate_commit','candidate_tree','integration_base','gate_manifest_sha256','branch_scope','source_branch','verification_profile','correlation_id']
 AND NOT EXISTS(SELECT FROM jsonb_each(p_request) x WHERE jsonb_typeof(x.value)<>'string')
 AND p_request@>jsonb_build_object('action_id',p_id,'workflow_id',p_scope,'project_alias',p_project,'candidate_commit',p_commit,'candidate_tree',p_tree,'integration_base',p_commit,'branch_scope','main','source_branch','main','verification_profile','forge-sdlc-jenkins-v2','correlation_id',p_correlation)
 AND p_request->>'gate_manifest_sha256'~'^[0-9a-f]{64}$' AND p_request->>'prepare_action_id'~'^[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$') IS TRUE
$fn$;
CREATE FUNCTION forge_coding_reconciler_api.delivery_checkpoint(p_id uuid,p_generation integer,p_token uuid,p_request jsonb) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.delivery_operations%ROWTYPE;d forge_agent.delivery_ledger%ROWTYPE;
BEGIN
 IF EXISTS(SELECT FROM forge_agent.post_merge_verifications WHERE post_merge_id=p_id) THEN RETURN forge_coding_reconciler_api.post_merge_checkpoint(p_id,p_generation,p_token,p_request);END IF;
 a:=forge_agent.delivery_operation_lock(p_id);
 IF NOT forge_coding_reconciler_api.delivery_start_allowed(p_id,p_generation,p_token) THEN RAISE EXCEPTION 'Delivery start denied' USING ERRCODE='P0008';END IF;
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=a.verification_id;
 IF NOT forge_agent.delivery_request_valid(p_request,p_id,d.transport_scope_id,d.project_alias,d.commit_sha,d.tree_sha,d.correlation_id) THEN RAISE EXCEPTION 'Invalid observed main request' USING ERRCODE='22023';END IF;
 IF a.request IS NOT NULL AND a.request IS DISTINCT FROM p_request THEN RAISE EXCEPTION 'Delivery request conflict' USING ERRCODE='P0009';END IF;
 UPDATE forge_agent.delivery_operations SET request=p_request,updated_at=clock_timestamp() WHERE operation_id=p_id;RETURN true;
END;$fn$;
CREATE FUNCTION forge_agent.delivery_main_receipt_valid(p_request jsonb,p_receipt jsonb,p_commit text) RETURNS boolean LANGUAGE sql IMMUTABLE SET search_path=pg_catalog,pg_temp AS $fn$
 SELECT(p_request IS NOT NULL AND jsonb_typeof(p_receipt)='object'
 AND p_receipt@>((p_request-ARRAY['prepare_action_id','integration_base','source_branch'])||jsonb_build_object('version',2,'expected_main',p_commit))
 AND p_receipt->>'provider'='jenkins' AND p_receipt->>'result'='SUCCESS' AND p_receipt->'result_version'='2'::jsonb AND p_receipt->'cleanup_confirmed'='true'::jsonb
 AND p_receipt->>'bundle_sha256'~'^[0-9a-f]{64}$' AND p_receipt->>'check_receipt_sha256'~'^[0-9a-f]{64}$' AND p_receipt->>'log_sha256'~'^[0-9a-f]{64}$'
 AND p_receipt->'coverage'='{"scope":"main","browser":"required","database":"required_zero_skips","backend_suites":"all_four"}'::jsonb
 AND p_receipt->'checks'='[{"index":0,"argv":["/usr/bin/make","ci-verified"],"returncode":0},{"index":1,"argv":["/usr/bin/make","ui-qa"],"returncode":0},{"index":2,"argv":["/usr/bin/python3","-B","deploy/mission-control/backend-suites.py","--scope","main"],"returncode":0}]'::jsonb) IS TRUE
$fn$;
CREATE FUNCTION forge_agent.delivery_own_cleanup(p_id uuid,p_request jsonb,p_receipt jsonb,p_commit text) RETURNS boolean LANGUAGE sql IMMUTABLE SET search_path=pg_catalog,pg_temp AS $fn$
 SELECT(CASE WHEN p_request IS NULL THEN p_receipt->'process_started'='false'::jsonb ELSE
 (p_receipt->>'provider'='jenkins' AND p_receipt->>'target_action_id'=p_id::text AND(p_receipt->'process_started'='false'::jsonb OR(p_receipt->'cleanup_confirmed'='true'::jsonb AND p_receipt->>'check_receipt_sha256'~'^[0-9a-f]{64}$')))
 OR(p_receipt->>'provider'='jenkins' AND p_receipt->'cleanup_confirmed'='true'::jsonb AND p_receipt->>'check_receipt_sha256'~'^[0-9a-f]{64}$'
 AND p_receipt@>((p_request-ARRAY['prepare_action_id','integration_base','source_branch'])||jsonb_build_object('version',2,'expected_main',p_commit))) END) IS TRUE
$fn$;
CREATE FUNCTION forge_agent.delivery_operation_replayed(p_id uuid,p_generation integer,p_token uuid,p_value jsonb) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.delivery_operations%ROWTYPE;
BEGIN
 a:=forge_agent.delivery_operation_lock(p_id);
 IF a.finish_token=p_token AND a.finish_generation=p_generation AND a.finish_role=session_user THEN
  IF a.finish_value IS DISTINCT FROM p_value THEN RAISE EXCEPTION 'Delivery outcome conflict' USING ERRCODE='P0009';END IF;RETURN true;END IF;
 RETURN false;
END;$fn$;
CREATE FUNCTION forge_agent.delivery_operation_finish(p_id uuid,p_generation integer,p_token uuid,p_status text,p_receipt jsonb,p_summary text) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE value jsonb:=jsonb_build_object('status',p_status,'receipt',p_receipt,'summary',p_summary);
BEGIN
 IF forge_agent.delivery_operation_replayed(p_id,p_generation,p_token,value) THEN RETURN true;END IF;
 PERFORM forge_agent.delivery_operation_renew(p_id,p_generation,p_token);
 IF p_summary IS NULL OR octet_length(p_summary)>4096 OR jsonb_typeof(p_receipt) IS DISTINCT FROM 'object' OR octet_length(p_receipt::text)>262144 THEN RAISE EXCEPTION 'Invalid delivery finish' USING ERRCODE='22023';END IF;
 UPDATE forge_agent.delivery_operations SET status=CASE WHEN p_status='pending' THEN 'queued' ELSE p_status END,receipt=p_receipt,summary=p_summary,
 cancel_requested=cancel_requested OR(p_receipt->'cancellation_required'='true'::jsonb) IS TRUE,
 lease_token=NULL,lease_role=NULL,lease_until=NULL,finish_token=p_token,finish_generation=p_generation,finish_role=session_user,finish_value=value,
 next_poll_at=CASE WHEN p_status='pending' THEN clock_timestamp()+interval '5 seconds' ELSE next_poll_at END,updated_at=clock_timestamp() WHERE operation_id=p_id;
 PERFORM pg_notify('forge_coding_jobs','');RETURN true;
END;$fn$;
CREATE FUNCTION forge_coding_reconciler_api.delivery_finish(p_id uuid,p_generation integer,p_token uuid,p_status text,p_receipt jsonb,p_summary text) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.delivery_operations%ROWTYPE;d forge_agent.delivery_ledger%ROWTYPE;
BEGIN
 IF EXISTS(SELECT FROM forge_agent.post_merge_verifications WHERE post_merge_id=p_id) THEN RETURN forge_coding_reconciler_api.post_merge_finish(p_id,p_generation,p_token,p_status,p_receipt,p_summary);END IF;
 a:=forge_agent.delivery_operation_lock(p_id);
 IF forge_agent.delivery_operation_replayed(p_id,p_generation,p_token,jsonb_build_object('status',p_status,'receipt',p_receipt,'summary',p_summary)) THEN RETURN true;END IF;
 IF a.kind<>'verify' OR p_status IS NULL OR p_status NOT IN('pending','passed','failed','superseded') THEN RAISE EXCEPTION 'Invalid verification outcome' USING ERRCODE='22023';END IF;
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=a.verification_id;
 IF p_status='passed' AND(a.cancel_requested OR NOT forge_agent.delivery_verify_granted(a.project_alias) OR NOT forge_agent.delivery_main_receipt_valid(a.request,p_receipt,d.commit_sha)) THEN RAISE EXCEPTION 'Unverified observed main result' USING ERRCODE='P0009';END IF;
 IF p_status IN('failed','superseded') AND NOT forge_agent.delivery_own_cleanup(p_id,a.request,p_receipt,d.commit_sha) THEN RAISE EXCEPTION 'Observed main cleanup unproven' USING ERRCODE='P0009';END IF;
 PERFORM forge_agent.delivery_operation_finish(p_id,p_generation,p_token,p_status,p_receipt,p_summary);
 UPDATE forge_agent.delivery_ledger SET verification_status=p_status,verification_receipt=p_receipt,verification_summary=p_summary,updated_at=clock_timestamp() WHERE verification_id=d.verification_id;
 PERFORM forge_agent.delivery_enqueue_successors(d.verification_id);RETURN true;
END;$fn$;
CREATE FUNCTION forge_coding_reconciler_api.delivery_release_claim(p_worker text) RETURNS jsonb LANGUAGE sql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
 SELECT forge_agent.delivery_operation_claim('release',p_worker)
$fn$;
CREATE FUNCTION forge_coding_reconciler_api.delivery_release_renew(p_id uuid,p_generation integer,p_token uuid) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.delivery_operations%ROWTYPE;
BEGIN a:=forge_agent.delivery_operation_lock(p_id);IF a.kind<>'release' THEN RAISE EXCEPTION 'Wrong release action' USING ERRCODE='42501';END IF;RETURN forge_agent.delivery_operation_renew(p_id,p_generation,p_token);END;$fn$;
CREATE FUNCTION forge_coding_reconciler_api.delivery_release_checkpoint(p_id uuid,p_generation integer,p_token uuid,p_plan jsonb) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.delivery_operations%ROWTYPE;d forge_agent.delivery_ledger%ROWTYPE;identity jsonb;keys text[]:=ARRAY['version','policy','release_id','verification_id','owner_id','project_alias','commit_sha','tree_sha','ci_action_id','ci_receipt_sha256','gate_manifest_sha256','artifact_manifest_sha256','plan_sha256'];
BEGIN
 a:=forge_agent.delivery_operation_lock(p_id);PERFORM forge_agent.delivery_operation_renew(p_id,p_generation,p_token);
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=a.verification_id;
 IF a.kind<>'release' OR d.verification_status<>'passed' OR((a.cancel_requested OR a.deadline_at<=clock_timestamp() OR NOT forge_agent.delivery_capable(a.project_alias,'release')) AND NOT(a.actor_started AND a.action_role=session_user)) THEN RAISE EXCEPTION 'Release start denied' USING ERRCODE='42501';END IF;
 identity:=jsonb_build_object('version',1,'policy','forge-portal-web-only-v1','release_id',p_id,'verification_id',d.verification_id,'owner_id',d.owner_id,'project_alias',d.project_alias,'commit_sha',d.commit_sha,'tree_sha',d.tree_sha,'ci_action_id',d.verification_receipt->>'action_id','ci_receipt_sha256',d.verification_receipt->>'check_receipt_sha256','gate_manifest_sha256',d.verification_receipt->>'gate_manifest_sha256');
 IF jsonb_typeof(p_plan) IS DISTINCT FROM 'object' OR p_plan-keys<>'{}'::jsonb OR NOT p_plan?&keys OR NOT p_plan@>identity
 OR(p_plan->>'artifact_manifest_sha256'~'^[0-9a-f]{64}$') IS DISTINCT FROM true
 OR p_plan->>'plan_sha256' IS DISTINCT FROM encode(sha256(convert_to(forge_agent.delivery_canonical(p_plan-'plan_sha256'),'UTF8')),'hex')
 THEN RAISE EXCEPTION 'Invalid frozen release plan' USING ERRCODE='22023';END IF;
 IF a.request IS NOT NULL AND a.request IS DISTINCT FROM p_plan THEN RAISE EXCEPTION 'Release plan conflict' USING ERRCODE='P0009';END IF;
 UPDATE forge_agent.delivery_operations SET request=p_plan,updated_at=clock_timestamp() WHERE operation_id=p_id;RETURN true;
END;$fn$;
CREATE FUNCTION forge_coding_reconciler_api.delivery_release_finish(p_id uuid,p_generation integer,p_token uuid,p_status text,p_receipt jsonb,p_summary text) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.delivery_operations%ROWTYPE;d forge_agent.delivery_ledger%ROWTYPE;h forge_agent.delivery_project_main_heads%ROWTYPE;
BEGIN
 a:=forge_agent.delivery_operation_lock(p_id);
 IF forge_agent.delivery_operation_replayed(p_id,p_generation,p_token,jsonb_build_object('status',p_status,'receipt',p_receipt,'summary',p_summary)) THEN RETURN true;END IF;
 IF a.kind<>'release' OR p_status IS NULL OR p_status NOT IN('pending','deployed','failed','superseded') THEN RAISE EXCEPTION 'Invalid release outcome' USING ERRCODE='22023';END IF;
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=a.verification_id;
 SELECT * INTO h FROM forge_agent.delivery_project_main_heads WHERE(owner_id,project_alias)=(d.owner_id,d.project_alias);
 IF p_status='deployed' AND(a.request IS NULL OR NOT a.actor_started OR NOT p_receipt@>a.request
 OR p_receipt->>'unit' IS DISTINCT FROM 'homelab-web.service' OR p_receipt->'health_confirmed' IS DISTINCT FROM 'true'::jsonb
 OR p_receipt->'cleanup_confirmed' IS DISTINCT FROM 'true'::jsonb OR p_receipt->'entry_bridge_confirmed' IS DISTINCT FROM 'true'::jsonb OR(p_receipt->>'activation_receipt_sha256'~'^[0-9a-f]{64}$') IS DISTINCT FROM true
 OR p_receipt->'rollback_confirmed' IS DISTINCT FROM 'false'::jsonb) THEN RAISE EXCEPTION 'Unproven exact source activation' USING ERRCODE='P0009';END IF;
 IF p_status IN('failed','superseded') AND a.request IS NOT NULL AND(p_receipt@>a.request AND p_receipt->>'unit'='homelab-web.service' AND p_receipt->'cleanup_confirmed'='true'::jsonb AND p_receipt->'health_confirmed'='true'::jsonb AND p_receipt->'entry_bridge_confirmed'='true'::jsonb AND p_receipt->'rollback_confirmed'='true'::jsonb AND p_receipt->>'activation_receipt_sha256'~'^[0-9a-f]{64}$') IS DISTINCT FROM true THEN RAISE EXCEPTION 'Unproven release cleanup' USING ERRCODE='P0009';END IF;
 IF p_status IN('failed','superseded') AND a.request IS NULL AND p_receipt->'process_started' IS DISTINCT FROM 'false'::jsonb AND(a.actor_started AND p_receipt@>jsonb_build_object('release_id',a.operation_id,'verification_id',d.verification_id,'owner_id',d.owner_id,'project_alias',d.project_alias,'commit_sha',d.commit_sha,'tree_sha',d.tree_sha,'ci_action_id',d.verification_receipt->>'action_id','ci_receipt_sha256',d.verification_receipt->>'check_receipt_sha256','gate_manifest_sha256',d.verification_receipt->>'gate_manifest_sha256') AND p_receipt->>'unit'='homelab-web.service' AND p_receipt->'cleanup_confirmed'='true'::jsonb AND p_receipt->'health_confirmed'='true'::jsonb AND p_receipt->'entry_bridge_confirmed'='true'::jsonb AND p_receipt->'rollback_confirmed'='true'::jsonb AND p_receipt->>'activation_receipt_sha256'~'^[0-9a-f]{64}$') IS DISTINCT FROM true THEN RAISE EXCEPTION 'Unstarted release proof required' USING ERRCODE='P0009';END IF;
 RETURN forge_agent.delivery_operation_finish(p_id,p_generation,p_token,p_status,p_receipt,p_summary);
END;$fn$;
CREATE FUNCTION forge_agent.delivery_repair_claim(p_worker text) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE job jsonb;a forge_agent.delivery_operations%ROWTYPE;d forge_agent.delivery_ledger%ROWTYPE;l forge_agent.delivery_lineages%ROWTYPE;h forge_agent.delivery_project_main_heads%ROWTYPE;existing jsonb;original jsonb;
BEGIN
 job:=forge_agent.delivery_operation_claim('repair',p_worker);IF job IS NULL THEN RETURN NULL;END IF;
 SELECT * INTO a FROM forge_agent.delivery_operations WHERE operation_id=(job->>'operation_id')::uuid;
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=a.verification_id;
 SELECT * INTO l FROM forge_agent.delivery_lineages WHERE lineage_id=d.lineage_id FOR UPDATE;
 IF a.request IS NULL AND l.rounds>=20 THEN
  UPDATE forge_agent.delivery_operations SET round=l.rounds WHERE operation_id=a.operation_id;
  PERFORM forge_agent.delivery_operation_finish(a.operation_id,a.generation,a.lease_token,'needs_attention',jsonb_build_object('reason','lineage_budget_exhausted'),'Twenty main-repair assignments were reserved; human attention is required.');RETURN NULL;END IF;
 SELECT * INTO h FROM forge_agent.delivery_project_main_heads WHERE(owner_id,project_alias)=(d.owner_id,d.project_alias);
 IF a.request IS NULL AND h.commit_sha IS NOT NULL AND h.commit_sha<>d.commit_sha THEN
  PERFORM forge_agent.delivery_operation_finish(a.operation_id,a.generation,a.lease_token,'admitted',jsonb_build_object('reason','source_superseded'),'A newer main source superseded this repair before any model admission.');RETURN NULL;END IF;
 IF a.request IS NOT NULL THEN
  SELECT e.payload INTO existing FROM forge_agent.coding_tasks t JOIN forge_agent.coding_events e ON(e.owner_id,e.project_alias,e.task_id)=(t.owner_id,t.project_alias,t.id) AND e.sequence=1
  WHERE(t.owner_id,t.project_alias,t.idempotency_key)=(a.owner_id,a.project_alias,a.request->'command'->>'idempotency_key') AND t.intent_digest=a.request->>'intent_digest';
 END IF;
 SELECT jsonb_build_object('prompt',left(prompt,1500),'acceptance_criteria',left(acceptance_criteria,512)) INTO original FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(d.owner_id,d.project_alias,d.root_task_id);
 RETURN job||jsonb_build_object('round',CASE WHEN a.round=0 THEN l.rounds+1 ELSE a.round END,'max_rounds',20,'base_revision',coalesce(h.commit_sha,d.commit_sha),'existing_receipt',existing,'original',original);
END;$fn$;
CREATE FUNCTION forge_agent.delivery_repair_renew(p_id uuid,p_generation integer,p_token uuid) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.delivery_operations%ROWTYPE;
BEGIN a:=forge_agent.delivery_operation_lock(p_id);IF a.kind<>'repair' THEN RAISE EXCEPTION 'Wrong repair action' USING ERRCODE='42501';END IF;RETURN forge_agent.delivery_operation_renew(p_id,p_generation,p_token);END;$fn$;
CREATE FUNCTION forge_agent.delivery_repair_checkpoint(p_id uuid,p_generation integer,p_token uuid,p_intent jsonb) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.delivery_operations%ROWTYPE;d forge_agent.delivery_ledger%ROWTYPE;l forge_agent.delivery_lineages%ROWTYPE;c jsonb;keys text[]:=ARRAY['project_alias','base_revision','mode','agent_profile','prompt','acceptance_criteria','idempotency_key','auto_repair','workflow_policy_digest'];
BEGIN
 a:=forge_agent.delivery_operation_lock(p_id);PERFORM forge_agent.delivery_operation_renew(p_id,p_generation,p_token);
 IF a.kind<>'repair' OR a.cancel_requested OR NOT forge_agent.delivery_capable(a.project_alias,'repair') THEN RAISE EXCEPTION 'Repair admission capability denied' USING ERRCODE='42501';END IF;
 IF a.request IS NOT NULL THEN IF a.request IS DISTINCT FROM p_intent THEN RAISE EXCEPTION 'Repair intent conflict' USING ERRCODE='P0009';END IF;RETURN true;END IF;
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=a.verification_id;
 SELECT * INTO l FROM forge_agent.delivery_lineages WHERE lineage_id=d.lineage_id FOR UPDATE;
 c:=p_intent->'command';
 IF jsonb_typeof(p_intent) IS DISTINCT FROM 'object' OR p_intent-ARRAY['command','intent_digest']<>'{}'::jsonb OR NOT p_intent?&ARRAY['command','intent_digest']
 OR jsonb_typeof(c) IS DISTINCT FROM 'object' OR c-keys<>'{}'::jsonb OR NOT c?&keys OR c->>'project_alias' IS DISTINCT FROM a.project_alias
 OR c->>'mode' IS DISTINCT FROM 'plan' OR c->'auto_repair' IS DISTINCT FROM 'false'::jsonb
 OR c->>'idempotency_key' IS DISTINCT FROM 'delivery-repair.'||p_id::text OR(c->>'workflow_policy_digest'~'^[0-9a-f]{64}$') IS DISTINCT FROM true
 OR(c->>'base_revision'~'^[0-9a-f]{40}$') IS DISTINCT FROM true
 OR jsonb_typeof(c->'prompt') IS DISTINCT FROM 'string' OR octet_length(c->>'prompt') NOT BETWEEN 1 AND 8192
 OR jsonb_typeof(c->'acceptance_criteria') IS DISTINCT FROM 'string' OR octet_length(c->>'acceptance_criteria')>2048
 OR p_intent->>'intent_digest' IS DISTINCT FROM encode(sha256(convert_to(forge_agent.delivery_canonical(c-ARRAY['idempotency_key','auto_repair']),'UTF8')),'hex')
 THEN RAISE EXCEPTION 'Invalid frozen normal repair admission' USING ERRCODE='22023';END IF;
 IF l.rounds>=20 THEN RAISE EXCEPTION 'Repair lineage budget exhausted' USING ERRCODE='P0009';END IF;
 UPDATE forge_agent.delivery_lineages SET rounds=rounds+1 WHERE lineage_id=d.lineage_id RETURNING * INTO l;
 UPDATE forge_agent.delivery_operations SET request=p_intent,round=l.rounds,updated_at=clock_timestamp() WHERE operation_id=p_id;RETURN true;
END;$fn$;
CREATE FUNCTION forge_agent.delivery_repair_finish(p_id uuid,p_generation integer,p_token uuid,p_status text,p_receipt jsonb,p_summary text) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.delivery_operations%ROWTYPE;d forge_agent.delivery_ledger%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;queued jsonb;
BEGIN
 a:=forge_agent.delivery_operation_lock(p_id);
 IF forge_agent.delivery_operation_replayed(p_id,p_generation,p_token,jsonb_build_object('status',p_status,'receipt',p_receipt,'summary',p_summary)) THEN RETURN true;END IF;
 IF a.kind<>'repair' OR p_status IS NULL OR p_status NOT IN('pending','admitted','needs_attention') THEN RAISE EXCEPTION 'Invalid repair finish' USING ERRCODE='22023';END IF;
 IF p_status='admitted' THEN
  IF a.request IS NULL OR(p_receipt->>'task_id'~'^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$') IS DISTINCT FROM true THEN RAISE EXCEPTION 'Normal admitted task proof required' USING ERRCODE='P0009';END IF;
  SELECT x.* INTO t FROM forge_agent.coding_tasks x WHERE(x.owner_id,x.project_alias,x.id)=(a.owner_id,a.project_alias,(p_receipt->>'task_id')::uuid)
  AND x.idempotency_key=a.request->'command'->>'idempotency_key' AND x.intent_digest=a.request->>'intent_digest';
  SELECT payload INTO queued FROM forge_agent.coding_events WHERE(owner_id,project_alias,task_id,sequence)=(a.owner_id,a.project_alias,t.id,1);
  IF t.id IS NULL OR queued IS DISTINCT FROM p_receipt THEN RAISE EXCEPTION 'Repair normal admission receipt mismatch' USING ERRCODE='P0009';END IF;
  SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=a.verification_id;
  INSERT INTO forge_agent.delivery_task_lineages(task_id,owner_id,project_alias,lineage_id,round) VALUES(t.id,t.owner_id,t.project_alias,d.lineage_id,a.round) ON CONFLICT(task_id) DO NOTHING;
  UPDATE forge_agent.delivery_operations SET task_id=t.id WHERE operation_id=p_id;
 END IF;
 RETURN forge_agent.delivery_operation_finish(p_id,p_generation,p_token,p_status,p_receipt,p_summary);
END;$fn$;
CREATE FUNCTION forge_agent.delivery_timestamp(p_time timestamptz) RETURNS text LANGUAGE sql IMMUTABLE SET search_path=pg_catalog,pg_temp AS $fn$
 SELECT to_char(p_time AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"')
$fn$;
CREATE FUNCTION forge_agent.delivery_projection(p_project text,p_task uuid DEFAULT NULL) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE d forge_agent.delivery_ledger%ROWTYPE;r forge_agent.delivery_operations%ROWTYPE;s forge_agent.delivery_operations%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;lineage uuid;repair_status text;release_status text;verification_status text;
BEGIN
 IF p_task IS NULL THEN
  SELECT l.* INTO d FROM forge_agent.delivery_project_main_heads h JOIN forge_agent.delivery_ledger l USING(verification_id) WHERE h.owner_id=current_setting('forge.coding_owner',true) AND h.project_alias=p_project;
  IF NOT FOUND THEN SELECT * INTO d FROM forge_agent.delivery_ledger WHERE owner_id=current_setting('forge.coding_owner',true) AND project_alias=p_project ORDER BY created_at DESC,verification_id DESC LIMIT 1;END IF;
 ELSE
  PERFORM 1 FROM forge_agent.coding_tasks WHERE owner_id=current_setting('forge.coding_owner',true) AND project_alias=p_project AND id=p_task;
  IF NOT FOUND THEN RETURN NULL;END IF;
  SELECT lineage_id INTO lineage FROM forge_agent.delivery_task_lineages WHERE owner_id=current_setting('forge.coding_owner',true) AND project_alias=p_project AND task_id=p_task;
  IF lineage IS NULL THEN RETURN NULL;END IF;
  SELECT * INTO d FROM forge_agent.delivery_ledger WHERE owner_id=current_setting('forge.coding_owner',true) AND project_alias=p_project AND lineage_id=lineage ORDER BY created_at DESC,verification_id DESC LIMIT 1;
  IF d.source_kind='observed' AND NOT EXISTS(SELECT FROM forge_agent.delivery_post_merge_links WHERE(owner_id,project_alias,root_task_id,verification_id)=(d.owner_id,d.project_alias,p_task,d.verification_id)) THEN RETURN NULL;END IF;
 END IF;
 IF d.verification_id IS NULL THEN RETURN NULL;END IF;
 SELECT * INTO r FROM forge_agent.delivery_operations WHERE verification_id=d.verification_id AND kind='repair' ORDER BY created_at DESC,operation_id DESC LIMIT 1;
 SELECT * INTO s FROM forge_agent.delivery_operations WHERE verification_id=d.verification_id AND kind='release' ORDER BY created_at DESC,operation_id DESC LIMIT 1;
 IF r.task_id IS NOT NULL THEN SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(d.owner_id,d.project_alias,r.task_id);END IF;
 repair_status:=CASE WHEN r.operation_id IS NULL THEN 'none' WHEN r.status='needs_attention' THEN 'needs_attention'
 WHEN r.status='superseded' THEN 'completed' WHEN r.status='admitted' AND r.task_id IS NULL THEN 'completed' WHEN r.status='admitted' AND t.status='completed' THEN 'completed'
 WHEN r.status='admitted' AND t.status='queued' THEN 'queued' WHEN r.status='admitted' AND t.status IN('running','awaiting_review','cancel_requested') THEN 'running'
 WHEN r.status='admitted' THEN 'needs_attention' WHEN r.status='leased' THEN 'running' ELSE 'queued' END;
 release_status:=CASE WHEN s.receipt->'operator_required'='true'::jsonb THEN 'failed' WHEN s.operation_id IS NULL THEN 'none' WHEN s.status IN('deployed','failed','superseded') THEN s.status
 WHEN s.request IS NOT NULL THEN 'deploying' WHEN s.status='leased' THEN 'building' ELSE 'queued' END;
 verification_status:=CASE WHEN d.superseded_by IS NOT NULL THEN 'superseded' WHEN d.verification_status='leased' THEN 'running' WHEN d.verification_status='pending' AND d.verification_receipt->'build_number' IS NOT NULL THEN 'running' WHEN d.verification_status='pending' THEN 'queued' ELSE d.verification_status END;
 RETURN jsonb_build_object('verification_id',d.verification_id,'source_kind',d.source_kind,'commit_sha',d.commit_sha,'tree_sha',d.tree_sha,
 'updated_at',forge_agent.delivery_timestamp(greatest(d.updated_at,coalesce(r.updated_at,d.updated_at),coalesce(s.updated_at,d.updated_at),coalesce(t.updated_at,d.updated_at))),
 'verification',jsonb_strip_nulls(jsonb_build_object('status',verification_status,'updated_at',forge_agent.delivery_timestamp(d.updated_at),'summary',left(coalesce(nullif(d.verification_summary,''),'Main verification is queued.'),512),'build_number',d.verification_receipt->'build_number')),
 'repair',jsonb_strip_nulls(jsonb_build_object('status',repair_status,'round',coalesce(r.round,0),'max_rounds',20,'task_id',r.task_id,'updated_at',forge_agent.delivery_timestamp(coalesce(r.updated_at,d.updated_at)),'summary',left(coalesce(nullif(r.summary,''),'No repair assignment is required.'),512))),
 'release',jsonb_strip_nulls(jsonb_build_object('status',release_status,'release_id',s.operation_id,'receipt_sha256',s.receipt->>'activation_receipt_sha256','updated_at',forge_agent.delivery_timestamp(coalesce(s.updated_at,d.updated_at)),'summary',left(CASE WHEN s.receipt->'operator_required'='true'::jsonb THEN 'Portal recovery requires operator attention; cleanup remains unconfirmed.' ELSE coalesce(nullif(s.summary,''),'No release is queued.') END,512))));
END;$fn$;
CREATE FUNCTION forge_agent.delivery_scope(p_projection jsonb) RETURNS text LANGUAGE sql IMMUTABLE SET search_path=pg_catalog,pg_temp AS $fn$
 SELECT CASE WHEN p_projection IS NULL THEN NULL
 WHEN p_projection->'verification'->>'status' IN('queued','running') OR p_projection->'repair'->>'status' IN('queued','running') OR p_projection->'release'->>'status' IN('queued','building','deploying') THEN 'active'
 WHEN p_projection->'repair'->>'status'='needs_attention' OR p_projection->'release'->>'status'='failed' OR p_projection->'verification'->>'status'='failed' THEN 'attention' ELSE 'history' END
$fn$;
CREATE FUNCTION forge_agent.delivery_task_scope(p_project text,p_task uuid) RETURNS text LANGUAGE sql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
 SELECT forge_agent.delivery_scope(forge_agent.delivery_projection(p_project,p_task))
$fn$;
-- Preserve outstanding M28 actions on an upgrade; never backfill completed history.
DO $pending_delivery$
DECLARE row record;
BEGIN
 FOR row IN SELECT post_merge_id FROM forge_agent.post_merge_verifications WHERE status IN('pending','leased') ORDER BY(request IS NOT NULL) DESC,created_at,post_merge_id LOOP
  PERFORM forge_agent.delivery_register_post_merge(row.post_merge_id);
 END LOOP;
END;$pending_delivery$;
-- Private helpers and all public entrypoints start closed in this transaction.
DO $delivery_revoke$
DECLARE fn record;
BEGIN
 FOR fn IN SELECT p.oid::regprocedure::text signature FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname IN('forge_agent','forge_coding_reconciler_api') AND left(p.proname,9)='delivery_' LOOP
  EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC',fn.signature);
 END LOOP;
END;$delivery_revoke$;
CREATE FUNCTION forge_coding_reconciler_api.delivery_release_start_allowed(p_id uuid,p_generation integer,p_token uuid) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.delivery_operations%ROWTYPE;d forge_agent.delivery_ledger%ROWTYPE;head text;
BEGIN
 a:=forge_agent.delivery_operation_lock(p_id);PERFORM forge_agent.delivery_operation_renew(p_id,p_generation,p_token);
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=a.verification_id;
 SELECT commit_sha INTO head FROM forge_agent.delivery_project_main_heads WHERE(owner_id,project_alias)=(d.owner_id,d.project_alias);
 RETURN a.kind='release' AND NOT a.cancel_requested AND a.deadline_at>clock_timestamp() AND forge_agent.delivery_capable(a.project_alias,'release') AND head=d.commit_sha AND d.verification_status='passed';
END;$fn$;
CREATE FUNCTION forge_agent.delivery_actor_authorize(p_id uuid,p_generation integer,p_token uuid,p_start boolean) RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.delivery_operations%ROWTYPE;d forge_agent.delivery_ledger%ROWTYPE;head text;allowed boolean;identity jsonb;
BEGIN
 IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname=session_user AND rolsuper) THEN RAISE EXCEPTION 'Privileged actor database authorization denied' USING ERRCODE='42501';END IF;
 SELECT * INTO a FROM forge_agent.delivery_operations WHERE operation_id=p_id FOR UPDATE;
 IF NOT FOUND OR a.kind<>'release' OR p_start IS NULL THEN RETURN jsonb_build_object('allowed',false);END IF;
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=a.verification_id;
 identity:=jsonb_build_object('release_id',a.operation_id,'verification_id',d.verification_id,'owner_id',d.owner_id,'project_alias',d.project_alias,'commit_sha',d.commit_sha,'tree_sha',d.tree_sha,'ci_action_id',d.verification_receipt->>'action_id','ci_receipt_sha256',d.verification_receipt->>'check_receipt_sha256','gate_manifest_sha256',d.verification_receipt->>'gate_manifest_sha256');
 allowed:=a.status='leased' AND a.generation=p_generation AND a.lease_token=p_token AND a.lease_role=a.action_role AND a.lease_until>clock_timestamp();
 IF p_start THEN
  SELECT commit_sha INTO head FROM forge_agent.delivery_project_main_heads WHERE(owner_id,project_alias)=(d.owner_id,d.project_alias);
  allowed:=allowed AND a.lease_until>clock_timestamp() AND a.deadline_at>clock_timestamp() AND NOT a.cancel_requested AND head=d.commit_sha AND d.verification_status='passed'
   AND EXISTS(SELECT FROM forge_agent.delivery_capability_grants g WHERE(g.worker_role,g.owner_id,g.project_alias,g.capability)=(a.action_role,a.owner_id,a.project_alias,'release') AND g.enabled);
 ELSE
  allowed:=allowed OR(a.status IN('deployed','failed','superseded') AND a.finish_generation=p_generation AND a.finish_token=p_token AND a.finish_role=a.action_role);
 END IF;
 IF p_start AND allowed IS TRUE THEN UPDATE forge_agent.delivery_operations SET actor_started=true WHERE operation_id=p_id;END IF;
 RETURN jsonb_build_object('allowed',allowed IS TRUE,'job',identity,'checkpointed',a.request IS NOT NULL,'plan_sha256',a.request->>'plan_sha256');
END;$fn$;
REVOKE ALL ON FUNCTION forge_coding_reconciler_api.delivery_release_start_allowed(uuid,integer,uuid),forge_agent.delivery_actor_authorize(uuid,integer,uuid,boolean) FROM PUBLIC;
CREATE FUNCTION forge_agent.delivery_repair_admit(p_id uuid,p_generation integer,p_token uuid,p_digest text) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.delivery_operations%ROWTYPE;
BEGIN
 a:=forge_agent.delivery_operation_lock(p_id);PERFORM forge_agent.delivery_operation_renew(p_id,p_generation,p_token);
 PERFORM 1 FROM forge_agent.delivery_capability_grants WHERE worker_role=session_user AND owner_id=a.owner_id AND project_alias=a.project_alias AND capability='repair' AND enabled FOR SHARE;
 IF NOT FOUND OR a.kind<>'repair' OR a.request IS NULL OR a.request->>'intent_digest' IS DISTINCT FROM p_digest OR a.cancel_requested OR a.deadline_at<=clock_timestamp() THEN RAISE EXCEPTION 'Normal repair admission authorization denied' USING ERRCODE='42501';END IF;
 -- The calling normal WorkStore transaction holds this lease/grant fence through
 -- its standard policy validation and task INSERT; there is no parallel inserter.
 RETURN true;
END;$fn$;
REVOKE ALL ON FUNCTION forge_agent.delivery_repair_admit(uuid,integer,uuid,text) FROM PUBLIC;
CREATE FUNCTION forge_agent.delivery_repair_bind(p_id uuid,p_generation integer,p_token uuid,p_task uuid,p_receipt jsonb) RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE a forge_agent.delivery_operations%ROWTYPE;d forge_agent.delivery_ledger%ROWTYPE;t forge_agent.coding_tasks%ROWTYPE;queued jsonb;linked forge_agent.delivery_task_lineages%ROWTYPE;
BEGIN
 a:=forge_agent.delivery_operation_lock(p_id);PERFORM forge_agent.delivery_operation_renew(p_id,p_generation,p_token);
 IF a.kind<>'repair' OR a.request IS NULL THEN RAISE EXCEPTION 'Frozen repair intent required' USING ERRCODE='42501';END IF;
 SELECT x.* INTO t FROM forge_agent.coding_tasks x WHERE(x.owner_id,x.project_alias,x.id)=(a.owner_id,a.project_alias,p_task)
 AND x.idempotency_key=a.request->'command'->>'idempotency_key' AND x.intent_digest=a.request->>'intent_digest';
 SELECT payload INTO queued FROM forge_agent.coding_events WHERE(owner_id,project_alias,task_id,sequence)=(a.owner_id,a.project_alias,t.id,1);
 IF t.id IS NULL OR queued IS DISTINCT FROM p_receipt THEN RAISE EXCEPTION 'Normal repair receipt mismatch' USING ERRCODE='P0009';END IF;
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=a.verification_id;
 INSERT INTO forge_agent.delivery_task_lineages(task_id,owner_id,project_alias,lineage_id,round) VALUES(t.id,t.owner_id,t.project_alias,d.lineage_id,a.round) ON CONFLICT(task_id) DO NOTHING;
 SELECT * INTO linked FROM forge_agent.delivery_task_lineages WHERE task_id=t.id;
 IF(linked.owner_id,linked.project_alias,linked.lineage_id,linked.round) IS DISTINCT FROM(a.owner_id,a.project_alias,d.lineage_id,a.round) THEN RAISE EXCEPTION 'Repair lineage conflict' USING ERRCODE='P0009';END IF;
 UPDATE forge_agent.delivery_operations SET task_id=t.id WHERE operation_id=p_id;RETURN true;
END;$fn$;
REVOKE ALL ON FUNCTION forge_agent.delivery_repair_bind(uuid,integer,uuid,uuid,jsonb) FROM PUBLIC;
