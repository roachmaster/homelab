//! Production composition for the coding worker host (work_worker.py's
//! default_store_factory and the default context factories).
//!
//! Connections are direct through this adapter: the Python proxy
//! switch was retired with the S2 config (database.proxy is rejected
//! at parse). Connection arguments mirror the Python
//! connection_arguments: socket host, connect_timeout 4s,
//! statement_timeout 10s, lock_timeout 4s (the session applies the
//! statement timeout; lock timeout rides the session options).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use forge_core::bootstrap::PreparedGroup;
use forge_core::provider::Factory;
use forge_usage::collector::UsagePollStore;
use forge_work_worker::config::WorkerConfig;
use forge_work_worker::error::WorkerError;
use forge_work_worker::ports_runtime::CommandGitRunner;
use forge_work_worker::ports_worker::{ExternalContextPort, PeerContextPort, ReviewContextPort};
use forge_work_worker::registrar::CODING_STORE;
use forge_work_worker::worker::{StoreFactory, StoreHandles};

use crate::client::PgSession;
use crate::config::PgConfig;
use crate::context_providers::{BuildContextProvider, PeerContextProvider, ReviewContextProvider};
use crate::error::AdapterError;
use crate::external_context::{ExternalContextProvider, ExternalGitPort};
use crate::factory::PgSessionFactory;
use crate::reconciler::ReconcilerApi;
use crate::registrar::{RECONCILER_SESSION, WORKER_SESSION};
use crate::usage_store_adapter::PgUsagePollStore;
use crate::worker::WorkerApi;

/// Read one entry from a libpq passfile (host:port:database:user:
/// password, `*` wildcards, backslash escapes).
pub(crate) fn passfile_password(
    path: &Path,
    host: &str,
    port: u16,
    database: &str,
    user: &str,
) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields: Vec<String> = Vec::new();
        let mut current = String::new();
        let mut chars = line.chars();
        while let Some(character) = chars.next() {
            match character {
                '\\' => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                ':' => {
                    fields.push(std::mem::take(&mut current));
                }
                _ => current.push(character),
            }
        }
        fields.push(current);
        if fields.len() != 5 {
            continue;
        }
        let matches = |field: &str, expected: &str| field == "*" || field == expected;
        if matches(&fields[0], host)
            && matches(&fields[1], &port.to_string())
            && matches(&fields[2], database)
            && matches(&fields[3], user)
        {
            return Some(fields[4].clone());
        }
    }
    None
}

/// The PgConfig for one database role (worker or reconciler).
pub fn pg_config_for(config: &WorkerConfig, reconciler: bool) -> PgConfig {
    let database = &config.database;
    let user = if reconciler {
        database.reconciler_user.clone()
    } else {
        database.user.clone()
    };
    let host = database.host.to_string_lossy().into_owned();
    let password = database
        .passfile
        .as_deref()
        .and_then(|path| passfile_password(path, &host, database.port, &database.name, &user));
    PgConfig {
        host,
        port: database.port,
        dbname: database.name.clone(),
        user,
        password,
        owner: config.owner.clone(),
        connect_timeout: Duration::from_secs(4),
        statement_timeout: Duration::from_secs(10),
    }
}

fn session(config: &WorkerConfig, reconciler: bool) -> Result<PgSession, AdapterError> {
    PgSession::connect(pg_config_for(config, reconciler))
}

fn worker_api(config: &WorkerConfig) -> Result<WorkerApi, WorkerError> {
    Ok(WorkerApi::new(
        session(config, false).map_err(crate::worker_adapter::map_err)?,
    ))
}

/// default_store_factory: separate handles over the same connect
/// factory (the Python facade opens a connection per call).
pub fn coding_store_handles(config: &WorkerConfig) -> Result<StoreHandles, WorkerError> {
    coding_store_handles_from(
        &PgSessionFactory::new(pg_config_for(config, false)),
        &PgSessionFactory::new(pg_config_for(config, true)),
    )
}

/// The store handles over the two role session factories: four
/// worker-role handles, then the reconciler handle, each its own
/// connection.
fn coding_store_handles_from(
    worker: &PgSessionFactory,
    reconciler: &PgSessionFactory,
) -> Result<StoreHandles, WorkerError> {
    let api = || -> Result<WorkerApi, WorkerError> {
        Ok(WorkerApi::new(
            worker.create().map_err(crate::worker_adapter::map_err)?,
        ))
    };
    Ok(StoreHandles {
        worker: Box::new(api()?),
        renew: Box::new(api()?),
        progress: Box::new(api()?),
        contexts: Box::new(api()?),
        reconciler: Box::new(ReconcilerApi::new(
            reconciler
                .create()
                .map_err(crate::worker_adapter::map_err)?,
        )),
    })
}

