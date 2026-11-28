//! The ExternalTx port over PgSession/Tx (F6): the
//! forge_external_workflow.* tables plus the coding_* reads
//! the external process manager interleaves, rows rendered
//! through to_jsonb in the Python dict shape. Also the
//! ExternalWorkDAO (work_external.py) over the same port and
//! the PgExternalWorkflow composition the host binds.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use forge_core::provider::Factory;

use forge_work::external_dto::{validate_external_artifact, ExternalReadiness, ExternalWorkPort};
use forge_work::external_tx::{ExternalTx, ExternalTxFactory, LockMode};
use forge_work::external_workflow::ExternalWorkflowStore;
use forge_work::retry_events::RetryEvents;
use forge_work::work_actors::Actor;
use forge_work::work_error::WorkError;

use crate::error::AdapterError;
use crate::factory::PgSessionFactory;
use crate::tx::Tx;
use crate::work_common::{actor_json, work_err};

const MARKER: &str = "forge-work: parked WorkError";

pub struct PgExternalTxFactory {
    factory: PgSessionFactory,
    owner: String,
}

impl PgExternalTxFactory {
    pub fn new(factory: PgSessionFactory, owner: String) -> Self {
        Self { factory, owner }
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }
}

impl ExternalTxFactory for PgExternalTxFactory {
    fn run(
        &self,
        read_only: bool,
        body: &mut dyn FnMut(&mut dyn ExternalTx) -> Result<Value, WorkError>,
    ) -> Result<Value, WorkError> {
        let mut session = self.factory.create().map_err(work_err)?;
        let slot: std::cell::RefCell<Option<WorkError>> = std::cell::RefCell::new(None);
        let owner = self.owner.clone();
        let out = session.in_tx(|tx| {
            if read_only {
                tx.call_void("SET TRANSACTION READ ONLY", &[])?;
            }
            // work_external_workflow._transaction's local
            // timeouts (the WorkStore's are 5s; the external
            // store sets 4s).
            tx.call_void("SET LOCAL lock_timeout='4s'", &[])?;
            tx.call_void("SET LOCAL statement_timeout='10s'", &[])?;
            let mut handle = ExternalTxImpl { tx, owner: &owner };
            match body(&mut handle) {
                Ok(value) => Ok(value),
                Err(error) => {
                    *slot.borrow_mut() = Some(error);
                    Err(AdapterError::Protocol(MARKER.to_string()))
                }
            }
        });
        match out {
            Ok(value) => Ok(value),
            Err(adapter) => match slot.into_inner() {
                Some(error) => Err(error),
                None => Err(work_err(adapter)),
            },
        }
    }
}

struct ExternalTxImpl<'x, 't, 'c, 'o> {
    tx: &'x mut Tx<'t, 'c>,
    owner: &'o str,
}

/// Enqueue through the existing external port in the caller's admission
/// transaction. The selected workflow object is private to this operation;
/// no coordinator, dispatcher or provider loop is started here.
pub(crate) fn enqueue_work_admission(
    tx: &mut Tx<'_, '_>,
    store: &crate::work_store::PgWorkStore,
    project: &str,
    task: Uuid,
    profile: &str,
) -> Result<Uuid, WorkError> {
    let config = store
        .catalog
        .as_ref()
        .ok_or_else(|| WorkError::rejected(503, "unavailable"))?;
    if !config
        .profile(profile)
        .is_some_and(|profile| profile.is_external() && profile.execution_enabled)
    {
        return Err(WorkError::rejected(503, "unavailable"));
    }
    let mut handle = ExternalTxImpl {
        tx,
        owner: &store.owner,
    };
    if let Some(workflow) =
        crate::runner_composition::build_external_store(config, store.factory.config())?
    {
        if workflow.manages(project) {
            return workflow.admit(&mut handle, project, task);
        }
    }
    handle.enqueue(project, task)
}

type Params = Vec<Box<dyn ToSql + Sync + Send>>;

