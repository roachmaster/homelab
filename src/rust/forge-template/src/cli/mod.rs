//! Opt-in single-shot terminal adapter. Rendering/public assembly do not require
//! this feature. No JSON fallback, shell execution, output-file writing or plugins.
mod application;
mod catalog;
mod input;
mod io;
mod task;

use self::io::{ConsoleRequest, NativeConsoleClient, NativeSourceClient, ReadSource, SourceError};
use forge_io_client::IoClient;
use std::{error::Error, ffi::OsString, fmt};

#[derive(Debug)]
enum CliError {
    Usage(String),
    Source(SourceError),
    Template(crate::TemplateError),
    Assembly(crate::assembly::AssemblyError),
    Registration(forge_bootstrap::RegistrationError),
    Validation(forge_bootstrap::ValidationError),
    Header(forge_ingress_contract::HeaderError),
    Descriptor(forge_command::DescriptorError),
    Duplicate(forge_bridge::DuplicateRoute),
    Invariant(&'static str),
}
impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(e) => f.write_str(e),
            Self::Source(e) => e.fmt(f),
            Self::Template(e) => e.fmt(f),
            Self::Assembly(e) => e.fmt(f),
            Self::Registration(e) => e.fmt(f),
            Self::Validation(e) => e.fmt(f),
            Self::Header(e) => e.fmt(f),
            Self::Descriptor(e) => e.fmt(f),
            Self::Duplicate(e) => e.fmt(f),
            Self::Invariant(e) => f.write_str(e),
        }
    }
}
impl Error for CliError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Source(e) => Some(e),
            Self::Template(e) => Some(e),
            Self::Assembly(e) => Some(e),
            Self::Registration(e) => Some(e),
            Self::Validation(e) => Some(e),
            Self::Header(e) => Some(e),
            Self::Descriptor(e) => Some(e),
            Self::Duplicate(e) => Some(e),
            _ => None,
        }
    }
}

/// Run the single-shot CLI with arguments excluding `argv[0]`. Exit codes: 0 for
/// success (including a downstream closed stdout pipe), 2 for usage, 1 for work or
/// other console failures. Errors never emit a partial rendered document; an OS
/// write failure can still follow partial byte delivery. No task/client executes
/// for help beyond the console operation that presents it.
pub fn run(args: impl IntoIterator<Item = OsString>) -> u8 {
    run_with(args, NativeSourceClient, &NativeConsoleClient)
}
fn run_with<C, O>(args: impl IntoIterator<Item = OsString>, source: C, console: &O) -> u8
where
    C: IoClient<Request = ReadSource, Response = String, Error = SourceError> + 'static,
    O: IoClient<Request = ConsoleRequest, Response = (), Error = std::io::Error>,
{
    let invocation = match input::parse(args) {
        Ok(value) => value,
        Err(error) => {
            return diagnostic(
                console,
                format!("error: {error}\nUse --help for usage.\n"),
                2,
            )
        }
    };
    let result = application::Application::bootstrap(source).and_then(|app| app.invoke(invocation));
    match result {
        Ok(text) => match console.execute(ConsoleRequest::Output(text)) {
            Ok(()) => 0,
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => 0,
            Err(error) => diagnostic(console, format!("output error: {error}\n"), 1),
        },
        Err(error) => diagnostic(console, format!("error: {error}\n"), 1),
    }
}