/// The coding worker's database group: it provides `coding.store`,
/// which the coding worker group requires, and requires the worker-
/// and reconciler-role session groups (`register_coding_sessions`).
/// Once the registry is frozen it turns those session factories into
/// the store factory the worker recipe binds. Staging it opens no
/// connection.
pub struct CodingStore;

impl CodingStore {
    /// The store factory the worker calls at create(). The sessions
    /// were configured from the worker config at registration, so the
    /// config the worker passes back is not consulted.
    pub fn bind(&self, worker: PgSessionFactory, reconciler: PgSessionFactory) -> StoreFactory {
        Box::new(move |_config| coding_store_handles_from(&worker, &reconciler))
    }
}

/// Stage the coding worker's database group.
pub fn register_coding_store() -> PreparedGroup<CodingStore> {
    PreparedGroup::new(
        "forge.postgres.coding_store",
        &[CODING_STORE],
        &[WORKER_SESSION, RECONCILER_SESSION],
        CodingStore,
    )
    .expect("fixed coding store group identifiers are valid")
}

/// Stage the worker- and reconciler-role session groups for a coding
/// worker config (`forge.postgres.worker`, `forge.postgres.reconciler`).
pub fn register_coding_sessions(
    config: &WorkerConfig,
) -> Result<
    (
        PreparedGroup<PgSessionFactory>,
        PreparedGroup<PgSessionFactory>,
    ),
    AdapterError,
> {
    Ok((
        crate::registrar::register_worker(pg_config_for(config, false))?,
        crate::registrar::register_reconciler(pg_config_for(config, true))?,
    ))
}

/// The default review context factory (ReviewContextProvider with a
/// BuildContextProvider when a build results root is configured).
pub fn coding_review_contexts(config: &WorkerConfig) -> Option<Box<dyn ReviewContextPort>> {
    let api = worker_api(config).ok()?;
    let build_provider = config.build_results_root.as_ref().map(|root| {
        BuildContextProvider::new(
            // A second handle for the build provider, as in Python
            // where both share the one store facade.
            worker_api(config)
                .unwrap_or_else(|_| WorkerApi::new(session(config, false).expect("session"))),
            &config.state_root,
            root,
        )
    });
    Some(Box::new(ReviewContextProvider::new(
        api,
        build_provider,
        Box::new(CommandGitRunner::default()),
    )))
}

/// The default peer context factory.
pub fn coding_peer_contexts(config: &WorkerConfig) -> Option<Box<dyn PeerContextPort>> {
    let api = worker_api(config).ok()?;
    let source = crate::candidate_source::PeerCandidateSource {
        state_root: config.state_root.clone(),
        projects: config.projects.clone(),
        git: Box::new(CommandGitRunner::default()),
    };
    Some(Box::new(PeerContextProvider::new(
        api,
        Some(source),
        Box::new(CommandGitRunner::default()),
    )))
}

/// Bounded workspace git for the external context (work_worker.py's
/// _workspace_git with the default_external_context_factory's 40s
/// bound).
struct WorkspaceGit;

impl ExternalGitPort for WorkspaceGit {
    fn git_bytes(&mut self, repo: &Path, args: &[&str]) -> Result<Vec<u8>, WorkerError> {
        let mut command = std::process::Command::new("/usr/bin/git");
        command
            .arg("-C")
            .arg(repo)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        use std::io::Read;
        let mut child = command
            .spawn()
            .map_err(|error| WorkerError::protocol(error.to_string()))?;
        let mut stdout_pipe = child.stdout.take().expect("piped stdout");
        let stdout_thread = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stdout_pipe.read_to_end(&mut bytes);
            bytes
        });
        let mut stderr_pipe = child.stderr.take().expect("piped stderr");
        let stderr_thread = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr_pipe.read_to_end(&mut bytes);
            bytes
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(40);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let stdout = stdout_thread.join().unwrap_or_default();
                    let stderr = stderr_thread.join().unwrap_or_default();
                    if status.success() {
                        return Ok(stdout);
                    }
                    return Err(WorkerError::protocol(format!(
                        "git exited with {status}: {}",
                        String::from_utf8_lossy(&stderr)
                    )));
                }
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(WorkerError::protocol("git timed out".to_string()));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(WorkerError::protocol(error.to_string())),
            }
        }
    }
}