fn refs(params: &Params) -> Vec<&(dyn ToSql + Sync)> {
    params
        .iter()
        .map(|param| param.as_ref() as &(dyn ToSql + Sync))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Ty {
    Uuid,
    Text,
    Bool,
    Int,
    Json,
    Ts,
}

/// The workflow adapter's column discipline, duplicated for
/// the external tables: *_id is a uuid except owner_id,
/// *_at is a timestamp, the value's JSON type decides the
/// rest. target_json is TEXT in reviews (a string value
/// reads Text anyway); created_at of plan_approve_overrides
/// binds an ISO string as timestamptz.
fn col_type(column: &str, value: &Value) -> Ty {
    if column == "owner_id" || column == "worker_id" {
        return Ty::Text;
    }
    if column == "id" || column.ends_with("_id") {
        return Ty::Uuid;
    }
    if column.ends_with("_at") {
        return Ty::Ts;
    }
    match value {
        Value::Bool(_) => Ty::Bool,
        Value::Number(_) => Ty::Int,
        Value::Object(_) | Value::Array(_) => Ty::Json,
        _ => Ty::Text,
    }
}

fn bind(params: &mut Params, ty: Ty, value: &Value) {
    match (ty, value) {
        (_, Value::Null) => match ty {
            Ty::Uuid => params.push(Box::new(Option::<Uuid>::None)),
            Ty::Bool => params.push(Box::new(Option::<bool>::None)),
            Ty::Int => params.push(Box::new(Option::<i64>::None)),
            Ty::Json => params.push(Box::new(Option::<Value>::None)),
            _ => params.push(Box::new(Option::<String>::None)),
        },
        (Ty::Uuid, Value::String(text)) => {
            params.push(Box::new(Uuid::parse_str(text).unwrap_or_default()))
        }
        (Ty::Bool, Value::Bool(flag)) => params.push(Box::new(*flag)),
        (Ty::Int, Value::Number(number)) => {
            params.push(Box::new(number.as_i64().unwrap_or_default()))
        }
        (Ty::Json, other) => params.push(Box::new(other.clone())),
        (_, Value::String(text)) => params.push(Box::new(text.clone())),
        (_, other) => params.push(Box::new(other.to_string())),
    }
}

fn placeholder(index: usize, ty: Ty) -> String {
    match ty {
        Ty::Ts => format!("${index}::timestamptz"),
        Ty::Json => format!("${index}::jsonb"),
        Ty::Uuid => format!("${index}::uuid"),
        _ => format!("${index}"),
    }
}

impl<'x, 't, 'c, 'o> ExternalTxImpl<'x, 't, 'c, 'o> {
    fn one(&mut self, sql: &str, params: &Params) -> Result<Option<Value>, WorkError> {
        self.tx.call_json(sql, &refs(params)).map_err(work_err)
    }

    fn many(&mut self, sql: &str, params: &Params) -> Result<Vec<Value>, WorkError> {
        let rows = self.tx.query_rows(sql, &refs(params)).map_err(work_err)?;
        Ok(rows
            .iter()
            .map(|row| row.get::<usize, Option<Value>>(0).unwrap_or(Value::Null))
            .collect())
    }

    fn exec(&mut self, sql: &str, params: &Params) -> Result<u64, WorkError> {
        self.tx.execute_count(sql, &refs(params)).map_err(work_err)
    }

    fn insert_sql(&self, table: &str, row: &Value) -> (String, Params) {
        let object = row.as_object().cloned().unwrap_or_default();
        let mut columns = Vec::new();
        let mut places = Vec::new();
        let mut params: Params = Vec::new();
        for (index, (column, value)) in object.iter().enumerate() {
            let ty = col_type(column, value);
            columns.push(format!("\"{column}\""));
            places.push(placeholder(index + 1, ty));
            bind(&mut params, ty, value);
        }
        (
            format!(
                "INSERT INTO {table}({}) VALUES({})",
                columns.join(","),
                places.join(",")
            ),
            params,
        )
    }

    fn insert_row(&mut self, table: &str, row: &Value) -> Result<(), WorkError> {
        let (sql, params) = self.insert_sql(table, row);
        self.exec(&sql, &params).map(|_| ())
    }

    /// The generic coding_tasks / reviews UPDATE: literal,
    /// null, {"$incr": n} and {"$now": true} values, plus
    /// updated_at=clock_timestamp() as Python's SETs carry.
    fn update_row(
        &mut self,
        table: &str,
        where_sql: &str,
        mut where_params: Params,
        set: &Value,
    ) -> Result<u64, WorkError> {
        let object = set.as_object().cloned().unwrap_or_default();
        let mut assignments = Vec::new();
        let mut params: Params = Vec::new();
        for (column, value) in object.iter() {
            if let Some(incr) = value.get("$incr").and_then(Value::as_i64) {
                params.push(Box::new(incr));
                assignments.push(format!("\"{column}\"=\"{column}\"+${}", params.len()));
            } else if value.get("$now").and_then(Value::as_bool) == Some(true) {
                assignments.push(format!("\"{column}\"=clock_timestamp()"));
            } else {
                let ty = col_type(column, value);
                bind(&mut params, ty, value);
                assignments.push(format!("\"{column}\"={}", placeholder(params.len(), ty)));
            }
        }
        assignments.push("updated_at=clock_timestamp()".to_string());
        let offset = params.len();
        let mut where_sql = where_sql.to_string();
        for index in (1..=where_params.len()).rev() {
            where_sql = where_sql.replace(&format!("${index}"), &format!("${}", index + offset));
        }
        params.append(&mut where_params);
        let sql = format!(
            "UPDATE {table} SET {} WHERE {where_sql}",
            assignments.join(",")
        );
        self.exec(&sql, &params)
    }

    fn task_row_sql(
        &mut self,
        project: &str,
        task_id: Uuid,
        for_update: bool,
    ) -> Result<Option<Value>, WorkError> {
        let lock = if for_update { " FOR UPDATE" } else { "" };
        let sql = format!(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=($1,$2,$3){lock}) t"
        );
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(project.to_string()),
            Box::new(task_id),
        ];
        self.one(&sql, &params)
    }
}

impl ExternalTx for ExternalTxImpl<'_, '_, '_, '_> {
    fn owner(&self) -> &str {
        self.owner
    }

    fn savepoint(&mut self, name: &str) -> Result<(), WorkError> {
        self.tx
            .call_void(&format!("SAVEPOINT {name}"), &[])
            .map_err(work_err)
    }

    fn rollback_to_savepoint(&mut self, name: &str) -> Result<(), WorkError> {
        self.tx
            .call_void(&format!("ROLLBACK TO SAVEPOINT {name}"), &[])
            .map_err(work_err)
    }

    fn release_savepoint(&mut self, name: &str) -> Result<(), WorkError> {
        self.tx
            .call_void(&format!("RELEASE SAVEPOINT {name}"), &[])
            .map_err(work_err)
    }

    fn notify_jobs(&mut self) -> Result<(), WorkError> {
        self.tx
            .call_void("SELECT pg_notify('forge_coding_jobs','')", &[])
            .map_err(work_err)
    }

    fn notify_delivery(&mut self) -> Result<(), WorkError> {
        self.tx
            .call_void("SELECT pg_notify('forge_external_delivery','')", &[])
            .map_err(work_err)
    }

    fn insert_event(
        &mut self,
        project: &str,
        task_id: Uuid,
        kind: &str,
        payload: Value,
        actor: &Actor,
        cause: Option<&str>,
        cause_detail: Option<&str>,
        first: bool,
    ) -> Result<(), WorkError> {
        let mut merged = payload;
        if let Some(object) = merged.as_object_mut() {
            object.insert("actor".to_string(), actor_json(actor));
            if let Some(cause) = cause {
                object.insert("cause".to_string(), Value::String(cause.to_string()));
                if let Some(detail) = cause_detail {
                    object.insert(
                        "cause_detail".to_string(),
                        Value::String(detail.to_string()),
                    );
                }
            }
        }
        crate::work_rows::insert_event_raw(
            self.tx, self.owner, project, task_id, kind, merged, first,
        )
    }

