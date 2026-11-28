//! Step bindings for features/forge_work_publication.feature, driving the real
//! S2 registrar, IoClientObserverExecute, and IoClientAstPort in
//! forge-work-publication over an in-memory StubExec IoClient.

use forge_core::bootstrap::Registry;
use forge_core::io_client::IoClient;
use forge_core::provider::Factory;
use forge_scene::bdd::{ensure, verify, Steps};
use forge_work_publication::jenkins_observer::ObserverExecute;
use forge_work_publication::registrar::{register_publication, register_publication_with};
use forge_work_worker::process_exec::{register_process_exec, ProcessOutput, ProcessRequest};
use forge_work_worker::WorkerError;
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Clone)]
struct StubExec {
    outcome: Result<ProcessOutput, WorkerError>,
    calls: Arc<AtomicUsize>,
}

impl StubExec {
    fn ok(exit_code: i32, stdout: &[u8], stderr: &[u8]) -> Self {
        Self {
            outcome: Ok(ProcessOutput {
                exit_code,
                stdout: stdout.to_vec(),
                stderr: stderr.to_vec(),
            }),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn err(error: WorkerError) -> Self {
        Self {
            outcome: Err(error),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl IoClient for StubExec {
    type Request = ProcessRequest;
    type Response = ProcessOutput;
    type Error = WorkerError;

    fn execute(&self, _request: Self::Request) -> Result<Self::Response, Self::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.outcome.clone()
    }
}

#[derive(Default)]
struct World {
    freeze_failed: bool,
    duplicate_refused: bool,
    factories_built: bool,
    observer_output: Option<Vec<u8>>,
    observer_error: Option<String>,
    executor_calls: usize,
    ast_value: Option<Value>,
    ast_error: Option<String>,
}

fn steps() -> Steps<World> {
    Steps::new()
        .given(
            "the publication runtime needs the worker process executor",
            |_: &mut World, _| Ok(()),
        )
        .given(
            "the publication runtime can register alongside the worker process executor",
            |_: &mut World, _| Ok(()),
        )
        .given(
            "the publication observer can run over an injected executor",
            |_, _| Ok(()),
        )
        .given(
            "the historical verifier factory can parse policy source",
            |_, _| Ok(()),
        )
        .when("publication is staged alone and frozen", |w, _| {
            let reg = Registry::new()
                .register(register_publication().map_err(|e| e.to_string())?)
                .map_err(|e| format!("{:?}", e.reason()))?;
            w.freeze_failed = reg.freeze().is_err();
            Ok(())
        })
        .when(
            "both groups are staged and publication is submitted a second time",
            |w, _| {
                let reg = Registry::new()
                    .register(register_process_exec().map_err(|e| format!("{e:?}"))?)
                    .map_err(|e| format!("{:?}", e.reason()))?
                    .register(register_publication().map_err(|e| e.to_string())?)
                    .map_err(|e| format!("{:?}", e.reason()))?;
                match reg.register(register_publication().map_err(|e| e.to_string())?) {
                    Ok(_) => w.duplicate_refused = false,
                    Err(rejected) => {
                        w.duplicate_refused = true;
                        let frozen = rejected
                            .into_parts()
                            .0
                            .freeze()
                            .map_err(|e| format!("{e:?}"))?;
                        let (((), _exec_factory), pub_factories) = frozen.into_factories();
                        let _obs = pub_factories
                            .observer_exec
                            .create()
                            .map_err(|e| e.to_string())?;
                        let _ast = pub_factories.ast_port.create().map_err(|e| e.to_string())?;
                        w.factories_built = true;
                    }
                }
                Ok(())
            },
        )
        .when(
            "the observer runs a command that exits with zero status",
            |w, _| {
                let stub = StubExec::ok(0, b"observer-ok", b"");
                let frozen = Registry::new()
                    .register(register_publication_with(stub.clone()).map_err(|e| e.to_string())?)
                    .map_err(|e| format!("{:?}", e.reason()))?
                    .freeze()
                    .map_err(|e| format!("{e:?}"))?;
                let ((), factories) = frozen.into_factories();
                match factories
                    .observer_exec()
                    .execute(&["/usr/bin/jenkins-obs".into()], &[], 5.0)
                {
                    Ok(bytes) => w.observer_output = Some(bytes),
                    Err(err) => w.observer_error = Some(format!("{err:?}")),
                }
                w.executor_calls = stub.call_count();
                Ok(())
            },
        )
        .when(
            "the executor reports a time-bound protocol error",
            |w, _| {
                let stub = StubExec::err(WorkerError::Protocol(
                    "command exceeded its time bound".into(),
                ));
                let frozen = Registry::new()
                    .register(register_publication_with(stub.clone()).map_err(|e| e.to_string())?)
                    .map_err(|e| format!("{:?}", e.reason()))?
                    .freeze()
                    .map_err(|e| format!("{e:?}"))?;
                let ((), factories) = frozen.into_factories();
                if let Err(err) =
                    factories
                        .observer_exec()
                        .execute(&["/usr/bin/jenkins-obs".into()], &[], 5.0)
                {
                    w.observer_error = Some(format!("{err:?}"));
                }
                w.executor_calls = stub.call_count();
                Ok(())
            },
        )
        .when("the observer is given an empty argument list", |w, _| {
            let stub = StubExec::ok(0, b"never-called", b"");
            let frozen = Registry::new()
                .register(register_publication_with(stub.clone()).map_err(|e| e.to_string())?)
                .map_err(|e| format!("{:?}", e.reason()))?
                .freeze()
                .map_err(|e| format!("{e:?}"))?;
            let ((), factories) = frozen.into_factories();
            if let Err(err) = factories.observer_exec().execute(&[], &[], 5.0) {
                w.observer_error = Some(format!("{err:?}"));
            }
            w.executor_calls = stub.call_count();
            Ok(())
        })
        .when(
            "the historical policy parser reads valid source and then encounters invalid syntax",
            |w, _| {
                let frozen = Registry::new()
                    .register(register_process_exec().unwrap())
                    .unwrap()
                    .register(register_publication().unwrap())
                    .unwrap()
                    .freeze()
                    .unwrap();
                let (_, factories) = frozen.into_factories();
                let parser = factories.ast_port.create().unwrap();
                use forge_work_publication::verifier_parity::VerifierAstPort;
                w.ast_value = Some(parser.parse(b"pass\n")?);
                w.ast_error = parser.parse(b"def (").err();
                Ok(())
            },
        )
        .then(
            "freeze fails because the required binding is missing",
            |w, _| ensure(w.freeze_failed, "freeze succeeded without process_exec"),
        )
        .then(
            "the duplicate is refused and the frozen factories construct cleanly",
            |w, _| {
                ensure(
                    w.duplicate_refused,
                    "duplicate publication group was not refused",
                )?;
                ensure(w.factories_built, "frozen factories failed to construct")
            },
        )
        .then(
            "the observer returns the captured stdout payload",
            |w, _| {
                let bytes = w
                    .observer_output
                    .as_ref()
                    .ok_or("missing observer output")?;
                ensure(
                    bytes == b"observer-ok",
                    format!("unexpected output: {:?}", String::from_utf8_lossy(bytes)),
                )?;
                ensure(
                    w.executor_calls == 1,
                    format!("calls was {}", w.executor_calls),
                )
            },
        )
        .then("the observer reports a timed-out command", |w, _| {
            let err = w.observer_error.as_ref().ok_or("missing observer error")?;
            ensure(
                err.contains("Observer command timed out"),
                format!("unexpected error: {err}"),
            )
        })
        .then(
            "the observer refuses the command with zero executor calls",
            |w, _| {
                let err = w.observer_error.as_ref().ok_or("missing observer error")?;
                ensure(
                    err.contains("Empty observer command"),
                    format!("unexpected error: {err}"),
                )?;
                ensure(
                    w.executor_calls == 0,
                    format!("calls was {}", w.executor_calls),
                )
            },
        )
        .then(
            "the first call yields the parsed module and the second returns the parser error",
            |w, _| {
                let ast = w.ast_value.as_ref().ok_or("missing parsed AST")?;
                ensure(ast["_type"] == "Module", format!("unexpected AST: {ast}"))?;
                let err = w.ast_error.as_ref().ok_or("missing AST syntax error")?;
                ensure(
                    err.contains("Invalid historical verifier syntax"),
                    format!("unexpected AST error: {err}"),
                )
            },
        )
}

#[test]
fn forge_work_publication_feature() {
    let report = verify(
        include_str!("../features/forge_work_publication.feature"),
        &steps(),
    )
    .unwrap_or_else(|errors| panic!("forge_work_publication.feature failed:\n{errors}"));
    assert_eq!(report.scenarios, 6);
}
