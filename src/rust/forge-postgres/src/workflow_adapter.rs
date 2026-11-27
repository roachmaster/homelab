//! The WorkflowTx port over PgSession/Tx (the fused store split
//! of forge-work-v2-design.yaml): row loads render through
//! to_jsonb so rows arrive in the Python dict shape, generic
//! inserts/updates build over a column-type discipline, and the
//! bespoke probes transcribe work_workflow.py's SQL directly.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use forge_core::provider::Factory;

use forge_work::retry_events::{
    scope_args, validate_proof, RetryEvents, RetryScope, RETRY_EVENT_KINDS,
};
use forge_work::work_actors::Actor;
use forge_work::work_error::WorkError;
use forge_work::workflow_tx::{BusyCheck, WorkflowTx, WorkflowTxFactory};

use crate::error::AdapterError;
use crate::factory::PgSessionFactory;
use crate::tx::Tx;
use crate::work_common::{actor_json, work_err, ADVISORY_LOCK_SALT};

const MARKER: &str = "forge-work: parked WorkError";

pub struct PgWorkflowTxFactory {
    factory: PgSessionFactory,
    owner: String,
    allowed_projects: Vec<String>,
    scope: AtomicU64,
}

impl PgWorkflowTxFactory {
    pub fn new(factory: PgSessionFactory, owner: String, allowed_projects: Vec<String>) -> Self {
        Self {
            factory,
            owner,
            allowed_projects,
            scope: AtomicU64::new(1),
        }
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn allowed_projects(&self) -> &[String] {
        &self.allowed_projects
    }
}

impl WorkflowTxFactory for PgWorkflowTxFactory {
    fn run(
        &self,
        read_only: bool,
        body: &mut dyn FnMut(&mut dyn WorkflowTx) -> Result<Value, WorkError>,
    ) -> Result<Value, WorkError> {
        let mut session = self.factory.create().map_err(work_err)?;
        let slot: std::cell::RefCell<Option<WorkError>> = std::cell::RefCell::new(None);
        let scope = self.scope.fetch_add(1, Ordering::Relaxed);
        let owner = self.owner.clone();
        let allowed = self.allowed_projects.clone();
        let out = session.in_tx(|tx| {
            if read_only {
                tx.call_void("SET TRANSACTION READ ONLY", &[])?;
            }
            tx.call_void("SET LOCAL lock_timeout='5s'", &[])?;
            tx.call_void("SET LOCAL statement_timeout='10s'", &[])?;
            let mut handle = WorkflowTxImpl {
                tx,
                owner: &owner,
                allowed_projects: &allowed,
                scope,
            };
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

struct WorkflowTxImpl<'x, 't, 'c, 'o> {
    tx: &'x mut Tx<'t, 'c>,
    owner: &'o str,
    allowed_projects: &'o [String],
    scope: u64,
}

type Params = Vec<Box<dyn ToSql + Sync + Send>>;

fn refs(params: &Params) -> Vec<&(dyn ToSql + Sync)> {
    params.iter().map(|param| param.as_ref() as &(dyn ToSql + Sync)).collect()
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

fn col_type(table: &str, column: &str, value: &Value) -> Ty {
    let _ = table;
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
        Value::String(_) => Ty::Text,
        Value::Null => Ty::Text,
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

impl<'x, 't, 'c, 'o> WorkflowTxImpl<'x, 't, 'c, 'o> {
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
            let ty = col_type(table, column, value);
            columns.push(format!("\"{column}\""));
            places.push(placeholder(index + 1, ty));
            bind(&mut params, ty, value);
        }
        (
            format!(
                "INSERT INTO forge_agent.{table}({}) VALUES({})",
                columns.join(","),
                places.join(",")
            ),
            params,
        )
    }

    fn insert_row(&mut self, table: &str, row: &Value, suffix: &str) -> Result<Option<Value>, WorkError> {
        let (sql, params) = self.insert_sql(table, row);
        let sql = format!("{sql} {suffix}");
        if suffix.contains("RETURNING") {
            self.one(&sql, &params)
        } else {
            self.exec(&sql, &params).map(|_| None)
        }
    }

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
                let ty = col_type(table, column, value);
                bind(&mut params, ty, value);
                assignments.push(format!(
                    "\"{column}\"={}",
                    placeholder(params.len(), ty)
                ));
            }
        }
        if ["coding_workflows", "coding_tasks", "coding_workflow_reviews", "coding_workflow_actions", "coding_reviews"]
            .contains(&table)
        {
            assignments.push("updated_at=clock_timestamp()".to_string());
        }
        // Renumber the where placeholders after the set params.
        let offset = params.len();
        let mut where_sql = where_sql.to_string();
        for index in (1..=where_params.len()).rev() {
            where_sql = where_sql.replace(&format!("${index}"), &format!("${}", index + offset));
        }
        params.append(&mut where_params);
        let sql = format!(
            "UPDATE forge_agent.{table} SET {} WHERE {where_sql}",
            assignments.join(",")
        );
        self.exec(&sql, &params)
    }
}

fn uuid_of(value: &Value) -> Uuid {
    value
        .as_str()
        .and_then(|text| Uuid::parse_str(text).ok())
        .unwrap_or_default()
}

impl WorkflowTx for WorkflowTxImpl<'_, '_, '_, '_> {
    fn scope_id(&self) -> u64 {
        self.scope
    }

    fn owner(&self) -> &str {
        self.owner
    }

    fn project_lock(&mut self, project: &str) -> Result<(), WorkError> {
        let key = format!("{}\n{}", self.owner, project);
        let params: &[&(dyn ToSql + Sync)] = &[&key, &ADVISORY_LOCK_SALT];
        self.tx
            .call_void(
                "SELECT pg_advisory_xact_lock(hashtextextended($1::text,$2::bigint))",
                params,
            )
            .map_err(work_err)
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
        crate::work_rows::insert_event_raw(self.tx, self.owner, project, task_id, kind, merged, first)
    }

