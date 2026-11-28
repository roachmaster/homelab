//! Runner host composition (runner.py's build_work_and_usage_
//! services + work_api.WorkServiceFactory): the database config
//! for the runner's fixed direct connection, the work API
//! service over the Pg ports, its catalog and policy-identity
//! ports, the external/native control adapters, the coordinator
//! builder for the background thread, and the usage service.
//!
//! Recorded boundaries (forge-ops-design.yaml h7e):
//! - The runner connects directly (host /var/run/postgresql,
//!   forge_agent) as the process user with the libpq passfile;
//!   Python's proxy switch stays retired, as everywhere else.
//! - The Pg submit's documented v1 boundary stands: delivery
//!   admission and external-profile enqueue inside the submit
//!   transaction are not wired through the store; the external
//!   workflow store is composed for control, catalog and the
//!   coordinator hooks.
//! - The workflow store is built twice (once for the API's
//!   native-control provider behind a mutex, once for the
//!   coordinator thread): the providers are stateless over the
//!   durable state, the same instance-per-consumer pattern as
//!   the publication composition.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use forge_work::catalog::{AgentProfile, ProjectCatalog, Recipe, WorkCatalog};
use forge_work::external_dto::ExternalReadiness;
use forge_work::external_workflow::ExternalWorkflowStore;
use forge_work::flow_config::ExternalWorkflowPolicy;
use forge_work::model_route::ExecutionPolicyProvider;
use forge_work::owner_control::OwnerControlProvider;
use forge_work::reviewer_correction::{ReviewerCorrectionProvider, ReviewerCorrectionResume};
use forge_work::work_api::{
    ExternalControlPort, NativeControlPort, PolicyIdentityPort, WorkApiService, WorkCatalogPort,
    WorkResume, WorkReviewDecide,
};
use forge_work::work_config::WorkApiConfig;
use forge_work::work_error::WorkError;
use forge_work::workflow::{ExternalWorkflowHook, RunOnceHook};
use forge_work::workflow_factory::{WorkflowCoordinatorFactory, WorkflowStoreFactory};
use forge_work::workflow_policy::CatalogProfile;

use crate::client::PgSession;
use crate::config::PgConfig;
use crate::external_adapter::PgExternalTxFactory;
use crate::factory::PgSessionFactory;
use crate::work_common::{format_ts, work_err};
use crate::work_store::{PgWorkStore, ReviewPolicy};
use crate::worker_composition::passfile_password;
use crate::workflow_adapter::PgWorkflowTxFactory;
use crate::workflow_decision::PgWorkflowDecisions;

const RUNNER_DB_HOST: &str = "/var/run/postgresql";
const RUNNER_DB_NAME: &str = "forge_agent";
const WORKER_FRESHNESS_SECONDS: i32 = 90;
const MODE_ORDER: [&str; 3] = ["plan", "review", "implement"];

/// The process identity the runner's database config is resolved
/// from. The composition root reads the environment once and hands
/// the values in, so this library never reads process state itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunnerPgEnv {
    /// `PGUSER`; an empty value counts as unset.
    pub pguser: Option<String>,
    /// `HOME`, where the libpq passfile lives; empty when unset.
    pub home: String,
}

impl RunnerPgEnv {
    /// Select the two runner database settings from an already
    /// collected environment map.
    pub fn from_environ(environ: &std::collections::HashMap<String, String>) -> Self {
        RunnerPgEnv {
            pguser: environ.get("PGUSER").cloned(),
            home: environ.get("HOME").cloned().unwrap_or_default(),
        }
    }
}

/// The runner's database config: Python's queue_store.connect
/// direct arm (socket dir, forge_agent) with the process user
/// and the libpq passfile password.
pub fn runner_pg_config(owner: &str, env: &RunnerPgEnv) -> Result<PgConfig, String> {
    let user = env
        .pguser
        .clone()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            nix::unistd::User::from_uid(nix::unistd::getuid())
                .ok()
                .flatten()
                .map(|user| user.name)
        })
        .ok_or("Unable to determine the runner database user")?;
    let home = env.home.as_str();
    let passfile = std::path::Path::new(home).join(".pgpass");
    let password = if passfile.exists() {
        passfile_password(&passfile, RUNNER_DB_HOST, 5432, RUNNER_DB_NAME, &user)
    } else {
        None
    };
    Ok(PgConfig {
        host: RUNNER_DB_HOST.to_string(),
        port: 5432,
        dbname: RUNNER_DB_NAME.to_string(),
        user,
        password,
        owner: owner.to_string(),
        connect_timeout: std::time::Duration::from_secs(4),
        statement_timeout: std::time::Duration::from_secs(10),
    })
}

