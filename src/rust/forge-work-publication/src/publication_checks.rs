//! Fixed operator checks in a separate credential-free
//! verification scope (work_publication_checks): the
//! ChecksPort implementation for the review worker.
//!
//! Recorded funnel: Python raises bare PublicationErrors for
//! the pre-flight dependency checks; the ChecksPort boundary
//! carries only VerificationFailure, so those codes are
//! projected into the failure report instead.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use forge_work_worker::cli_argv::CHILD_STATE;
use forge_work_worker::lease::Lease;
use forge_work_worker::ports_runtime::SupervisorPort;
use forge_work_worker::process::terminate_and_reap;
use forge_work_worker::review_ports::{ChecksPort, ReviewError, VerificationFailure};
use forge_work_worker::supervisor::unit_name;
use forge_work_worker::types::Invocation;
use forge_work_worker::workspace::WorkspaceLease;

use crate::publication_clients::{uuid, PublicationError};
use crate::publication_config::PublicationConfig;
use crate::verification::{project_verification, VerificationMetadata, VerificationReport};
use forge_work_worker::attempt_helper;

const O_NOFOLLOW: i32 = 0o400000;
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const CAPTURE_TAIL_BYTES: usize = 32768;

fn exclusive_output(path: &Path) -> std::io::Result<File> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(O_NOFOLLOW)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn sha256_file(path: &Path) -> std::io::Result<[u8; 32]> {
    let data = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    Ok(hasher.finalize().into())
}

fn fail_report(checks: &[Vec<String>], code: &str) -> VerificationFailure {
    VerificationFailure {
        report: Box::new(
            project_verification(
                checks,
                Some(code),
                -1,
                b"",
                &format!("{:x}", Sha256::digest(b"")),
                false,
                None,
            )
            .into_data(),
        ),
    }
}

pub struct IsolatedCheckRunner {
    pub config: PublicationConfig,
    pub supervisor: Box<dyn SupervisorPort + Send>,
    pub timeout: f64,
}

impl IsolatedCheckRunner {
    pub fn new(config: PublicationConfig, supervisor: Box<dyn SupervisorPort + Send>) -> Self {
        IsolatedCheckRunner {
            config,
            supervisor,
            timeout: 1800.0,
        }
    }

