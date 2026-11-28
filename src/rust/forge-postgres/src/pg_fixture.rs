//! Disposable socket-only PostgreSQL 16 cluster for store tests
//! (work_pg_fixture.py), behind the `test-support` feature.
//!
//! Branch-scope CI sets FORGE_DISABLE_DISPOSABLE_POSTGRES=1: database
//! tests run only on the main pipeline build. The SQL directory
//! resolves to deploy/mission-control/sql relative to the repository
//! root (FORGE_SQL_DIR overrides it), mirroring the Python module's
//! location-relative lookup.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const POSTGRES_BIN: &str = "/usr/lib/postgresql/16/bin";

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

pub fn sql_dir() -> PathBuf {
    if let Ok(override_dir) = std::env::var("FORGE_SQL_DIR") {
        if !override_dir.is_empty() {
            return PathBuf::from(override_dir);
        }
    }
    repo_root().join("deploy/mission-control/sql")
}

/// The Python module's PG16_AVAILABLE gate.
pub fn pg16_available() -> bool {
    std::env::var("FORGE_DISABLE_DISPOSABLE_POSTGRES").as_deref() != Ok("1")
        && nix::unistd::geteuid().as_raw() != 0
        && Path::new(POSTGRES_BIN).join("initdb").is_file()
}

fn outside_repo_tempdir() -> Result<PathBuf, String> {
    let mut base = std::env::temp_dir();
    if let Ok(tmpdir) = std::env::var("TMPDIR") {
        if !tmpdir.is_empty() {
            let candidate = PathBuf::from(&tmpdir);
            let resolved = candidate.canonicalize().unwrap_or(candidate);
            if resolved.starts_with(repo_root().canonicalize().unwrap_or(repo_root())) {
                base = PathBuf::from("/tmp");
            }
        }
    }
    for _ in 0..16 {
        let candidate = base.join(format!("forge-work-pg-{}", uuid::Uuid::new_v4().simple()));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("could not create a temporary directory".to_string())
}

fn run(binary: &str, args: &[String], timeout_seconds: u64) -> Result<String, String> {
    let mut child = Command::new(Path::new(POSTGRES_BIN).join(binary))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_end(&mut stdout);
                }
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_end(&mut stderr);
                }
                if status.success() {
                    return Ok(String::from_utf8_lossy(&stdout).into_owned());
                }
                return Err(format!(
                    "{binary} exited with {status}: {}",
                    String::from_utf8_lossy(&stderr)
                ));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{binary} timed out"));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

/// Owns a non-root, socket-only PostgreSQL 16 test cluster outside
/// the repository. Dropping the value stops the cluster and removes
/// its directory, like the Python context manager.
pub struct DisposablePostgresCluster {
    pub root: PathBuf,
    pub data: PathBuf,
    stopped: bool,
}

impl DisposablePostgresCluster {
    pub fn start() -> Result<DisposablePostgresCluster, String> {
        let root = outside_repo_tempdir()?;
        let data = root.join("data");
        let mut cluster = DisposablePostgresCluster {
            root: root.clone(),
            data: data.clone(),
            stopped: false,
        };
        let result = (|| -> Result<(), String> {
            run(
                "initdb",
                &[
                    "-D".to_string(),
                    data.to_string_lossy().into_owned(),
                    "--username=postgres".to_string(),
                    "--auth=trust".to_string(),
                    "--no-locale".to_string(),
                    "--encoding=UTF8".to_string(),
                ],
                40,
            )?;
            run(
                "pg_ctl",
                &[
                    "-D".to_string(),
                    data.to_string_lossy().into_owned(),
                    "-l".to_string(),
                    root.join("postgres.log").to_string_lossy().into_owned(),
                    "-o".to_string(),
                    format!("-k {} -c listen_addresses=", root.to_string_lossy()),
                    "-w".to_string(),
                    "start".to_string(),
                ],
                40,
            )?;
            let psql = cluster.psql_args("postgres");
            let mut args = psql.clone();
            args.extend([
                "-d".to_string(),
                "postgres".to_string(),
                "-f".to_string(),
                sql_dir()
                    .join("provision.sql")
                    .to_string_lossy()
                    .into_owned(),
            ]);
            run("psql", &args, 40)?;
            cluster.apply_and_verify()?;
            cluster.apply_and_verify()?;
            Ok(())
        })();
        if let Err(error) = result {
            cluster.stop();
            return Err(error);
        }
        Ok(cluster)
    }

    fn psql_args(&self, user: &str) -> Vec<String> {
        vec![
            "-h".to_string(),
            self.root.to_string_lossy().into_owned(),
            "-U".to_string(),
            user.to_string(),
            "-X".to_string(),
            "-v".to_string(),
            "ON_ERROR_STOP=1".to_string(),
        ]
    }

    pub fn apply_and_verify(&self) -> Result<(), String> {
        let sql = sql_dir();
        let mut args = self.psql_args("postgres");
        args.extend([
            "-d".to_string(),
            "forge_agent".to_string(),
            "-c".to_string(),
            "SET ROLE forge_agent_owner".to_string(),
            "-f".to_string(),
            sql.join("apply.sql").to_string_lossy().into_owned(),
            "-f".to_string(),
            sql.join("management/grant-runner.sql")
                .to_string_lossy()
                .into_owned(),
            "-f".to_string(),
            sql.join("management/verify.sql")
                .to_string_lossy()
                .into_owned(),
        ]);
        run("psql", &args, 40)?;
        let mut args = self.psql_args("forge_agent");
        args.extend([
            "-d".to_string(),
            "forge_agent".to_string(),
            "-f".to_string(),
            sql.join("management/verify-runner-privileges.sql")
                .to_string_lossy()
                .into_owned(),
        ]);
        run("psql", &args, 40)?;
        Ok(())
    }