    fn lock_task_row(&mut self, project: &str, task_id: Uuid) -> Result<bool, WorkError> {
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(project.to_string()),
            Box::new(task_id),
        ];
        let rows = self.many(
            "SELECT to_jsonb(t) FROM (SELECT id FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=($1,$2,$3) FOR UPDATE) t",
            &params,
        )?;
        Ok(!rows.is_empty())
    }

    fn scope_enabled(
        &mut self,
        project: &str,
        profile: &str,
        active: bool,
    ) -> Result<bool, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&self.owner, &project, &profile, &active];
        self.tx
            .call_bool(
                "SELECT forge_external_workflow_api.service_scope($1,$2,$3,$4) AS enabled",
                params,
            )
            .map_err(work_err)
    }

    fn enqueue(&mut self, project: &str, task_id: Uuid) -> Result<Uuid, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&self.owner, &project, &task_id];
        self.tx
            .call_scalar(
                "SELECT forge_external_agent_api.enqueue($1,$2,$3) AS assignment_id",
                params,
            )
            .map_err(work_err)
            .map(|value: Option<Uuid>| value.unwrap_or_default())
    }

    fn catalogue(&mut self) -> Result<Value, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&self.owner];
        self.tx
            .call_json(
                "SELECT forge_external_agent_api.catalogue($1) AS entries",
                params,
            )
            .map_err(work_err)
            .map(|value| value.unwrap_or(Value::Null))
    }

    fn reconcile_external(&mut self) -> Result<(), WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&self.owner];
        self.tx
            .call_void("SELECT forge_external_agent_api.reconcile($1)", params)
            .map_err(work_err)
    }

    fn artifact_by_task(
        &mut self,
        project: &str,
        task_id: Uuid,
    ) -> Result<Option<Value>, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&self.owner, &project, &task_id];
        self.tx
            .call_json(
                "SELECT forge_external_agent_api.artifact_by_task($1,$2,$3) AS value",
                params,
            )
            .map_err(work_err)
    }

    fn enqueue_v2(&mut self, project: &str, task_id: Uuid) -> Result<Uuid, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&self.owner, &project, &task_id];
        self.tx
            .call_scalar(
                "SELECT forge_external_workflow_api.enqueue_v2($1,$2,$3) AS assignment_id",
                params,
            )
            .map_err(work_err)
            .map(|value: Option<Uuid>| value.unwrap_or_default())
    }

    fn insert_context(
        &mut self,
        assignment_id: Uuid,
        content: &str,
        sha256: &str,
    ) -> Result<(), WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&assignment_id, &content, &sha256];
        self.tx
            .call_void(
                "INSERT INTO forge_external_workflow.contexts(assignment_id,content,sha256) VALUES($1,$2,$3)",
                params,
            )
            .map_err(work_err)
    }

    fn workflow_for_task(
        &mut self,
        project: &str,
        task_id: Uuid,
    ) -> Result<Option<Value>, WorkError> {
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(project.to_string()),
            Box::new(task_id),
        ];
        self.one(
            "SELECT to_jsonb(w) FROM forge_external_workflow.tasks x JOIN forge_external_workflow.workflows w USING(workflow_id) WHERE(x.owner_id,x.project_alias,x.task_id)=($1,$2,$3)",
            &params,
        )
    }

    fn workflow_author_for_task(
        &mut self,
        project: &str,
        task_id: Uuid,
    ) -> Result<Option<String>, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&self.owner, &project, &task_id];
        self.tx
            .call_scalar(
                "SELECT w.author_profile FROM forge_external_workflow.tasks x JOIN forge_external_workflow.workflows w USING(workflow_id) WHERE(x.owner_id,x.project_alias,x.task_id)=($1,$2,$3)",
                params,
            )
            .map_err(work_err)
    }

    fn workflow_by_identity(
        &mut self,
        project: &str,
        workflow_id: Uuid,
        root_task_id: Uuid,
        for_update: bool,
    ) -> Result<Option<Value>, WorkError> {
        let lock = if for_update { " FOR UPDATE" } else { "" };
        let sql = format!(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_external_workflow.workflows WHERE(owner_id,project_alias,workflow_id,root_task_id)=($1,$2,$3,$4){lock}) t"
        );
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(project.to_string()),
            Box::new(workflow_id),
            Box::new(root_task_id),
        ];
        self.one(&sql, &params)
    }

    fn workflow_by_join(
        &mut self,
        project: &str,
        task_id: Uuid,
    ) -> Result<Option<Value>, WorkError> {
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(project.to_string()),
            Box::new(task_id),
        ];
        self.one(
            "SELECT to_jsonb(w) FROM forge_external_workflow.workflows w JOIN forge_external_workflow.tasks x USING(workflow_id) WHERE(x.owner_id,x.project_alias,x.task_id)=($1,$2,$3) FOR UPDATE OF w",
            &params,
        )
    }

    fn create_workflow(&mut self, row: &Value) -> Result<(Value, bool), WorkError> {
        let (sql, params) = self.insert_sql("forge_external_workflow.workflows", row);
        let sql = format!(
            "WITH ins AS ({sql} ON CONFLICT(owner_id,project_alias,root_task_id) DO NOTHING RETURNING *) SELECT to_jsonb(ins) FROM ins"
        );
        if let Some(created) = self.one(&sql, &params)? {
            return Ok((created, true));
        }
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(
                row["project_alias"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            ),
            Box::new(
                row["root_task_id"]
                    .as_str()
                    .and_then(|text| Uuid::parse_str(text).ok())
                    .unwrap_or_default(),
            ),
        ];
        let existing = self
            .one(
                "SELECT to_jsonb(t) FROM (SELECT * FROM forge_external_workflow.workflows WHERE(owner_id,project_alias,root_task_id)=($1,$2,$3)) t",
                &params,
            )?
            .ok_or_else(|| WorkError::store("External workflow insert vanished"))?;
        Ok((existing, false))
    }

    fn save_workflow(&mut self, w: &Value) -> Result<Option<i64>, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[
            &w["active_task_id"]
                .as_str()
                .and_then(|text| Uuid::parse_str(text).ok())
                .unwrap_or_default(),
            &w["phase"].as_str().unwrap_or_default(),
            &w["plan_revisions"].as_i64().unwrap_or_default(),
            &w["code_revisions"].as_i64().unwrap_or_default(),
            &w["enabled"].as_bool().unwrap_or_default(),
            &w["state"],
            &w["workflow_id"]
                .as_str()
                .and_then(|text| Uuid::parse_str(text).ok())
                .unwrap_or_default(),
            &w["revision"].as_i64().unwrap_or_default(),
        ];
        self.tx
            .call_scalar(
                "UPDATE forge_external_workflow.workflows SET active_task_id=$1,phase=$2,plan_revisions=$3,code_revisions=$4,enabled=$5,state=$6::jsonb,revision=revision+1,updated_at=clock_timestamp() WHERE workflow_id=$7 AND revision=$8 RETURNING revision",
                params,
            )
            .map_err(work_err)
    }

    fn update_workflow_state(
        &mut self,
        workflow_id: Uuid,
        state: &Value,
        revision: Option<i64>,
    ) -> Result<u64, WorkError> {
        match revision {
            Some(revision) => {
                let params: &[&(dyn ToSql + Sync)] = &[&state, &workflow_id, &revision];
                self.tx
                    .execute_count(
                        "UPDATE forge_external_workflow.workflows SET state=$1::jsonb WHERE workflow_id=$2 AND revision=$3",
                        params,
                    )
                    .map_err(work_err)
            }
            None => {
                let params: &[&(dyn ToSql + Sync)] = &[&state, &workflow_id];
                self.tx
                    .execute_count(
                        "UPDATE forge_external_workflow.workflows SET state=$1::jsonb WHERE workflow_id=$2",
                        params,
                    )
                    .map_err(work_err)
            }
        }
    }

    fn insert_workflow_task(
        &mut self,
        project: &str,
        task_id: Uuid,
        workflow_id: Uuid,
        role: &str,
    ) -> Result<(), WorkError> {
        let params: &[&(dyn ToSql + Sync)] =
            &[&self.owner, &project, &task_id, &workflow_id, &role];
        self.tx
            .call_void(
                "INSERT INTO forge_external_workflow.tasks(owner_id,project_alias,task_id,workflow_id,role) VALUES($1,$2,$3,$4,$5)",
                params,
            )
            .map_err(work_err)
    }

    fn related_task(
        &mut self,
        project: &str,
        workflow_id: Uuid,
        task_id: Uuid,
    ) -> Result<bool, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&self.owner, &project, &workflow_id, &task_id];
        self.tx
            .call_bool(
                "SELECT EXISTS(SELECT FROM forge_external_workflow.tasks WHERE(owner_id,project_alias,workflow_id,task_id)=($1,$2,$3,$4)) AS belongs",
                params,
            )
            .map_err(work_err)
    }

    fn leased_workflows(&mut self, projects: &[String]) -> Result<Vec<Value>, WorkError> {
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(projects.to_vec()),
        ];
        self.many(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_external_workflow.workflows WHERE owner_id=$1 AND project_alias=ANY($2) AND enabled AND phase NOT IN('completed','cancelled','escalated') AND(lease_until IS NULL OR lease_until<=clock_timestamp()) ORDER BY updated_at,workflow_id LIMIT 16 FOR UPDATE SKIP LOCKED) t",
            &params,
        )
    }

    fn discover_tasks(
        &mut self,
        project: &str,
        author_profile: &str,
    ) -> Result<Vec<Value>, WorkError> {
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(project.to_string()),
            Box::new(author_profile.to_string()),
        ];
        self.many(
            "SELECT to_jsonb(t) FROM (SELECT t.* FROM forge_agent.coding_tasks t WHERE t.owner_id=$1 AND t.project_alias=$2 AND t.agent_profile=$3 AND t.mode='plan' AND t.status IN('queued','running','awaiting_review') AND t.latest_attempt_id IS NULL AND t.pinned_recipe->>'resource_profile' IN('forge-external-db-v1','forge-external-db-v2') AND NOT EXISTS(SELECT FROM forge_external_workflow.tasks x WHERE(x.owner_id,x.project_alias,x.task_id)=(t.owner_id,t.project_alias,t.id)) ORDER BY t.created_at LIMIT 16 FOR UPDATE OF t SKIP LOCKED) t",
            &params,
        )
    }

    fn task_row(
        &mut self,
        project: &str,
        task_id: Uuid,
        for_update: bool,
    ) -> Result<Option<Value>, WorkError> {
        self.task_row_sql(project, task_id, for_update)
    }

    fn update_task_row(
        &mut self,
        project: &str,
        task_id: Uuid,
        set: &Value,
    ) -> Result<u64, WorkError> {
        let where_params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(project.to_string()),
            Box::new(task_id),
        ];
        self.update_row(
            "forge_agent.coding_tasks",
            "(owner_id,project_alias,id)=($1,$2,$3)",
            where_params,
            set,
        )
    }

    fn insert_task_row(&mut self, row: &Value) -> Result<(), WorkError> {
        self.insert_row("forge_agent.coding_tasks", row)
    }

    fn workflow_tasks_locked(&mut self, workflow_id: Uuid) -> Result<Vec<Value>, WorkError> {
        let params: Params = vec![Box::new(workflow_id)];
        self.many(
            "SELECT to_jsonb(t) FROM (SELECT t.* FROM forge_agent.coding_tasks t JOIN forge_external_workflow.tasks x ON(x.owner_id,x.project_alias,x.task_id)=(t.owner_id,t.project_alias,t.id) WHERE x.workflow_id=$1 AND t.status IN('queued','running','awaiting_review','needs_reconcile','blocked_auth','blocked_permission','quota_blocked') FOR UPDATE OF t) t",
            &params,
        )
    }

    fn clock_now(&mut self) -> Result<i128, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[];
        self.tx
            .call_scalar(
                "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000000000)::bigint AS now_ns",
                params,
            )
            .map_err(work_err)
            .map(|value: Option<i64>| value.unwrap_or_default() as i128)
    }

    fn task_attempt_active(&mut self, task_id: Uuid) -> Result<bool, WorkError> {
        // Python's probe joins the task's owner/project from
        // the task row itself; the workflow's tasks all share
        // the store owner, and the attempt rows key on the
        // task id under it - resolved through the task row.
        let params: &[&(dyn ToSql + Sync)] = &[&self.owner, &task_id];
        self.tx
            .call_bool(
                "SELECT EXISTS(SELECT FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id) WHERE t.owner_id=$1 AND t.id=$2 AND(a.phase<>'stopped' OR a.reservation_active OR a.lease_expires_at IS NOT NULL)) AS active",
                params,
            )
            .map_err(work_err)
    }

    fn attempts_active(&mut self, workflow_id: Uuid) -> Result<bool, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&workflow_id];
        self.tx
            .call_bool(
                "SELECT EXISTS(SELECT FROM forge_agent.coding_attempts a JOIN forge_external_workflow.tasks t ON(t.owner_id,t.project_alias,t.task_id)=(a.owner_id,a.project_alias,a.task_id) WHERE t.workflow_id=$1 AND(a.phase<>'stopped' OR a.reservation_active OR a.lease_expires_at IS NOT NULL)) AS active",
                params,
            )
            .map_err(work_err)
    }

    fn actions_active(&mut self, workflow_id: Uuid) -> Result<bool, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&workflow_id];
        self.tx
            .call_bool(
                "SELECT EXISTS(SELECT FROM forge_external_workflow.actions WHERE workflow_id=$1 AND status IN('queued','leased')) AS active",
                params,
            )
            .map_err(work_err)
    }

    fn tasks_active(&mut self, workflow_id: Uuid) -> Result<bool, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&workflow_id];
        self.tx
            .call_bool(
                "SELECT EXISTS(SELECT FROM forge_agent.coding_tasks t JOIN forge_external_workflow.tasks x ON(x.owner_id,x.project_alias,x.task_id)=(t.owner_id,t.project_alias,t.id) WHERE x.workflow_id=$1 AND t.status IN('queued','running','cancel_requested')) AS active",
                params,
            )
            .map_err(work_err)
    }

    fn any_active(&mut self, workflow_id: Uuid) -> Result<bool, WorkError> {
        Ok(self.attempts_active(workflow_id)?
            || self.actions_active(workflow_id)?
            || self.tasks_active(workflow_id)?)
    }

    fn cancel_receipt_exists(&mut self, task_ids: &[Uuid]) -> Result<bool, WorkError> {
        if task_ids.len() == 1 {
            let params: &[&(dyn ToSql + Sync)] = &[&self.owner, &task_ids[0]];
            return self
                .tx
                .call_bool(
                    "SELECT EXISTS(SELECT FROM forge_agent.coding_control_receipts WHERE(owner_id,task_id,action)=($1,$2,'cancel')) AS cancelled",
                    params,
                )
                .map_err(work_err);
        }
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(task_ids.to_vec()),
        ];
        let rows = self.many(
            "SELECT to_jsonb(t) FROM (SELECT EXISTS(SELECT FROM forge_agent.coding_control_receipts WHERE owner_id=$1 AND task_id=ANY($2::uuid[]) AND action='cancel') AS cancelled) t",
            &params,
        )?;
        Ok(rows
            .first()
            .map(|row| row["cancelled"].as_bool().unwrap_or(false))
            .unwrap_or(false))
    }

    fn control_receipt(
        &mut self,
        project: &str,
        task_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Option<Value>, WorkError> {
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(project.to_string()),
            Box::new(task_id),
            Box::new(idempotency_key.to_string()),
        ];
        self.one(
            "SELECT to_jsonb(t) FROM (SELECT action,intent_digest,receipt_json FROM forge_agent.coding_control_receipts WHERE(owner_id,project_alias,task_id,idempotency_key)=($1,$2,$3,$4)) t",
            &params,
        )
    }

    fn insert_control_receipt(
        &mut self,
        project: &str,
        task_id: Uuid,
        action: &str,
        idempotency_key: &str,
        intent_digest: &str,
        base_revision: i64,
        result_revision: i64,
        receipt: &Value,
    ) -> Result<(), WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[
            &self.owner,
            &project,
            &task_id,
            &action,
            &idempotency_key,
            &intent_digest,
            &base_revision,
            &result_revision,
            &receipt,
        ];
        self.tx
            .call_void(
                "INSERT INTO forge_agent.coding_control_receipts(owner_id,project_alias,task_id,action,idempotency_key,intent_digest,base_revision,result_revision,receipt_json) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9::jsonb)",
                params,
            )
            .map_err(work_err)
    }

    fn insert_review_row(&mut self, row: &Value) -> Result<(), WorkError> {
        self.insert_row("forge_external_workflow.reviews", row)
    }

    fn review_row(&mut self, review_id: Uuid, lock: LockMode) -> Result<Option<Value>, WorkError> {
        let lock_sql = match lock {
            LockMode::None => "",
            LockMode::Update => " FOR UPDATE",
            LockMode::Share => " FOR SHARE",
        };
        let sql = format!(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_external_workflow.reviews WHERE review_id=$1{lock_sql}) t"
        );
        let params: Params = vec![Box::new(review_id)];
        self.one(&sql, &params)
    }

    fn update_review_row(&mut self, review_id: Uuid, set: &Value) -> Result<u64, WorkError> {
        let where_params: Params = vec![Box::new(review_id)];
        self.update_row(
            "forge_external_workflow.reviews",
            "review_id=$1",
            where_params,
            set,
        )
    }

    fn reviewing_reviewer_task(&mut self, workflow_id: Uuid) -> Result<Option<Uuid>, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&workflow_id];
        self.tx
            .call_scalar(
                "SELECT reviewer_task_id FROM forge_external_workflow.reviews WHERE workflow_id=$1 AND status='reviewing' ORDER BY created_at DESC LIMIT 1",
                params,
            )
            .map_err(work_err)
    }

    fn action_by_key(
        &mut self,
        workflow_id: Uuid,
        kind: &str,
        source_key: &str,
    ) -> Result<Option<Value>, WorkError> {
        let params: Params = vec![
            Box::new(workflow_id),
            Box::new(kind.to_string()),
            Box::new(source_key.to_string()),
        ];
        self.one(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_external_workflow.actions WHERE workflow_id=$1 AND kind=$2 AND source_key=$3) t",
            &params,
        )
    }

    fn expire_action(&mut self, action_id: Uuid) -> Result<Option<Value>, WorkError> {
        let expired = json!({
            "status": "failed",
            "receipt": {},
            "summary": "The bounded trusted delivery retry deadline was exhausted.",
        });
        let params: &[&(dyn ToSql + Sync)] = &[&expired, &action_id];
        self.tx
            .call_json(
                "WITH upd AS (UPDATE forge_external_workflow.actions SET status='failed',lease_token=<redacted>,lease_role=NULL,lease_until=NULL,result=$1::jsonb,updated_at=clock_timestamp() WHERE action_id=$2 AND status IN('queued','leased') AND(deadline_at<=clock_timestamp() OR generation>=10000) RETURNING *) SELECT to_jsonb(upd) FROM upd",
                params,
            )
            .map_err(work_err)
    }

    fn insert_action_row(&mut self, row: &Value) -> Result<Value, WorkError> {
        let (sql, params) = self.insert_sql("forge_external_workflow.actions", row);
        let sql = format!("WITH ins AS ({sql} RETURNING *) SELECT to_jsonb(ins) FROM ins");
        self.one(&sql, &params)?
            .ok_or_else(|| WorkError::store("External action insert vanished"))
    }

    fn action_row(
        &mut self,
        workflow_id: Uuid,
        action_id: Uuid,
        for_share: bool,
    ) -> Result<Option<Value>, WorkError> {
        let lock = if for_share { " FOR SHARE" } else { "" };
        let sql = format!(
            "SELECT to_jsonb(t) FROM (SELECT intent,result,status,kind FROM forge_external_workflow.actions WHERE workflow_id=$1 AND action_id=$2{lock}) t"
        );
        let params: Params = vec![Box::new(workflow_id), Box::new(action_id)];
        self.one(&sql, &params)
    }

    fn verify_actions(&mut self, workflow_id: Uuid) -> Result<Vec<Value>, WorkError> {
        let params: Params = vec![Box::new(workflow_id)];
        self.many(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_external_workflow.actions WHERE workflow_id=$1 AND kind='verify' AND status IN('queued','leased','failed')) t",
            &params,
        )
    }

    fn fail_open_actions(&mut self, workflow_id: Uuid, result: &Value) -> Result<u64, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&result, &workflow_id];
        self.tx
            .execute_count(
                "UPDATE forge_external_workflow.actions SET status='failed',result=$1::jsonb,lease_token=<redacted>,lease_role=NULL,lease_until=NULL,updated_at=clock_timestamp() WHERE workflow_id=$2 AND status IN('queued','leased')",
                params,
            )
            .map_err(work_err)
    }

    fn native_attempt(
        &mut self,
        project: &str,
        task_id: Uuid,
        attempt_id: Uuid,
        revision: i64,
    ) -> Result<Option<Value>, WorkError> {
        let params: Params = vec![
            Box::new(revision),
            Box::new(self.owner.to_string()),
            Box::new(project.to_string()),
            Box::new(task_id),
            Box::new(attempt_id),
        ];
        self.one(
            "SELECT to_jsonb(s) FROM (SELECT a.*,r.receipt_id FROM forge_agent.coding_attempts a JOIN forge_agent.coding_reconciliation_receipts r ON(r.owner_id,r.project_alias,r.task_id,r.attempt_id,r.task_revision)=(a.owner_id,a.project_alias,a.task_id,a.attempt_id,$1) WHERE(a.owner_id,a.project_alias,a.task_id,a.attempt_id)=($2,$3,$4,$5) AND a.phase='stopped' AND NOT a.reservation_active AND a.lease_expires_at IS NULL AND a.outcome='awaiting_review' AND a.published_at IS NOT NULL ORDER BY r.created_at DESC LIMIT 1) s",
            &params,
        )
    }

    fn result_chunks(
        &mut self,
        project: &str,
        task_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Vec<String>, WorkError> {
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(project.to_string()),
            Box::new(task_id),
            Box::new(attempt_id),
        ];
        let rows = self
            .tx
            .query_rows(
                "SELECT content FROM forge_agent.coding_result_chunks WHERE(owner_id,project_alias,task_id,attempt_id)=($1,$2,$3,$4) ORDER BY chunk_index",
                &refs(&params),
            )
            .map_err(work_err)?;
        Ok(rows.iter().map(|row| row.get::<usize, String>(0)).collect())
    }

    fn failed_attempt(
        &mut self,
        project: &str,
        task_id: Uuid,
        attempt_id: Uuid,
        revision: i64,
        outcome: &str,
        summary: &str,
    ) -> Result<Option<Value>, WorkError> {
        // The plan path's query selects a.summary; the code
        // path's does not (its summary is a filter literal in
        // both). The outcome distinguishes them: 'failed'
        // carries the summary column, 'needs_reconcile' does
        // not - matching the two Python SELECT lists.
        let select_summary = if outcome == "failed" {
            "a.summary,"
        } else {
            ""
        };
        let sql = format!(
            "SELECT to_jsonb(s) FROM (SELECT a.attempt_id,{select_summary}a.unit_name,r.receipt_id,r.stop_proof FROM forge_agent.coding_attempts a JOIN forge_agent.coding_reconciliation_receipts r ON(r.owner_id,r.project_alias,r.task_id,r.attempt_id,r.task_revision)=(a.owner_id,a.project_alias,a.task_id,a.attempt_id,$1) WHERE(a.owner_id,a.project_alias,a.task_id,a.attempt_id)=($2,$3,$4,$5) AND a.phase='stopped' AND NOT a.reservation_active AND a.lease_expires_at IS NULL AND a.published_at IS NOT NULL AND a.outcome=$6 AND a.result_sha256 IS NULL AND a.result_byte_count IS NULL AND a.result_chunk_count IS NULL AND NOT EXISTS(SELECT FROM forge_agent.coding_result_chunks c WHERE c.attempt_id=a.attempt_id) AND a.summary=$7 ORDER BY r.created_at DESC LIMIT 1) s"
        );
        let params: Params = vec![
            Box::new(revision),
            Box::new(self.owner.to_string()),
            Box::new(project.to_string()),
            Box::new(task_id),
            Box::new(attempt_id),
            Box::new(outcome.to_string()),
            Box::new(summary.to_string()),
        ];
        self.one(&sql, &params)
    }

    fn reconciliation_by(
        &mut self,
        project: &str,
        task_id: Uuid,
        attempt_id: Uuid,
        receipt_id: Uuid,
    ) -> Result<Option<Value>, WorkError> {
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(project.to_string()),
            Box::new(task_id),
            Box::new(attempt_id),
            Box::new(receipt_id),
        ];
        self.one(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE(owner_id,project_alias,task_id,attempt_id,receipt_id)=($1,$2,$3,$4,$5)) t",
            &params,
        )
    }

    fn attempt_unit_name(&mut self, attempt_id: Uuid) -> Result<Option<String>, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&attempt_id];
        self.tx
            .call_scalar(
                "SELECT unit_name FROM forge_agent.coding_attempts WHERE attempt_id=$1",
                params,
            )
            .map_err(work_err)
    }

    fn existing_override(&mut self, workflow_id: Uuid) -> Result<Option<Value>, WorkError> {
        let params: Params = vec![Box::new(workflow_id)];
        self.one(
            "SELECT to_jsonb(t) FROM (SELECT idempotency_key,intent_digest,receipt FROM forge_external_workflow.plan_approve_overrides WHERE workflow_id=$1) t",
            &params,
        )
    }

    fn insert_plan_override(&mut self, row: &Value) -> Result<(), WorkError> {
        self.insert_row("forge_external_workflow.plan_approve_overrides", row)
    }
}

