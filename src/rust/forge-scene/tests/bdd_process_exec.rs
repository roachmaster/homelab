//! Step bindings for features/process_exec.feature, over the real
//! process_exec IoClient, registrar, and ProcessGitRunner in forge-work-worker.

use forge_core::bootstrap::Registry;
use forge_core::io_client::IoClient;
use forge_core::provider::{Factory, InstanceProvider, Singleton};
use forge_scene::bdd::{ensure, verify, Steps};
use forge_work_worker::ports_runtime::{GitRunner, ProcessGitRunner};
use forge_work_worker::process_exec::{
    register_process_exec, ProcessOutput, ProcessRequest, SystemProcessExec,
};
use forge_work_worker::WorkerError;
use std::path::Path;

#[derive(Default)]
struct World {
    output: Option<ProcessOutput>,
    error: Option<WorkerError>,
    duplicate_rejected: bool,
    git_error: Option<WorkerError>,
}

struct FailingExec;

impl IoClient for FailingExec {
    type Request = ProcessRequest;
    type Response = ProcessOutput;
    type Error = WorkerError;

    fn execute(&self, _request: Self::Request) -> Result<Self::Response, Self::Error> {
        Ok(ProcessOutput {
            exit_code: 128,
            stdout: Vec::new(),
            stderr: b"fatal: not a git repository\n".to_vec(),
        })
    }
}

fn steps() -> Steps<World> {
    Steps::new()
        .given("the process executor can run bounded commands", |_, _| Ok(()))
        .given(
            "the process executor can be registered at bootstrap",
            |_, _| Ok(()),
        )
        .given("a git runner needs a process executor", |_, _| Ok(()))
        .when("an empty argument list is submitted", |w, _| {
            match SystemProcessExec.execute(ProcessRequest::inherited(Vec::new(), 5)) {
                Ok(out) => w.output = Some(out),
                Err(err) => w.error = Some(err),
            }
            Ok(())
        })
        .when("a command reads its provided standard input", |w, _| {
            let req = ProcessRequest::inherited(vec!["/bin/cat".into()], 5)
                .with_stdin(b"bdd-stdin-ok".to_vec());
            match SystemProcessExec.execute(req) {
                Ok(out) => w.output = Some(out),
                Err(err) => w.error = Some(err),
            }
            Ok(())
        })
        .when(
            "a command runs with a cleared environment and one explicit marker",
            |w, _| {
                let req = ProcessRequest::cleared(
                    vec!["/usr/bin/env".into()],
                    5,
                    &[],
                    &[("FORGE_BDD_MARKER", "active")],
                );
                match SystemProcessExec.execute(req) {
                    Ok(out) => w.output = Some(out),
                    Err(err) => w.error = Some(err),
                }
                Ok(())
            },
        )
        .when("a command sleeps past a one-second deadline", |w, _| {
            let req = ProcessRequest::inherited(vec!["/bin/sleep".into(), "5".into()], 1);
            match SystemProcessExec.execute(req) {
                Ok(out) => w.output = Some(out),
                Err(err) => w.error = Some(err),
            }
            Ok(())
        })
        .when("the group is registered twice before freeze", |w, _| {
            let reg = Registry::new()
                .register(register_process_exec().map_err(|e| format!("{e:?}"))?)
                .map_err(|e| format!("{:?}", e.reason()))?;
            match reg.register(register_process_exec().map_err(|e| format!("{e:?}"))?) {
                Ok(_) => w.duplicate_rejected = false,
                Err(rejected) => {
                    w.duplicate_rejected = true;
                    let frozen = rejected
                        .into_original()
                        .freeze()
                        .map_err(|e| format!("{e:?}"))?;
                    let ((), factory) = frozen.into_factories();
                    let provider = Singleton::new(factory.create().map_err(|e| format!("{e:?}"))?);
                    let client = provider.acquire().map_err(|e| format!("{e:?}"))?;
                    let out = client
                        .execute(ProcessRequest::inherited(
                            vec!["/bin/echo".into(), "frozen-ok".into()],
                            5,
                        ))
                        .map_err(|e| format!("{e:?}"))?;
                    w.output = Some(out);
                }
            }
            Ok(())
        })
        .when("the underlying process exits with a nonzero status", |w, _| {
            let mut runner = ProcessGitRunner::new(FailingExec, 40);
            if let Err(err) = runner.git(Path::new("/tmp/missing"), &["status"]) {
                w.git_error = Some(err);
            }
            Ok(())
        })
        .then("execution fails with a protocol error", |w, _| {
            ensure(
                matches!(w.error, Some(WorkerError::Protocol(_))),
                format!("expected protocol error, got {:?}", w.error),
            )
        })
        .then(
            "the exit status is zero and stdout matches the input",
            |w, _| {
                let out = w.output.as_ref().ok_or("no output captured")?;
                ensure(out.exit_code == 0, format!("exit_code was {}", out.exit_code))?;
                ensure(
                    out.stdout == b"bdd-stdin-ok",
                    format!("stdout was {:?}", String::from_utf8_lossy(&out.stdout)),
                )
            },
        )
        .then(
            "stdout contains the marker and omits ambient home variables",
            |w, _| {
                let out = w.output.as_ref().ok_or("no output captured")?;
                let text = String::from_utf8_lossy(&out.stdout);
                ensure(
                    text.contains("FORGE_BDD_MARKER=active"),
                    format!("marker missing in {text}"),
                )?;
                ensure(!text.contains("HOME="), "ambient HOME leaked into child")
            },
        )
        .then("execution fails with a time-bound error", |w, _| {
            match &w.error {
                Some(WorkerError::Protocol(msg)) => ensure(
                    msg.contains("time bound"),
                    format!("unexpected message: {msg}"),
                ),
                other => Err(format!("expected time-bound protocol error, got {other:?}")),
            }
        })
        .then(
            "the duplicate is rejected and the frozen factory produces a working client",
            |w, _| {
                ensure(w.duplicate_rejected, "duplicate registration was not rejected")?;
                let out = w.output.as_ref().ok_or("frozen client did not run")?;
                ensure(out.exit_code == 0, format!("exit_code was {}", out.exit_code))?;
                ensure(
                    String::from_utf8_lossy(&out.stdout).trim() == "frozen-ok",
                    "unexpected output from frozen client",
                )
            },
        )
        .then("the runner reports the repository as unavailable", |w, _| {
            let err = w.git_error.as_ref().ok_or("expected git error")?;
            ensure(
                format!("{err:?}").contains("approved repository or revision is unavailable"),
                format!("unexpected git error: {err:?}"),
            )
        })
}

#[test]
fn process_exec_feature() {
    let report = verify(include_str!("../features/process_exec.feature"), &steps())
        .unwrap_or_else(|errors| panic!("process_exec.feature failed:\n{errors}"));
    assert_eq!(report.scenarios, 6);
}