    /// Apply ONLY the cutover through sql/apply-cutover.sql, then
    /// re-run grant-runner.sql (see the Python docstring).
    pub fn apply_cutover(&self) -> Result<(), String> {
        let sql = sql_dir();
        let mut args = self.psql_args("postgres");
        args.extend([
            "-d".to_string(),
            "forge_agent".to_string(),
            "-c".to_string(),
            "SET ROLE forge_agent_owner".to_string(),
            "-f".to_string(),
            sql.join("apply-cutover.sql").to_string_lossy().into_owned(),
            "-f".to_string(),
            sql.join("management/grant-runner.sql")
                .to_string_lossy()
                .into_owned(),
        ]);
        run("psql", &args, 40)?;
        Ok(())
    }

    pub fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        if self.data.join("postmaster.pid").exists() {
            let _ = run(
                "pg_ctl",
                &[
                    "-D".to_string(),
                    self.data.to_string_lossy().into_owned(),
                    "-m".to_string(),
                    "immediate".to_string(),
                    "-w".to_string(),
                    "stop".to_string(),
                ],
                30,
            );
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }

    /// Connect over the cluster's unix socket (trust auth).
    pub async fn connect(&self, user: &str) -> Result<tokio_postgres::Client, String> {
        let mut config = tokio_postgres::Config::new();
        config
            .host_path(&self.root)
            .dbname("forge_agent")
            .user(user)
            .connect_timeout(Duration::from_secs(2));
        let (client, connection) = config
            .connect(tokio_postgres::NoTls)
            .await
            .map_err(|error| error.to_string())?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(client)
    }

    pub fn run_sql_script(
        &self,
        script_path: &Path,
        user: &str,
    ) -> Result<(i32, String, String), String> {
        let output = Command::new(Path::new(POSTGRES_BIN).join("psql"))
            .args([
                "-h",
                &self.root.to_string_lossy(),
                "-U",
                user,
                "-X",
                "-v",
                "ON_ERROR_STOP=1",
                "-d",
                "forge_agent",
                "-f",
                &script_path.to_string_lossy(),
            ])
            .stdin(Stdio::null())
            .output()
            .map_err(|error| error.to_string())?;
        Ok((
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }

    pub async fn admin_exec(&self, statements: &[&str], owner: bool) -> Result<(), String> {
        let mut client = self.connect("postgres").await?;
        let mut tx = client
            .transaction()
            .await
            .map_err(|error| error.to_string())?;
        if owner {
            tx.execute("SET ROLE forge_agent_owner", &[])
                .await
                .map_err(|error| error.to_string())?;
        }
        for statement in statements {
            tx.execute(*statement, &[])
                .await
                .map_err(|error| error.to_string())?;
        }
        tx.commit().await.map_err(|error| error.to_string())
    }

    pub async fn reset_retry_events(&self) -> Result<(), String> {
        self.admin_exec(
            &[r#"DO $reset$ BEGIN
              IF to_regclass('forge_agent.coding_retry_events') IS NOT NULL THEN
                EXECUTE 'TRUNCATE forge_agent.coding_retry_events CASCADE';
              END IF;
            END;$reset$;"#],
            false,
        )
        .await
    }

    pub async fn reset_coding_tables(&self) -> Result<(), String> {
        self.reset_retry_events().await?;
        self.admin_exec(
            &[
                "TRUNCATE forge_agent.coding_tasks, forge_agent.coding_worker_grants, \
               forge_agent.coding_worker_readiness, forge_agent.coding_provider_usage, \
               forge_agent.coding_usage_refresh_requests CASCADE",
            ],
            false,
        )
        .await
    }

    pub async fn reset_jenkins_tables(&self) -> Result<(), String> {
        self.reset_retry_events().await?;
        self.admin_exec(
            &[
                "TRUNCATE forge_agent.jenkins_events, forge_agent.jenkins_event_deliveries, \
               forge_agent.jenkins_observations CASCADE",
            ],
            false,
        )
        .await
    }

    pub async fn grant_worker(
        &self,
        owner_id: &str,
        project_alias: &str,
        agent_profile: &str,
        modes: &[&str],
        worker_role: &str,
    ) -> Result<(), String> {
        let mut client = self.connect("postgres").await?;
        let mut tx = client
            .transaction()
            .await
            .map_err(|error| error.to_string())?;
        for mode in modes {
            tx.execute(
                "INSERT INTO forge_agent.coding_worker_grants ( \
                 worker_role, owner_id, project_alias, agent_profile, mode) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
                &[
                    &worker_role,
                    &owner_id,
                    &project_alias,
                    &agent_profile,
                    &mode,
                ],
            )
            .await
            .map_err(|error| error.to_string())?;
        }
        tx.commit().await.map_err(|error| error.to_string())
    }
}

impl Drop for DisposablePostgresCluster {
    fn drop(&mut self) {
        self.stop();
    }
}