fn invalid(message: String) -> WorkError {
    WorkError::store(message)
}

impl RetryEvents for ExternalTxImpl<'_, '_, '_, '_> {
    fn observe_failure(
        &mut self,
        scope: &forge_work::retry_events::RetryScope,
        kind: &str,
        source_id: &str,
        digest: &str,
        passed_kinds: &[String],
        phase_id: &str,
        passed_phase_ids: &[String],
    ) -> Result<Value, WorkError> {
        use forge_work::retry_events::{scope_args, validate_proof, RETRY_EVENT_KINDS};
        validate_proof(kind, source_id, digest).map_err(invalid)?;
        let mut seen = std::collections::HashSet::new();
        if passed_kinds.iter().any(|passed| {
            !seen.insert(passed.clone())
                || passed == kind
                || !RETRY_EVENT_KINDS.contains(&passed.as_str())
        }) {
            return Err(invalid("Invalid trusted passed stages".to_string()));
        }
        let (project, scope_kind, scope_id) = scope_args(self.owner, scope).map_err(invalid)?;
        let passed: Vec<String> = passed_kinds.to_vec();
        let passed_phases: Vec<String> = passed_phase_ids.to_vec();
        let params: &[&(dyn ToSql + Sync)] = &[
            &project,
            &scope_kind,
            &scope_id,
            &kind,
            &source_id,
            &digest,
            &passed,
            &phase_id,
            &passed_phases,
        ];
        self.tx
            .call_json(
                "SELECT forge_agent.retry_event_observe($1,$2,$3,$4,$5,$6,$7,$8,$9) AS event",
                params,
            )
            .map_err(work_err)
            .map(|value| value.unwrap_or(Value::Null))
    }

