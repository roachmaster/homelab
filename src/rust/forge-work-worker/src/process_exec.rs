//! Bounded subprocess execution behind `forge_core::io_client::IoClient`.
//!
//! Unifies the worker's bounded subprocess calls (`CommandGitRunner`,
//! `SystemCommandPort`, and `consumer_cache::git`) behind a single typed
//! `IoClient` contract with an explicit `EnvPolicy`, plus an effect-free
//! `forge_core::bootstrap` registrar for host composition.

use crate::error::WorkerError;
use forge_core::bootstrap::PreparedGroup;
use forge_core::io_client::IoClient;
use forge_core::provider::Factory;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub const PROCESS_EXEC_GROUP: &str = "forge.work_worker.process_exec";
pub const PROCESS_EXEC_BINDING: &str = "forge.work_worker.process_exec.client";

/// Explicit environment policy for a bounded subprocess invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvPolicy {
    /// Inherit the parent process environment unchanged.
    Inherit,
    /// Clear the environment, retain only allowlisted host keys, and set explicit overrides.
    Cleared {
        retain: Vec<String>,
        set: Vec<(String, String)>,
    },
}

/// Prepared request for one bounded subprocess execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRequest {
    pub argv: Vec<String>,
    pub timeout_seconds: u64,
    pub env: EnvPolicy,
}

impl ProcessRequest {
    pub fn inherited(argv: Vec<String>, timeout_seconds: u64) -> Self {
        Self {
            argv,
            timeout_seconds,
            env: EnvPolicy::Inherit,
        }
    }

    pub fn cleared(
        argv: Vec<String>,
        timeout_seconds: u64,
        retain: &[&str],
        set: &[(&str, &str)],
    ) -> Self {
        Self {
            argv,
            timeout_seconds,
            env: EnvPolicy::Cleared {
                retain: retain.iter().map(|key| (*key).to_string()).collect(),
                set: set
                    .iter()
                    .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                    .collect(),
            },
        }
    }
}

/// Captured exit status and stdout bytes from a bounded subprocess execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
}

/// Concrete `IoClient` that spawns a bounded subprocess with stdout piped,
/// stdin/stderr suppressed, and a hard wall-clock kill deadline.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemProcessExec;

impl IoClient for SystemProcessExec {
    type Request = ProcessRequest;
    type Response = ProcessOutput;
    type Error = WorkerError;

    fn execute(&self, request: Self::Request) -> Result<Self::Response, Self::Error> {
        let program = request
            .argv
            .first()
            .filter(|arg| !arg.is_empty())
            .ok_or_else(|| WorkerError::Protocol("command argv must not be empty".to_string()))?;

        let mut builder = Command::new(program);
        builder
            .args(&request.argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        match &request.env {
            EnvPolicy::Inherit => {}
            EnvPolicy::Cleared { retain, set } => {
                builder.env_clear();
                for (key, value) in std::env::vars() {
                    if retain.iter().any(|allowed| allowed == &key) {
                        builder.env(key, value);
                    }
                }
                for (key, value) in set {
                    builder.env(key, value);
                }
            }
        }

        let mut child = builder
            .spawn()
            .map_err(|error| WorkerError::Protocol(format!("command spawn failed: {error}")))?;
        let mut pipe = child.stdout.take().expect("stdout piped");
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut data = Vec::new();
            let _ = pipe.read_to_end(&mut data);
            let _ = tx.send(data);
        });

        let deadline = Instant::now() + Duration::from_secs(request.timeout_seconds.max(1));
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(WorkerError::Protocol(
                            "command exceeded its time bound".to_string(),
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => {
                    return Err(WorkerError::Protocol(format!(
                        "command wait failed: {error}"
                    )));
                }
            }
        };

        let stdout = rx.recv().unwrap_or_default();
        Ok(ProcessOutput {
            exit_code: status.code().unwrap_or(-1),
            stdout,
        })
    }
}

/// Factory for `SystemProcessExec`. Creation is explicit and side-effect-free.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessExecFactory;

impl Factory<SystemProcessExec> for ProcessExecFactory {
    type Error = WorkerError;

    fn create(&self) -> Result<SystemProcessExec, Self::Error> {
        Ok(SystemProcessExec)
    }
}

