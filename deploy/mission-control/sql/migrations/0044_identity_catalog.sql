-- M44 (phase 0d step 3): identity catalog, dual-written beside legacy digests.
--
-- Policies and recipes get a catalog UUID plus a per-subject integer
-- generation, assigned the first time a document is frozen. Equality is
-- id (+ generation), never a re-hash: a document matches its catalog row by
-- jsonb equality. The legacy 64-hex digest is recorded alongside so that
-- readers can accept either identity during the dual-write window.
--
-- Additive and safe to apply after M43: two new tables and one function.
-- No existing table, function or digest column changes.

CREATE TABLE forge_agent.coding_identity_catalog(
 identity_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
 owner_id text NOT NULL CHECK(owner_id<>'' AND octet_length(owner_id)<=128),
 project_alias text NOT NULL CHECK(project_alias~'^[A-Za-z0-9_.-]{1,64}$'),
 kind text NOT NULL CHECK(kind IN('workflow_policy','external_workflow_policy','recipe')),
 subject text NOT NULL CHECK(subject~'^[A-Za-z0-9_.:-]{1,160}$'),
 generation integer NOT NULL CHECK(generation>0),
 legacy_digest text CHECK(legacy_digest IS NULL OR legacy_digest~'^[0-9a-f]{64}$'),
 document jsonb NOT NULL CHECK(jsonb_typeof(document)='object'),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 UNIQUE(owner_id,project_alias,kind,subject,generation)
);
CREATE INDEX coding_identity_catalog_legacy_digest ON forge_agent.coding_identity_catalog(owner_id,project_alias,kind,legacy_digest);

-- Identity bindings for rows that today carry only digests. One row per
-- task, written in the same transaction as the task (dual-write).
CREATE TABLE forge_agent.coding_task_identities(
 owner_id text NOT NULL,
 project_alias text NOT NULL,
 task_id uuid NOT NULL,
 recipe_id uuid NOT NULL REFERENCES forge_agent.coding_identity_catalog(identity_id),
 workflow_id uuid REFERENCES forge_agent.coding_workflows(workflow_id),
 workflow_policy_id uuid REFERENCES forge_agent.coding_identity_catalog(identity_id),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 PRIMARY KEY(owner_id,project_alias,task_id),
 FOREIGN KEY(owner_id,project_alias,task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id),
 CHECK((workflow_id IS NULL)=(workflow_policy_id IS NULL))
);

-- Freeze a document: return the latest catalog row for the subject when its
-- document is jsonb-equal, else insert the next generation. Serialized per
-- subject; never hashes anything.
CREATE FUNCTION forge_agent.coding_identity_register(p_owner text,p_project text,p_kind text,p_subject text,p_document jsonb,p_legacy_digest text)
 RETURNS TABLE(identity_id uuid,generation integer) LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $fn$
DECLARE latest forge_agent.coding_identity_catalog%ROWTYPE;
BEGIN
 PERFORM pg_advisory_xact_lock(20261126,hashtext(p_owner||'/'||p_project||'/'||p_kind||'/'||p_subject));
 SELECT * INTO latest FROM forge_agent.coding_identity_catalog c
  WHERE (c.owner_id,c.project_alias,c.kind,c.subject)=(p_owner,p_project,p_kind,p_subject)
  ORDER BY c.generation DESC LIMIT 1;
 IF FOUND AND latest.document=p_document AND latest.legacy_digest IS NOT DISTINCT FROM p_legacy_digest THEN
  identity_id:=latest.identity_id;generation:=latest.generation;RETURN NEXT;RETURN;
 END IF;
 INSERT INTO forge_agent.coding_identity_catalog(owner_id,project_alias,kind,subject,generation,legacy_digest,document)
  VALUES(p_owner,p_project,p_kind,p_subject,coalesce(latest.generation,0)+1,p_legacy_digest,p_document)
  RETURNING coding_identity_catalog.identity_id,coding_identity_catalog.generation INTO identity_id,generation;
 RETURN NEXT;
END;
$fn$;
REVOKE ALL ON FUNCTION forge_agent.coding_identity_register(text,text,text,text,jsonb,text) FROM PUBLIC;
REVOKE ALL ON forge_agent.coding_identity_catalog,forge_agent.coding_task_identities FROM PUBLIC;
-- The optional Muse reader gets future tables through a default ACL; the
-- identity catalog is not part of its exact read surface.
DO $muse$
BEGIN
 IF to_regrole('muse_events') IS NOT NULL THEN
  REVOKE ALL ON forge_agent.coding_identity_catalog,forge_agent.coding_task_identities FROM muse_events;
 END IF;
END;
$muse$;