    fn reserve(&mut self, event_id: Uuid, action_key: &str) -> Result<Value, WorkError> {
        if action_key.is_empty()
            || action_key.len() > 256
            || action_key.chars().any(|c| (c as u32) < 32)
        {
            return Err(invalid("Invalid retry admission key".to_string()));
        }
        let params: &[&(dyn ToSql + Sync)] = &[&event_id, &action_key];
        self.tx
            .call_json(
                "SELECT forge_agent.retry_event_reserve($1,$2) AS event",
                params,
            )
            .map_err(work_err)
            .map(|value| value.unwrap_or(Value::Null))
    }

    fn resolve_verified(
        &mut self,
        scope: &forge_work::retry_events::RetryScope,
        kind: &str,
        source_id: &str,
        digest: &str,
        phase_id: Option<&str>,
    ) -> Result<Value, WorkError> {
        use forge_work::retry_events::{scope_args, validate_proof};
        validate_proof(kind, source_id, digest).map_err(invalid)?;
        let (project, scope_kind, scope_id) = scope_args(self.owner, scope).map_err(invalid)?;
        let params: &[&(dyn ToSql + Sync)] = &[
            &project,
            &scope_kind,
            &scope_id,
            &kind,
            &source_id,
            &digest,
            &phase_id,
        ];
        self.tx
            .call_json(
                "SELECT forge_agent.retry_event_resolve($1,$2,$3,$4,$5,$6,$7) AS event",
                params,
            )
            .map_err(work_err)
            .map(|value| value.unwrap_or(Value::Null))
    }

