-- M50 (phase 0d): delivery lineage adoption binds the passed observation by
-- its source (the verification id), not by a re-hash of the receipt.
--
-- _delivery_adopt_repair_lineage() recomputed sha256(delivery_canonical(
-- verification_receipt)) and required the recorded passed observation to
-- carry the same digest. The observation is already bound by
-- source_id=verification_id (and M48 deduplicates by source), so the re-hash
-- only added a JSON-formatting false failure that handed confirmed repairs
-- off as 'provisional_lineage_used'. The digest is still recorded as
-- evidence (observed_receipt_sha256) but never compared. The live
-- definition is patched in place (owner and grants unchanged).

DO $m50$
DECLARE src text;old text:=$o$ AND o.source_id=d.verification_id::text AND o.source_digest=receipt_digest AND o.phase_id='*'$o$;
BEGIN
 src:=pg_get_functiondef('forge_agent._delivery_adopt_repair_lineage(uuid,uuid)'::regprocedure);
 IF (length(src)-length(replace(src,old,'')))/length(old)<>1 THEN
  RAISE EXCEPTION 'M50 delivery adoption patch target not unique';
 END IF;
 EXECUTE replace(src,old,$n$ AND o.source_id=d.verification_id::text AND o.phase_id='*'$n$);
END
$m50$;
