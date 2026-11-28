//! Step bindings for features/worker_supervisor.feature, driving the real
//! SystemdSupervisor in forge-work-worker over ProcessCommandPort and an
//! in-memory ScriptedExec IoClient.

use forge_core::io_client::IoClient;
use forge_scene::bdd::{ensure, verify, Steps};
use forge_work_worker::config::{parse_config, WorkerConfig};
use forge_work_worker::ports_runtime::{ProcessCommandPort, SupervisorPort};
use forge_work_worker::process_exec::{ProcessOutput, ProcessRequest};
use forge_work_worker::supervisor::SystemdSupervisor;
use forge_work_worker::types::StopProof;
use forge_work_worker::WorkerError;
use serde_json::json;
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const VALID_UNIT: &str = "forge-coding-12345678123412341234123412345678";

struct TempCgroup(PathBuf);

impl TempCgroup {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("forge-scene-cg-{}", Uuid::new_v4().simple()));
        fs::create_dir(&path).expect("create cgroup root");
        Self(path)
    }

    fn write_populated(&self, unit: &str, populated: &str) {
        let dir = self.0.join("system.slice").join(format!("{unit}.service"));
        fs::create_dir_all(&dir).expect("create unit cgroup dir");
        fs::write(dir.join("cgroup.events"), format!("populated {populated}\nfrozen 0\n"))
            .expect("write cgroup.events");
    }
}

impl Drop for TempCgroup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone)]
struct ScriptedExec {
    responses: Arc<Mutex<VecDeque<Result<ProcessOutput, WorkerError>>>>,
    captured: Arc<Mutex<Vec<ProcessRequest>>>,
}

impl ScriptedExec {
    fn new(responses: Vec<Result<ProcessOutput, WorkerError>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into())),
            captured: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn requests(&self) -> Vec<ProcessRequest> {
        self.captured.lock().unwrap().clone()
    }
}

impl IoClient for ScriptedExec {
    type Request = ProcessRequest;
    type Response = ProcessOutput;
    type Error = WorkerError;

    fn execute(&self, request: Self::Request) -> Result<Self::Response, Self::Error> {
        self.captured.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(WorkerError::Protocol("no scripted response left".into())))
    }
}

fn worker_config() -> WorkerConfig {
    let sample = json!({
        "worker_id": "worker-bdd",
        "owner": "forge",
        "database": {"host": "/run/postgresql", "port": 5432, "name": "forge_agent", "user": "forge_coding_worker"},
        "account": {"user": "forgeqa", "uid": 1001, "gid": 1001, "home": "/home/forgeqa"},
        "workspace_root": "/srv/forge/workspaces",
        "state_root": "/tmp/forge-bdd-state",
        "projects": {"homelab": "/srv/repos/homelab"},
        "profiles": {
            "codex-author": {"backend": "codex", "model": "gpt-5", "modes": ["plan", "review", "implement"]}
        },
        "binaries": {"codex": "/usr/local/bin/codex"}
    });
    parse_config(&sample).expect("valid worker config")
}

fn show_output(active_state: &str, main_pid: &str, control_group: &str) -> ProcessOutput {
    let body = format!(
        "LoadState=loaded\nActiveState={active_state}\nSubState=dead\nMainPID={main_pid}\nControlGroup={control_group}\n"
    );
    ProcessOutput {
        exit_code: 0,
        stdout: body.into_bytes(),
        stderr: Vec::new(),
    }
}

#[derive(Default)]
struct World {
    _cgroup: Option<TempCgroup>,
    proof: Option<StopProof>,
    error: Option<WorkerError>,
    requests: Vec<ProcessRequest>,
}

fn run_proof(w: &mut World, exec: ScriptedExec, cg: TempCgroup, unit: &str) {
    let mut sup = SystemdSupervisor::new(
        worker_config(),
        Box::new(ProcessCommandPort::new(exec.clone())),
        None,
    )
    .with_cgroup_root(cg.0.clone());
    match sup.proof(unit) {
        Ok(p) => w.proof = Some(p),
        Err(e) => w.error = Some(e),
    }
    w.requests = exec.requests();
    w._cgroup = Some(cg);
}