    fn workflow(&mut self, workflow_id: Uuid, for_update: bool) -> Result<Option<Value>, WorkError> {
        let lock = if for_update { " FOR UPDATE" } else { "" };
        let sql = format!(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=$1{lock}) t"
        );
        self.one(&sql, &vec![Box::new(workflow_id) as Box<dyn ToSql + Sync + Send>])
    }

    fn task(&mut self, task_id: Uuid, for_update: bool) -> Result<Option<Value>, WorkError> {
        let lock = if for_update { " FOR UPDATE" } else { "" };
        let sql = format!(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_tasks WHERE owner_id=$1 AND id=$2{lock}) t"
        );
        let params: Params = vec![Box::new(self.owner.to_string()), Box::new(task_id)];
        self.one(&sql, &params)
    }

    fn task_in_project(
        &mut self,
        project: &str,
        task_id: Uuid,
        for_update: bool,
    ) -> Result<Option<Value>, WorkError> {
        let lock = if for_update { " FOR UPDATE" } else { "" };
        let sql = format!(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND id=$3{lock}) t"
        );
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(project.to_string()),
            Box::new(task_id),
        ];
        self.one(&sql, &params)
    }

    fn tasks_of_workflow(&mut self, workflow_id: Uuid) -> Result<Vec<Value>, WorkError> {
        let params: Params = vec![Box::new(self.owner.to_string()), Box::new(workflow_id)];
        self.many(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_tasks WHERE owner_id=$1 AND workflow_id=$2 ORDER BY created_at) t",
            &params,
        )
    }

    fn lock_workflow_tasks(&mut self, workflow_id: Uuid) -> Result<(), WorkError> {
        let params: Params = vec![Box::new(workflow_id)];
        self.many(
            "SELECT id FROM forge_agent.coding_tasks WHERE workflow_id=$1 ORDER BY id FOR UPDATE",
            &params,
        )
        .map(|_| ())
    }

    fn workflow_review(
        &mut self,
        workflow_review_id: Uuid,
        for_update: bool,
    ) -> Result<Option<Value>, WorkError> {
        let lock = if for_update { " FOR UPDATE" } else { "" };
        let sql = format!(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=$1{lock}) t"
        );
        let params: Params = vec![Box::new(workflow_review_id)];
        self.one(&sql, &params)
    }

    fn action(&mut self, action_id: Uuid, for_update: bool) -> Result<Option<Value>, WorkError> {
        let lock = if for_update { " FOR UPDATE" } else { "" };
        let sql = format!(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_workflow_actions WHERE action_id=$1{lock}) t"
        );
        let params: Params = vec![Box::new(action_id)];
        self.one(&sql, &params)
    }

    fn action_by_source(
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
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_id=$1 AND kind=$2 AND source_key=$3) t",
            &params,
        )
    }

    fn actions_by_source_prefix(
        &mut self,
        workflow_id: Uuid,
        kind: Option<&str>,
        prefix: &str,
    ) -> Result<Vec<Value>, WorkError> {
        let pattern = format!("{prefix}%");
        let params: Params = vec![
            Box::new(workflow_id),
            Box::new(pattern),
            Box::new(kind.map(str::to_string)),
        ];
        self.many(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_id=$1 AND source_key LIKE $2 AND ($3::text IS NULL OR kind=$3)) t",
            &params,
        )
    }

    fn count_actions_by_source_prefix(
        &mut self,
        workflow_id: Uuid,
        kind: Option<&str>,
        prefix: &str,
    ) -> Result<i64, WorkError> {
        let pattern = format!("{prefix}%");
        let params: &[&(dyn ToSql + Sync)] = &[&workflow_id, &pattern, &kind];
        self.tx
            .call_scalar(
                "SELECT count(*) FROM forge_agent.coding_workflow_actions WHERE workflow_id=$1 AND source_key LIKE $2 AND ($3::text IS NULL OR kind=$3)",
                params,
            )
            .map_err(work_err)
            .map(|value: Option<i64>| value.unwrap_or(0))
    }

    fn attempt(&mut self, task_id: Uuid, attempt_id: Uuid) -> Result<Option<Value>, WorkError> {
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(task_id),
            Box::new(attempt_id),
        ];
        self.one(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_attempts WHERE owner_id=$1 AND task_id=$2 AND attempt_id=$3) t",
            &params,
        )
    }

    fn attempts_of_task(&mut self, task_id: Uuid) -> Result<Vec<Value>, WorkError> {
        let params: Params = vec![Box::new(task_id)];
        self.many(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_attempts WHERE task_id=$1 ORDER BY attempt_no) t",
            &params,
        )
    }

    fn receipt_by_id(&mut self, task_id: Uuid, receipt_id: Uuid) -> Result<Option<Value>, WorkError> {
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(task_id),
            Box::new(receipt_id),
        ];
        self.one(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=$1 AND task_id=$2 AND receipt_id=$3) t",
            &params,
        )
    }

    fn receipt_for_attempt(
        &mut self,
        task_id: Uuid,
        attempt_id: Uuid,
        task_revision: i64,
    ) -> Result<Option<Value>, WorkError> {
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(task_id),
            Box::new(attempt_id),
            Box::new(task_revision),
        ];
        self.one(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=$1 AND task_id=$2 AND attempt_id=$3 AND task_revision=$4) t",
            &params,
        )
    }

    fn review_row(&mut self, review_id: Uuid, for_update: bool) -> Result<Option<Value>, WorkError> {
        let lock = if for_update { " FOR UPDATE" } else { "" };
        let sql = format!(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_reviews WHERE review_id=$1{lock}) t"
        );
        let params: Params = vec![Box::new(review_id)];
        self.one(&sql, &params)
    }

    fn latest_local_review(
        &mut self,
        project: &str,
        task_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<Option<Value>, WorkError> {
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(project.to_string()),
            Box::new(task_id),
            Box::new(attempt_id),
        ];
        self.one(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_reviews WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND attempt_id=$4 AND phase='local' ORDER BY created_at DESC LIMIT 1) t",
            &params,
        )
    }

    fn plan_decision(&mut self, decision_id: Uuid) -> Result<Option<Value>, WorkError> {
        let params: Params = vec![Box::new(self.owner.to_string()), Box::new(decision_id)];
        self.one(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_plan_decisions WHERE owner_id=$1 AND decision_id=$2) t",
            &params,
        )
    }

    fn budget(&mut self, project: &str, root_task_id: Uuid) -> Result<Option<Value>, WorkError> {
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(project.to_string()),
            Box::new(root_task_id),
        ];
        self.one(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_review_budget WHERE owner_id=$1 AND project_alias=$2 AND root_task_id=$3) t",
            &params,
        )
    }

    fn readiness(&mut self, policy_digests: Option<&[String]>) -> Result<Vec<Value>, WorkError> {
        let digests: Option<Vec<String>> = policy_digests.map(|values| values.to_vec());
        let params: Params = vec![Box::new(self.owner.to_string()), Box::new(digests)];
        self.many(
            "SELECT to_jsonb(t) FROM (SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=$1 AND observed_at>=clock_timestamp()-interval '90 seconds' AND ($2::text[] IS NULL OR policy_digest=ANY($2))) t",
            &params,
        )
    }

    fn role_granted(&mut self, workflow_id: Uuid, profile: &str, mode: &str) -> Result<bool, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&workflow_id, &profile, &mode];
        self.tx
            .call_bool(
                "SELECT forge_agent.coding_workflow_role_granted($1,$2,$3) AS allowed",
                params,
            )
            .map_err(work_err)
    }

    fn policy_drifted(
        &mut self,
        workflow_id: Uuid,
        current_snapshot: &Value,
    ) -> Result<Option<bool>, WorkError> {
        let ready: bool = self
            .tx
            .call_bool(
                "SELECT to_regprocedure('forge_agent.coding_identity_register(text,text,text,text,jsonb,text)') IS NOT NULL AS ready",
                &[],
            )
            .map_err(work_err)?;
        if !ready {
            return Ok(None);
        }
        let params: Params = vec![Box::new(self.owner.to_string()), Box::new(workflow_id)];
        let document = self.one(
            "SELECT c.document FROM forge_agent.coding_task_identities i JOIN forge_agent.coding_identity_catalog c ON c.identity_id=i.workflow_policy_id WHERE i.owner_id=$1 AND i.workflow_id=$2 ORDER BY i.created_at LIMIT 1",
            &params,
        )?;
        let Some(document) = document else {
            return Ok(None);
        };
        let mut current = current_snapshot.clone();
        if let Some(object) = current.as_object_mut() {
            object.remove("policy_digest");
        }
        Ok(Some(document != current))
    }

    fn artifact(&mut self, task_id: Uuid, attempt_id: Uuid) -> Result<Value, WorkError> {
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(task_id),
            Box::new(attempt_id),
        ];
        let row = self.one(
            "SELECT to_jsonb(t) FROM (SELECT a.result_sha256,a.result_byte_count,a.phase,a.reservation_active,a.lease_expires_at,string_agg(c.content,'' ORDER BY c.chunk_index) AS content FROM forge_agent.coding_attempts a JOIN forge_agent.coding_result_chunks c ON(c.owner_id,c.project_alias,c.task_id,c.attempt_id)=(a.owner_id,a.project_alias,a.task_id,a.attempt_id) WHERE a.owner_id=$1 AND a.task_id=$2 AND a.attempt_id=$3 GROUP BY a.owner_id,a.project_alias,a.task_id,a.attempt_id) t",
            &params,
        )?;
        let mismatch = || WorkError::rejected(409, "result_mismatch");
        let row = row.ok_or_else(mismatch)?;
        let content = row["content"].as_str().ok_or_else(mismatch)?;
        if row["phase"].as_str() != Some("stopped")
            || row["reservation_active"].as_bool() != Some(false)
            || !row["lease_expires_at"].is_null()
            || content.len() as i64 != row["result_byte_count"].as_i64().unwrap_or(-1)
            || forge_work::canonical::sha256_hex(content.as_bytes())
                != row["result_sha256"].as_str().unwrap_or_default()
        {
            return Err(mismatch());
        }
        Ok(row)
    }

    fn owner_continuation_used(&mut self, workflow_id: Uuid) -> Result<bool, WorkError> {
        let key = format!("owner-code-resume:{workflow_id}");
        let params: &[&(dyn ToSql + Sync)] = &[&workflow_id, &key];
        self.tx
            .call_bool(
                "SELECT EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions WHERE workflow_id=$1 AND kind='decide' AND source_key=$2 AND status='completed') AS used",
                params,
            )
            .map_err(work_err)
    }

    fn revival_floor(&mut self, workflow_id: Uuid) -> Result<Option<String>, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&workflow_id];
        self.tx
            .call_text(
                "SELECT max(created_at)::text FROM forge_agent.coding_workflow_actions WHERE workflow_id=$1 AND kind='decide' AND status='completed' AND source_key LIKE 'native-revival:%'",
                params,
            )
            .map_err(work_err)
    }

    fn retained_sources(&mut self, workflow_id: Uuid) -> Result<Vec<Value>, WorkError> {
        let params: Params = vec![Box::new(workflow_id)];
        let rows = self.many(
            "SELECT finish_receipt FROM forge_agent.coding_workflow_actions WHERE workflow_id=$1 AND kind='decide' AND status='completed' AND source_key LIKE 'policy-continuation:%'",
            &params,
        )?;
        let mut bindings = Vec::new();
        for row in rows {
            if let Some(sources) = row["finish_receipt"]["receipt"]["retained_sources"].as_array() {
                bindings.extend(sources.iter().cloned());
            }
        }
        Ok(bindings)
    }

    fn queue_count(&mut self, project: &str) -> Result<i64, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&self.owner, &project];
        self.tx
            .call_scalar(
                "SELECT count(*) FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND status IN('queued','running','cancel_requested')",
                params,
            )
            .map_err(work_err)
            .map(|value: Option<i64>| value.unwrap_or(0))
    }

    fn max_review_no(&mut self, project: &str, task_id: Uuid) -> Result<Option<i64>, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&self.owner, &project, &task_id];
        self.tx
            .call_scalar(
                "SELECT max(review_no) FROM forge_agent.coding_reviews WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3",
                params,
            )
            .map_err(work_err)
    }

    // -- process / authority / revival probes ----------------------

    fn review_identity(
        &mut self,
        workflow_review_id: Uuid,
    ) -> Result<Option<(Uuid, Uuid)>, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&workflow_review_id];
        let rows = self
            .tx
            .query_rows(
                "SELECT r.source_task_id,w.root_task_id FROM forge_agent.coding_workflow_reviews r JOIN forge_agent.coding_workflows w USING(workflow_id) WHERE r.workflow_review_id=$1",
                params,
            )
            .map_err(work_err)?;
        Ok(rows.first().map(|row| {
            (
                row.get::<usize, Uuid>(0),
                row.get::<usize, Uuid>(1),
            )
        }))
    }

    fn lock_task(&mut self, task_id: Uuid) -> Result<(), WorkError> {
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(task_id),
        ];
        self.many(
            "SELECT id FROM forge_agent.coding_tasks WHERE owner_id=$1 AND id=$2 FOR UPDATE",
            &params,
        )
        .map(|_| ())
    }

    fn lease_valid(&mut self, action_id: Uuid) -> Result<bool, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&action_id];
        self.tx
            .call_bool(
                "SELECT lease_expires_at>clock_timestamp() AS valid FROM forge_agent.coding_workflow_actions WHERE action_id=$1",
                params,
            )
            .map_err(work_err)
    }

    fn action_lease_state(
        &mut self,
        action_id: Uuid,
    ) -> Result<Option<(String, Option<String>, bool)>, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&action_id];
        let rows = self
            .tx
            .query_rows(
                "SELECT status,lease_token,lease_expires_at>clock_timestamp() AS valid FROM forge_agent.coding_workflow_actions WHERE action_id=$1 FOR UPDATE",
                params,
            )
            .map_err(work_err)?;
        Ok(rows.first().map(|row| {
            (
                row.get::<usize, String>(0),
                row.get::<usize, Option<String>>(1),
                row.get::<usize, Option<bool>>(2).unwrap_or(false),
            )
        }))
    }

    fn workflow_state(
        &mut self,
        workflow_id: Uuid,
    ) -> Result<Option<(bool, String)>, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&workflow_id];
        let rows = self
            .tx
            .query_rows(
                "SELECT enabled,phase FROM forge_agent.coding_workflows WHERE workflow_id=$1",
                params,
            )
            .map_err(work_err)?;
        Ok(rows.first().map(|row| {
            (
                row.get::<usize, bool>(0),
                row.get::<usize, String>(1),
            )
        }))
    }

    fn revival_live(&mut self, workflow_id: Uuid) -> Result<bool, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&workflow_id];
        self.tx
            .call_bool(
                "SELECT EXISTS(SELECT 1 FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id) WHERE t.workflow_id=$1 AND(a.reservation_active OR a.lease_expires_at IS NOT NULL)) OR EXISTS(SELECT 1 FROM forge_agent.coding_tasks WHERE workflow_id=$1 AND status IN('queued','running','cancel_requested')) OR EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions WHERE workflow_id=$1 AND status IN('pending','leased')) AS live",
                params,
            )
            .map_err(work_err)
    }

    fn operator_cancelled(&mut self, workflow_id: Uuid) -> Result<bool, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&workflow_id];
        self.tx
            .call_bool(
                "SELECT EXISTS(SELECT 1 FROM forge_agent.coding_events e JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(e.owner_id,e.project_alias,e.task_id) WHERE t.workflow_id=$1 AND e.event_kind IN('cancelled','cancel_requested') AND(e.payload->>'cause'='operator_request' OR e.payload->'actor'->>'kind'='operator')) AS by_person",
                params,
            )
            .map_err(work_err)
    }

    // -- row writes ------------------------------------------------

    fn insert_task(&mut self, row: &Value) -> Result<(), WorkError> {
        self.insert_row("coding_tasks", row, "").map(|_| ())
    }

    fn queue_workflow_review(&mut self, row: &Value) -> Result<Option<Uuid>, WorkError> {
        let (sql, params) = self.insert_sql("coding_workflow_reviews", row);
        let sql = format!("{sql} ON CONFLICT DO NOTHING RETURNING workflow_review_id");
        let rows = self
            .tx
            .query_rows(&sql, &refs(&params))
            .map_err(work_err)?;
        Ok(rows
            .first()
            .and_then(|row| row.get::<usize, Option<Uuid>>(0)))
    }

    fn insert_action(&mut self, row: &Value) -> Result<Uuid, WorkError> {
        let (sql, params) = self.insert_sql("coding_workflow_actions", row);
        let sql = format!("{sql} RETURNING action_id");
        let rows = self
            .tx
            .query_rows(&sql, &refs(&params))
            .map_err(work_err)?;
        rows.first()
            .and_then(|row| row.get::<usize, Option<Uuid>>(0))
            .ok_or_else(|| WorkError::store("insert_action returned no action_id"))
    }

    fn insert_action_if_absent(&mut self, row: &Value) -> Result<(), WorkError> {
        self.insert_row("coding_workflow_actions", row, "ON CONFLICT DO NOTHING")
            .map(|_| ())
    }

    fn update_workflow(&mut self, workflow_id: Uuid, set: &Value) -> Result<u64, WorkError> {
        let params: Params = vec![Box::new(workflow_id)];
        self.update_row("coding_workflows", "workflow_id=$1", params, set)
    }

    fn update_task(&mut self, task_id: Uuid, set: &Value) -> Result<u64, WorkError> {
        let params: Params = vec![Box::new(self.owner.to_string()), Box::new(task_id)];
        self.update_row("coding_tasks", "owner_id=$1 AND id=$2", params, set)
    }

    fn update_workflow_review(&mut self, review_id: Uuid, set: &Value) -> Result<u64, WorkError> {
        let params: Params = vec![Box::new(review_id)];
        self.update_row(
            "coding_workflow_reviews",
            "workflow_review_id=$1",
            params,
            set,
        )
    }

    fn update_action(&mut self, action_id: Uuid, set: &Value) -> Result<u64, WorkError> {
        let params: Params = vec![Box::new(action_id)];
        self.update_row("coding_workflow_actions", "action_id=$1", params, set)
    }

    fn update_review_row(&mut self, review_id: Uuid, set: &Value) -> Result<u64, WorkError> {
        let params: Params = vec![Box::new(review_id)];
        self.update_row("coding_reviews", "review_id=$1", params, set)
    }

    fn set_workflow_phase(
        &mut self,
        workflow_id: Uuid,
        phase: &str,
    ) -> Result<Option<Uuid>, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&phase, &workflow_id, &phase];
        let rows = self
            .tx
            .query_rows(
                "UPDATE forge_agent.coding_workflows SET phase=$1,updated_at=clock_timestamp() WHERE workflow_id=$2 AND enabled AND phase NOT IN('completed','cancelled','escalated') AND phase IS DISTINCT FROM $3 RETURNING root_task_id",
                params,
            )
            .map_err(work_err)?;
        Ok(rows
            .first()
            .and_then(|row| row.get::<usize, Option<Uuid>>(0)))
    }

    fn cancel_workflow_tasks(&mut self, workflow_id: Uuid) -> Result<Vec<Value>, WorkError> {
        let params: Params = vec![Box::new(workflow_id)];
        let affected = self.many(
            "SELECT to_jsonb(t) FROM (SELECT id,project_alias,status,revision FROM forge_agent.coding_tasks WHERE workflow_id=$1 AND status NOT IN('completed','cancelled','cancel_requested') FOR UPDATE) t",
            &params,
        )?;
        self.exec(
            "UPDATE forge_agent.coding_tasks SET cancelled_at=CASE WHEN status='running' THEN NULL ELSE clock_timestamp() END,status=CASE WHEN status='running' THEN 'cancel_requested' ELSE 'cancelled' END,revision=revision+1,updated_at=clock_timestamp() WHERE workflow_id=$1 AND status NOT IN('completed','cancelled','cancel_requested')",
            &params,
        )?;
        Ok(affected)
    }

    fn escalate_workflow_tasks(&mut self, workflow_id: Uuid) -> Result<Vec<Value>, WorkError> {
        let params: Params = vec![Box::new(workflow_id)];
        let affected = self.many(
            "SELECT to_jsonb(t) FROM (SELECT id,project_alias,status,revision FROM forge_agent.coding_tasks WHERE workflow_id=$1 AND status IN('queued','running') FOR UPDATE) t",
            &params,
        )?;
        self.exec(
            "UPDATE forge_agent.coding_tasks SET cancelled_at=CASE WHEN status='queued' THEN clock_timestamp() ELSE NULL END,status=CASE WHEN status='queued' THEN 'cancelled' ELSE 'cancel_requested' END,revision=revision+1,updated_at=clock_timestamp() WHERE workflow_id=$1 AND status IN('queued','running')",
            &params,
        )?;
        Ok(affected)
    }

    fn cancel_pending_actions(&mut self, workflow_id: Uuid) -> Result<(), WorkError> {
        let params: Params = vec![Box::new(workflow_id)];
        self.exec(
            "UPDATE forge_agent.coding_workflow_actions SET status='cancelled',lease_token=NULL,lease_expires_at=NULL,updated_at=clock_timestamp() WHERE workflow_id=$1 AND status IN('pending','leased')",
            &params,
        )
        .map(|_| ())
    }

    fn handoff_pending_actions(&mut self, workflow_id: Uuid) -> Result<(), WorkError> {
        let params: Params = vec![Box::new(workflow_id)];
        self.exec(
            "UPDATE forge_agent.coding_workflow_actions SET status='handoff',lease_token=NULL,lease_expires_at=NULL,updated_at=clock_timestamp() WHERE workflow_id=$1 AND status IN('pending','leased')",
            &params,
        )
        .map(|_| ())
    }

    // -- claim + recover -------------------------------------------

    fn claim_action(&mut self, worker_id: &str) -> Result<Option<Value>, WorkError> {
        let params: Params = vec![Box::new(self.owner.to_string())];
        let candidate = self.one(
            "SELECT to_jsonb(t) FROM (SELECT a.* FROM forge_agent.coding_workflow_actions a JOIN forge_agent.coding_workflows w ON w.workflow_id=a.workflow_id WHERE a.owner_id=$1 AND a.kind IN('review','decide') AND w.enabled AND w.phase NOT IN('completed','cancelled','escalated') AND(a.status='pending' OR(a.status='leased' AND a.lease_expires_at<=clock_timestamp())) ORDER BY a.created_at FOR UPDATE OF a SKIP LOCKED LIMIT 1) t",
            &params,
        )?;
        let Some(candidate) = candidate else {
            return Ok(None);
        };
        let action_id = uuid_of(&candidate["action_id"]);
        let params: Params = vec![Box::new(worker_id.to_string()), Box::new(action_id)];
        self.one(
            "WITH u AS (UPDATE forge_agent.coding_workflow_actions SET status='leased',worker_id=$1,lease_token=gen_random_uuid(),lease_expires_at=clock_timestamp()+interval '60 seconds',updated_at=clock_timestamp() WHERE action_id=$2 RETURNING *) SELECT to_jsonb(u) FROM u",
            &params,
        )
    }

    fn recover_workflows(&mut self) -> Result<Vec<Value>, WorkError> {
        let params: Params = vec![Box::new(self.owner.to_string())];
        self.many(
            "SELECT to_jsonb(t) FROM (SELECT * FROM forge_agent.coding_workflows WHERE owner_id=$1 AND phase NOT IN('completed','cancelled','escalated') ORDER BY created_at) t",
            &params,
        )
    }

    fn recover_tasks(&mut self, workflow_id: Uuid, project: &str) -> Result<Vec<Value>, WorkError> {
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(project.to_string()),
            Box::new(workflow_id),
        ];
        self.many(
            "SELECT to_jsonb(t) FROM (SELECT t.*,a.phase AS attempt_phase,a.result_sha256,a.reservation_active,a.lease_expires_at,a.published_at,a.summary AS attempt_summary,a.outcome AS attempt_outcome FROM forge_agent.coding_tasks t LEFT JOIN forge_agent.coding_attempts a ON(a.owner_id,a.project_alias,a.task_id,a.attempt_id)=(t.owner_id,t.project_alias,t.id,t.latest_attempt_id) WHERE t.owner_id=$1 AND t.project_alias=$2 AND t.workflow_id=$3 ORDER BY t.created_at) t",
            &params,
        )
    }

    fn peer_review_state(
        &mut self,
        workflow_review_id: Uuid,
    ) -> Result<Option<Value>, WorkError> {
        let params: Params = vec![Box::new(workflow_review_id)];
        self.one(
            "SELECT to_jsonb(t) FROM (SELECT target_kind,status FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=$1) t",
            &params,
        )
    }

    fn reconciliation_exists(
        &mut self,
        project: &str,
        task_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<bool, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&self.owner, &project, &task_id, &attempt_id];
        self.tx
            .call_bool(
                "SELECT EXISTS(SELECT 1 FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND attempt_id=$4) AS ready",
                params,
            )
            .map_err(work_err)
    }

    fn latest_publication(&mut self, review_id: Uuid) -> Result<Option<Value>, WorkError> {
        let params: Params = vec![Box::new(review_id)];
        self.one(
            "SELECT to_jsonb(t) FROM (SELECT publication_id,status FROM forge_agent.coding_publications WHERE review_id=$1 ORDER BY created_at DESC LIMIT 1) t",
            &params,
        )
    }

    fn merge_action_active(&mut self, publication_id: Uuid) -> Result<bool, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&publication_id];
        self.tx
            .call_bool(
                "SELECT EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions WHERE source_publication_id=$1 AND kind='merge' AND status IN('pending','leased')) AS active",
                params,
            )
            .map_err(work_err)
    }

    fn ensure_decide_action(
        &mut self,
        workflow_review_id: Uuid,
        source_key: &str,
    ) -> Result<(), WorkError> {
        let params: Params = vec![
            Box::new(source_key.to_string()),
            Box::new(workflow_review_id),
        ];
        self.exec(
            "INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,workflow_review_id,kind,source_key) SELECT workflow_id,owner_id,project_alias,workflow_review_id,'decide',$1 FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=$2 AND status='reviewing' ON CONFLICT DO NOTHING",
            &params,
        )
        .map(|_| ())
    }

    fn hosted_merge_candidates(&mut self, workflow_id: Uuid) -> Result<Vec<Value>, WorkError> {
        let params: Params = vec![Box::new(workflow_id)];
        self.many(
            "SELECT to_jsonb(t) FROM (SELECT p.*,q.candidate_commit,q.gate_manifest_sha256 FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id,t.latest_attempt_id)=(p.owner_id,p.project_alias,p.task_id,p.attempt_id) JOIN forge_agent.coding_reviews q ON q.review_id=p.review_id WHERE t.workflow_id=$1 AND p.status='published' AND q.status='approved' AND q.repair_task_id IS NULL AND q.owner_decision->>'closed_by'='peer_reviewer' AND (SELECT h.required_green FROM forge_agent.coding_hosted_checks h WHERE h.publication_id=p.publication_id AND h.candidate_commit=p.commit_sha ORDER BY h.observed_at DESC,h.observation_id DESC LIMIT 1) IS TRUE ORDER BY p.created_at DESC) t",
            &params,
        )
    }

    fn insert_merge_action(
        &mut self,
        workflow_id: Uuid,
        project: &str,
        publication_id: Uuid,
        commit_sha: &str,
    ) -> Result<(), WorkError> {
        let source_key = format!("{publication_id}:{commit_sha}");
        let params: Params = vec![
            Box::new(workflow_id),
            Box::new(self.owner.to_string()),
            Box::new(project.to_string()),
            Box::new(source_key),
            Box::new(publication_id),
        ];
        self.exec(
            "INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,source_publication_id) VALUES($1,$2,$3,'merge',$4,$5) ON CONFLICT DO NOTHING",
            &params,
        )?;
        let params: Params = vec![Box::new(workflow_id)];
        self.exec(
            "UPDATE forge_agent.coding_workflows SET phase='merging',updated_at=clock_timestamp() WHERE workflow_id=$1 AND phase NOT IN('cancelled','escalated','completed')",
            &params,
        )
        .map(|_| ())
    }

    fn workflow_busy(&mut self, workflow_id: Uuid, check: BusyCheck) -> Result<bool, WorkError> {
        let tasks_broad = "EXISTS(SELECT 1 FROM forge_agent.coding_tasks WHERE workflow_id=$1 AND status IN('queued','running','cancel_requested','failed','needs_reconcile','quota_blocked','blocked_auth','blocked_permission','cancelled'))";
        let tasks_narrow = "EXISTS(SELECT 1 FROM forge_agent.coding_tasks WHERE workflow_id=$1 AND status IN('queued','running','cancel_requested'))";
        let attempts = "EXISTS(SELECT 1 FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id) WHERE t.workflow_id=$1 AND(a.reservation_active OR a.lease_expires_at IS NOT NULL))";
        let actions = "EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions WHERE workflow_id=$1 AND status IN('pending','leased'))";
        let pubs_any = "EXISTS(SELECT 1 FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(p.owner_id,p.project_alias,p.task_id) WHERE t.workflow_id=$1)";
        let pubs_checking = "EXISTS(SELECT 1 FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(p.owner_id,p.project_alias,p.task_id) WHERE t.workflow_id=$1 AND p.status IN('pending','checking','publishing'))";
        let reviews = "EXISTS(SELECT 1 FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id) WHERE t.workflow_id=$1 AND q.status IN('pending','checking'))";
        let workflow_reviews = "EXISTS(SELECT 1 FROM forge_agent.coding_workflow_reviews WHERE workflow_id=$1 AND status IN('pending','reviewing'))";
        let parts: Vec<&str> = match check {
            BusyCheck::PeerSource => vec![tasks_broad, attempts, actions, pubs_any, reviews],
            BusyCheck::Ingress => vec![tasks_narrow, actions, pubs_checking, reviews],
            BusyCheck::ModelRoute => {
                vec![attempts, actions, workflow_reviews, reviews, pubs_checking]
            }
            BusyCheck::Preparation => {
                vec![tasks_broad, attempts, actions, pubs_checking, reviews]
            }
        };
        let sql = format!("SELECT {} AS busy", parts.join(" OR "));
        let params: &[&(dyn ToSql + Sync)] = &[&workflow_id];
        self.tx.call_bool(&sql, params).map_err(work_err)
    }

    fn writer_active(&mut self, workflow_id: Uuid) -> Result<bool, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&workflow_id];
        self.tx
            .call_bool(
                "SELECT EXISTS(SELECT 1 FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id) WHERE t.workflow_id=$1 AND(a.reservation_active OR a.lease_expires_at IS NOT NULL)) AS writer",
                params,
            )
            .map_err(work_err)
    }

    // -- retry allowance amendment ---------------------------------

    fn amend_policy_available(&mut self) -> Result<bool, WorkError> {
        self.tx
            .call_bool(
                "SELECT to_regprocedure('forge_agent.amend_agent_retry_policy(text,uuid,jsonb)') IS NOT NULL AS available",
                &[],
            )
            .map_err(work_err)
    }

    fn allowance_workflows(
        &mut self,
        position: Option<(&str, Uuid)>,
    ) -> Result<Vec<Value>, WorkError> {
        let (created_at, workflow_id) = match position {
            Some((created_at, workflow_id)) => (Some(created_at.to_string()), Some(workflow_id)),
            None => (None, None),
        };
        let projects: Vec<String> = self.allowed_projects.to_vec();
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(projects),
            Box::new(created_at.clone()),
            Box::new(created_at),
            Box::new(workflow_id),
        ];
        self.many(
            "SELECT to_jsonb(t) FROM (SELECT w.* FROM forge_agent.coding_workflows w JOIN forge_agent.coding_review_budget b ON(b.owner_id,b.project_alias,b.root_task_id)=(w.owner_id,w.project_alias,w.root_task_id) WHERE w.owner_id=$1 AND w.project_alias=ANY($2) AND w.phase NOT IN('completed','cancelled') AND ((w.snapshot->>'max_plan_revisions')::integer<20 OR (w.snapshot->>'max_code_revisions')::integer<20 OR b.policy->'max_rounds'<>'20'::jsonb OR b.policy->'max_stagnant_rounds'<>'20'::jsonb OR b.policy->'max_infrastructure_retries'<>'20'::jsonb) AND NOT EXISTS(SELECT FROM forge_agent.coding_events e WHERE(e.owner_id,e.project_alias,e.task_id)=(w.owner_id,w.project_alias,w.root_task_id) AND e.event_kind='checkpoint' AND e.payload->>'stage'='retry_policy_updated' AND e.payload->>'policy'='forge-agent-retry-v1' AND e.payload->>'scope_id'=w.workflow_id::text) AND ($3::timestamptz IS NULL OR (w.created_at,w.workflow_id)>($4::timestamptz,$5::uuid)) ORDER BY w.created_at,w.workflow_id LIMIT 64) t",
            &params,
        )
    }

    fn allowance_manual_tasks(
        &mut self,
        repair_policy: &Value,
        position: Option<(&str, Uuid)>,
    ) -> Result<Vec<Value>, WorkError> {
        let (created_at, task_id) = match position {
            Some((created_at, task_id)) => (Some(created_at.to_string()), Some(task_id)),
            None => (None, None),
        };
        let projects: Vec<String> = self.allowed_projects.to_vec();
        let params: Params = vec![
            Box::new(self.owner.to_string()),
            Box::new(projects),
            Box::new(repair_policy.clone()),
            Box::new(repair_policy.clone()),
            Box::new(created_at.clone()),
            Box::new(created_at),
            Box::new(task_id),
        ];
        self.many(
            "SELECT to_jsonb(t) FROM (SELECT t.project_alias,t.id,t.created_at,t.agent_profile,t.mode,b.policy FROM forge_agent.coding_tasks t JOIN forge_agent.coding_review_budget b ON(b.owner_id,b.project_alias,b.root_task_id)=(t.owner_id,t.project_alias,t.id) WHERE t.owner_id=$1 AND t.project_alias=ANY($2) AND t.workflow_id IS NULL AND (t.status NOT IN('completed','cancelled') OR EXISTS(SELECT FROM forge_agent.coding_tasks child WHERE(child.owner_id,child.project_alias,child.repair_root_task_id)=(t.owner_id,t.project_alias,t.id) AND child.status NOT IN('completed','cancelled'))) AND b.policy IS DISTINCT FROM $3 AND (b.policy-ARRAY['max_rounds','max_stagnant_rounds','max_infrastructure_retries'])=($4::jsonb-ARRAY['max_rounds','max_stagnant_rounds','max_infrastructure_retries']) AND ($5::timestamptz IS NULL OR (t.created_at,t.id)>($6::timestamptz,$7::uuid)) ORDER BY t.created_at,t.id LIMIT 64) t",
            &params,
        )
    }

    fn amend_retry_allowance(
        &mut self,
        project: &str,
        root_task_id: Uuid,
        policy: &Value,
    ) -> Result<(), WorkError> {
        self.savepoint("retry_allowance_scope")?;
        let params: &[&(dyn ToSql + Sync)] = &[&project, &root_task_id, policy];
        let result = self.tx.call_void(
            "SELECT forge_agent.amend_agent_retry_policy($1,$2,$3)",
            params,
        );
        let outcome = match result {
            Ok(()) => Ok(()),
            Err(error) => {
                self.rollback_to_savepoint("retry_allowance_scope")?;
                match &error {
                    AdapterError::Sql { state, .. }
                        if state.as_deref() == Some("P0009")
                            || state.as_deref() == Some("P0006") =>
                    {
                        Ok(())
                    }
                    _ => Err(work_err(error)),
                }
            }
        };
        self.release_savepoint("retry_allowance_scope")?;
        outcome
    }
}

