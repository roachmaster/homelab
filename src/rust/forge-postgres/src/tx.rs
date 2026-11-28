/// The handle for one open owner-scoped transaction, handed to the closure
/// of PgSession::in_tx. Its call_* helpers mirror PgSession's, but every
/// statement runs on the one transaction in_tx opened instead of a fresh
/// one. A Tx is valid only inside that closure: its methods block on the
/// runtime handle, which in_tx makes legal by running the closure inside
/// tokio::task::block_in_place.
pub struct Tx<'t, 'c> {
    /// The open tokio-postgres transaction, owned by PgSession::in_tx.
    pg: &'t mut tokio_postgres::Transaction<'c>,
    /// The session runtime's handle, used to drive each statement.
    handle: tokio::runtime::Handle,
}

impl<'t, 'c> Tx<'t, 'c> {
    /// Builds the handle over an open transaction. Crate-internal: only
    /// PgSession::in_tx constructs one, after it has set the owner config.
    pub(crate) fn new(
        pg: &'t mut tokio_postgres::Transaction<'c>,
        handle: tokio::runtime::Handle,
    ) -> Tx<'t, 'c> {
        Tx { pg, handle }
    }

    /// Executes a query on the open transaction and returns the first
    /// column of the optional row as a scalar value.
    pub fn call_scalar<T>(
        &mut self,
        select: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<Option<T>, crate::error::AdapterError>
    where
        T: for<'b> tokio_postgres::types::FromSql<'b>,
    {
        let handle = self.handle.clone();
        let pg = &mut *self.pg;
        let row = handle.block_on(async move { pg.query_opt(select, params).await })?;
        // Decode as Option<T>: a SQL NULL first column is no result,
        // not a decode panic - row.get::<T> panics on NULL.
        Ok(row.and_then(|row| row.get::<usize, Option<T>>(0)))
    }

    /// Executes a statement on the open transaction that returns no result.
    pub fn call_void(
        &mut self,
        select: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<(), crate::error::AdapterError> {
        let handle = self.handle.clone();
        let pg = &mut *self.pg;
        handle.block_on(async move { pg.execute(select, params).await })?;
        Ok(())
    }

    /// Executes a query on the open transaction and returns the result as
    /// a JSON value.
    pub fn call_json(
        &mut self,
        select: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<Option<serde_json::Value>, crate::error::AdapterError> {
        self.call_scalar(select, params)
    }

    /// Executes a query on the open transaction and returns the result as
    /// a boolean value; no row reads as false.
    pub fn call_bool(
        &mut self,
        select: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<bool, crate::error::AdapterError> {
        self.call_scalar(select, params).map(|v| v.unwrap_or(false))
    }

    /// Runs a general row query on the open transaction and returns all
    /// rows. The work-store adapters use this for the raw-SQL surface
    /// (forge_agent.coding_* tables) that the call_* helpers, shaped for
    /// single-value SQL functions, cannot express.
    pub fn query_rows(
        &mut self,
        select: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<Vec<tokio_postgres::Row>, crate::error::AdapterError> {
        let handle = self.handle.clone();
        let pg = &mut *self.pg;
        let rows = handle.block_on(async move { pg.query(select, params).await })?;
        Ok(rows)
    }

    /// Executes a statement on the open transaction and returns the
    /// number of rows it affected (the rowcount conditional updates
    /// check).
    pub fn execute_count(
        &mut self,
        select: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<u64, crate::error::AdapterError> {
        let handle = self.handle.clone();
        let pg = &mut *self.pg;
        let count = handle.block_on(async move { pg.execute(select, params).await })?;
        Ok(count)
    }

    /// Executes a query on the open transaction and returns the result as
    /// a text value.
    pub fn call_text(
        &mut self,
        select: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<Option<String>, crate::error::AdapterError> {
        self.call_scalar(select, params)
    }
}