    fn resolve_open_scope(
        &mut self,
        scope: &forge_work::retry_events::RetryScope,
    ) -> Result<i64, WorkError> {
        use forge_work::retry_events::scope_args;
        let (project, scope_kind, scope_id) = scope_args(self.owner, scope).map_err(invalid)?;
        let available: bool = self
            .tx
            .call_bool(
                "SELECT to_regprocedure('forge_agent.retry_event_resolve_open_scope(text,text,uuid)') IS NOT NULL AS available",
                &[],
            )
            .map_err(work_err)?;
        if !available {
            return Ok(0);
        }
        let params: &[&(dyn ToSql + Sync)] = &[&project, &scope_kind, &scope_id];
        self.tx
            .call_scalar(
                "SELECT forge_agent.retry_event_resolve_open_scope($1,$2,$3) AS resolved",
                params,
            )
            .map_err(work_err)
            .map(|value: Option<i64>| value.unwrap_or(0))
    }

    fn adopt_legacy(
        &mut self,
        event_id: Uuid,
        used: i64,
        source_id: &str,
        digest: &str,
    ) -> Result<Value, WorkError> {
        use forge_work::retry_events::validate_proof;
        if !(0..=forge_work::retry_policy::AGENT_RETRY_LIMIT).contains(&used) {
            return Err(invalid("Invalid retained event count".to_string()));
        }
        validate_proof("planning", source_id, digest).map_err(invalid)?;
        let params: &[&(dyn ToSql + Sync)] = &[&event_id, &used, &source_id, &digest];
        self.tx
            .call_json(
                "SELECT forge_agent.retry_event_adopt_legacy($1,$2,$3,$4) AS event",
                params,
            )
            .map_err(work_err)
            .map(|value| value.unwrap_or(Value::Null))
    }