/// The default external context factory. Recorded divergence: the
/// artifact port is None — Python's default supplies a
/// GitArtifactProviderFactory product; the Rust provider fails
/// closed on the path that needs artifacts until an artifact-id
/// adapter over publication's GitArtifactSource is designed.
pub fn coding_external_contexts(config: &WorkerConfig) -> Option<Box<dyn ExternalContextPort>> {
    let api = worker_api(config).ok()?;
    Some(Box::new(ExternalContextProvider::new(
        api,
        Box::new(WorkspaceGit),
        None,
    )))
}

/// The usage poll store the collector factory composes over.
pub fn coding_usage_poll_store(config: &WorkerConfig) -> Arc<dyn UsagePollStore> {
    Arc::new(PgUsagePollStore::new(pg_config_for(config, false)))
}

#[cfg(test)]
mod coding_store_tests {
    use super::*;
    use forge_core::bootstrap::Registry;
    use forge_work_worker::config::parse_config;
    use forge_work_worker::registrar::register;
    use forge_work_worker::worker::WorkerBindings;

    fn coding_config() -> WorkerConfig {
        parse_config(&serde_json::json!({
            "worker_id": "worker-1",
            "owner": "forge",
            "database": {"host": "/run/postgresql", "port": 5432, "name": "forge_agent", "user": "forge_coding_worker"},
            "account": {"user": "forgeqa", "uid": 1001, "gid": 1001, "home": "/home/forgeqa"},
            "workspace_root": "/srv/forge/workspaces",
            "state_root": "/srv/forge/state",
            "projects": {"homelab": "/srv/repos/homelab"},
            "profiles": {"codex-author": {"backend": "codex", "model": "gpt-5", "modes": ["plan"]}},
            "binaries": {"codex": "/usr/local/bin/codex"}
        }))
        .unwrap()
    }

    #[test]
    fn the_postgres_store_group_satisfies_the_worker_group() {
        let config = coding_config();
        let (worker_session, reconciler_session) = register_coding_sessions(&config).unwrap();
        let frozen = Registry::new()
            .register(worker_session)
            .unwrap()
            .register(reconciler_session)
            .unwrap()
            .register(register_coding_store())
            .unwrap()
            .register(register(config, WorkerBindings::default()).unwrap())
            .unwrap()
            .freeze();
        let Ok(frozen) = frozen else {
            panic!("the postgres store group provides coding.store");
        };
        let (((((), worker_session), reconciler_session), store), worker) = frozen.into_factories();
        // Binding connects nothing; only create() would open PostgreSQL.
        assert!(worker
            .bind(store.bind(worker_session, reconciler_session))
            .is_ok());
    }

    #[test]
    fn the_session_groups_carry_each_role_from_the_worker_config() {
        let config = coding_config();
        let (worker_session, reconciler_session) = register_coding_sessions(&config).unwrap();
        let ((((), worker_session), reconciler_session), _) = Registry::new()
            .register(worker_session)
            .unwrap()
            .register(reconciler_session)
            .unwrap()
            .register(register_coding_store())
            .unwrap()
            .freeze()
            .unwrap_or_else(|_| panic!("the store's sessions are staged"))
            .into_factories();
        assert_eq!(worker_session.config().user, "forge_coding_worker");
        assert_eq!(reconciler_session.config().user, "forge_coding_reconciler");
        for (session, reconciler) in [(&worker_session, false), (&reconciler_session, true)] {
            let expected = pg_config_for(&config, reconciler);
            let staged = session.config();
            assert_eq!(
                (
                    &staged.host,
                    staged.port,
                    &staged.dbname,
                    &staged.user,
                    &staged.owner
                ),
                (
                    &expected.host,
                    expected.port,
                    &expected.dbname,
                    &expected.user,
                    &expected.owner
                )
            );
            assert_eq!(staged.statement_timeout, expected.statement_timeout);
        }
    }

    #[test]
    fn a_coding_store_without_the_reconciler_session_is_rejected_at_freeze() {
        let (worker_session, _) = register_coding_sessions(&coding_config()).unwrap();
        let Err(rejected) = Registry::new()
            .register(worker_session)
            .unwrap()
            .register(register_coding_store())
            .unwrap()
            .freeze()
        else {
            panic!("the coding store requires the reconciler session");
        };
        assert_eq!(
            rejected.reason(),
            &forge_core::bootstrap::ValidationError::MissingRequirement {
                group: "forge.postgres.coding_store".to_string(),
                binding: RECONCILER_SESSION.to_string(),
            }
        );
    }
}