    fn run_inner(
        &mut self,
        job: &Value,
        repository: &Path,
        stop: &AtomicBool,
    ) -> Result<VerificationReport, VerificationFailure> {
        let directory = repository
            .parent()
            .unwrap_or_else(|| Path::new("/"))
            .join(format!(
                "checks-{}",
                uuid(Some(&job["lease_token"]))
                    .map_err(|error| fail_report(&self.config.checks, error.code()))?
            ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|_| fail_report(&self.config.checks, "verification_process_unavailable"))?;
        for name in ["tmp", "cargo-home", "cargo-target", "home", "qa"] {
            let _ = std::fs::DirBuilder::new()
                .mode(0o700)
                .create(directory.join(name));
        }
        let browser = Path::new("src/rust/forge-ide-web/tests/browser");
        let project_root = crate::publication_clients::project_path(
            &self.config,
            job["project_alias"].as_str().unwrap_or_default(),
        )
        .ok_or_else(|| fail_report(&self.config.checks, "verification_process_unavailable"))?;
        let source = project_root.join(browser);
        let target = repository.join(browser);
        if self
            .config
            .checks
            .iter()
            .any(|command| command == &vec!["/usr/bin/make".to_string(), "ui-qa".to_string()])
        {
            let source_lock = source.join("package-lock.json");
            let target_lock = target.join("package-lock.json");
            let available = source.join("node_modules").is_dir()
                && source_lock.is_file()
                && target_lock.is_file()
                && sha256_file(&source_lock).ok() == sha256_file(&target_lock).ok();
            if !available {
                return Err(fail_report(
                    &self.config.checks,
                    "verification_dependencies_unavailable",
                ));
            }
            let support = target.join("node_modules");
            let changed = support
                .symlink_metadata()
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false)
                || (support.exists()
                    && std::fs::read_dir(&support)
                        .map(|mut rows| rows.next().is_some())
                        .unwrap_or(false));
            if changed {
                return Err(fail_report(
                    &self.config.checks,
                    "verification_dependencies_changed",
                ));
            }
            let _ = std::fs::create_dir(&support);
        }
        let unavailable = || fail_report(&self.config.checks, "verification_process_unavailable");
        let helper =
            attempt_helper::stage(&directory, attempt_helper::CHECKS).map_err(|_| unavailable())?;
        let plan = directory.join("checks.json");
        let mut output = exclusive_output(&plan).map_err(|_| unavailable())?;
        serde_json::to_writer(&mut output, &self.config.checks).map_err(|_| unavailable())?;
        output
            .set_permissions(std::fs::Permissions::from_mode(0o444))
            .map_err(|_| unavailable())?;
        output.sync_all().map_err(|_| unavailable())?;
        drop(output);
        let workspace = WorkspaceLease {
            path: repository.to_path_buf(),
            repository: project_root.clone(),
            git_directory: repository.join(".git"),
            task_id: job["task_id"].as_str().unwrap_or_default().to_string(),
            base_revision: job["base_revision"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            source_repository: None,
            execution_branch: None,
        };
        let invocation = Invocation {
            argv: vec![format!("{CHILD_STATE}/{}", attempt_helper::CHECKS)],
            stdin: Vec::new(),
            backend: "verification".to_string(),
            cwd: repository.to_path_buf(),
            scratch: directory.clone(),
            workspace,
            implement: true,
            session: None,
            plan_file: None,
            readonly_files: vec![helper, plan],
            external_sandbox: false,
            native_auth: None,
            transport: None,
        };
        let publication_id = uuid(Some(&job["publication_id"]))
            .map_err(|error| fail_report(&self.config.checks, error.code()))?;
        let unit = unit_name(&publication_id)
            .map_err(|_| fail_report(&self.config.checks, "verification_process_unavailable"))?;
        let lease = Lease {
            task_id: String::new(),
            project_alias: String::new(),
            base_revision: String::new(),
            mode: "implement".to_string(),
            agent_profile: String::new(),
            prompt: String::new(),
            acceptance_criteria: String::new(),
            recipe: Value::Null,
            attempt_id: publication_id.clone(),
            attempt_token: String::new(),
            worker_id: String::new(),
            incarnation: String::new(),
            lease_expires_at: String::new(),
            deadline_at: String::new(),
            unit_name: unit.clone(),
            task_kind: "feature".to_string(),
            defect: None,
            resume_workspace_digest: None,
            resume_attempt_id: None,
            plan_decision_id: None,
            feedback_id: None,
            feedback_sha256: None,
            repair_source_review_id: None,
            repair_candidate_commit: None,
            scope_plan_decision_id: None,
            parent_task_id: None,
            workflow_id: None,
            workflow_review_id: None,
            review_target_sha256: None,
            workflow_feedback_review_id: None,
            workflow_feedback_sha256: None,
        };
        let argv = self
            .supervisor
            .argv(&lease, &invocation)
            .map_err(|_| fail_report(&self.config.checks, "verification_process_unavailable"))?;

        let mut metadata = VerificationMetadata::new(self.config.checks.len() as i64);
        let mut evidence = Sha256::new();
        let mut captured: Vec<u8> = Vec::new();
        let mut total: usize = 0;
        let mut failure: Option<PublicationError> = None;
        let mut returncode: i64 = -1;
        let mut log: Option<File> = None;
        let mut process: Option<Child> = None;
        let mut outcome = || {
            if stop.load(Ordering::SeqCst) {
                failure = Some(PublicationError::Code("publication_lease_lost".to_string()));
                return;
            }
            // Open and protect the descriptor before any
            // untrusted process can replace the visible filename.
            match exclusive_output(&directory.join("checks.log")) {
                Ok(file) => log = Some(file),
                Err(_) => {
                    failure = Some(PublicationError::Code(
                        "verification_process_unavailable".to_string(),
                    ));
                    return;
                }
            }
            let spawned = Command::new(&argv[0])
                .args(&argv[1..])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("LANG", "C.UTF-8")
                .process_group(0)
                .spawn();
            let mut child = match spawned {
                Ok(child) => child,
                Err(_) => {
                    failure = Some(PublicationError::Code(
                        "verification_process_unavailable".to_string(),
                    ));
                    return;
                }
            };
            let (sender, receiver) = mpsc::channel::<(&'static str, Vec<u8>)>();
            let mut threads = Vec::new();
            if let Some(mut stdout) = child.stdout.take() {
                let sender = sender.clone();
                threads.push(std::thread::spawn(move || {
                    use std::io::Read;
                    let mut buffer = [0u8; 65536];
                    loop {
                        match stdout.read(&mut buffer) {
                            Ok(0) | Err(_) => break,
                            Ok(count) => {
                                if sender.send(("stdout", buffer[..count].to_vec())).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }));
            }
            if let Some(mut stderr) = child.stderr.take() {
                let sender = sender.clone();
                threads.push(std::thread::spawn(move || {
                    use std::io::Read;
                    let mut buffer = [0u8; 65536];
                    loop {
                        match stderr.read(&mut buffer) {
                            Ok(0) | Err(_) => break,
                            Ok(count) => {
                                if sender.send(("stderr", buffer[..count].to_vec())).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }));
            }
            drop(sender);
            let deadline = Instant::now() + Duration::from_secs_f64(self.timeout);
            let mut open = true;
            while open {
                if stop.load(Ordering::SeqCst) || Instant::now() >= deadline {
                    failure = Some(PublicationError::Code(
                        "verification_timeout_or_cancelled".to_string(),
                    ));
                    break;
                }
                match receiver.recv_timeout(Duration::from_millis(200)) {
                    Ok((stream, data)) => {
                        total += data.len();
                        if total > MAX_OUTPUT_BYTES {
                            failure = Some(PublicationError::Code(
                                "verification_output_limit".to_string(),
                            ));
                            break;
                        }
                        if let Some(file) = log.as_mut() {
                            let _ = file.write_all(&data);
                        }
                        evidence.update(&data);
                        metadata.feed(stream, &data);
                        captured.extend_from_slice(&data);
                        if captured.len() > CAPTURE_TAIL_BYTES {
                            let excess = captured.len() - CAPTURE_TAIL_BYTES;
                            captured.drain(..excess);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if child
                            .try_wait()
                            .map(|status| status.is_some())
                            .unwrap_or(false)
                        {
                            // Give the reader threads a brief
                            // window to flush the tail.
                            match receiver.recv_timeout(Duration::from_millis(50)) {
                                Ok(_) => {}
                                Err(_) => open = false,
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => open = false,
                }
            }
            // Python waits up to 15s for the exit; a process
            // still running after that is reaped in the finally
            // path with returncode unset.
            let wait_deadline = Instant::now() + Duration::from_secs(15);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        returncode = status.code().map(|code| code as i64).unwrap_or(-1);
                        if returncode != 0 && failure.is_none() {
                            failure =
                                Some(PublicationError::Code("verification_failed".to_string()));
                        }
                        break;
                    }
                    Ok(None) => {
                        if Instant::now() >= wait_deadline {
                            if failure.is_none() {
                                failure = Some(PublicationError::Code(
                                    "verification_process_unavailable".to_string(),
                                ));
                            }
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => {
                        if failure.is_none() {
                            failure = Some(PublicationError::Code(
                                "verification_process_unavailable".to_string(),
                            ));
                        }
                        break;
                    }
                }
            }
            process = Some(child);
        };
        outcome();
        // finally: stop the scope and reap the process.
        let proof = self.supervisor.stop(&unit);
        let reaped = match process.as_mut() {
            Some(child) => terminate_and_reap(child, 2.0, 2.0).unwrap_or(false),
            None => true,
        };
        let cleanup = reaped && proof.map(|proof| proof.scope_stopped).unwrap_or(false);
        if let Some(file) = log.take() {
            drop(file);
        }
        if !cleanup {
            failure = Some(PublicationError::Code(
                "verification_scope_unstopped".to_string(),
            ));
        }
        let evidence_digest = format!("{:x}", evidence.finalize());
        let outcome = project_verification(
            &self.config.checks,
            failure.as_ref().map(|error| error.code()),
            returncode,
            &captured,
            &evidence_digest,
            cleanup,
            Some(&metadata),
        );
        // The receipt is parent-owned and outside the
        // child-visible scratch.
        let report_path = repository
            .parent()
            .unwrap_or_else(|| Path::new("/"))
            .join(format!(
                "checks-{}.json",
                job["lease_token"].as_str().unwrap_or_default()
            ));
        let receipt = json!({
            "status": if outcome.checks_passed { "passed" } else { "failed" },
            "commands": self.config.checks,
            "publication_id": job["publication_id"],
            "verification": outcome.to_dict(),
        });
        match exclusive_output(&report_path).and_then(|mut file| {
            file.write_all(
                format!(
                    "{}\n",
                    serde_json::to_string_pretty(&receipt).unwrap_or_default()
                )
                .as_bytes(),
            )
        }) {
            Ok(()) => {}
            Err(_) => {
                let report = project_verification(
                    &self.config.checks,
                    Some("verification_receipt_unavailable"),
                    returncode,
                    b"",
                    &evidence_digest,
                    cleanup,
                    None,
                );
                return Err(VerificationFailure {
                    report: Box::new(report.into_data()),
                });
            }
        }
        if failure.is_some() {
            return Err(VerificationFailure {
                report: Box::new(outcome.into_data()),
            });
        }
        Ok(outcome)
    }
}

impl ChecksPort for IsolatedCheckRunner {
    fn quiesce(&mut self, job: &Value) -> Result<(), ReviewError> {
        let publication_id = uuid(Some(&job["publication_id"]))?;
        let unit =
            unit_name(&publication_id).map_err(|error| ReviewError::other(error.to_string()))?;
        let proof = self
            .supervisor
            .stop(&unit)
            .map_err(|error| ReviewError::other(error.to_string()))?;
        if !proof.scope_stopped {
            return Err(
                PublicationError::Code("prior_verification_scope_unstopped".to_string()).into(),
            );
        }
        Ok(())
    }

    fn run(
        &mut self,
        job: &Value,
        repo: &Path,
        lost: &AtomicBool,
    ) -> Result<forge_work_worker::review_ports::VerificationReportData, VerificationFailure> {
        match self.run_inner(job, repo, lost) {
            Ok(report) => Ok(report.into_data()),
            Err(failure) => Err(failure),
        }
    }
}
