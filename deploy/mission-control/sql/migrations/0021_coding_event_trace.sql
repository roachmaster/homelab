-- Trace metadata only: frozen task recipes, receipts and M20 intents are unchanged.
-- Volatile UUID defaults/backfill may rewrite these tables. Deploy with the
-- reviewed backup, maintenance window and bounded DDL timeouts.
ALTER TABLE forge_agent.coding_tasks
    ADD COLUMN correlation_id uuid NOT NULL DEFAULT pg_catalog.gen_random_uuid(),
    ADD CONSTRAINT coding_tasks_correlation_id_key UNIQUE (correlation_id);

ALTER TABLE forge_agent.coding_events
    ADD COLUMN event_id uuid NOT NULL DEFAULT pg_catalog.gen_random_uuid(),
    ADD COLUMN correlation_id uuid,
    ADD CONSTRAINT coding_events_event_id_key UNIQUE (event_id);

UPDATE forge_agent.coding_events AS event
SET correlation_id = task.correlation_id
FROM forge_agent.coding_tasks AS task
WHERE (event.owner_id, event.project_alias, event.task_id) =
      (task.owner_id, task.project_alias, task.id);

ALTER TABLE forge_agent.coding_events ALTER COLUMN correlation_id SET NOT NULL;

CREATE INDEX coding_events_by_correlation_sequence
    ON forge_agent.coding_events (correlation_id, sequence);

CREATE FUNCTION forge_agent.coding_task_trace_immutable()
RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, pg_temp AS $trace$
BEGIN
    IF NEW.correlation_id IS DISTINCT FROM OLD.correlation_id THEN
        RAISE EXCEPTION 'Immutable task correlation' USING ERRCODE = 'P0009';
    END IF;
    RETURN NEW;
END;
$trace$;
REVOKE ALL ON FUNCTION forge_agent.coding_task_trace_immutable() FROM PUBLIC;

CREATE FUNCTION forge_agent.coding_event_trace_identity()
RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, pg_temp AS $trace$
BEGIN
    IF TG_OP = 'INSERT' THEN
        SELECT task.correlation_id INTO NEW.correlation_id
        FROM forge_agent.coding_tasks AS task
        WHERE (task.owner_id, task.project_alias, task.id) =
              (NEW.owner_id, NEW.project_alias, NEW.task_id);
        IF NOT FOUND THEN
            RAISE EXCEPTION 'Event task does not exist in the supplied scope'
                USING ERRCODE = '23503', CONSTRAINT = 'coding_events_task_fk';
        END IF;
    ELSIF (NEW.owner_id, NEW.project_alias, NEW.task_id, NEW.sequence,
           NEW.event_id, NEW.correlation_id)
          IS DISTINCT FROM
          (OLD.owner_id, OLD.project_alias, OLD.task_id, OLD.sequence,
           OLD.event_id, OLD.correlation_id) THEN
        RAISE EXCEPTION 'Immutable event identity' USING ERRCODE = 'P0009';
    END IF;
    RETURN NEW;
END;
$trace$;
REVOKE ALL ON FUNCTION forge_agent.coding_event_trace_identity() FROM PUBLIC;

CREATE TRIGGER coding_tasks_trace_immutable
    BEFORE UPDATE ON forge_agent.coding_tasks
    FOR EACH ROW EXECUTE FUNCTION forge_agent.coding_task_trace_immutable();

CREATE TRIGGER coding_events_trace_identity
    BEFORE INSERT OR UPDATE ON forge_agent.coding_events
    FOR EACH ROW EXECUTE FUNCTION forge_agent.coding_event_trace_identity();

-- Validate external progress before a final artifact exists. Never expose its
-- claim token or grant private-storage access to the admitted history reader.
CREATE FUNCTION forge_external_agent_api.event_binding(
    p_owner text, p_project text, p_task uuid
)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, pg_temp AS $binding$
DECLARE
    assignment forge_external_agent.assignments%ROWTYPE;
BEGIN
    SELECT * INTO assignment FROM forge_external_agent.assignments
    WHERE (owner_id, project_alias, task_id) = (p_owner, p_project, p_task);
    IF NOT FOUND THEN RETURN NULL; END IF;
    PERFORM 1 FROM forge_external_agent.scope_grants AS scope
    WHERE scope.service_role = session_user AND scope.enabled
      AND (scope.external_role, scope.owner_id, scope.project_alias, scope.profile_alias) =
          (assignment.external_role, assignment.owner_id, assignment.project_alias,
           assignment.profile_alias)
    FOR SHARE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'External history scope denied' USING ERRCODE = '42501';
    END IF;
    RETURN jsonb_build_object('assignment_id', assignment.assignment_id,
                              'generation', assignment.generation);
END;
$binding$;
REVOKE ALL ON FUNCTION forge_external_agent_api.event_binding(text,text,uuid)
    FROM PUBLIC;