/// Stage the worker's process-exec `IoClient` factory into a `PreparedGroup`.
pub fn register_process_exec() -> Result<PreparedGroup<ProcessExecFactory>, WorkerError> {
    Ok(PreparedGroup::new(
        PROCESS_EXEC_GROUP,
        &[PROCESS_EXEC_BINDING],
        &[],
        ProcessExecFactory,
    )
    .expect("fixed process-exec group identifiers are valid"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports_runtime::{CommandPort, GitRunner, ProcessCommandPort, ProcessGitRunner};
    use forge_core::bootstrap::Registry;
    use forge_core::provider::{InstanceProvider, Singleton};
    use std::cell::RefCell;
    use std::path::Path;

    struct FakeProcessExec {
        captured: RefCell<Vec<ProcessRequest>>,
        response: RefCell<Result<ProcessOutput, WorkerError>>,
    }

    impl FakeProcessExec {
        fn returning(response: Result<ProcessOutput, WorkerError>) -> Self {
            Self {
                captured: RefCell::new(Vec::new()),
                response: RefCell::new(response),
            }
        }
    }

    impl IoClient for FakeProcessExec {
        type Request = ProcessRequest;
        type Response = ProcessOutput;
        type Error = WorkerError;

        fn execute(&self, request: Self::Request) -> Result<Self::Response, Self::Error> {
            self.captured.borrow_mut().push(request);
            match &*self.response.borrow() {
                Ok(out) => Ok(out.clone()),
                Err(WorkerError::Protocol(msg)) => Err(WorkerError::Protocol(msg.clone())),
                Err(_) => Err(WorkerError::Protocol("fake failure".to_string())),
            }
        }
    }

    #[test]
    fn rejects_empty_argv_without_spawning() {
        let err = SystemProcessExec
            .execute(ProcessRequest::inherited(Vec::new(), 5))
            .unwrap_err();
        assert!(matches!(err, WorkerError::Protocol(_)));

        let err = SystemProcessExec
            .execute(ProcessRequest::inherited(vec![String::new()], 5))
            .unwrap_err();
        assert!(matches!(err, WorkerError::Protocol(_)));
    }

    #[test]
    fn executes_real_subprocess_and_captures_stdout_and_exit_code() {
        let out = SystemProcessExec
            .execute(ProcessRequest::inherited(
                vec!["/bin/echo".into(), "forge-ok".into()],
                5,
            ))
            .unwrap();
        assert_eq!(out.exit_code, 0);
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "forge-ok");

        let failed = SystemProcessExec
            .execute(ProcessRequest::inherited(vec!["/bin/false".into()], 5))
            .unwrap();
        assert_ne!(failed.exit_code, 0);
    }

    #[test]
    fn cleared_env_policy_strips_unlisted_vars_and_sets_overrides() {
        let out = SystemProcessExec
            .execute(ProcessRequest::cleared(
                vec!["/usr/bin/env".into()],
                5,
                &[],
                &[("FORGE_EXEC_MARKER", "present")],
            ))
            .unwrap();
        assert_eq!(out.exit_code, 0);
        let env_text = String::from_utf8_lossy(&out.stdout);
        assert!(env_text.contains("FORGE_EXEC_MARKER=present"));
        assert!(!env_text.contains("HOME="));
    }

    #[test]
    fn kills_subprocess_when_timeout_expires() {
        let err = SystemProcessExec
            .execute(ProcessRequest::inherited(
                vec!["/bin/sleep".into(), "5".into()],
                1,
            ))
            .unwrap_err();
        match err {
            WorkerError::Protocol(msg) => assert!(msg.contains("time bound"), "got: {msg}"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn registrar_stages_freezes_and_rejects_duplicates() {
        let reg = Registry::new()
            .register(register_process_exec().unwrap())
            .expect("first registration succeeds");
        let rejected = reg
            .register(register_process_exec().unwrap())
            .expect_err("duplicate binding rejected");
        assert_eq!(rejected.original().group_count(), 1);

        let frozen = rejected.into_parts().0.freeze().expect("freeze succeeds");
        let ((), factory) = frozen.into_factories();
        let provider = Singleton::new(factory.create().expect("factory creates client"));
        let client = provider.acquire().expect("provider acquires client");
        let out = client
            .execute(ProcessRequest::inherited(
                vec!["/bin/echo".into(), "wired".into()],
                5,
            ))
            .unwrap();
        assert_eq!(out.exit_code, 0);
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "wired");
    }

    #[test]
    fn process_git_runner_builds_hardened_request_and_masks_failures() {
        let fake = FakeProcessExec::returning(Ok(ProcessOutput {
            exit_code: 0,
            stdout: b"abc123\n".to_vec(),
        }));
        let mut runner = ProcessGitRunner::new(fake, 40);
        let out = runner
            .git(Path::new("/tmp/repo"), &["rev-parse", "HEAD"])
            .unwrap();
        assert_eq!(out, "abc123\n");

        let reqs = runner.client().captured.borrow();
        assert_eq!(reqs.len(), 1);
        assert_eq!(
            reqs[0].argv,
            vec![
                "/usr/bin/git",
                "-c",
                "core.hooksPath=/dev/null",
                "-C",
                "/tmp/repo",
                "rev-parse",
                "HEAD",
            ]
        );
        assert_eq!(reqs[0].timeout_seconds, 40);
        match &reqs[0].env {
            EnvPolicy::Cleared { retain, set } => {
                assert!(retain.iter().any(|k| k == "PATH"));
                assert!(set.contains(&("GIT_CONFIG_NOSYSTEM".into(), "1".into())));
                assert!(set.contains(&("GIT_CONFIG_GLOBAL".into(), "/dev/null".into())));
                assert!(set.contains(&("GIT_TERMINAL_PROMPT".into(), "0".into())));
            }
            EnvPolicy::Inherit => panic!("git runner must clear environment"),
        }
        drop(reqs);

        let failing = FakeProcessExec::returning(Ok(ProcessOutput {
            exit_code: 128,
            stdout: Vec::new(),
        }));
        let mut failing_runner = ProcessGitRunner::new(failing, 40);
        let err = failing_runner
            .git(Path::new("/tmp/repo"), &["status"])
            .unwrap_err();
        assert!(format!("{err:?}").contains("approved repository or revision is unavailable"));
    }

    #[test]
    fn process_command_port_forwards_inherited_request() {
        let fake = FakeProcessExec::returning(Ok(ProcessOutput {
            exit_code: 0,
            stdout: b"ActiveState=inactive\n".to_vec(),
        }));
        let mut port = ProcessCommandPort::new(fake);
        let argv = vec!["systemctl".into(), "show".into(), "unit.scope".into()];
        let (code, stdout) = port.run(&argv, 10).unwrap();
        assert_eq!(code, 0);
        assert_eq!(stdout, b"ActiveState=inactive\n");

        let reqs = port.client().captured.borrow();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].argv, argv);
        assert_eq!(reqs[0].timeout_seconds, 10);
        assert_eq!(reqs[0].env, EnvPolicy::Inherit);
    }
}