fn diagnostic<O>(console: &O, text: String, code: u8) -> u8
where
    O: IoClient<Request = ConsoleRequest, Response = (), Error = std::io::Error>,
{
    if console.execute(ConsoleRequest::Diagnostic(text)).is_ok() {
        code
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_ingress_contract::{Envelope, Header, RouteKey};
    use std::{cell::RefCell, rc::Rc};
    #[derive(Clone)]
    struct SourceProbe(Rc<RefCell<Vec<ReadSource>>>);
    impl IoClient for SourceProbe {
        type Request = ReadSource;
        type Response = String;
        type Error = SourceError;
        fn execute(&self, request: ReadSource) -> Result<String, SourceError> {
            self.0.borrow_mut().push(request);
            Ok("{{ name.to_uppercase() }}".into())
        }
    }
    #[derive(Default)]
    struct ConsoleProbe {
        output: RefCell<Vec<String>>,
        errors: RefCell<Vec<String>>,
        fail: Option<std::io::ErrorKind>,
    }
    impl IoClient for ConsoleProbe {
        type Request = ConsoleRequest;
        type Response = ();
        type Error = std::io::Error;
        fn execute(&self, request: ConsoleRequest) -> std::io::Result<()> {
            match request {
                ConsoleRequest::Output(text) => {
                    if let Some(kind) = self.fail {
                        return Err(std::io::Error::from(kind));
                    }
                    self.output.borrow_mut().push(text);
                }
                ConsoleRequest::Diagnostic(text) => self.errors.borrow_mut().push(text),
            }
            Ok(())
        }
    }
    fn args(text: &[&str]) -> Vec<OsString> {
        text.iter().map(OsString::from).collect()
    }
    #[test]
    fn admitted_file_task_prepares_request_and_preserves_exact_output() {
        let reads = Rc::new(RefCell::new(Vec::new()));
        let console = ConsoleProbe::default();
        let code = run_with(
            args(&[
                "render",
                "--template-file",
                "file.forge",
                "--text",
                "name=Forge",
            ]),
            SourceProbe(reads.clone()),
            &console,
        );
        assert_eq!(code, 0);
        assert_eq!(*console.output.borrow(), ["FORGE"]);
        let calls = reads.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].path, std::path::PathBuf::from("file.forge"));
        assert_eq!(
            calls[0].max_bytes,
            crate::Limits::default().max_source_bytes
        );
    }
    #[test]
    fn rejected_headers_and_mismatched_payloads_never_read_files() {
        let reads = Rc::new(RefCell::new(Vec::new()));
        let app = application::Application::bootstrap(SourceProbe(reads.clone())).unwrap();
        for (route, version, allowed, mismatch) in [
            (catalog::RENDER_ROUTE, 1, false, false),
            ("missing", 1, true, false),
            (catalog::RENDER_ROUTE, 2, true, false),
            (catalog::RENDER_ROUTE, 1, true, true),
        ] {
            let command = if mismatch {
                input::Command::Methods
            } else {
                input::parse(args(&["render", "--template-file", "never-read"]))
                    .unwrap()
                    .command
            };
            let header = Header::new(RouteKey::new(route, version).unwrap(), "test").unwrap();
            assert!(app
                .bridge
                .dispatch(Envelope::new(header, command), &allowed)
                .is_err());
        }
        assert!(reads.borrow().is_empty());
    }
    #[test]
    fn help_methods_and_bad_input_never_acquire_source_or_emit_partial_render() {
        for arguments in [
            vec!["render", "--template-file", "never-read", "--help"],
            vec!["methods"],
            vec!["--help"],
        ] {
            let reads = Rc::new(RefCell::new(Vec::new()));
            let console = ConsoleProbe::default();
            assert_eq!(
                run_with(args(&arguments), SourceProbe(reads.clone()), &console),
                0
            );
            assert!(reads.borrow().is_empty());
            assert_eq!(console.output.borrow().len(), 1);
        }
        let reads = Rc::new(RefCell::new(Vec::new()));
        let console = ConsoleProbe::default();
        assert_eq!(
            run_with(
                args(&["render", "--template-file", "never-read", "--int", "n=bad"]),
                SourceProbe(reads.clone()),
                &console
            ),
            2
        );
        assert!(reads.borrow().is_empty());
        assert!(console.output.borrow().is_empty());
        let console = ConsoleProbe::default();
        assert_eq!(
            run_with(
                args(&["render", "--template", "literal then {{missing}}"]),
                SourceProbe(reads),
                &console
            ),
            1
        );
        assert!(console.output.borrow().is_empty());
    }
    #[test]
    fn output_broken_pipe_is_quiet_success_other_write_errors_fail() {
        for (kind, expected) in [
            (std::io::ErrorKind::BrokenPipe, 0),
            (std::io::ErrorKind::PermissionDenied, 1),
        ] {
            let console = ConsoleProbe {
                fail: Some(kind),
                ..ConsoleProbe::default()
            };
            assert_eq!(
                run_with(args(&["--help"]), SourceProbe(Rc::default()), &console),
                expected
            );
            assert_eq!(console.errors.borrow().is_empty(), expected == 0);
        }
    }
    #[test]
    fn source_error_is_not_retried_and_has_no_stdout() {
        struct FailingSource(Rc<RefCell<usize>>);
        impl IoClient for FailingSource {
            type Request = ReadSource;
            type Response = String;
            type Error = SourceError;
            fn execute(&self, _: ReadSource) -> Result<String, SourceError> {
                *self.0.borrow_mut() += 1;
                Err(SourceError::Io(std::io::Error::from(
                    std::io::ErrorKind::PermissionDenied,
                )))
            }
        }
        let calls = Rc::new(RefCell::new(0));
        let console = ConsoleProbe::default();
        assert_eq!(
            run_with(
                args(&["render", "--template-file", "denied"]),
                FailingSource(calls.clone()),
                &console
            ),
            1
        );
        assert_eq!(*calls.borrow(), 1);
        assert!(console.output.borrow().is_empty());
        assert!(console.errors.borrow()[0].contains("template file"));
    }
}
