//! PostgreSQL admission, event history and attempt fencing for
//! Forge Studio (studio_store.py). A synchronous facade over
//! tokio-postgres: one connection per transaction, like the Python
//! store. The DSN names Studio's own forge_game roles — this store
//! deliberately does not use the forge_agent adapter.

use std::str::FromStr;

use tokio_postgres::{Client, Config, IsolationLevel, NoTls, Transaction};
use uuid::Uuid;

pub const MAX_ATTEMPTS: i32 = 3;
pub const LEASE_SECONDS: i32 = 30;

#[derive(Debug)]
pub enum StoreError {
    Rejected { status: u16, code: String },
    Db(String),
}

impl StoreError {
    pub fn rejected(status: u16, code: &str) -> StoreError {
        StoreError::Rejected {
            status,
            code: code.to_string(),
        }
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, Clone)]
pub struct EventRow {
    pub sequence: i32,
    pub state: String,
}

#[derive(Debug, Clone)]
pub struct BuildRow {
    pub build_id: String,
    pub node_alias: String,
    pub project_alias: String,
    pub revision: String,
    pub profile: String,
    pub source_recipe: String,
    pub intent_key: String,
    pub state: String,
    pub attempts: i32,
    pub attempt_token: Option<String>,
    pub artifact_digest: Option<String>,
    pub artifact_bytes: Option<i64>,
    pub events: Vec<EventRow>,
}