    fn bind_review(
        &mut self,
        review_id: Uuid,
        event_id: Uuid,
        action_key: &str,
    ) -> Result<(), WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&review_id, &event_id, &action_key];
        self.tx
            .call_void(
                "SELECT forge_agent.retry_event_bind_review($1,$2,$3)",
                params,
            )
            .map_err(work_err)
    }

    fn get(&mut self, event_id: Uuid) -> Result<Value, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&event_id];
        self.tx
            .call_json("SELECT forge_agent.retry_event_get($1) AS event", params)
            .map_err(work_err)
            .map(|value| value.unwrap_or(Value::Null))
    }

    fn project(
        &mut self,
        scope: &forge_work::retry_events::RetryScope,
    ) -> Result<Option<Value>, WorkError> {
        use forge_work::retry_events::scope_args;
        let (project, scope_kind, scope_id) = scope_args(self.owner, scope).map_err(invalid)?;
        let available: bool = self
            .tx
            .call_bool(
                "SELECT to_regprocedure('forge_agent.retry_event_project(text,text,uuid)') IS NOT NULL AS available",
                &[],
            )
            .map_err(work_err)?;
        if !available {
            return Ok(None);
        }
        let params: &[&(dyn ToSql + Sync)] = &[&project, &scope_kind, &scope_id];
        self.tx
            .call_json(
                "SELECT forge_agent.retry_event_project($1,$2,$3) AS event",
                params,
            )
            .map_err(work_err)
    }
}

