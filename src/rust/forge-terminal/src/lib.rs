//! Thin local terminal adapter: registered metadata -> typed command -> checked
//! application ingress. No task/client lookup, shell execution or remote transport.
pub mod console;
pub mod parser;

use console::{ConsoleRequest, ConsoleResponse, InputLine};
use forge_io_client::IoClient;
#[cfg(feature = "disk")]
use forge_local::DiskResponse;
use forge_local::{Application, CommandInfo, NodeResponse, NodeState, Output};
#[cfg(feature = "disk")]
use parser::quote_os;
use parser::{decode, decode_session, parse_options, safe_text, session_arguments, Mode};
use std::{ffi::OsString, io};

const STARTUP_HELP: &str = "Forge local system\n\
Usage: forge [--node-id ID] [--cwd PATH | --no-disk] COMMAND [ARGUMENTS]\n\
       forge [OPTIONS] --session\n\
       forge [OPTIONS] --help\n\
Options precede the command. --cwd requires the disk build feature.\n\
Session: one command per line; paths are literal remaining text, not shell syntax.\n\
Use help for registered commands; exit or quit ends a session.\n\
No job, remote-node, shell, create or delete commands are implemented.\n";

struct Failure {
    code: u8,
    message: String,
    stop_session: bool,
}
impl Failure {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            code: 2,
            message: message.into(),
            stop_session: false,
        }
    }
    fn runtime(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: message.into(),
            stop_session: false,
        }
    }
    fn console(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: message.into(),
            stop_session: true,
        }
    }
}

/// Execute one invocation. Returns 0 success, 2 bad input/configuration, or 1
/// construction/application/console failure. A session continues after command
/// errors and returns the highest code encountered. No hidden retry occurs.
pub fn run<C>(args: Vec<OsString>, console: &C) -> u8
where
    C: IoClient<Request = ConsoleRequest, Response = ConsoleResponse, Error = io::Error>,
{
    match run_inner(args, console) {
        Ok(code) => code,
        Err(error) => report(console, error).unwrap_or(1),
    }
}

fn run_inner<C>(args: Vec<OsString>, console: &C) -> Result<u8, Failure>
where
    C: IoClient<Request = ConsoleRequest, Response = ConsoleResponse, Error = io::Error>,
{
    let options = parse_options(args).map_err(Failure::usage)?;
    let (config, mode) = options
        .into_config()
        .map_err(|error| Failure::runtime(error.to_string()))?;
    let app = Application::bootstrap(config).map_err(|error| {
        if error.is_configuration() {
            Failure::usage(error.to_string())
        } else {
            Failure::runtime(error.to_string())
        }
    })?;
    match mode {
        Mode::Help => {
            show_help(&app, console)?;
            Ok(0)
        }
        Mode::Command(args) => {
            run_command(&app, args, false, console)?;
            Ok(0)
        }
        Mode::Session => session(&app, console),
    }
}

fn write<C>(console: &C, request: ConsoleRequest) -> Result<(), Failure>
where
    C: IoClient<Request = ConsoleRequest, Response = ConsoleResponse, Error = io::Error>,
{
    match console
        .execute(request)
        .map_err(|error| Failure::console(error.to_string()))?
    {
        ConsoleResponse::Written => Ok(()),
        _ => Err(Failure::console(
            "console returned the wrong response variant",
        )),
    }
}
fn report<C>(console: &C, failure: Failure) -> Result<u8, ()>
where
    C: IoClient<Request = ConsoleRequest, Response = ConsoleResponse, Error = io::Error>,
{
    let message = format!("error: {}\n", safe_text(&failure.message));
    if write(console, ConsoleRequest::WriteErr(message)).is_err() {
        Err(())
    } else {
        Ok(failure.code)
    }
}

fn catalog_text(commands: &[CommandInfo]) -> String {
    let mut output = String::from("Registered commands:\n");
    for command in commands {
        output.push_str(&format!(
            "  {}  {}\n",
            safe_text(command.usage),
            safe_text(command.summary)
        ));
    }
    output
}
fn show_help<C>(app: &Application, console: &C) -> Result<(), Failure>
where
    C: IoClient<Request = ConsoleRequest, Response = ConsoleResponse, Error = io::Error>,
{
    // Discovery is itself an admitted typed application command.
    let commands = app
        .commands()
        .map_err(|error| Failure::runtime(error.to_string()))?;
    write(
        console,
        ConsoleRequest::WriteOut(format!("{STARTUP_HELP}\n{}", catalog_text(&commands))),
    )
}
fn run_command<C>(
    app: &Application,
    args: Vec<OsString>,
    literal_session: bool,
    console: &C,
) -> Result<(), Failure>
where
    C: IoClient<Request = ConsoleRequest, Response = ConsoleResponse, Error = io::Error>,
{
    let name = args
        .first()
        .ok_or_else(|| Failure::usage("missing command"))?
        .to_str()
        .ok_or_else(|| Failure::usage("command name must be UTF-8"))?;
    if matches!(name, "help" | "--help") {
        if args.len() != 1 {
            return Err(Failure::usage("help takes no arguments"));
        }
        return show_help(app, console);
    }
    let commands = app
        .commands()
        .map_err(|error| Failure::runtime(error.to_string()))?;
    let info = commands
        .iter()
        .find(|command| command.route.destination() == name)
        .ok_or_else(|| {
            Failure::usage(format!("unknown or unavailable command {name:?}; use help"))
        })?;
    let payload = if literal_session {
        decode_session(info, &args[1..])
    } else {
        decode(info, &args[1..])
    }
    .map_err(Failure::usage)?;
    let result = app
        .invoke(info.route.clone(), payload)
        .map_err(|error| Failure::runtime(error.to_string()))?;
    write(console, ConsoleRequest::WriteOut(format_output(result)))
}

