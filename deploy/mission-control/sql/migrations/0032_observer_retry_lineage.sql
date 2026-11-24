-- M32 binds observer-first confirmations to their genuine normal repair episode.
CREATE TABLE forge_agent.delivery_lineage_handoffs(
 handoff_id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
 verification_id uuid NOT NULL REFERENCES forge_agent.delivery_ledger,
 post_merge_id uuid NOT NULL REFERENCES forge_agent.post_merge_verifications,
 owner_id text NOT NULL,project_alias text NOT NULL,root_task_id uuid NOT NULL,
 expected_lineage_id uuid NOT NULL REFERENCES forge_agent.delivery_lineages,
 provisional_lineage_id uuid NOT NULL REFERENCES forge_agent.delivery_lineages,
 reason text NOT NULL CHECK(reason IN('repair_proof_missing','provisional_lineage_used')),
 evidence jsonb NOT NULL CHECK(jsonb_typeof(evidence)='object'),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 UNIQUE(verification_id,expected_lineage_id,root_task_id),
 FOREIGN KEY(owner_id,project_alias,root_task_id) REFERENCES forge_agent.coding_tasks(owner_id,project_alias,id)
);
REVOKE ALL ON forge_agent.delivery_lineage_handoffs FROM PUBLIC;
DO $reader$ BEGIN IF EXISTS(SELECT FROM pg_roles WHERE rolname='muse_events') THEN
 REVOKE ALL ON forge_agent.delivery_lineage_handoffs FROM muse_events;
END IF;END;$reader$;
CREATE FUNCTION forge_agent.delivery_lineage_handoff_immutable() RETURNS trigger LANGUAGE plpgsql SET search_path=pg_catalog,pg_temp AS $f$
BEGIN RAISE EXCEPTION 'Delivery lineage handoff evidence is immutable' USING ERRCODE='P0006';END;$f$;
REVOKE ALL ON FUNCTION forge_agent.delivery_lineage_handoff_immutable() FROM PUBLIC;
CREATE TRIGGER delivery_lineage_handoff_immutable BEFORE UPDATE OR DELETE ON forge_agent.delivery_lineage_handoffs FOR EACH ROW EXECUTE FUNCTION forge_agent.delivery_lineage_handoff_immutable();

CREATE FUNCTION forge_agent._delivery_lineage_blocked(p_owner text,p_project text,p_lineage uuid) RETURNS boolean LANGUAGE sql STABLE SET search_path=pg_catalog,pg_temp AS $f$
 SELECT EXISTS(SELECT FROM forge_agent.delivery_lineage_handoffs WHERE owner_id=p_owner AND project_alias=p_project AND p_lineage IN(expected_lineage_id,provisional_lineage_id))
$f$;
REVOKE ALL ON FUNCTION forge_agent._delivery_lineage_blocked(text,text,uuid) FROM PUBLIC;

CREATE FUNCTION forge_agent._delivery_adopt_repair_lineage(p_post_merge uuid,p_verification uuid) RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog,pg_temp AS $f$
DECLARE r forge_agent.post_merge_verifications%ROWTYPE;d forge_agent.delivery_ledger%ROWTYPE;
 t forge_agent.coding_tasks%ROWTYPE;linked forge_agent.delivery_task_lineages%ROWTYPE;
 source_operation uuid;handoff uuid;proof boolean;unused boolean;reason text;receipt_digest text;
