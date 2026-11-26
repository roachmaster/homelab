-- M50 delivery receipt source key: adoption binds observations by source id. Read only.
DO $delivery_receipt_source_key_verify$
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=50) THEN RETURN;END IF;
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=50 AND name='delivery_receipt_source_key') THEN RAISE EXCEPTION 'Delivery receipt source key migration identity drift';END IF;
 IF position('o.source_digest=receipt_digest' IN (SELECT prosrc FROM pg_proc WHERE oid='forge_agent._delivery_adopt_repair_lineage(uuid,uuid)'::regprocedure))<>0
    OR position('o.source_id=d.verification_id::text AND o.phase_id' IN (SELECT prosrc FROM pg_proc WHERE oid='forge_agent._delivery_adopt_repair_lineage(uuid,uuid)'::regprocedure))=0 THEN
  RAISE EXCEPTION 'Delivery receipt source key function drift';
 END IF;
END;
$delivery_receipt_source_key_verify$;