fn steps() -> Steps<World> {
    Steps::new()
        .given("the supervisor can validate coding scope names", |_: &mut World, _| Ok(()))
        .given(
            "the supervisor can inspect systemd state over an injected executor",
            |_: &mut World, _| Ok(()),
        )
        .given(
            "the supervisor can stop a coding scope over an injected executor",
            |_: &mut World, _| Ok(()),
        )
        .when("a stop proof is requested for an invalid scope name", |w, _| {
            let exec = ScriptedExec::new(Vec::new());
            let cg = TempCgroup::new();
            run_proof(w, exec, cg, "forge-coding-INVALID");
            Ok(())
        })
        .when(
            "systemctl reports inactive with MainPID zero and an unpopulated cgroup",
            |w, _| {
                let cg = TempCgroup::new();
                cg.write_populated(VALID_UNIT, "0");
                let exec = ScriptedExec::new(vec![Ok(show_output(
                    "inactive",
                    "0",
                    &format!("/system.slice/{VALID_UNIT}.service"),
                ))]);
                run_proof(w, exec, cg, VALID_UNIT);
                Ok(())
            },
        )
        .when(
            "systemctl reports inactive with MainPID zero while cgroup events remain populated",
            |w, _| {
                let cg = TempCgroup::new();
                cg.write_populated(VALID_UNIT, "1");
                let exec = ScriptedExec::new(vec![Ok(show_output(
                    "inactive",
                    "0",
                    &format!("/system.slice/{VALID_UNIT}.service"),
                ))]);
                run_proof(w, exec, cg, VALID_UNIT);
                Ok(())
            },
        )
        .when("systemctl reports active with a nonzero MainPID", |w, _| {
            let cg = TempCgroup::new();
            let exec = ScriptedExec::new(vec![Ok(show_output(
                "active",
                "4242",
                &format!("/system.slice/{VALID_UNIT}.service"),
            ))]);
            run_proof(w, exec, cg, VALID_UNIT);
            Ok(())
        })
        .when("systemctl reports a different ControlGroup path", |w, _| {
            let cg = TempCgroup::new();
            let exec = ScriptedExec::new(vec![Ok(show_output(
                "inactive",
                "0",
                "/system.slice/other.service",
            ))]);
            run_proof(w, exec, cg, VALID_UNIT);
            Ok(())
        })
        .when("stop is requested for a valid coding scope", |w, _| {
            let cg = TempCgroup::new();
            let exec = ScriptedExec::new(vec![
                Ok(ProcessOutput {
                    exit_code: 0,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                }),
                Ok(show_output(
                    "inactive",
                    "0",
                    &format!("/system.slice/{VALID_UNIT}.service"),
                )),
            ]);
            let mut sup = SystemdSupervisor::new(
                worker_config(),
                Box::new(ProcessCommandPort::new(exec.clone())),
                None,
            )
            .with_cgroup_root(cg.0.clone());
            match sup.stop(VALID_UNIT) {
                Ok(p) => w.proof = Some(p),
                Err(e) => w.error = Some(e),
            }
            w.requests = exec.requests();
            w._cgroup = Some(cg);
            Ok(())
        })
        .then("proof fails with a protocol error and zero commands run", |w, _| {
            ensure(
                matches!(w.error, Some(WorkerError::Protocol(_))),
                format!("expected protocol error, got {:?}", w.error),
            )?;
            ensure(
                w.requests.is_empty(),
                format!("expected 0 commands, got {}", w.requests.len()),
            )
        })
        .then("the stop proof confirms the scope is stopped", |w, _| {
            let proof = w.proof.as_ref().ok_or("missing stop proof")?;
            ensure(proof.scope_stopped, format!("scope_stopped was false: {proof:?}"))?;
            ensure(proof.source == "systemd", format!("source was {}", proof.source))
        })
        .then("the stop proof reports the scope is still running", |w, _| {
            let proof = w.proof.as_ref().ok_or("missing stop proof")?;
            ensure(!proof.scope_stopped, format!("expected false, got {proof:?}"))
        })
        .then(
            "systemctl stop runs before systemctl show and confirms the scope is stopped",
            |w, _| {
                ensure(
                    w.requests.len() == 2,
                    format!("expected 2 commands, got {}", w.requests.len()),
                )?;
                ensure(
                    w.requests[0].argv.iter().any(|a| a == "stop")
                        && w.requests[0].timeout_seconds == 15,
                    format!("first command was {:?}", w.requests[0]),
                )?;
                ensure(
                    w.requests[1].argv.iter().any(|a| a == "show")
                        && w.requests[1].timeout_seconds == 10,
                    format!("second command was {:?}", w.requests[1]),
                )?;
                let proof = w.proof.as_ref().ok_or("missing stop proof")?;
                ensure(proof.scope_stopped, "stop proof did not confirm stopped")
            },
        )
}

#[test]
fn worker_supervisor_feature() {
    let report = verify(
        include_str!("../features/worker_supervisor.feature"),
        &steps(),
    )
    .unwrap_or_else(|errors| panic!("worker_supervisor.feature failed:\n{errors}"));
    assert_eq!(report.scenarios, 6);
}