BEGIN
 SELECT * INTO r FROM forge_agent.post_merge_verifications WHERE post_merge_id=p_post_merge;
 SELECT * INTO d FROM forge_agent.delivery_ledger WHERE verification_id=p_verification;
 IF r.post_merge_id IS NULL OR d.verification_id IS NULL OR
 (r.owner_id,r.project_alias,r.merge_commit,r.merge_tree) IS DISTINCT FROM(d.owner_id,d.project_alias,d.commit_sha,d.tree_sha)
 THEN RAISE EXCEPTION 'Confirmed delivery lineage identity mismatch' USING ERRCODE='P0006';END IF;
 IF d.source_kind<>'observed' THEN RETURN;END IF;
 -- The caller already owns the same project advisory lock used by observation,
 -- normal admission, reservations and confirmed merge registration.
 SELECT * INTO linked FROM forge_agent.delivery_task_lineages WHERE(owner_id,project_alias,task_id)=(r.owner_id,r.project_alias,r.root_task_id);
 IF NOT FOUND OR linked.lineage_id=d.lineage_id THEN RETURN;END IF;
 SELECT * INTO t FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(r.owner_id,r.project_alias,r.root_task_id);
 SELECT operation.operation_id INTO source_operation FROM forge_agent.delivery_operations operation
 JOIN forge_agent.delivery_ledger parent ON parent.verification_id=operation.verification_id
 JOIN forge_agent.coding_events queued ON(queued.owner_id,queued.project_alias,queued.task_id,queued.sequence)=(t.owner_id,t.project_alias,t.id,1)
 WHERE(operation.owner_id,operation.project_alias,operation.task_id,operation.kind)=(t.owner_id,t.project_alias,t.id,'repair')
 AND operation.status IN('leased','admitted') AND operation.request IS NOT NULL AND operation.round=linked.round AND linked.round>0
 AND parent.lineage_id=linked.lineage_id AND(parent.owner_id,parent.project_alias)=(t.owner_id,t.project_alias)
 AND t.idempotency_key=operation.request->'command'->>'idempotency_key'
 AND t.idempotency_key='delivery-repair.'||operation.operation_id::text
 AND t.intent_digest=operation.request->>'intent_digest'
 AND operation.request->'command'->>'project_alias'=r.project_alias
 AND operation.request->'command'->>'mode'='plan'
 AND operation.request->'command'->'auto_repair'='false'::jsonb
 AND queued.event_kind='queued' AND queued.payload->>'task_id'=t.id::text
 AND(operation.receipt IS NULL AND operation.status='leased' OR operation.receipt=queued.payload)
 AND(t.workflow_id=r.workflow_id OR EXISTS(SELECT FROM forge_external_workflow.tasks x JOIN forge_external_workflow.workflows w USING(workflow_id)
  WHERE(x.owner_id,x.project_alias,x.task_id,x.workflow_id,w.root_task_id)=(r.owner_id,r.project_alias,t.id,r.workflow_id,t.id)))
 ORDER BY operation.created_at LIMIT 1;
 proof:=FOUND;
 receipt_digest:=encode(sha256(convert_to(forge_agent.delivery_canonical(d.verification_receipt),'UTF8')),'hex');
 SELECT l.rounds=0
 AND NOT EXISTS(SELECT FROM forge_agent.coding_retry_events e WHERE(e.owner_id,e.project_alias,e.scope_kind,e.scope_id)=(d.owner_id,d.project_alias,'delivery',d.lineage_id))
 AND NOT EXISTS(SELECT FROM forge_agent.coding_retry_event_observations o WHERE(o.owner_id,o.project_alias,o.scope_kind,o.scope_id)=(d.owner_id,d.project_alias,'delivery',d.lineage_id)
  AND NOT(d.verification_status='passed' AND o.observation_kind='passed' AND o.event_id IS NULL AND o.source_id=d.verification_id::text AND o.source_digest=receipt_digest AND o.phase_id='*'
   AND o.kind=ANY(ARRAY['compilation','tests','browser','lint','formatting','verification','infrastructure'])))
 AND NOT EXISTS(SELECT FROM forge_agent.delivery_task_lineages x WHERE x.lineage_id=d.lineage_id)
 AND NOT EXISTS(SELECT FROM forge_agent.delivery_post_merge_links x WHERE x.verification_id=d.verification_id)
 AND NOT EXISTS(SELECT FROM forge_agent.delivery_ledger x WHERE x.lineage_id=d.lineage_id AND x.verification_id<>d.verification_id)
 AND NOT EXISTS(SELECT FROM forge_agent.delivery_operations x WHERE x.verification_id=d.verification_id AND x.kind='repair' AND(x.request IS NOT NULL OR x.task_id IS NOT NULL OR x.receipt IS NOT NULL OR x.round<>0 OR x.status NOT IN('queued','superseded')))
 AND NOT forge_agent._delivery_lineage_blocked(d.owner_id,d.project_alias,d.lineage_id)
 AND NOT forge_agent._delivery_lineage_blocked(d.owner_id,d.project_alias,linked.lineage_id)
 INTO unused FROM forge_agent.delivery_lineages l WHERE(l.owner_id,l.project_alias,l.lineage_id)=(d.owner_id,d.project_alias,d.lineage_id);
 IF proof AND coalesce(unused,false) THEN
  UPDATE forge_agent.delivery_ledger SET lineage_id=linked.lineage_id WHERE verification_id=d.verification_id;
  RETURN;
 END IF;
 reason:=CASE WHEN NOT proof THEN 'repair_proof_missing' ELSE 'provisional_lineage_used' END;
 INSERT INTO forge_agent.delivery_lineage_handoffs(verification_id,post_merge_id,owner_id,project_alias,root_task_id,expected_lineage_id,provisional_lineage_id,reason,evidence)
 VALUES(d.verification_id,r.post_merge_id,d.owner_id,d.project_alias,t.id,linked.lineage_id,d.lineage_id,reason,
  jsonb_build_object('commit_sha',d.commit_sha,'tree_sha',d.tree_sha,'repair_operation_id',source_operation,'task_intent_digest',t.intent_digest,'observed_receipt_sha256',receipt_digest))
 ON CONFLICT(verification_id,expected_lineage_id,root_task_id) DO NOTHING RETURNING handoff_id INTO handoff;
 IF handoff IS NOT NULL THEN
  INSERT INTO forge_agent.delivery_operations(operation_id,verification_id,owner_id,project_alias,kind,status,occurrence,round,receipt,summary)
  SELECT handoff,d.verification_id,d.owner_id,d.project_alias,'repair','needs_attention',
   coalesce((SELECT max(occurrence)+1 FROM forge_agent.delivery_operations WHERE verification_id=d.verification_id AND kind='repair'),0),
   l.rounds,jsonb_build_object('reason','delivery_lineage_conflict','handoff_id',handoff),
   'A confirmed repair conflicts with existing observed work. Review retained lineage evidence before another repair.'
  FROM forge_agent.delivery_lineages l WHERE l.lineage_id=d.lineage_id;
 END IF;
END;$f$;
REVOKE ALL ON FUNCTION forge_agent._delivery_adopt_repair_lineage(uuid,uuid) FROM PUBLIC;
\ir ../fragments/0032_observer_retry_lineage.sql
