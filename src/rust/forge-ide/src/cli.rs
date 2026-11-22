//! Thin command-line boundary and explicit single-shot application composition.
use crate::{
    assembly::{register_ast_graph, register_ide, AstGraphConfig, IdeConfig},
    ast_graph::{AstGraphReport, AstGraphRequest, AstGraphTask},
    filesystem::{FsError, FsRequest, FsResponse},
    model::ProjectReport,
    task::{InspectError, InspectRequest, InspectTask},
    yaml,
};
use forge_core::bootstrap::Registry;
use forge_core::bridge::{Bridge, RegistryBuilder};
use forge_core::command::{
    ArgumentDescriptor, CommandDescriptor, DataType, DescribeCommand, Presence,
};
use forge_core::ingress::{AdmissionDenied, Envelope, Header, Payload, RouteKey};
use forge_core::io_client::IoClient;
use forge_core::provider::{Factory, InstanceProvider, Singleton};
use forge_core::task::Task;
use std::{
    ffi::OsString,
    io::{self, Write},
    path::PathBuf,
};

const ROUTE: &str = "ide.inspect";
const AST_GRAPH_ROUTE: &str = "ide.ast_graph";
const HELP_ROUTE: &str = "ide.help";
const MAX_OUTPUT: usize = 16 * 1024 * 1024;
const MAX_AST_OUTPUT: usize = 64 * 1024 * 1024;
pub struct Catalog;
impl DescribeCommand for Catalog {
    fn descriptors(&self) -> &'static [CommandDescriptor] {
        &[
            CommandDescriptor {
                name: ROUTE,
                help: "Inspect a read-only project tree and Rust declaration syntax.",
                arguments: &[ArgumentDescriptor {
                    name: "root",
                    help: "Root directory; .git and target names are excluded at every depth.",
                    data_type: DataType::Path,
                    presence: Presence::Required,
                }],
            },
            CommandDescriptor {
                name: AST_GRAPH_ROUTE,
                help: "Extract expanded Rust AST graph (visibility, imports, modules, attrs, refs) as YAML or JSON.",
                arguments: &[
                    ArgumentDescriptor {
                        name: "root",
                        help: "Root directory; .git, target and node_modules are excluded at every depth.",
                        data_type: DataType::Path,
                        presence: Presence::Required,
                    },
                    ArgumentDescriptor {
                        name: "--format",
                        help: "Output format: yaml (default) or json.",
                        data_type: DataType::Text,
                        presence: Presence::Optional {
                            default: Some("yaml"),
                        },
                    },
                ],
            },
            CommandDescriptor {
                name: HELP_ROUTE,
                help: "Show command usage without opening a project.",
                arguments: &[],
            },
        ]
    }
}
const HELP_OPTION: ArgumentDescriptor = ArgumentDescriptor {
    name: "--help / -h",
    help: "Show help without inspecting files.",
    data_type: DataType::Bool,
    presence: Presence::Optional {
        default: Some("false"),
    },
};
fn help() -> String {
    let inspect = &Catalog.descriptors()[0];
    let ast = &Catalog.descriptors()[1];
    format!(
        "Forge IDE — local project inspector\n\nUsage: forge-ide inspect ROOT\n       forge-ide ast-graph ROOT\n       forge-ide --help\n\n{} [{}]\n  {}: {} (required) — {}\n{} [{}]\n  {}: {} (required) — {}\n  {} — {}\n\nYAML to stdout; no writes, macro expansion or type checking.\n",
        inspect.help,
        inspect.name,
        inspect.arguments[0].name,
        inspect.arguments[0].data_type,
        inspect.arguments[0].help,
        ast.help,
        ast.name,
        ast.arguments[0].name,
        ast.arguments[0].data_type,
        ast.arguments[0].help,
        HELP_OPTION.name,
        HELP_OPTION.help,
    )
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Inspect,
    AstGraph,
    Help,
}
enum Command {
    Inspect,
    AstGraph,
    Help,
}
impl Payload for Command {
    type Kind = Kind;
    fn kind(&self) -> Kind {
        match self {
            Self::Inspect => Kind::Inspect,
            Self::AstGraph => Kind::AstGraph,
            Self::Help => Kind::Help,
        }
    }
}
enum Output {
    Report(Box<ProjectReport>),
    AstGraph(Box<AstGraphReport>),
    Help(String),
}
fn admit(_: &Header, allowed: &bool) -> Result<(), AdmissionDenied> {
    if *allowed {
        Ok(())
    } else {
        Err(AdmissionDenied)
    }
}
type Policy = fn(&Header, &bool) -> Result<(), AdmissionDenied>;
type AppBridge = Bridge<'static, Command, bool, Output, InspectError, Policy>;
fn bridge<C: IoClient<Request = FsRequest, Response = FsResponse, Error = FsError> + 'static>(
    task: Option<InspectTask<C>>,
    ast_task: Option<AstGraphTask<C>>,
) -> Result<AppBridge, String> {
    forge_core::command::validate_descriptors(Catalog.descriptors()).map_err(|e| e.to_string())?;
    let mut routes = RegistryBuilder::new();
    if let Some(task) = task {
        let provider = Singleton::new(task);
        routes
            .register(
                RouteKey::new(Catalog.descriptors()[0].name, 1).map_err(|e| e.to_string())?,
                Kind::Inspect,
                move |command, _: &bool| {
                    if !matches!(command, Command::Inspect) {
                        return Err(InspectError::Protocol("inspect binding mismatch"));
                    }
                    let lease = provider.acquire().unwrap_or_else(|never| match never {});
                    lease
                        .run(InspectRequest::Project)
                        .map(|report| Output::Report(Box::new(report)))
                },
            )
            .map_err(|e| e.to_string())?;
    }
    if let Some(ast_task) = ast_task {
        let provider = Singleton::new(ast_task);
        routes
            .register(
                RouteKey::new(Catalog.descriptors()[1].name, 1).map_err(|e| e.to_string())?,
                Kind::AstGraph,
                move |command, _: &bool| {
                    if !matches!(command, Command::AstGraph) {
                        return Err(InspectError::Protocol("ast-graph binding mismatch"));
                    }
                    let lease = provider.acquire().unwrap_or_else(|never| match never {});
                    lease
                        .run(AstGraphRequest::Workspace)
                        .map(|report| Output::AstGraph(Box::new(report)))
                },
            )
            .map_err(|e| e.to_string())?;
    }
    routes
        .register(
            RouteKey::new(Catalog.descriptors()[2].name, 1).map_err(|e| e.to_string())?,
            Kind::Help,
            |command, _: &bool| {
                if !matches!(command, Command::Help) {
                    return Err(InspectError::Protocol("help binding mismatch"));
                }
                Ok(Output::Help(help()))
            },
        )
        .map_err(|e| e.to_string())?;
    Ok(Bridge::new(routes.freeze(), admit as Policy))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AstFormat {
    Yaml,
    Json,
}
enum Invocation {
    Help,
    Inspect(PathBuf),
    AstGraph { root: PathBuf, format: AstFormat },
}
fn parse_format(value: &OsString) -> Result<AstFormat, String> {
    match value.to_str() {
        Some("yaml") => Ok(AstFormat::Yaml),
        Some("json") => Ok(AstFormat::Json),
        _ => Err("expected --format yaml or --format json".into()),
    }
}
fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Invocation, String> {
    let mut args = args.into_iter();
    let mut parts = Vec::new();
    let mut bytes = 0usize;
    for item in args.by_ref().take(5) {
        bytes = bytes.saturating_add(item.as_encoded_bytes().len());
        if bytes > 64 * 1024 {
            return Err("arguments exceed 64 KiB".into());
        }
        parts.push(item);
    }
    if parts.len() > 4 || args.next().is_some() {
        return Err("too many arguments".into());
    }
    match parts.as_slice() {
        [] => Ok(Invocation::Help),
        [one] if one == "--help" || one == "-h" || one == "help" => Ok(Invocation::Help),
        [one, two]
            if ((one == "inspect" || one == "ast-graph")
                && (two == "--help" || two == "-h"))
                || (one == "help" && (two == "inspect" || two == "ast-graph")) =>
        {
            Ok(Invocation::Help)
        }
        [one, root]
            if one == "inspect"
                && !root.is_empty()
                && !root.to_string_lossy().starts_with("--") =>
        {
            Ok(Invocation::Inspect(PathBuf::from(root)))
        }
        [one, root]
            if one == "ast-graph"
                && !root.is_empty()
                && !root.to_string_lossy().starts_with("--") =>
        {
            Ok(Invocation::AstGraph {
                root: PathBuf::from(root),
                format: AstFormat::Yaml,
            })
        }
        [one, flag, fmt, root]
            if one == "ast-graph"
                && flag == "--format"
                && !root.is_empty()
                && !root.to_string_lossy().starts_with("--") =>
        {
            Ok(Invocation::AstGraph {
                root: PathBuf::from(root),
                format: parse_format(fmt)?,
            })
        }
        [one, root, flag, fmt]
            if one == "ast-graph"
                && flag == "--format"
                && !root.is_empty()
                && !root.to_string_lossy().starts_with("--") =>
        {
            Ok(Invocation::AstGraph {
                root: PathBuf::from(root),
                format: parse_format(fmt)?,
            })
        }
        _ => Err(
            "expected inspect ROOT, ast-graph [--format yaml|json] ROOT, or --help; paths are native arguments, not shell commands"
                .into(),
        ),
    }
}
enum ConsoleRequest {
    Output(Vec<u8>),
    Diagnostic(String),
}
struct Console;
impl IoClient for Console {
    type Request = ConsoleRequest;
    type Response = ();
    type Error = io::Error;
    fn execute(&self, request: ConsoleRequest) -> io::Result<()> {
        match request {
            ConsoleRequest::Output(bytes) => {
                let mut w = io::stdout().lock();
                w.write_all(&bytes)?;
                w.flush()
            }
            ConsoleRequest::Diagnostic(message) => {
                let mut w = io::stderr().lock();
                w.write_all(message.as_bytes())?;
                w.flush()
            }
        }
    }
}
fn resolve_root(root: PathBuf) -> Result<PathBuf, String> {
    if root.is_absolute() {
        Ok(root)
    } else {
        Ok(std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(root))
    }
}
fn execute(invocation: Invocation) -> Result<(Vec<u8>, u8), String> {
    let mut ast_format = AstFormat::Yaml;
    let (app, route, command) = match invocation {
        Invocation::Help => (
            bridge::<crate::filesystem::NativeFiles>(None, None)?,
            HELP_ROUTE,
            Command::Help,
        ),
        Invocation::Inspect(root) => {
            let root = resolve_root(root)?;
            let group =
                register_ide("ide.project", IdeConfig::project(root)).map_err(|e| e.to_string())?;
            let pending = Registry::new()
                .register(group)
                .map_err(|e| e.reason().to_string())?;
            let frozen = pending.freeze().map_err(|e| e.reason().to_string())?;
            let ((), factory) = frozen.into_factories();
            // Explicit startup authority acquisition only: walking/reading starts
            // inside the admitted task. No caller-provided remote authentication.
            let task = factory.create().map_err(|e| e.to_string())?;
            (bridge(Some(task), None)?, ROUTE, Command::Inspect)
        }
        Invocation::AstGraph { root, format } => {
            ast_format = format;
            let root = resolve_root(root)?;
            let group = register_ast_graph("ide.ast_graph", AstGraphConfig::project(root))
                .map_err(|e| e.to_string())?;
            let pending = Registry::new()
                .register(group)
                .map_err(|e| e.reason().to_string())?;
            let frozen = pending.freeze().map_err(|e| e.reason().to_string())?;
            let ((), factory) = frozen.into_factories();
            let task = factory.create().map_err(|e| e.to_string())?;
            (
                bridge(None, Some(task))?,
                AST_GRAPH_ROUTE,
                Command::AstGraph,
            )
        }
    };
    let header = Header::new(
        RouteKey::new(route, 1).map_err(|e| e.to_string())?,
        "ide-cli",
    )
    .map_err(|e| e.to_string())?;
    match app
        .dispatch(Envelope::new(header, command), &true)
        .map_err(|e| e.to_string())?
        .payload
    {
        Output::Help(text) => Ok((text.into_bytes(), 0)),
        Output::Report(report) => {
            let code = if report.complete { 0 } else { 3 };
            yaml::to_yaml(&report, MAX_OUTPUT)
                .map(|bytes| (bytes, code))
                .map_err(|e| e.to_string())
        }
        Output::AstGraph(report) => {
            let code = if report.complete { 0 } else { 3 };
            match ast_format {
                AstFormat::Yaml => yaml::to_ast_graph_yaml(&report, MAX_AST_OUTPUT)
                    .map(|bytes| (bytes, code))
                    .map_err(|e| e.to_string()),
                AstFormat::Json => {
                    yaml::to_ast_graph_json(&report, MAX_AST_OUTPUT).map(|bytes| (bytes, code))
                }
            }
        }
    }
}
/// Arguments exclude the executable name. Returns 0 success/help, 1 operational
/// failure, 2 usage, or 3 an emitted incomplete report. Broken stdout pipe is quiet.
pub fn run(args: impl IntoIterator<Item = OsString>) -> u8 {
    let invocation = match parse(args) {
        Ok(i) => i,
        Err(e) => {
            let _ = Console.execute(ConsoleRequest::Diagnostic(format!("error: {e}\n")));
            return 2;
        }
    };
    match execute(invocation) {
        Ok((bytes, code)) => match Console.execute(ConsoleRequest::Output(bytes)) {
            Ok(()) => code,
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => 0,
            Err(e) => {
                let _ = Console.execute(ConsoleRequest::Diagnostic(format!("output: {e}\n")));
                1
            }
        },
        Err(e) => {
            let _ = Console.execute(ConsoleRequest::Diagnostic(format!("error: {e}\n")));
            1
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    struct Fake(Rc<Cell<usize>>);
    impl IoClient for Fake {
        type Request = FsRequest;
        type Response = FsResponse;
        type Error = FsError;
        fn execute(&self, _: FsRequest) -> Result<FsResponse, FsError> {
            self.0.set(self.0.get() + 1);
            Ok(FsResponse::Listed(vec![]))
        }
    }
    #[test]
    fn rejected_requests_and_help_do_not_inspect() {
        let calls = Rc::new(Cell::new(0));
        let task = InspectTask::new(Fake(calls.clone()), Default::default(), vec![]).unwrap();
        let ast_task = AstGraphTask::new(Fake(calls.clone()), Default::default(), vec![]).unwrap();
        let app = bridge(Some(task), Some(ast_task)).unwrap();
        for (route, version, command, allowed) in [
            (ROUTE, 1, Command::Inspect, false),
            (ROUTE, 1, Command::Help, true),
            (ROUTE, 2, Command::Inspect, true),
            (AST_GRAPH_ROUTE, 1, Command::AstGraph, false),
            (AST_GRAPH_ROUTE, 1, Command::Inspect, true),
            ("missing", 1, Command::Inspect, true),
        ] {
            let h = Header::new(RouteKey::new(route, version).unwrap(), "test").unwrap();
            assert!(app.dispatch(Envelope::new(h, command), &allowed).is_err());
        }
        let h = Header::new(RouteKey::new(HELP_ROUTE, 1).unwrap(), "help").unwrap();
        assert!(app.dispatch(Envelope::new(h, Command::Help), &true).is_ok());
        assert_eq!(calls.get(), 0);
        let h = Header::new(RouteKey::new(ROUTE, 1).unwrap(), "ok").unwrap();
        assert!(app
            .dispatch(Envelope::new(h, Command::Inspect), &true)
            .is_ok());
        assert_eq!(calls.get(), 1);
        let h = Header::new(RouteKey::new(AST_GRAPH_ROUTE, 1).unwrap(), "ok-ast").unwrap();
        assert!(app
            .dispatch(Envelope::new(h, Command::AstGraph), &true)
            .is_ok());
        assert_eq!(calls.get(), 2);
    }
    #[test]
    fn grammar_is_small_and_bounded() {
        assert!(parse(["inspect", "x", "--bad"].map(Into::into)).is_err());
        assert!(parse(["inspect"].map(Into::into)).is_err());
        assert!(parse(["ast-graph"].map(Into::into)).is_err());
        assert!(matches!(
            parse(["inspect", "--help"].map(Into::into)),
            Ok(Invocation::Help)
        ));
        assert!(matches!(
            parse(["ast-graph", "--help"].map(Into::into)),
            Ok(Invocation::Help)
        ));
        assert!(matches!(
            parse(["ast-graph", "."].map(Into::into)),
            Ok(Invocation::AstGraph {
                format: AstFormat::Yaml,
                ..
            })
        ));
        assert!(matches!(
            parse(["ast-graph", "--format", "json", "."].map(Into::into)),
            Ok(Invocation::AstGraph {
                format: AstFormat::Json,
                ..
            })
        ));
        assert!(matches!(
            parse(["ast-graph", ".", "--format", "json"].map(Into::into)),
            Ok(Invocation::AstGraph {
                format: AstFormat::Json,
                ..
            })
        ));
        assert!(parse(["ast-graph", "--format", "xml", "."].map(Into::into)).is_err());
        assert!(parse(std::iter::repeat(OsString::from("x"))).is_err());
        assert!(parse([OsString::from("x".repeat(65537))]).is_err());
    }
}
