use super::config::{authenticate, passwords, token_hash};
use super::{Caller, OperationPlan, ProxyConfig, ProxyError};
use forge_core::{bootstrap::PreparedGroup, io_client::IoClient, provider::Factory};
use futures_util::TryStreamExt;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use tokio_postgres::{types::ToSql, NoTls};

pub struct ProxyFactory {
    config: ProxyConfig,
}
impl ProxyFactory {
    pub fn new(config: ProxyConfig) -> Self {
        Self { config }
    }
}
pub fn register_proxy(config: ProxyConfig) -> PreparedGroup<ProxyFactory> {
    PreparedGroup::new(
        "forge.postgres.proxy",
        &["pg-proxy"],
        &[],
        ProxyFactory::new(config),
    )
    .expect("fixed proxy registrar")
}
impl Factory<Proxy> for ProxyFactory {
    type Error = ProxyError;
    fn create(&self) -> Result<Proxy, ProxyError> {
        Proxy::connect(self.config.clone())
    }
}
/// A request contains an authenticated capability selected by this proxy, not a caller label.
pub struct ProxyCommand {
    pub bearer: String,
    pub operation: String,
    pub args: Value,
}
pub struct Proxy {
    pub(crate) config: ProxyConfig,
    pub(crate) runtime: Arc<tokio::runtime::Runtime>,
    tokens: Vec<(Caller, [u8; 32])>,
    passwords: std::collections::BTreeMap<String, String>,
    write: Mutex<()>,
    pub(crate) closed: Arc<AtomicBool>,
    active: Mutex<usize>,
}
impl Proxy {
    pub fn connect(config: ProxyConfig) -> Result<Self, ProxyError> {
        let mut tokens = Vec::new();
        for (c, path) in &config.callers {
            let hash = token_hash(path)?;
            if tokens.iter().any(|(_, h)| h == &hash) {
                return Err(ProxyError::Config);
            }
            tokens.push((c.clone(), hash));
        }
        let passwords = passwords(&config)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| ProxyError::Config)?;
        Ok(Self {
            config,
            passwords,
            runtime: Arc::new(runtime),
            tokens,
            write: Mutex::new(()),
            closed: Arc::new(AtomicBool::new(false)),
            active: Mutex::new(0),
        })
    }
    pub fn authenticate(&self, bearer: &str) -> Result<&Caller, ProxyError> {
        authenticate(&self.tokens, bearer)
    }
    pub fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
    }
    pub fn health(&self) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        self.runtime.block_on(async {
            let Ok((client, driver)) = self.connection("forge_pg_proxy").await else {
                return false;
            };
            let ok = tokio::time::timeout(Duration::from_secs(5), client.simple_query("SELECT 1"))
                .await
                .is_ok_and(|r| r.is_ok());
            drop(client);
            driver.abort();
            let _ = driver.await;
            ok
        })
    }
    pub(crate) fn pg_config(&self, role: &str) -> tokio_postgres::Config {
        let mut c = tokio_postgres::Config::new();
        c.host(&self.config.host)
            .port(self.config.port)
            .dbname(&self.config.dbname)
            .user(role)
            .application_name("forge-pg-proxy")
            .connect_timeout(Duration::from_secs(5));
        if let Some(password) = self.passwords.get(role) {
            c.password(password);
        }
        c
    }
    async fn connection(
        &self,
        role: &str,
    ) -> Result<
        (
            tokio_postgres::Client,
            tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>,
        ),
        ProxyError,
    > {
        let (c, connection) = self
            .pg_config(role)
            .connect(NoTls)
            .await
            .map_err(|_| ProxyError::Database)?;
        let driver = self.runtime.spawn(connection);
        Ok((c, driver))
    }
    fn perform(&self, caller: &Caller, plan: OperationPlan) -> Result<Value, ProxyError> {
        let _write = if plan.write {
            Some(self.write.lock().map_err(|_| ProxyError::Closed)?)
        } else {
            None
        };
        self.runtime.block_on(async {
            let (mut client, driver) = self.connection(&caller.role).await?;
            let result = tokio::time::timeout(Duration::from_secs(40), async {
                let tx = client.transaction().await.map_err(|_| ProxyError::Database)?;
                let limits = if plan.write {
                    "SET LOCAL statement_timeout='30s'; SET LOCAL lock_timeout='5s'"
                } else {
                    "SET TRANSACTION READ ONLY; SET LOCAL statement_timeout='30s'; SET LOCAL lock_timeout='5s'"
                };
                tx.batch_execute(limits).await.map_err(|_| ProxyError::Database)?;
                tx.query_one("SELECT set_config('forge.coding_owner',$1,true)", &[&caller.owner])
                    .await.map_err(|_| ProxyError::Database)?;
                let params: Vec<WireText> = plan.params.iter().map(|p| WireText(p.as_deref())).collect();
                let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
                let stream = tx.query_raw(&plan.sql, refs).await.map_err(|_| ProxyError::Database)?;
                tokio::pin!(stream);
                let mut rows = Vec::new();
                let mut bytes = 0usize;
                while let Some(row) = stream.try_next().await.map_err(|_| ProxyError::Database)? {
                    let value: Value = row.get::<_, Option<Value>>(0).unwrap_or(Value::Null);
                    bytes = bytes.saturating_add(value.to_string().len() + 1);
                    if bytes > 16 * 1024 * 1024 || rows.len() >= 10000 {
                        return Err(ProxyError::TooLarge);
                    }
                    rows.push(value);
                }
                let result = if let Some(channel) = plan.audit_operation.strip_prefix("notify.") {
                    json!({"notified": channel})
                } else if plan.scalar {
                    match rows.pop().unwrap_or(Value::Null) {
                        Value::Object(map) => Value::Object(map),
                        Value::Null => json!({}),
                        value => json!({"value": value}),
                    }
                } else if plan.sql.starts_with("SELECT jsonb_build_object") {
                    rows.pop().unwrap_or(json!({}))
                } else {
                    json!({"rows": rows})
                };
                if plan.write {
                    let details = json!({"args": plan.audit_args});
                    tx.query_one("SELECT forge_pg_proxy_api.audit($1::text,$2::text,$3::text,$4::jsonb)",
                        &[&caller.actor, &caller.name, &plan.audit_operation, &details])
                        .await.map_err(|_| ProxyError::Database)?;
                }
                tx.commit().await.map_err(|_| ProxyError::Database)?;
                Ok(result)
            }).await.unwrap_or(Err(ProxyError::Database));
            drop(client);
            driver.abort();
            let _ = driver.await;
            result
        })
    }
}
impl IoClient for Proxy {
    type Request = ProxyCommand;
    type Response = Value;
    type Error = ProxyError;
    fn execute(&self, request: Self::Request) -> Result<Value, ProxyError> {
        self.operation(
            self.authenticate(&request.bearer)?,
            &request.operation,
            &request.args,
        )
    }
}
impl Proxy {
    pub fn operation(
        &self,
        c: &Caller,
        operation: &str,
        args: &Value,
    ) -> Result<Value, ProxyError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ProxyError::Closed);
        } // Reject foreign capabilities even in in-process consumers.
        if !self.tokens.iter().any(|(known, _)| std::ptr::eq(known, c)) {
            return Err(ProxyError::Forbidden);
        }
        let plan = OperationPlan::prepare(c, operation, args)?;
        let mut active = self.active.lock().map_err(|_| ProxyError::Closed)?;
        if *active >= 24 {
            return Err(ProxyError::Busy);
        }
        *active += 1;
        drop(active);
        let result = self.perform(c, plan);
        if let Ok(mut active) = self.active.lock() {
            *active -= 1
        }
        result
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// Fixed-query placeholders retain PostgreSQL contextual inference, including one UUID
// used as both a UUID predicate and a JSON-text predicate. No SQL literals interpolate values.
struct WireText<'a>(Option<&'a str>);
impl std::fmt::Debug for WireText<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WireText([redacted])")
    }
}
impl ToSql for WireText<'_> {
    fn to_sql(
        &self,
        _: &tokio_postgres::types::Type,
        out: &mut bytes::BytesMut,
    ) -> Result<tokio_postgres::types::IsNull, Box<dyn std::error::Error + Sync + Send>> {
        match self.0 {
            None => Ok(tokio_postgres::types::IsNull::Yes),
            Some(s) => {
                out.extend_from_slice(s.as_bytes());
                Ok(tokio_postgres::types::IsNull::No)
            }
        }
    }
    fn accepts(_: &tokio_postgres::types::Type) -> bool {
        true
    }
    fn encode_format(&self, _: &tokio_postgres::types::Type) -> tokio_postgres::types::Format {
        tokio_postgres::types::Format::Text
    }
    tokio_postgres::types::to_sql_checked!();
}
