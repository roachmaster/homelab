-- M48 retry source key: observe/resolve deduplicate by source id. Read only.
DO $retry_source_key_verify$
BEGIN
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=48) THEN RETURN;END IF;
 IF NOT EXISTS(SELECT FROM forge_agent.schema_migrations WHERE version=48 AND name='retry_source_key') THEN RAISE EXCEPTION 'Retry source key migration identity drift';END IF;
 IF position('o.source_id,o.observation_kind)' IN (SELECT prosrc FROM pg_proc WHERE oid='forge_agent.retry_event_observe(text,text,uuid,text,text,text,text[],text,text[])'::regprocedure))=0
    OR position('inserted:=0;' IN (SELECT prosrc FROM pg_proc WHERE oid='forge_agent.retry_event_resolve(text,text,uuid,text,text,text,text)'::regprocedure))=0 THEN
  RAISE EXCEPTION 'Retry source key function drift';
 END IF;
END;
$retry_source_key_verify$;
