//! S2 bootstrap registrar for the publication crate's bounded process-backed
//! capabilities (`jenkins_observer` and `verifier_parity`), mirroring
//! `forge-work-worker/src/registrar.rs`.
//!
//! Registration validates and stages factories only—no subprocesses, network
//! calls, or filesystem I/O occur until the host freezes its registry and
//! calls `Factory::create()`.

use crate::jenkins_observer::{IoClientObserverExecute, ProcessObserverExecute};
use crate::verifier_parity::{IoClientAstPort, PythonAstPort};
use forge_core::bootstrap::{PreparedGroup, RegistrationError};
use forge_core::io_client::IoClient;
use forge_core::provider::Factory;
use forge_work_worker::process_exec::{ProcessOutput, ProcessRequest};
use forge_work_worker::WorkerError;

pub const PUBLICATION_GROUP: &str = "forge.work_publication.runtime";
pub const OBSERVER_BINDING: &str = "forge.work_publication.observer_exec";
pub const AST_PORT_BINDING: &str = "forge.work_publication.ast_port";

/// Factory for the production Jenkins observer command adapter.
#[derive(Debug, Default, Clone, Copy)]
pub struct ObserverExecFactory;

impl Factory<ProcessObserverExecute> for ObserverExecFactory {
    type Error = RegistrationError;

    fn create(&self) -> Result<ProcessObserverExecute, Self::Error> {
        Ok(ProcessObserverExecute)
    }
}

/// Factory for the production Python AST parser port.
#[derive(Debug, Default, Clone, Copy)]
pub struct AstPortFactory;

impl Factory<PythonAstPort> for AstPortFactory {
    type Error = RegistrationError;

    fn create(&self) -> Result<PythonAstPort, Self::Error> {
        Ok(PythonAstPort)
    }
}

/// Generic factory group allowing tests or hosts to inject any `process_exec` `IoClient`.
#[derive(Debug, Clone)]
pub struct GenericPublicationFactories<C> {
    client: C,
}

impl<C> GenericPublicationFactories<C>
where
    C: IoClient<Request = ProcessRequest, Response = ProcessOutput, Error = WorkerError>
        + Clone
        + Send
        + Sync,
{
    pub fn new(client: C) -> Self {
        Self { client }
    }

    pub fn observer_exec(&self) -> IoClientObserverExecute<C> {
        IoClientObserverExecute::new(self.client.clone())
    }

    pub fn ast_port(&self) -> IoClientAstPort<C> {
        IoClientAstPort::new(self.client.clone())
    }
}

/// Cohesive factory group returned from `Frozen::into_factories()`.
#[derive(Debug, Default, Clone, Copy)]
pub struct PublicationFactories {
    pub observer_exec: ObserverExecFactory,
    pub ast_port: AstPortFactory,
}

/// Stage the publication runtime's bounded process-backed factories.
/// Declares a dependency on `forge.work_worker.process_exec.client` so the
/// host's bootstrap freeze enforces ordering.
pub fn register_publication() -> Result<PreparedGroup<PublicationFactories>, RegistrationError> {
    PreparedGroup::new(
        PUBLICATION_GROUP,
        &[OBSERVER_BINDING, AST_PORT_BINDING],
        &[forge_work_worker::process_exec::PROCESS_EXEC_BINDING],
        PublicationFactories::default(),
    )
}

/// Stage a custom `IoClient`-backed publication group (e.g. for in-memory test doubles).
pub fn register_publication_with<C>(
    client: C,
) -> Result<PreparedGroup<GenericPublicationFactories<C>>, RegistrationError>
where
    C: IoClient<Request = ProcessRequest, Response = ProcessOutput, Error = WorkerError>
        + Clone
        + Send
        + Sync,
{
    PreparedGroup::new(
        PUBLICATION_GROUP,
        &[OBSERVER_BINDING, AST_PORT_BINDING],
        &[],
        GenericPublicationFactories::new(client),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jenkins_observer::ObserverExecute;
    use crate::verifier_parity::VerifierAstPort;
    use forge_core::bootstrap::Registry;
    use forge_work_worker::process_exec::register_process_exec;

    #[derive(Clone)]
    struct StubExec {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        exit_code: i32,
    }

    impl IoClient for StubExec {
        type Request = ProcessRequest;
        type Response = ProcessOutput;
        type Error = WorkerError;

        fn execute(&self, _request: Self::Request) -> Result<Self::Response, Self::Error> {
            Ok(ProcessOutput {
                exit_code: self.exit_code,
                stdout: self.stdout.clone(),
                stderr: self.stderr.clone(),
            })
        }
    }

    #[test]
    fn freeze_rejects_missing_process_exec_binding() {
        let reg = Registry::new()
            .register(register_publication().unwrap())
            .expect("staging succeeds");
        assert!(reg.freeze().is_err(), "freeze must fail without process_exec");
    }

    #[test]
    fn register_with_process_exec_freezes_and_rejects_duplicates() {
        let reg = Registry::new()
            .register(register_process_exec().unwrap())
            .unwrap()
            .register(register_publication().unwrap())
            .expect("publication group stages after process_exec");

        let dup = reg
            .register(register_publication().unwrap())
            .expect_err("duplicate publication group rejected");
        assert_eq!(dup.original().group_count(), 2);

        let frozen = dup.into_original().freeze().expect("freeze succeeds");
        let (((), _exec_factory), pub_factories) = frozen.into_factories();
        let _observer = pub_factories.observer_exec.create().unwrap();
        let _ast = pub_factories.ast_port.create().unwrap();
    }

    #[test]
    fn generic_registrar_wires_observer_and_ast_over_fake_ioclient() {
        let stub = StubExec {
            stdout: br#"{"_type":"Module","body":[]}"#.to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
        };
        let frozen = Registry::new()
            .register(register_publication_with(stub).unwrap())
            .unwrap()
            .freeze()
            .unwrap();
        let ((), factories) = frozen.into_factories();

        let observer_bytes = factories
            .observer_exec()
            .execute(&["/usr/bin/jenkins-obs".into()], &[], 5.0)
            .unwrap();
        assert_eq!(observer_bytes, br#"{"_type":"Module","body":[]}"#);

        let ast = factories.ast_port().parse(b"pass\n").unwrap();
        assert_eq!(ast["_type"], "Module");

        let failing_stub = StubExec {
            stdout: Vec::new(),
            stderr: b"SyntaxError: invalid syntax\n".to_vec(),
            exit_code: 1,
        };
        let failing = GenericPublicationFactories::new(failing_stub);
        let err = failing.ast_port().parse(b"def (").unwrap_err();
        assert!(err.contains("SyntaxError"));
    }
}
