use crate::config::PgConfig;
use crate::error::AdapterError;
use serde_json::Value;
use tokio::runtime::{Builder, Runtime};
use tokio_postgres::{types::ToSql, Client, NoTls, Row};

/// A synchronous adapter session for a PostgreSQL database.
pub struct PgSession {
    /// The configuration for the PostgreSQL connection.
    config: PgConfig,
    /// The Tokio runtime for handling asynchronous operations.
    rt: Runtime,
    /// The PostgreSQL client.
    client: Client,
}

impl PgSession {
    /// Connects to the PostgreSQL database using the provided configuration.
    /// Validates the configuration, initializes the Tokio runtime, establishes the connection,
    /// sets the statement timeout, and runs a readiness probe.
    pub fn connect(config: PgConfig) -> Result<PgSession, AdapterError> {
        config.validate()?;
        let rt = Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| AdapterError::Protocol(e.to_string()))?;
        let (mut client, connection) = rt.block_on(config.to_tokio_config().connect(NoTls))?;
        rt.spawn(async move {
            let _ = connection.await;
        });
        let set_timeout = format!(
            "SET statement_timeout = '{}s'",
            config.statement_timeout.as_secs()
        );
        rt.block_on(client.batch_execute(&set_timeout))?;
        let mut session = PgSession { config, rt, client };
        session.call_scalar::<i32>("SELECT 1", &[])?;
        Ok(session)
    }

    /// Runs a query and returns the result.
    fn run_query(
        &mut self,
        select: &str,
        params: &[&(dyn ToSql + Sync)],
        want_row: bool,
    ) -> Result<Option<Row>, AdapterError> {
        let owner = self.config.owner.clone();
        let client = &mut self.client;
        let out = self.rt.block_on(async {
            let mut tx = client.transaction().await?;
            tx.execute(
                "SELECT set_config('forge.coding_owner', $1, true)",
                &[&owner],
            )
            .await?;
            let row = if want_row {
                tx.query_opt(select, params).await?
            } else {
                tx.execute(select, params).await?;
                None
            };
            tx.commit().await?;
            Ok::<_, tokio_postgres::Error>(row)
        });
        Ok(out?)
    }

    /// Returns the configuration for the session.
    pub fn config(&self) -> &PgConfig {
        &self.config
    }

    /// Runs `f` inside one owner-scoped transaction: the same envelope as
    /// every call_* (BEGIN, the owner set_config, COMMIT on success), but
    /// the closure's statements all run on the one open transaction through
    /// the Tx handle. An error from the closure or from any statement drops
    /// the transaction uncommitted, which rolls it back. The closure is
    /// synchronous and must only talk to the database through the handle.
    pub fn in_tx<T>(
        &mut self,
        f: impl FnOnce(&mut crate::tx::Tx<'_, '_>) -> Result<T, AdapterError>,
    ) -> Result<T, AdapterError> {
        let owner = self.config.owner.clone();
        let handle = self.rt.handle().clone();
        let client = &mut self.client;
        let out = self.rt.block_on(async {
            let mut tx = client.transaction().await?;
            tx.execute(
                "SELECT set_config('forge.coding_owner', $1, true)",
                &[&owner],
            )
            .await?;
            let value = {
                let mut handle_tx = crate::tx::Tx::new(&mut tx, handle);
                tokio::task::block_in_place(|| f(&mut handle_tx))?
            };
            tx.commit().await?;
            Ok::<_, AdapterError>(value)
        });
        Ok(out?)
    }

    /// Runs a general row query inside the standard envelope (its own
    /// transaction with the owner set_config) and returns all rows. The
    /// work-store adapters use this for the raw-SQL surface
    /// (forge_agent.coding_* tables) that the call_* helpers, shaped for
    /// single-value SQL functions, cannot express.
    pub fn query_rows(
        &mut self,
        select: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, AdapterError> {
        let owner = self.config.owner.clone();
        let client = &mut self.client;
        let out = self.rt.block_on(async {
            let mut tx = client.transaction().await?;
            tx.execute(
                "SELECT set_config('forge.coding_owner', $1, true)",
                &[&owner],
            )
            .await?;
            let rows = tx.query(select, params).await?;
            tx.commit().await?;
            Ok::<_, tokio_postgres::Error>(rows)
        });
        Ok(out?)
    }

    /// Executes a statement inside the standard envelope and returns the
    /// number of rows it affected.
    pub fn execute_count(
        &mut self,
        select: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<u64, AdapterError> {
        let owner = self.config.owner.clone();
        let client = &mut self.client;
        let out = self.rt.block_on(async {
            let mut tx = client.transaction().await?;
            tx.execute(
                "SELECT set_config('forge.coding_owner', $1, true)",
                &[&owner],
            )
            .await?;
            let count = tx.execute(select, params).await?;
            tx.commit().await?;
            Ok::<_, tokio_postgres::Error>(count)
        });
        Ok(out?)
    }

    /// Executes a query and returns the result as a scalar value.
    pub fn call_scalar<T>(
        &mut self,
        select: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Option<T>, AdapterError>
    where
        T: for<'a> tokio_postgres::types::FromSql<'a>,
    {
        Ok(self.run_query(select, params, true)?.map(|row| row.get(0)))
    }

    /// Executes a query that does not return any result.
    pub fn call_void(
        &mut self,
        select: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<(), AdapterError> {
        self.run_query(select, params, false).map(|_| ())
    }

    /// Executes a query and returns the result as a JSON value.
    pub fn call_json(
        &mut self,
        select: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Option<Value>, AdapterError> {
        self.call_scalar::<Value>(select, params)
    }

    /// Executes a query and returns the result as a boolean value.
    pub fn call_bool(
        &mut self,
        select: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<bool, AdapterError> {
        Ok(self.call_scalar::<bool>(select, params)?.unwrap_or(false))
    }

    /// Executes a query and returns the result as a text value.
    pub fn call_text(
        &mut self,
        select: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Option<String>, AdapterError> {
        self.call_scalar::<String>(select, params)
    }
}