fn review_policy(config: &WorkApiConfig) -> ReviewPolicy {
    let policy = &config.repair_policy;
    let text = |key: &str| {
        policy
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let number = |key: &str| policy.get(key).and_then(Value::as_i64).unwrap_or(20);
    ReviewPolicy {
        version: text("version"),
        max_rounds: number("max_rounds"),
        max_stagnant_rounds: number("max_stagnant_rounds"),
        max_infrastructure_retries: number("max_infrastructure_retries"),
        required_hosted_checks: policy
            .get("required_hosted_checks")
            .and_then(Value::as_array)
            .map(|checks| {
                checks
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn pg_work_store(config: &WorkApiConfig, pg: &PgConfig) -> PgWorkStore {
    PgWorkStore::new(
        PgSessionFactory::new(pg.clone()),
        config.owner.clone(),
        config.allowed_projects.clone(),
        config.max_pending,
        review_policy(config),
    )
}

// ---------------------------------------------------------------------
// Policy identity port (WorkStore.policy_identity)
// ---------------------------------------------------------------------

pub struct PgPolicyIdentity {
    store: PgWorkStore,
}

impl PolicyIdentityPort for PgPolicyIdentity {
    fn policy_identity(&self, project_alias: &str, kind: &str, snapshot: &Value) -> Option<Value> {
        let store = &self.store;
        let digest = snapshot.get("policy_digest")?.as_str()?;
        let document = policy_document(snapshot);
        let result = crate::work_common::run_tx(store, |tx| {
            if !identity_ready(tx)? {
                return Ok(None);
            }
            register_identity(tx, &store.owner, project_alias, kind, &document, digest)
        });
        result.ok().flatten()
    }
}

fn policy_document(snapshot: &Value) -> Value {
    let mut document = snapshot.clone();
    if let Some(map) = document.as_object_mut() {
        map.remove("policy_digest");
    }
    document
}

// ---------------------------------------------------------------------
// Catalog port (WorkStore.catalog)
// ---------------------------------------------------------------------

pub struct PgWorkCatalog {
    pg: PgConfig,
    owner: String,
    has_external_profiles: bool,
    delivery_enabled: bool,
    retry_events: bool,
}

/// work_config's profile label: the frozen label or the alias.
fn profile_label(config: &WorkApiConfig, alias: &str) -> String {
    config
        .profile(alias)
        .and_then(|profile| profile.label.clone())
        .unwrap_or_else(|| alias.to_string())
}

impl PgWorkCatalog {
    fn session(&self) -> Result<PgSession, WorkError> {
        PgSession::connect(self.pg.clone())
            .map_err(|error| WorkError::store(format!("catalog session: {error}")))
    }

    fn external_entries(&self) -> Result<Vec<ExternalReadiness>, WorkError> {
        if !self.has_external_profiles {
            return Ok(Vec::new());
        }
        let mut session = self.session()?;
        let params: &[&(dyn ToSql + Sync)] = &[&self.owner];
        let entries = session
            .call_json(
                "SELECT forge_external_agent_api.catalogue($1) AS entries",
                params,
            )
            .map_err(work_err)?
            .unwrap_or(Value::Null);
        let list = entries
            .as_array()
            .filter(|list| list.len() <= 256)
            .ok_or_else(|| WorkError::store("Invalid external readiness collection"))?;
        list.iter()
            .map(|entry| ExternalReadiness::from_value(entry).map_err(WorkError::store))
            .collect()
    }
}

impl WorkCatalogPort for PgWorkCatalog {
    fn catalog(&self, config: &WorkApiConfig) -> Result<Value, WorkError> {
        if config.owner != self.owner {
            return Err(WorkError::store(
                "Catalog requires matching frozen WorkConfig",
            ));
        }
        let mut session = self.session()?;
        let external = self.external_entries()?;
        let mut worker_status = "unknown";
        let mut observed_at: Option<String> = None;
        let mut advertised_revisions: BTreeMap<String, Option<String>> = BTreeMap::new();
        let mut advertised_modes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

        let rows = {
            let params: &[&(dyn ToSql + Sync)] = &[&self.owner, &WORKER_FRESHNESS_SECONDS];
            session
                .query_rows(
                    "SELECT worker_id, policy_digest, projects, profiles, observed_at, (observed_at >= clock_timestamp() - make_interval(secs => $2)) AS is_fresh FROM forge_agent.coding_worker_readiness WHERE owner_id = $1 ORDER BY observed_at DESC, worker_id ASC",
                    params,
                )
                .map_err(work_err)?
        };
        if let Some(first) = rows.first() {
            let observed: chrono::DateTime<chrono::Utc> = first
                .try_get("observed_at")
                .map_err(|e| WorkError::store(e.to_string()))?;
            observed_at = Some(format_ts(observed));
            worker_status = if rows
                .iter()
                .any(|row| row.try_get::<_, bool>("is_fresh").unwrap_or(false))
            {
                "ready"
            } else {
                "offline"
            };
        }
        for entry in &external {
            let Some(profile) = config.profile(&entry.profile_alias) else {
                continue;
            };
            if !config
                .allowed_projects
                .iter()
                .any(|p| p == &entry.project_alias)
                || !profile.is_external()
                || !profile.execution_enabled
            {
                continue;
            }
            if let Some(entry_observed) = &entry.observed_at {
                if observed_at
                    .as_ref()
                    .map(|current| entry_observed > current)
                    .unwrap_or(true)
                {
                    observed_at = Some(entry_observed.clone());
                }
            }
            if entry.is_fresh {
                worker_status = "ready";
                let policy_modes: &[&str] =
                    match forge_work::pinned_recipe::external_policy(&profile.resource_profile) {
                        Some((_, modes)) => modes,
                        None => &[],
                    };
                let modes: BTreeSet<String> = config
                    .allowed_modes
                    .iter()
                    .filter(|mode| policy_modes.contains(&mode.as_str()))
                    .cloned()
                    .collect();
                advertised_modes
                    .entry(entry.profile_alias.clone())
                    .or_default()
                    .extend(modes);
            } else if worker_status == "unknown" {
                worker_status = "offline";
            }
        }
        let native_digests: BTreeSet<String> = forge_work::workflow::NATIVE_POLICY_DIGESTS
            .iter()
            .map(|digest| digest.to_string())
            .collect();
        for row in &rows {
            if !row.try_get::<_, bool>("is_fresh").unwrap_or(false) {
                continue;
            }
            let projects: Value = row
                .try_get("projects")
                .map_err(|e| WorkError::store(e.to_string()))?;
            if let Some(entries) = projects.as_array() {
                for entry in entries {
                    let Some(alias) = entry.get("alias").and_then(Value::as_str) else {
                        continue;
                    };
                    if !config.allowed_projects.iter().any(|p| p == alias)
                        || advertised_revisions.contains_key(alias)
                    {
                        continue;
                    }
                    let revision = entry.get("base_revision");
                    let valid = match revision {
                        None | Some(Value::Null) => true,
                        Some(Value::String(text)) => {
                            text.len() == 40 && text.chars().all(|c| c.is_ascii_hexdigit())
                        }
                        _ => false,
                    };
                    if valid {
                        advertised_revisions.insert(
                            alias.to_string(),
                            revision.and_then(Value::as_str).map(str::to_string),
                        );
                    }
                }
            }
            let digest: String = row
                .try_get("policy_digest")
                .map_err(|e| WorkError::store(e.to_string()))?;
            if !native_digests.contains(&digest) {
                continue;
            }
            let profiles: Value = row
                .try_get("profiles")
                .map_err(|e| WorkError::store(e.to_string()))?;
            if let Some(entries) = profiles.as_array() {
                for entry in entries {
                    let Some(alias) = entry.get("alias").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(profile) = config.profile(alias) else {
                        continue;
                    };
                    if !profile.execution_enabled
                        || !forge_work::work_config::native_policy_matches(
                            Some(&profile.resource_profile),
                            Some(&profile.backend),
                            Some(&digest),
                        )
                    {
                        continue;
                    }
                    if entry.get("backend").and_then(Value::as_str) != Some(&profile.backend)
                        || entry.get("model").and_then(Value::as_str) != Some(&profile.model)
                    {
                        continue;
                    }
                    let Some(modes) = entry.get("modes").and_then(Value::as_array) else {
                        continue;
                    };
                    let valid: BTreeSet<String> = modes
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|mode| config.allowed_modes.iter().any(|m| m == mode))
                        .map(str::to_string)
                        .collect();
                    if !valid.is_empty() {
                        advertised_modes
                            .entry(alias.to_string())
                            .or_default()
                            .extend(valid);
                    }
                }
            }
        }
        {
            let params: &[&(dyn ToSql + Sync)] = &[&self.owner];
            let merged = session
                .query_rows(
                    "SELECT DISTINCT ON (a.project_alias) a.project_alias, a.finish_receipt->'receipt'->>'merge_commit' AS merge_commit FROM forge_agent.coding_workflow_actions a JOIN forge_agent.coding_workflows w USING (workflow_id) WHERE a.owner_id = $1 AND a.kind = 'merge' AND a.status = 'completed' AND a.finish_receipt->>'status' = 'merged' AND a.finish_receipt->'receipt'->>'merge_commit' = a.intent->>'merge_commit' AND w.phase = 'completed' ORDER BY a.project_alias, a.updated_at DESC, a.action_id DESC",
                    params,
                )
                .map_err(work_err)?;
            for row in &merged {
                let alias: String = row
                    .try_get("project_alias")
                    .map_err(|e| WorkError::store(e.to_string()))?;
                if config.project_workflow(&alias).is_some() {
                    let commit: Option<String> = row
                        .try_get("merge_commit")
                        .map_err(|e| WorkError::store(e.to_string()))?;
                    advertised_revisions.insert(alias, commit);
                }
            }
        }

        let mut projects = Vec::new();
        for alias in config.allowed_projects.iter().take(64) {
            let workflow = match config.project_workflow(alias) {
                Some(policy) => {
                    let snapshot = policy.snapshot(
                        config,
                        &config.repair_policy,
                        &forge_work::work_config::native_policy_matches,
                    )?;
                    Some(project_summary(&snapshot))
                }
                None => None,
            };
            let external_workflow = config
                .external_workflow(alias)
                .and_then(|policy| policy.summary(config, &config.native_policy_ref()).ok());
            let mut project = Map::new();
            project.insert("alias".to_string(), json!(alias));
            project.insert("label".to_string(), json!(config.project_label(alias)));
            project.insert(
                "base_revision".to_string(),
                advertised_revisions
                    .get(alias)
                    .cloned()
                    .flatten()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            );
            project.insert("workflow".to_string(), workflow.unwrap_or(Value::Null));
            project.insert(
                "external_workflow".to_string(),
                external_workflow.unwrap_or(Value::Null),
            );
            projects.push(Value::Object(project));
        }
        self.attach_policy_identities(&mut projects, config)?;
        if self.delivery_enabled {
            for project in &mut projects {
                let alias = project["alias"].as_str().unwrap_or_default().to_string();
                if let Some(head) = self.delivery_head(&mut session, &alias)? {
                    project["base_revision"] = json!(head);
                }
                if let Some(delivery) = self.delivery_projection(&mut session, &alias)? {
                    project["delivery"] = delivery;
                }
            }
        }
        let mut profiles = Vec::new();
        let mut enabled: Vec<&forge_work::work_config::ApiProfile> = config
            .profiles
            .iter()
            .filter(|profile| profile.execution_enabled)
            .collect();
        enabled.sort_by(|a, b| a.alias.cmp(&b.alias));
        for profile in enabled.into_iter().take(64) {
            let Some(modes) = advertised_modes.get(&profile.alias) else {
                continue;
            };
            if modes.is_empty() {
                continue;
            }
            let mut entry = Map::new();
            entry.insert("alias".to_string(), json!(profile.alias));
            entry.insert(
                "label".to_string(),
                json!(profile_label(config, &profile.alias)),
            );
            entry.insert("backend".to_string(), json!(profile.backend));
            entry.insert("model".to_string(), json!(profile.model));
            entry.insert(
                "modes".to_string(),
                json!(MODE_ORDER
                    .iter()
                    .filter(|mode| modes.contains(**mode))
                    .collect::<Vec<_>>()),
            );
            entry.insert("branch".to_string(), json!(profile.branch));
            if profile.is_external() {
                entry.insert("transport".to_string(), json!("external_db"));
            }
            profiles.push(Value::Object(entry));
        }
        let agent_branches = self.agent_branch_states(&mut session, config)?;
        Ok(json!({
            "projects": projects,
            "profiles": profiles,
            "worker_status": worker_status,
            "observed_at": observed_at,
            "agent_branches": agent_branches,
        }))
    }
}

fn project_summary(snapshot: &Value) -> Value {
    let mut summary = Map::new();
    for key in [
        "policy_digest",
        "author_profile",
        "reviewer_profile",
        "max_plan_revisions",
        "max_code_revisions",
        "auto_merge",
    ] {
        summary.insert(
            key.to_string(),
            snapshot.get(key).cloned().unwrap_or(Value::Null),
        );
    }
    Value::Object(summary)
}

/// Whether the identity catalog (M44) exists yet
/// (WorkStore._identity_catalog_ready).
fn identity_ready(tx: &mut crate::tx::Tx<'_, '_>) -> Result<bool, WorkError> {
    let rows = tx
        .query_rows(
            "SELECT to_regprocedure('forge_agent.coding_identity_register(text,text,text,text,jsonb,text)') IS NOT NULL AS ready",
            &[],
        )
        .map_err(work_err)?;
    Ok(rows
        .first()
        .and_then(|row| row.try_get("ready").ok())
        .unwrap_or(false))
}

/// Registers one identity and returns the wire identity object
/// (WorkStore._register_identity).
fn register_identity(
    tx: &mut crate::tx::Tx<'_, '_>,
    owner: &str,
    project_alias: &str,
    kind: &str,
    document: &Value,
    legacy_digest: &str,
) -> Result<Option<Value>, WorkError> {
    let subject = "policy";
    let params: &[&(dyn ToSql + Sync)] = &[
        &owner,
        &project_alias,
        &kind,
        &subject,
        document,
        &legacy_digest,
    ];
    let rows = tx
        .query_rows(
            "SELECT identity_id, generation FROM forge_agent.coding_identity_register($1,$2,$3,$4,$5,$6)",
            params,
        )
        .map_err(work_err)?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let identity_id: Uuid = row
        .try_get("identity_id")
        .map_err(|e| WorkError::store(e.to_string()))?;
    let generation: i64 = row
        .try_get("generation")
        .map_err(|e| WorkError::store(e.to_string()))?;
    Ok(Some(json!({
        "policy_id": identity_id.to_string(),
        "policy_generation": generation,
    })))
}

impl PgWorkCatalog {
    /// _attach_policy_identities: register each advertised
    /// workflow/external policy snapshot and merge its identity
    /// into the summary object.
    fn attach_policy_identities(
        &self,
        projects: &mut [Value],
        config: &WorkApiConfig,
    ) -> Result<(), WorkError> {
        let mut pending: Vec<(usize, &'static str, String, Value)> = Vec::new();
        for (index, project) in projects.iter().enumerate() {
            let alias = project["alias"].as_str().unwrap_or_default().to_string();
            if project["workflow"] != Value::Null {
                if let Some(policy) = config.project_workflow(&alias) {
                    let snapshot = policy.snapshot(
                        config,
                        &config.repair_policy,
                        &forge_work::work_config::native_policy_matches,
                    )?;
                    pending.push((index, "workflow_policy", alias.clone(), snapshot));
                }
            }
            if project["external_workflow"] != Value::Null {
                if let Some(policy) = config.external_workflow(&alias) {
                    let snapshot = policy
                        .snapshot(config, &config.native_policy_ref())
                        .map_err(WorkError::store)?;
                    pending.push((index, "external_workflow_policy", alias, snapshot));
                }
            }
        }
        if pending.is_empty() {
            return Ok(());
        }
        let store = pg_work_store(config, &self.pg);
        let identities = crate::work_common::run_tx(&store, |tx| {
            if !identity_ready(tx)? {
                return Ok(Vec::new());
            }
            let mut resolved = Vec::new();
            for (index, kind, alias, snapshot) in &pending {
                let digest = snapshot
                    .get("policy_digest")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let document = policy_document(snapshot);
                if let Some(identity) =
                    register_identity(tx, &store.owner, alias, kind, &document, digest)?
                {
                    resolved.push((*index, *kind, identity));
                }
            }
            Ok(resolved)
        })?;
        for (index, kind, identity) in identities {
            let key = if kind == "workflow_policy" {
                "workflow"
            } else {
                "external_workflow"
            };
            if let (Value::Object(summary), Value::Object(identity)) =
                (&mut projects[index][key], &identity)
            {
                for (field, value) in identity {
                    summary.insert(field.clone(), value.clone());
                }
            }
        }
        Ok(())
    }

    fn delivery_head(
        &self,
        session: &mut PgSession,
        project_alias: &str,
    ) -> Result<Option<String>, WorkError> {
        let params: &[&(dyn ToSql + Sync)] = &[&project_alias];
        let value = session
            .call_json(
                "SELECT forge_agent.delivery_project_head($1) AS value",
                params,
            )
            .map_err(work_err)?
            .unwrap_or(Value::Null);
        Ok(value
            .get("commit_sha")
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    fn delivery_projection(
        &self,
        session: &mut PgSession,
        project_alias: &str,
    ) -> Result<Option<Value>, WorkError> {
        let function = if self.retry_events {
            "delivery_retry_projection"
        } else {
            "delivery_projection"
        };
        let sql = format!("SELECT forge_agent.{function}($1, $2) AS value");
        let task: Option<Uuid> = None;
        let params: &[&(dyn ToSql + Sync)] = &[&project_alias, &task];
        let value = session
            .call_json(&sql, params)
            .map_err(work_err)?
            .unwrap_or(Value::Null);
        Ok(if value.is_null() { None } else { Some(value) })
    }

    /// _agent_branch_states: live rows from agent_branch_state
    /// when the table is visible, with the config-derived
    /// default DTO for every other (project, profile) pair.
    fn agent_branch_states(
        &self,
        session: &mut PgSession,
        config: &WorkApiConfig,
    ) -> Result<Value, WorkError> {
        let mut live: BTreeMap<(String, String), Value> = BTreeMap::new();
        let has_table = session
            .call_bool(
                "SELECT to_regclass('forge_agent.agent_branch_state') IS NOT NULL AND has_table_privilege(current_user, 'forge_agent.agent_branch_state', 'SELECT') AS has_table",
                &[],
            )
            .map_err(work_err)?;
        if has_table {
            let projects: Vec<String> = config.allowed_projects.clone();
            let params: &[&(dyn ToSql + Sync)] = &[&self.owner, &projects];
            let rows = session
                .query_rows(
                    "SELECT project_alias, agent_profile, branch_name, branch_scope, head_commit_sha, fetched_main_sha, fetched_main_at, sync_status, conflict_paths, cache_status, cache_updated_at, last_task_id, last_correlation_id, reservation_status, active_scope_kind, active_scope_id, active_root_task_id, active_task_id, active_pr_url, reservation_epoch, reservation_blocker FROM forge_agent.agent_branch_state WHERE owner_id = $1 AND project_alias = ANY($2) ORDER BY project_alias ASC, agent_profile ASC",
                    params,
                )
                .map_err(work_err)?;
            for row in &rows {
                let dto = branch_row_dto(row)?;
                let key = (
                    dto["project_alias"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    dto["agent_profile"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                );
                live.insert(key, dto);
            }
        }
        let mut result = Vec::new();
        for project_alias in config.allowed_projects.iter().take(64) {
            let mut profiles: Vec<&forge_work::work_config::ApiProfile> =
                config.profiles.iter().collect();
            profiles.sort_by(|a, b| a.alias.cmp(&b.alias));
            for profile in profiles.into_iter().take(64) {
                if let Some(existing) = live.get(&(project_alias.clone(), profile.alias.clone())) {
                    result.push(existing.clone());
                } else {
                    let scope =
                        forge_work_worker::agent_branch::branch_scope_for_branch(&profile.branch)
                            .map_err(|error| WorkError::store(error.to_string()))?;
                    result.push(json!({
                        "project_alias": project_alias,
                        "agent_profile": profile.alias,
                        "branch_name": profile.branch,
                        "branch_scope": scope,
                        "head_commit_sha": null,
                        "fetched_main_sha": null,
                        "fetched_main_at": null,
                        "sync_status": "unknown",
                        "conflict_paths": [],
                        "cache_status": "unknown",
                        "cache_updated_at": null,
                        "last_task_id": null,
                        "last_correlation_id": null,
                        "reservation_status": "idle",
                        "active_scope_kind": null,
                        "active_scope_id": null,
                        "active_root_task_id": null,
                        "active_task_id": null,
                        "active_pr_url": null,
                        "reservation_epoch": 0,
                        "reservation_blocker": null,
                    }));
                }
                if result.len() >= 128 {
                    return Ok(Value::Array(result));
                }
            }
        }
        Ok(Value::Array(result))
    }
}

fn branch_row_dto(row: &tokio_postgres::Row) -> Result<Value, WorkError> {
    let text = |name: &str| -> Result<Option<String>, WorkError> {
        row.try_get(name)
            .map_err(|e| WorkError::store(e.to_string()))
    };
    let stamp = |name: &str| -> Result<Option<String>, WorkError> {
        let value: Option<chrono::DateTime<chrono::Utc>> = row
            .try_get(name)
            .map_err(|e| WorkError::store(e.to_string()))?;
        Ok(value.map(format_ts))
    };
    let uuid = |name: &str| -> Result<Option<String>, WorkError> {
        let value: Option<Uuid> = row
            .try_get(name)
            .map_err(|e| WorkError::store(e.to_string()))?;
        Ok(value.map(|value| value.to_string()))
    };
    let conflicts: Value = row
        .try_get("conflict_paths")
        .map_err(|e| WorkError::store(e.to_string()))?;
    let epoch: i64 = row
        .try_get("reservation_epoch")
        .map_err(|e| WorkError::store(e.to_string()))?;
    Ok(json!({
        "project_alias": text("project_alias")?,
        "agent_profile": text("agent_profile")?,
        "branch_name": text("branch_name")?,
        "branch_scope": text("branch_scope")?,
        "head_commit_sha": text("head_commit_sha")?,
        "fetched_main_sha": text("fetched_main_sha")?,
        "fetched_main_at": stamp("fetched_main_at")?,
        "sync_status": text("sync_status")?,
        "conflict_paths": conflicts,
        "cache_status": text("cache_status")?,
        "cache_updated_at": stamp("cache_updated_at")?,
        "last_task_id": uuid("last_task_id")?,
        "last_correlation_id": uuid("last_correlation_id")?,
        "reservation_status": text("reservation_status")?,
        "active_scope_kind": text("active_scope_kind")?,
        "active_scope_id": text("active_scope_id")?,
        "active_root_task_id": uuid("active_root_task_id")?,
        "active_task_id": uuid("active_task_id")?,
        "active_pr_url": text("active_pr_url")?,
        "reservation_epoch": epoch,
        "reservation_blocker": text("reservation_blocker")?,
    }))
}

// ---------------------------------------------------------------------
// Workflow-thread control channel
//
// The workflow store, the external workflow store and the
// coordinator are single-threaded by construction (their hook
// and grant slots are RefCells and their catalog/tx trait
// objects carry no Send bound), exactly the state Python shares
// under the GIL. The Rust host therefore owns them on one
// workflow thread; the API's native/external control ports are
// channel adapters that hand the command to that thread and
// wait for its reply.
// ---------------------------------------------------------------------

type Reply<T> = std::sync::mpsc::Sender<Result<T, WorkError>>;

enum ControlRequest {
    NativeResume(WorkResume, Reply<Option<Value>>),
    NativeReviewDecide(WorkReviewDecide, Reply<Option<Value>>),
    ExternalCancel {
        project_alias: String,
        task_id: Uuid,
        expected_revision: i64,
        idempotency_key: String,
        actor: forge_work::work_actors::Actor,
        reply: Reply<Option<Value>>,
    },
    ExternalForbid {
        project_alias: String,
        task_id: Uuid,
        reply: Reply<()>,
    },
}

fn send_request<T>(
    sender: &std::sync::mpsc::Sender<ControlRequest>,
    request: ControlRequest,
    receiver: std::sync::mpsc::Receiver<Result<T, WorkError>>,
) -> Result<T, WorkError> {
    sender
        .send(request)
        .map_err(|_| WorkError::store("workflow thread unavailable"))?;
    receiver
        .recv()
        .map_err(|_| WorkError::store("workflow thread unavailable"))?
}

pub struct ChannelNativeControl {
    sender: std::sync::mpsc::Sender<ControlRequest>,
    owner: String,
}

impl NativeControlPort for ChannelNativeControl {
    fn resume(&self, command: &WorkResume) -> Result<Option<Value>, WorkError> {
        let (reply, receiver) = std::sync::mpsc::channel();
        send_request(
            &self.sender,
            ControlRequest::NativeResume(command.clone(), reply),
            receiver,
        )
    }

    fn review_decide(&self, command: &WorkReviewDecide) -> Result<Option<Value>, WorkError> {
        let _ = &self.owner;
        let (reply, receiver) = std::sync::mpsc::channel();
        send_request(
            &self.sender,
            ControlRequest::NativeReviewDecide(command.clone(), reply),
            receiver,
        )
    }
}

pub struct ChannelExternalControl {
    sender: std::sync::mpsc::Sender<ControlRequest>,
}

impl ExternalControlPort for ChannelExternalControl {
    fn cancel(
        &self,
        project_alias: &str,
        task_id: Uuid,
        expected_revision: i64,
        idempotency_key: &str,
        actor: &forge_work::work_actors::Actor,
    ) -> Result<Option<Value>, WorkError> {
        let (reply, receiver) = std::sync::mpsc::channel();
        send_request(
            &self.sender,
            ControlRequest::ExternalCancel {
                project_alias: project_alias.to_string(),
                task_id,
                expected_revision,
                idempotency_key: idempotency_key.to_string(),
                actor: actor.clone(),
                reply,
            },
            receiver,
        )
    }

    fn forbid_manual_control(&self, project_alias: &str, task_id: Uuid) -> Result<(), WorkError> {
        let (reply, receiver) = std::sync::mpsc::channel();
        send_request(
            &self.sender,
            ControlRequest::ExternalForbid {
                project_alias: project_alias.to_string(),
                task_id,
                reply,
            },
            receiver,
        )
    }
}

// ---------------------------------------------------------------------
// Workflow store builder (WorkflowStoreFactory's composition)
// ---------------------------------------------------------------------

fn coordinator_config(config: &WorkApiConfig) -> forge_work::workflow::CoordinatorConfig {
    forge_work::workflow::CoordinatorConfig {
        owner: config.owner.clone(),
        allowed_projects: config.allowed_projects.clone(),
        allowed_modes: config.allowed_modes.clone(),
        max_pending: config.max_pending,
        profiles: config
            .profiles
            .iter()
            .map(|profile| {
                (
                    profile.alias.clone(),
                    CatalogProfile {
                        execution_enabled: profile.execution_enabled,
                        backend: profile.backend.clone(),
                        model: profile.model.clone(),
                        backup_models: profile.backup_models.clone(),
                        quota_handoff_profile: profile.quota_handoff_profile.clone(),
                    },
                )
            })
            .collect(),
        project_workflows: config.project_workflows.clone(),
        repair_policy: config.repair_policy.clone(),
        workflow_policy_compatibility: config.workflow_policy_compatibility.clone(),
    }
}

fn pg_workflow_tx_factory(config: &WorkApiConfig, pg: &PgConfig) -> PgWorkflowTxFactory {
    PgWorkflowTxFactory::new(
        PgSessionFactory::new(pg.clone()),
        config.owner.clone(),
        config.allowed_projects.clone(),
    )
}

fn build_workflow_store(
    config: &WorkApiConfig,
    pg: &PgConfig,
) -> Result<Arc<forge_work::workflow::WorkflowStore>, WorkError> {
    let handoff = forge_work::agent_handoff::AgentHandoffProvider::new(
        Box::new(pg_workflow_tx_factory(config, pg)),
        Box::new(config.clone()),
        Box::new(forge_work::work_config::native_policy_matches),
        config.owner.clone(),
    );
    let execution_policy = ExecutionPolicyProvider::new(
        Box::new(config.clone()),
        Box::new(pg_workflow_tx_factory(config, pg)),
        Box::new(forge_work::work_config::native_policy_matches),
        config.owner.clone(),
        Box::new(handoff),
    );
    let store_factory = WorkflowStoreFactory {
        config: coordinator_config(config),
        catalog: Box::new(config.clone()),
        tx_factory: Box::new(pg_workflow_tx_factory(config, pg)),
        decisions: Box::new(PgWorkflowDecisions::new(pg_work_store(config, pg))),
        native_policy_matches: Box::new(forge_work::work_config::native_policy_matches),
        retry_events_enabled: true,
        execution_policy: Some(Box::new(execution_policy)),
    };
    Ok(store_factory.create())
}

/// The bridge context catalog (catalog.rs reference data) built
/// from the frozen work config: every allowed project carries
/// every profile; recipes are the resolved per-mode digests.
fn bridge_catalog(config: &WorkApiConfig) -> WorkCatalog {
    let projects = config
        .allowed_projects
        .iter()
        .map(|alias| {
            let profiles = config
                .profiles
                .iter()
                .map(|profile| AgentProfile {
                    name: profile.alias.clone(),
                    backend: profile.backend.clone(),
                    model: profile.model.clone(),
                    transport: if profile.is_external() {
                        "external_db".to_string()
                    } else {
                        "native".to_string()
                    },
                    modes: config.allowed_modes.clone(),
                })
                .collect();
            let mut recipes = Vec::new();
            for profile in &config.profiles {
                for mode in &config.allowed_modes {
                    if let Some(recipe) = config.resolve_recipe(&profile.alias, mode) {
                        recipes.push(Recipe {
                            id: format!("{}:{}", profile.alias, mode),
                            digest: recipe.digest.clone(),
                        });
                    }
                }
            }
            let workflow = config.project_workflow(alias).and_then(|policy| {
                policy
                    .snapshot(
                        config,
                        &config.repair_policy,
                        &forge_work::work_config::native_policy_matches,
                    )
                    .ok()
                    .map(|snapshot| forge_work::catalog::WorkflowPolicy {
                        digest: snapshot
                            .get("policy_digest")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        document: snapshot,
                    })
            });
            ProjectCatalog {
                alias: alias.clone(),
                profiles,
                recipes,
                workflow,
            }
        })
        .collect();
    WorkCatalog::new(projects)
}

// ---------------------------------------------------------------------
// Delivery repair composition (thread-local)
// ---------------------------------------------------------------------

struct RepairSnapshots {
    config: WorkApiConfig,
}

impl forge_work_publication::delivery_repair::RepairSnapshotsPort for RepairSnapshots {
    fn native_snapshot(
        &mut self,
        policy: &forge_work::workflow_policy::ProjectWorkflowPolicy,
        repair_policy: &Value,
    ) -> Result<Value, WorkError> {
        policy.snapshot(
            &self.config,
            repair_policy,
            &forge_work::work_config::native_policy_matches,
        )
    }

    fn external_snapshot(&mut self, policy: &ExternalWorkflowPolicy) -> Result<Value, WorkError> {
        policy
            .snapshot(&self.config, &self.config.native_policy_ref())
            .map_err(WorkError::store)
    }
}

struct RepairSubmit {
    service: Arc<WorkApiService>,
}

impl forge_work_publication::delivery_repair::RepairSubmitPort for RepairSubmit {
    fn submit(
        &mut self,
        submit: &forge_work_publication::delivery_repair::RepairSubmit,
    ) -> Result<Value, WorkError> {
        let command = forge_work::work_api::WorkSubmit {
            project_alias: submit.project_alias.clone(),
            base_revision: submit.base_revision.clone(),
            mode: submit.mode.clone(),
            agent_profile: submit.agent_profile.clone(),
            prompt: submit.prompt.clone(),
            acceptance_criteria: submit.acceptance_criteria.clone(),
            idempotency_key: submit.idempotency_key.clone(),
            auto_repair: submit.auto_repair,
            workflow_policy_digest: submit.workflow_policy_digest.clone(),
            workflow_policy_id: submit.workflow_policy_id,
            task_kind: "feature".to_string(),
            defect: None,
        };
        let correlation = Uuid::new_v4().to_string();
        let (_, body) = self.service.dispatch(
            forge_work::work_api::WorkDestination::Submit,
            &correlation,
            forge_work::work_api::WorkCommand::Submit(command),
            None,
        )?;
        let mut event = body.get("event").cloned().unwrap_or(Value::Null);
        if let Some(map) = event.as_object_mut() {
            map.remove("kind");
            map.remove("request_id");
        }
        Ok(event)
    }
}

fn build_delivery_repair(
    config: &WorkApiConfig,
    pg: &PgConfig,
    service: &Arc<WorkApiService>,
) -> Result<forge_work_publication::delivery_repair::DeliveryRepairConsumer, WorkError> {
    let repair_config = forge_work_publication::delivery_repair::DeliveryRepairConfig {
        delivery_enabled: config.delivery_enabled,
        delivery_repair_projects: config.delivery_repair_projects.iter().cloned().collect(),
        project_workflows: config.project_workflows.clone(),
        external_workflows: config.external_workflows.clone(),
        profiles: config
            .profiles
            .iter()
            .map(|profile| (profile.alias.clone(), profile.execution_enabled))
            .collect(),
        repair_policy: config.repair_policy.clone(),
    };
    let store = crate::delivery_repair_adapter::PgDeliveryRepairStore::connect(pg.clone())
        .map_err(work_err)?;
    forge_work_publication::delivery_repair::DeliveryRepairConsumer::new(
        repair_config,
        Box::new(RepairSnapshots {
            config: config.clone(),
        }),
        Box::new(store),
        Box::new(RepairSubmit {
            service: Arc::clone(service),
        }),
        "delivery-repair",
    )
    .map_err(WorkError::store)
}

// ---------------------------------------------------------------------
// The workflow thread
// ---------------------------------------------------------------------

struct ExternalMaintenance {
    factory: PgExternalTxFactory,
}

impl RunOnceHook for ExternalMaintenance {
    fn run_once(&self) -> Result<(), WorkError> {
        use forge_work::external_tx::ExternalTxFactory as _;
        self.factory.run(false, &mut |tx| {
            tx.reconcile_external()?;
            Ok(Value::Null)
        })?;
        Ok(())
    }
}

struct ExternalPass {
    store: Arc<ExternalWorkflowStore>,
}

impl ExternalWorkflowHook for ExternalPass {
    fn run_once(&self) -> Result<bool, WorkError> {
        self.store.run_once()
    }
}

struct RepairPass {
    consumer: Mutex<forge_work_publication::delivery_repair::DeliveryRepairConsumer>,
}

impl ExternalWorkflowHook for RepairPass {
    fn run_once(&self) -> Result<bool, WorkError> {
        let mut consumer = self
            .consumer
            .lock()
            .map_err(|_| WorkError::store("delivery repair lock"))?;
        consumer.run_once()
    }
}

/// Everything the workflow thread needs, handed over after the
/// API service exists (the delivery-repair submit port calls
/// back into it).
pub struct WorkflowThreadStart {
    config: WorkApiConfig,
    pg: PgConfig,
    receiver: std::sync::mpsc::Receiver<ControlRequest>,
}

impl WorkflowThreadStart {
    /// Python's WorkflowCoordinator defaults.
    pub fn interval_seconds(&self) -> u64 {
        30
    }

    pub fn spawn(
        self,
        service: Arc<WorkApiService>,
        stop: Arc<std::sync::atomic::AtomicBool>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            if let Err(error) = workflow_thread_main(self, service, stop) {
                eprintln!("workflow thread failed to start: {error}");
            }
        })
    }
}

fn build_external_store(
    config: &WorkApiConfig,
    pg: &PgConfig,
) -> Result<Option<Arc<ExternalWorkflowStore>>, WorkError> {
    if config.external_workflows.is_empty() {
        return Ok(None);
    }
    let tx_factory = Arc::new(PgExternalTxFactory::new(
        PgSessionFactory::new(pg.clone()),
        config.owner.clone(),
    ));
    let profiles: BTreeMap<String, forge_work::external_workflow::ExternalProfileInfo> = config
        .profiles
        .iter()
        .filter(|profile| profile.is_external())
        .map(|profile| {
            (
                profile.alias.clone(),
                forge_work::external_workflow::ExternalProfileInfo {
                    execution_enabled: profile.execution_enabled,
                    branch: Some(profile.branch.clone()),
                },
            )
        })
        .collect();
    let policies: BTreeMap<String, ExternalWorkflowPolicy> =
        config.external_workflows.iter().cloned().collect();
    Ok(Some(Arc::new(ExternalWorkflowStore::new(
        config.owner.clone(),
        policies,
        Arc::new(config.clone()),
        config.native_policy_ref(),
        tx_factory,
        profiles,
        config.identity_protocol.clone(),
        true,
        None,
        None,
        None,
    ))))
}

fn workflow_thread_main(
    start: WorkflowThreadStart,
    service: Arc<WorkApiService>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), WorkError> {
    let WorkflowThreadStart {
        config,
        pg,
        receiver,
    } = start;
    let external_store = build_external_store(&config, &pg)?;
    let workflow_store = build_workflow_store(&config, &pg)?;
    let correction = Arc::new(ReviewerCorrectionProvider::new(Arc::clone(&workflow_store)));
    let owner_control = OwnerControlProvider::new(workflow_store, Some(correction));
    let external_maintenance: Option<Box<dyn RunOnceHook>> =
        if config.profiles.iter().any(|p| p.is_external()) {
            Some(Box::new(ExternalMaintenance {
                factory: PgExternalTxFactory::new(
                    PgSessionFactory::new(pg.clone()),
                    config.owner.clone(),
                ),
            }))
        } else {
            None
        };
    let external_workflow: Option<Box<dyn ExternalWorkflowHook>> =
        external_store.as_ref().map(|store| {
            Box::new(ExternalPass {
                store: Arc::clone(store),
            }) as Box<dyn ExternalWorkflowHook>
        });
    let delivery_repair: Option<Box<dyn ExternalWorkflowHook>> =
        if config.delivery_repair_projects.is_empty() {
            None
        } else {
            Some(Box::new(RepairPass {
                consumer: Mutex::new(build_delivery_repair(&config, &pg, &service)?),
            }))
        };
    let coordinator = WorkflowCoordinatorFactory {
        store: build_workflow_store(&config, &pg)?,
        worker_id: "workflow-coordinator".to_string(),
        interval_seconds: 30,
        external_maintenance,
        external_workflow,
        delivery_repair,
    }
    .create();
    let owner = config.owner.clone();
    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
        while let Ok(request) = receiver.try_recv() {
            handle_control(request, &owner_control, external_store.as_deref(), &owner);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs_f64())
            .unwrap_or(0.0);
        let busy = coordinator.run_once(now).unwrap_or(false);
        if !busy {
            match receiver.recv_timeout(std::time::Duration::from_millis(250)) {
                Ok(request) => {
                    handle_control(request, &owner_control, external_store.as_deref(), &owner)
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }
    Ok(())
}

fn handle_control(
    request: ControlRequest,
    owner_control: &OwnerControlProvider,
    external_store: Option<&ExternalWorkflowStore>,
    owner: &str,
) {
    match request {
        ControlRequest::NativeResume(command, reply) => {
            let result = owner_control.resume(&ReviewerCorrectionResume {
                project_alias: command.project_alias.clone(),
                task_id: command.task_id,
                expected_revision: command.expected_revision,
                agent_profile: command.agent_profile.clone(),
                reconciliation_receipt_id: command.reconciliation_receipt_id,
                idempotency_key: command.idempotency_key.clone(),
                note: command.note.clone(),
            });
            let _ = reply.send(result);
        }
        ControlRequest::NativeReviewDecide(command, reply) => {
            let result = (|| {
                let decision =
                    forge_work::decision_requests::ReviewDecision::parse(&command.decision)
                        .ok_or_else(|| WorkError::rejected(400, "invalid_request"))?;
                owner_control.review_decide(&forge_work::decision_requests::ReviewDecideRequest {
                    project_alias: command.project_alias.clone(),
                    task_id: command.task_id,
                    expected_revision: command.expected_revision,
                    review_id: command.review_id,
                    decision,
                    agent_profile: Some(command.agent_profile.clone()),
                    note: command.note.clone(),
                    idempotency_key: command.idempotency_key.clone(),
                    findings: command.findings.clone(),
                    verified_finding_ids: command.verified_finding_ids.clone(),
                    recipe_digest: None,
                    actor: forge_work::work_actors::Actor::new(
                        forge_work::work_actors::ActorKind::Operator,
                        owner.to_string(),
                    ),
                })
            })();
            let _ = reply.send(result);
        }
        ControlRequest::ExternalCancel {
            project_alias,
            task_id,
            expected_revision,
            idempotency_key,
            actor,
            reply,
        } => {
            let result = match external_store {
                Some(store) => store.cancel(
                    &project_alias,
                    task_id,
                    expected_revision,
                    &idempotency_key,
                    Some(&actor),
                    None,
                ),
                None => Ok(None),
            };
            let _ = reply.send(result);
        }
        ControlRequest::ExternalForbid {
            project_alias,
            task_id,
            reply,
        } => {
            let result = match external_store {
                Some(store) => store.forbid_manual_control(&project_alias, task_id),
                None => Ok(()),
            };
            let _ = reply.send(result);
        }
    }
}

// ---------------------------------------------------------------------
// Service factory (WorkServiceFactory.create)
// ---------------------------------------------------------------------

pub struct RunnerWork {
    pub service: Arc<WorkApiService>,
    pub workflow_thread: WorkflowThreadStart,
}

pub fn create_work_service(config: &WorkApiConfig, pg: &PgConfig) -> Result<RunnerWork, WorkError> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let has_external_workflows = !config.external_workflows.is_empty();
    let external_control: Option<Arc<dyn ExternalControlPort + Send + Sync>> =
        if has_external_workflows {
            Some(Arc::new(ChannelExternalControl {
                sender: sender.clone(),
            }))
        } else {
            None
        };
    let catalog_port = PgWorkCatalog {
        pg: pg.clone(),
        owner: config.owner.clone(),
        has_external_profiles: config.profiles.iter().any(|p| p.is_external()),
        delivery_enabled: config.delivery_enabled,
        retry_events: true,
    };
    let service = WorkApiService::new(
        config.clone(),
        Arc::new(pg_work_store(config, pg)),
        Arc::new(pg_work_store(config, pg)),
        Arc::new(pg_work_store(config, pg)),
        Arc::new(catalog_port),
        bridge_catalog(config),
        external_control,
        Some(Arc::new(ChannelNativeControl {
            sender,
            owner: config.owner.clone(),
        })),
        Arc::new(PgPolicyIdentity {
            store: pg_work_store(config, pg),
        }),
    )?;
    Ok(RunnerWork {
        service: Arc::new(service),
        workflow_thread: WorkflowThreadStart {
            config: config.clone(),
            pg: pg.clone(),
            receiver,
        },
    })
}

// ---------------------------------------------------------------------
// Usage service (UsageServiceFactory over the Pg store)
// ---------------------------------------------------------------------

pub fn create_usage_service(
    config: &WorkApiConfig,
    pg: &PgConfig,
) -> Result<Option<forge_usage::api::UsageService>, String> {
    let usage_config = forge_usage::store::UsageConfig {
        owner: config.owner.clone(),
        allowed_projects: config.allowed_projects.clone(),
        profiles: config
            .profiles
            .iter()
            .map(|profile| {
                (
                    profile.alias.clone(),
                    forge_usage::store::UsageProfileInfo {
                        label: profile_label(config, &profile.alias),
                        backend: profile.backend.clone(),
                        model: profile.model.clone(),
                    },
                )
            })
            .collect(),
    };
    let pg_config = pg.clone();
    let factory = forge_usage::api::UsageServiceFactory::new(
        Some(usage_config),
        Some(Arc::new(move |config: &forge_usage::store::UsageConfig| {
            let port = crate::usage_store_adapter::PgUsageStore::new(pg_config.clone());
            Arc::new(
                forge_usage::store::UsageStore::new(config.clone(), Arc::new(port), 900, 15)
                    .expect("usage store"),
            ) as Arc<dyn forge_usage::store::UsageReadStore>
        })),
    )?;
    factory.create()
}

#[cfg(test)]
mod runner_pg_env_tests {
    use super::{runner_pg_config, RunnerPgEnv};
    use std::collections::HashMap;

    #[test]
    fn the_runner_database_user_comes_from_the_handed_in_environment() {
        let environ: HashMap<String, String> = [
            ("PGUSER".to_string(), "forge_runner".to_string()),
            ("HOME".to_string(), "/nonexistent-runner-home".to_string()),
        ]
        .into_iter()
        .collect();
        let env = RunnerPgEnv::from_environ(&environ);
        let pg = runner_pg_config("lrocha", &env).expect("explicit user");
        assert_eq!(pg.user, "forge_runner");
        assert_eq!(pg.owner, "lrocha");
        assert_eq!(pg.dbname, "forge_agent");
        assert_eq!(pg.password, None, "no passfile under the handed-in home");
    }

    #[test]
    fn an_empty_pguser_falls_back_to_the_process_user() {
        let env = RunnerPgEnv {
            pguser: Some(String::new()),
            home: String::new(),
        };
        let pg = runner_pg_config("lrocha", &env).expect("process user resolves");
        assert!(!pg.user.is_empty());
    }

    #[test]
    fn a_missing_home_is_empty_not_an_error() {
        let env = RunnerPgEnv::from_environ(&HashMap::new());
        assert_eq!(env, RunnerPgEnv::default());
    }
}