/// The Python provider raises ValueError for these; the closest
/// WorkError is a 400 rejection, which process() escalates exactly
/// as it escalates a ValueError.
fn invalid(_message: String) -> WorkError {
    WorkError::rejected(400, "invalid_request")
}

impl RetryEvents for WorkflowTxImpl<'_, '_, '_, '_> {
    fn observe_failure(
        &mut self,
        scope: &RetryScope,
        kind: &str,
        source_id: &str,
        digest: &str,
        passed_kinds: &[String],
        phase_id: &str,
        passed_phase_ids: &[String],
    ) -> Result<Value, WorkError> {
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
        scope: &RetryScope,
        kind: &str,
        source_id: &str,
        digest: &str,
        phase_id: Option<&str>,
    ) -> Result<Value, WorkError> {
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

    fn resolve_open_scope(&mut self, scope: &RetryScope) -> Result<i64, WorkError> {
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

    fn project(&mut self, scope: &RetryScope) -> Result<Option<Value>, WorkError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn column_types_follow_the_python_row_shapes() {
        // owner_id and worker_id are names, not uuids, despite the
        // _id suffix.
        assert_eq!(col_type("coding_tasks", "owner_id", &json!("lrocha")), Ty::Text);
        assert_eq!(col_type("coding_workflow_actions", "worker_id", &json!("w")), Ty::Text);
        assert_eq!(col_type("coding_tasks", "workflow_id", &json!("x")), Ty::Uuid);
        assert_eq!(col_type("coding_tasks", "id", &json!("x")), Ty::Uuid);
        assert_eq!(col_type("coding_tasks", "created_at", &json!("x")), Ty::Ts);
        assert_eq!(col_type("coding_tasks", "revision", &json!(3)), Ty::Int);
        assert_eq!(col_type("coding_tasks", "enabled", &json!(true)), Ty::Bool);
        assert_eq!(col_type("coding_tasks", "snapshot", &json!({})), Ty::Json);
        assert_eq!(col_type("coding_tasks", "title", &json!("t")), Ty::Text);
        // A null falls back to the name rules.
        assert_eq!(col_type("coding_tasks", "plan_decision_id", &Value::Null), Ty::Uuid);
        assert_eq!(col_type("coding_tasks", "cancelled_at", &Value::Null), Ty::Ts);
    }

    #[test]
    fn placeholders_carry_the_casts() {
        assert_eq!(placeholder(2, Ty::Ts), "$2::timestamptz");
        assert_eq!(placeholder(3, Ty::Json), "$3::jsonb");
        assert_eq!(placeholder(1, Ty::Uuid), "$1::uuid");
        assert_eq!(placeholder(4, Ty::Text), "$4");
    }

    #[test]
    fn uuid_of_is_lenient() {
        let id = Uuid::new_v4();
        assert_eq!(uuid_of(&json!(id.to_string())), id);
        assert_eq!(uuid_of(&json!("nope")), Uuid::default());
        assert_eq!(uuid_of(&Value::Null), Uuid::default());
    }
}