fn row_to_build(row: &tokio_postgres::Row) -> BuildRow {
    let build_id: Uuid = row.get("build_id");
    let attempt_token: Option<Uuid> = row.get("attempt_token");
    BuildRow {
        build_id: build_id.to_string(),
        node_alias: row.get("node_alias"),
        project_alias: row.get("project_alias"),
        revision: row.get("revision"),
        profile: row.get("profile"),
        source_recipe: row.get("source_recipe"),
        intent_key: row.get("intent_key"),
        state: row.get("state"),
        attempts: row.get("attempts"),
        attempt_token: attempt_token.map(|token| token.to_string()),
        artifact_digest: row.get("artifact_digest"),
        artifact_bytes: row.get::<_, Option<i32>>("artifact_bytes").map(i64::from),
        events: Vec::new(),
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(future)
}

async fn connect(dsn: &str) -> StoreResult<Client> {
    let mut config = Config::from_str(dsn).map_err(|error| StoreError::Db(error.to_string()))?;
    config
        .application_name("forge-studio")
        .options("-c statement_timeout=5000 -c lock_timeout=4000");
    let connect = config.connect(NoTls);
    let (client, connection) = tokio::time::timeout(std::time::Duration::from_secs(4), connect)
        .await
        .map_err(|_| StoreError::Db("connection timed out".to_string()))?
        .map_err(|error| StoreError::Db(error.to_string()))?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

async fn begin_tx(client: &mut Client, read_only: bool) -> StoreResult<Transaction<'_>> {
    let builder = client.build_transaction();
    let builder = if read_only {
        builder
            .isolation_level(IsolationLevel::RepeatableRead)
            .read_only(true)
    } else {
        builder
    };
    builder
        .start()
        .await
        .map_err(|error| StoreError::Db(error.to_string()))
}

async fn finish_tx<T>(tx: Transaction<'_>, result: StoreResult<T>) -> StoreResult<T> {
    match result {
        Ok(value) => {
            tx.commit()
                .await
                .map_err(|error| StoreError::Db(error.to_string()))?;
            Ok(value)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

async fn event(tx: &mut Transaction<'_>, build_id: &str, state: &str) -> StoreResult<()> {
    let id = Uuid::parse_str(build_id).map_err(|error| StoreError::Db(error.to_string()))?;
    tx.execute(
        "INSERT INTO forge_game.build_events (build_id, sequence, state) \
         SELECT $1, COALESCE(MAX(sequence), 0) + 1, $2 \
         FROM forge_game.build_events WHERE build_id = $1",
        &[&id, &state],
    )
    .await
    .map_err(|error| StoreError::Db(error.to_string()))?;
    Ok(())
}

async fn build(
    tx: &mut Transaction<'_>,
    project: &(String, String),
    build_id: &str,
    lock: bool,
) -> StoreResult<BuildRow> {
    let id = Uuid::parse_str(build_id).map_err(|_| StoreError::rejected(404, "not_found"))?;
    let query = format!(
        "SELECT * FROM forge_game.builds \
         WHERE node_alias = $1 AND project_alias = $2 AND build_id = $3{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = tx
        .query_opt(&query, &[&project.0, &project.1, &id])
        .await
        .map_err(|error| StoreError::Db(error.to_string()))?;
    row.map(|row| row_to_build(&row))
        .ok_or_else(|| StoreError::rejected(404, "not_found"))
}

async fn with_events(
    tx: &mut Transaction<'_>,
    mut row: BuildRow,
    after_sequence: i32,
) -> StoreResult<BuildRow> {
    let id = Uuid::parse_str(&row.build_id).map_err(|error| StoreError::Db(error.to_string()))?;
    let rows = tx
        .query(
            "SELECT sequence, state FROM forge_game.build_events \
             WHERE build_id = $1 AND sequence > $2 ORDER BY sequence LIMIT 50",
            &[&id, &after_sequence],
        )
        .await
        .map_err(|error| StoreError::Db(error.to_string()))?;
    row.events = rows
        .iter()
        .map(|row| EventRow {
            sequence: row.get("sequence"),
            state: row.get("state"),
        })
        .collect();
    Ok(row)
}

#[derive(Clone)]
pub struct Store {
    dsn: String,
}

impl Store {
    pub fn new(dsn: String) -> Store {
        Store { dsn }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Preserve the public Studio submission contract and its callers"
    )]
    pub fn submit(
        &self,
        project: &(String, String),
        revision: &str,
        profile: &str,
        key: &str,
        max_builds: i32,
        approved_revision: &str,
        source_recipe: &str,
    ) -> StoreResult<BuildRow> {
        let (revision, profile, key) = (revision.to_string(), profile.to_string(), key.to_string());
        let (approved_revision, source_recipe) =
            (approved_revision.to_string(), source_recipe.to_string());
        let project = project.clone();
        let dsn = self.dsn.clone();
        block_on(async move {
            let mut client = connect(&dsn).await?;
            let mut tx = begin_tx(&mut client, false).await?;
            let result = submit_body(
                &mut tx,
                &project,
                &revision,
                &profile,
                &key,
                max_builds,
                &approved_revision,
                &source_recipe,
            )
            .await;
            finish_tx(tx, result).await
        })
    }

    pub fn status(
        &self,
        project: &(String, String),
        build_id: &str,
        after_sequence: i32,
    ) -> StoreResult<BuildRow> {
        let project = project.clone();
        let build_id = build_id.to_string();
        let dsn = self.dsn.clone();
        block_on(async move {
            let mut client = connect(&dsn).await?;
            let mut tx = begin_tx(&mut client, true).await?;
            let result = async {
                let row = build(&mut tx, &project, &build_id, false).await?;
                with_events(&mut tx, row, after_sequence).await
            }
            .await;
            finish_tx(tx, result).await
        })
    }

    pub fn cancel(&self, project: &(String, String), build_id: &str) -> StoreResult<BuildRow> {
        let project = project.clone();
        let build_id = build_id.to_string();
        let dsn = self.dsn.clone();
        block_on(async move {
            let mut client = connect(&dsn).await?;
            let mut tx = begin_tx(&mut client, false).await?;
            let result = async {
                let mut row = build(&mut tx, &project, &build_id, true).await?;
                if row.state == "queued" || row.state == "running" {
                    let id = Uuid::parse_str(&build_id)
                        .map_err(|error| StoreError::Db(error.to_string()))?;
                    let updated = tx
                        .query_one(
                            "UPDATE forge_game.builds SET state = 'cancelled', \
                             attempt_token = NULL, lease_until = NULL \
                             WHERE build_id = $1 RETURNING *",
                            &[&id],
                        )
                        .await
                        .map_err(|error| StoreError::Db(error.to_string()))?;
                    row = row_to_build(&updated);
                    event(&mut tx, &build_id, "cancelled").await?;
                }
                with_events(&mut tx, row, 0).await
            }
            .await;
            finish_tx(tx, result).await
        })
    }

    pub fn claim(&self) -> StoreResult<Option<BuildRow>> {
        let dsn = self.dsn.clone();
        block_on(async move {
            let mut client = connect(&dsn).await?;
            let mut tx = begin_tx(&mut client, false).await?;
            let result = claim_body(&mut tx).await;
            finish_tx(tx, result).await
        })
    }

    pub fn renew(&self, claim: &BuildRow) -> StoreResult<bool> {
        let claim = claim.clone();
        let dsn = self.dsn.clone();
        block_on(async move {
            let mut client = connect(&dsn).await?;
            let tx = begin_tx(&mut client, false).await?;
            let result = async {
                let id = Uuid::parse_str(&claim.build_id)
                    .map_err(|error| StoreError::Db(error.to_string()))?;
                let token = claim
                    .attempt_token
                    .as_deref()
                    .and_then(|value| Uuid::parse_str(value).ok());
                let changed = tx
                    .execute(
                        "UPDATE forge_game.builds \
                         SET lease_until = clock_timestamp() + $1 * INTERVAL '1 second' \
                         WHERE build_id = $2 AND attempt_token = $3 AND state = 'running' \
                         AND lease_until > clock_timestamp()",
                        &[&LEASE_SECONDS, &id, &token],
                    )
                    .await
                    .map_err(|error| StoreError::Db(error.to_string()))?;
                Ok(changed == 1)
            }
            .await;
            finish_tx(tx, result).await
        })
    }

    pub fn finish(&self, claim: &BuildRow, artifact: Option<(&str, i64)>) -> StoreResult<bool> {
        let claim = claim.clone();
        let artifact = artifact.map(|(digest, size)| (digest.to_string(), size));
        let dsn = self.dsn.clone();
        block_on(async move {
            let mut client = connect(&dsn).await?;
            let mut tx = begin_tx(&mut client, false).await?;
            let result = async {
                let state = if artifact.is_some() {
                    "succeeded"
                } else {
                    "failed"
                };
                let (digest, size) = match &artifact {
                    Some((digest, size)) => (Some(digest.clone()), Some(*size as i32)),
                    None => (None, None),
                };
                let id = Uuid::parse_str(&claim.build_id)
                    .map_err(|error| StoreError::Db(error.to_string()))?;
                let token = claim
                    .attempt_token
                    .as_deref()
                    .and_then(|value| Uuid::parse_str(value).ok());
                let changed = tx
                    .execute(
                        "UPDATE forge_game.builds SET state = $1, artifact_digest = $2, \
                         artifact_bytes = $3, attempt_token = NULL, lease_until = NULL \
                         WHERE build_id = $4 AND attempt_token = $5 AND state = 'running' \
                         AND lease_until > clock_timestamp()",
                        &[&state, &digest, &size, &id, &token],
                    )
                    .await
                    .map_err(|error| StoreError::Db(error.to_string()))?;
                if changed != 1 {
                    return Ok(false);
                }
                event(&mut tx, &claim.build_id, state).await?;
                Ok(true)
            }
            .await;
            finish_tx(tx, result).await
        })
    }
}

#[allow(clippy::too_many_arguments)]
async fn submit_body(
    tx: &mut Transaction<'_>,
    project: &(String, String),
    revision: &str,
    profile: &str,
    key: &str,
    max_builds: i32,
    approved_revision: &str,
    source_recipe: &str,
) -> StoreResult<BuildRow> {
    tx.execute(
        "SELECT pg_advisory_xact_lock(hashtext($1), hashtext($2))",
        &[&project.0, &project.1],
    )
    .await
    .map_err(|error| StoreError::Db(error.to_string()))?;
    let existing = tx
        .query_opt(
            "SELECT * FROM forge_game.builds \
             WHERE node_alias = $1 AND project_alias = $2 AND intent_key = $3 FOR UPDATE",
            &[&project.0, &project.1, &key],
        )
        .await
        .map_err(|error| StoreError::Db(error.to_string()))?;
    if let Some(row) = existing {
        let row = row_to_build(&row);
        if (row.revision.as_str(), row.profile.as_str()) != (revision, profile) {
            return Err(StoreError::rejected(409, "idempotency_conflict"));
        }
        return with_events(tx, row, 0).await;
    }
    if revision != approved_revision {
        return Err(StoreError::rejected(404, "not_found"));
    }
    let count_row = tx
        .query_one(
            "SELECT count(*) AS count FROM forge_game.builds \
             WHERE node_alias = $1 AND project_alias = $2",
            &[&project.0, &project.1],
        )
        .await
        .map_err(|error| StoreError::Db(error.to_string()))?;
    let count: i64 = count_row.get("count");
    if count >= i64::from(max_builds) {
        return Err(StoreError::rejected(429, "capacity"));
    }
    let build_id = Uuid::new_v4();
    let row = tx
        .query_one(
            "INSERT INTO forge_game.builds \
             (build_id, node_alias, project_alias, revision, profile, source_recipe, intent_key, state) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'queued') RETURNING *",
            &[
                &build_id,
                &project.0,
                &project.1,
                &revision,
                &profile,
                &source_recipe,
                &key,
            ],
        )
        .await
        .map_err(|error| StoreError::Db(error.to_string()))?;
    let row = row_to_build(&row);
    event(tx, &row.build_id, "queued").await?;
    with_events(tx, row, 0).await
}

async fn claim_body(tx: &mut Transaction<'_>) -> StoreResult<Option<BuildRow>> {
    let expired = tx
        .query(
            "SELECT build_id, attempts FROM forge_game.builds \
             WHERE state = 'running' AND lease_until <= clock_timestamp() \
             ORDER BY lease_until, build_id LIMIT 32 FOR UPDATE SKIP LOCKED",
            &[],
        )
        .await
        .map_err(|error| StoreError::Db(error.to_string()))?;
    for row in &expired {
        let id: Uuid = row.get("build_id");
        let attempts: i32 = row.get("attempts");
        let state = if attempts >= MAX_ATTEMPTS {
            "failed"
        } else {
            "queued"
        };
        tx.execute(
            "UPDATE forge_game.builds SET state = $1, \
             attempt_token = NULL, lease_until = NULL WHERE build_id = $2",
            &[&state, &id],
        )
        .await
        .map_err(|error| StoreError::Db(error.to_string()))?;
        event(tx, &id.to_string(), state).await?;
    }
    let next = tx
        .query_opt(
            "SELECT build_id FROM forge_game.builds \
             WHERE state = 'queued' ORDER BY created_at, build_id LIMIT 1 \
             FOR UPDATE SKIP LOCKED",
            &[],
        )
        .await
        .map_err(|error| StoreError::Db(error.to_string()))?;
    let Some(next) = next else { return Ok(None) };
    let id: Uuid = next.get("build_id");
    let token = Uuid::new_v4();
    let claim = tx
        .query_one(
            "UPDATE forge_game.builds \
             SET state = 'running', attempts = attempts + 1, attempt_token = $1, \
             lease_until = clock_timestamp() + $2 * INTERVAL '1 second' \
             WHERE build_id = $3 RETURNING *",
            &[&token, &LEASE_SECONDS, &id],
        )
        .await
        .map_err(|error| StoreError::Db(error.to_string()))?;
    let claim = row_to_build(&claim);
    event(tx, &claim.build_id, "running").await?;
    Ok(Some(claim))
}

/// Studio's build store port: admission, status and cancellation for
/// the API, and claim/renew/finish attempt fencing for the worker.
/// `Store` is the PostgreSQL implementation; tests bind fakes.
pub trait BuildStore: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    fn submit(
        &self,
        project: &(String, String),
        revision: &str,
        profile: &str,
        key: &str,
        max_builds: i32,
        approved_revision: &str,
        source_recipe: &str,
    ) -> StoreResult<BuildRow>;
    fn status(
        &self,
        project: &(String, String),
        build_id: &str,
        after_sequence: i32,
    ) -> StoreResult<BuildRow>;
    fn cancel(&self, project: &(String, String), build_id: &str) -> StoreResult<BuildRow>;
    fn claim(&self) -> StoreResult<Option<BuildRow>>;
    fn renew(&self, claim: &BuildRow) -> StoreResult<bool>;
    fn finish(&self, claim: &BuildRow, artifact: Option<(&str, i64)>) -> StoreResult<bool>;
}

impl BuildStore for Store {
    fn submit(
        &self,
        project: &(String, String),
        revision: &str,
        profile: &str,
        key: &str,
        max_builds: i32,
        approved_revision: &str,
        source_recipe: &str,
    ) -> StoreResult<BuildRow> {
        Store::submit(
            self,
            project,
            revision,
            profile,
            key,
            max_builds,
            approved_revision,
            source_recipe,
        )
    }

    fn status(
        &self,
        project: &(String, String),
        build_id: &str,
        after_sequence: i32,
    ) -> StoreResult<BuildRow> {
        Store::status(self, project, build_id, after_sequence)
    }

    fn cancel(&self, project: &(String, String), build_id: &str) -> StoreResult<BuildRow> {
        Store::cancel(self, project, build_id)
    }

    fn claim(&self) -> StoreResult<Option<BuildRow>> {
        Store::claim(self)
    }

    fn renew(&self, claim: &BuildRow) -> StoreResult<bool> {
        Store::renew(self, claim)
    }

    fn finish(&self, claim: &BuildRow, artifact: Option<(&str, i64)>) -> StoreResult<bool> {
        Store::finish(self, claim, artifact)
    }
}