fn session<C>(app: &Application, console: &C) -> Result<u8, Failure>
where
    C: IoClient<Request = ConsoleRequest, Response = ConsoleResponse, Error = io::Error>,
{
    write(
        console,
        ConsoleRequest::WriteOut("Forge local session. Type help; exit ends the session.\n".into()),
    )?;
    let mut code = 0;
    loop {
        write(console, ConsoleRequest::WriteOut("forge> ".into()))?;
        let line = match console
            .execute(ConsoleRequest::ReadLine)
            .map_err(|error| Failure::runtime(error.to_string()))?
        {
            ConsoleResponse::Input(line) => line,
            _ => return Err(Failure::runtime("console returned the wrong input variant")),
        };
        match line {
            InputLine::End => {
                write(console, ConsoleRequest::WriteOut("\n".into()))?;
                break;
            }
            InputLine::Rejected(message) => match report(console, Failure::usage(message)) {
                Ok(result) => code = code.max(result),
                Err(()) => return Ok(1),
            },
            InputLine::Line(line) => {
                let args = session_arguments(&line);
                if args.is_empty() {
                    continue;
                }
                if args.len() == 1 && matches!(args[0].to_str(), Some("exit" | "quit")) {
                    break;
                }
                if let Err(error) = run_command(app, args, true, console) {
                    let stop = error.stop_session;
                    match report(console, error) {
                        Ok(result) => code = code.max(result),
                        Err(()) => return Ok(1),
                    }
                    if stop {
                        return Ok(1);
                    }
                }
            }
        }
    }
    Ok(code)
}

/// Presentation stays at this adapter boundary. Native paths are quoted encoded
/// bytes (non-ASCII/control bytes escaped), never lossy-converted or executed.
pub fn format_output(output: Output) -> String {
    match output {
        Output::Commands(commands) => catalog_text(&commands),
        Output::Node(NodeResponse::Info(info)) => format!(
            "node_id={}\ntarget_os={}\ntarget_arch={}\n",
            safe_text(&info.id), safe_text(info.target_os), safe_text(info.target_arch)),
        Output::Node(NodeResponse::Status(status)) => {
            let observation = match status.state { NodeState::Responding => "responding" };
            format!(
            "node_id={}\nobservation={observation}\ninstance_uptime_ms={}\nscope=this node client answered; not OS uptime or overall health\n",
            safe_text(&status.id), status.instance_uptime.as_millis())
        },
        #[cfg(feature = "disk")]
        Output::Disk(DiskResponse::WorkingDirectory(path)) => format!("cwd={}\n", quote_os(path.as_os_str())),
        #[cfg(feature = "disk")]
        Output::Disk(DiskResponse::DirectoryChanged(path)) => format!("cwd={}\n", quote_os(path.as_os_str())),
        #[cfg(feature = "disk")]
        Output::Disk(DiskResponse::Listed(listing)) => {
            let mut output = format!("directory={}\n", quote_os(listing.directory.as_os_str()));
            for entry in listing.entries {
                output.push_str(&format!("{:?}\t{}\treadonly={}\tmode={}\t{}\n",
                    entry.kind, entry.size_bytes, entry.permissions.readonly,
                    mode_text(entry.permissions.unix_mode), quote_os(&entry.name)));
            }
            output
        }
        #[cfg(feature = "disk")]
        Output::Disk(DiskResponse::Permissions(inspection)) => format!(
            "path={}\nkind={:?}\nreadonly={}\nunix_mode={}\nmetadata_only=true; not effective access authorization\n",
            quote_os(inspection.path.as_os_str()), inspection.kind,
            inspection.permissions.readonly, mode_text(inspection.permissions.unix_mode)),
    }
}
#[cfg(feature = "disk")]
fn mode_text(mode: Option<u32>) -> String {
    mode.map_or_else(|| "unavailable".into(), |value| format!("{value:04o}"))
}