/// work_external.py's ExternalWorkDAO over the external
/// transaction port: managed projects route to the workflow
/// store's admit inside the caller's transaction; the rest
/// go to the forge_external_agent_api functions.
pub struct ExternalWorkDAO {
    owner: String,
    profiles: BTreeSet<String>,
    workflow: Option<Arc<ExternalWorkflowStore>>,
}

impl ExternalWorkDAO {
    pub fn new(
        owner: String,
        profiles: BTreeSet<String>,
        workflow: Option<Arc<ExternalWorkflowStore>>,
    ) -> Self {
        Self {
            owner,
            profiles,
            workflow,
        }
    }

    pub fn workflow(&self) -> Option<&Arc<ExternalWorkflowStore>> {
        self.workflow.as_ref()
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }
}

impl<Tx: ExternalTx> ExternalWorkPort<Tx> for ExternalWorkDAO {
    type Error = WorkError;

    fn profiles(&self) -> &BTreeSet<String> {
        &self.profiles
    }

    fn enqueue(&self, tx: &mut Tx, project_alias: &str, task_id: Uuid) -> Result<Uuid, WorkError> {
        if let Some(flow) = &self.workflow {
            if flow.manages(project_alias) {
                return flow.admit(tx, project_alias, task_id);
            }
        }
        tx.enqueue(project_alias, task_id)
    }

    fn catalogue(&self, tx: &mut Tx) -> Result<Vec<ExternalReadiness>, WorkError> {
        let entries = tx.catalogue()?;
        let list = entries
            .as_array()
            .filter(|list| list.len() <= 256)
            .ok_or_else(|| WorkError::store("Invalid external readiness collection"))?;
        list.iter()
            .map(|entry| ExternalReadiness::from_value(entry).map_err(WorkError::store))
            .collect()
    }

    fn artifact_by_task(
        &self,
        tx: &mut Tx,
        project_alias: &str,
        task_id: Uuid,
    ) -> Result<Option<Value>, WorkError> {
        let Some(value) = tx.artifact_by_task(project_alias, task_id)? else {
            return Ok(None);
        };
        validate_external_artifact(&value)
            .map(Some)
            .map_err(WorkError::store)
    }

    fn reconcile(&self, tx: &mut Tx) -> Result<(), WorkError> {
        tx.reconcile_external()
    }
}

/// The F6 composition: the workflow store plus its DAO,
/// sharing one transaction factory (the host binds grant
/// providers and the verification port into the store at
/// construction).
pub struct PgExternalWorkflow {
    pub store: Arc<ExternalWorkflowStore>,
    pub dao: ExternalWorkDAO,
    pub tx_factory: Arc<PgExternalTxFactory>,
}

impl PgExternalWorkflow {
    pub fn new(
        store: Arc<ExternalWorkflowStore>,
        dao: ExternalWorkDAO,
        tx_factory: Arc<PgExternalTxFactory>,
    ) -> Self {
        Self {
            store,
            dao,
            tx_factory,
        }
    }

    /// maintain_external's reconcile half (the coordinator
    /// loop's run_once is the store's own entry). The DAO's
    /// reconcile is a direct forward to the tx, so this
    /// calls it without going through the port trait.
    pub fn maintain_external(&self) -> Result<(), WorkError> {
        self.tx_factory.run(false, &mut |tx| {
            tx.reconcile_external()?;
            Ok(Value::Null)
        })?;
        Ok(())
    }
}
