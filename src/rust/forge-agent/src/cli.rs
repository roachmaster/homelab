//! Thin bounded argument/output adapters. Config and response JSON never become
//! untyped application payloads. Help invokes no factory, network or project read.
use crate::{host, AppError};
use forge_command::{ArgumentDescriptor, CommandDescriptor, DataType, DescribeCommand, Presence};
use forge_ide::assembly::ProjectAlias;
use forge_io_client::IoClient;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::OsString,
    io::{self, Write},
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc},
};

struct Catalog;
impl DescribeCommand for Catalog {
    fn descriptors(&self) -> &'static [CommandDescriptor] {
        &[
            CommandDescriptor {
                name: "node.info",
                help: "Read a configured remote node through admitted HTTPS.",
                arguments: &[ArgumentDescriptor {
                    name: "node",
                    help: "Configured node name, not an endpoint URL.",
                    data_type: DataType::Text,
                    presence: Presence::Required,
                }],
            },
            CommandDescriptor {
                name: "ide.inspect",
                help: "Inspect an authorized server-owned project alias.",
                arguments: &[
                    ArgumentDescriptor {
                        name: "node",
                        help: "Configured node name.",
                        data_type: DataType::Text,
                        presence: Presence::Required,
                    },
                    ArgumentDescriptor {
                        name: "project",
                        help: "Server-owned project alias, not a filesystem path.",
                        data_type: DataType::Text,
                        presence: Presence::Required,
                    },
                ],
            },
        ]
    }
}
const HELP: ArgumentDescriptor = ArgumentDescriptor {
    name: "--help / -h",
    help: "Show usage without opening config or connecting.",
    data_type: DataType::Bool,
    presence: Presence::Optional {
        default: Some("false"),
    },
};
pub(crate) fn help_text() -> String {
    let mut s=String::from("Forge agent — configured read-only remote inspection\n\nUsage:\n  forge-agent serve --config FILE [--max-connections N]\n  forge-agent node.info --config FILE --node NAME\n  forge-agent inspect --config FILE --node NAME --project ALIAS\n  forge-agent --help\n\n");
    for d in Catalog.descriptors() {
        s.push_str(&format!("{}: {}\n", d.name, d.help));
        for a in d.arguments {
            s.push_str(&format!(
                "  --{}: {} (required) — {}\n",
                a.name, a.data_type, a.help
            ));
        }
    }
    s.push_str(&format!("  {}: {} — {}\n\nJSON startup configuration; DER certificate/key files are supplied, never generated.\nPOST /forge/v1/dispatch only. SIGINT/SIGTERM request graceful stop after active work.\nReports are metadata, not secret-redacted. Use trusted configured projects.\n",HELP.name,HELP.data_type,HELP.help));
    s
}
enum Invocation {
    Help,
    Serve {
        config: PathBuf,
        max: Option<usize>,
    },
    Client {
        config: PathBuf,
        node: String,
        project: Option<ProjectAlias>,
    },
}
fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Invocation, AppError> {
    let mut parts = Vec::new();
    let mut bytes = 0usize;
    for part in args {
        bytes = bytes
            .checked_add(part.as_encoded_bytes().len())
            .ok_or(AppError::Usage("argument size overflow"))?;
        if parts.len() == 9 || bytes > 64 * 1024 {
            return Err(AppError::Usage("too many or oversized arguments"));
        }
        parts.push(part);
    }
    if parts.is_empty() || matches!(parts.as_slice(),[one] if one=="--help"||one=="-h"||one=="help")
    {
        return Ok(Invocation::Help);
    }
    let command = parts[0]
        .to_str()
        .ok_or(AppError::Usage("invalid command"))?;
    if !matches!(command, "serve" | "node.info" | "inspect") {
        return Err(AppError::Usage("unknown command"));
    }
    if matches!(parts.get(1),Some(v) if v=="--help"||v=="-h") && parts.len() == 2 {
        return Ok(Invocation::Help);
    }
    if (parts.len() - 1) % 2 != 0 {
        return Err(AppError::Usage("options require values"));
    }
    let mut options = BTreeMap::new();
    for pair in parts[1..].as_chunks::<2>().0 {
        let key = pair[0].to_str().ok_or(AppError::Usage("invalid option"))?;
        if !matches!(
            key,
            "--config" | "--node" | "--project" | "--max-connections"
        ) || options.insert(key, pair[1].clone()).is_some()
        {
            return Err(AppError::Usage("unknown or duplicate option"));
        }
    }
    let config = PathBuf::from(
        options
            .remove("--config")
            .ok_or(AppError::Usage("--config is required"))?,
    );
    if command == "serve" {
        let max = options
            .remove("--max-connections")
            .map(|v| {
                v.to_str()
                    .and_then(|s| s.parse::<usize>().ok())
                    .filter(|n| *n > 0 && *n <= 1_000_000)
                    .ok_or(AppError::Usage("max-connections must be 1..1000000"))
            })
            .transpose()?;
        if !options.is_empty() {
            return Err(AppError::Usage("invalid serve option"));
        }
        return Ok(Invocation::Serve { config, max });
    }
    let node = options
        .remove("--node")
        .ok_or(AppError::Usage("--node is required"))?
        .into_string()
        .map_err(|_| AppError::Usage("node must be UTF-8"))?;
    let project = if command == "inspect" {
        Some(
            ProjectAlias::new(
                options
                    .remove("--project")
                    .ok_or(AppError::Usage("--project is required"))?
                    .into_string()
                    .map_err(|_| AppError::Usage("project must be UTF-8"))?,
            )
            .map_err(crate::assembly)?,
        )
    } else {
        None
    };
    if !options.is_empty() {
        return Err(AppError::Usage("invalid client option"));
    }
    Ok(Invocation::Client {
        config,
        node,
        project,
    })
}
struct Output<W>(RefCell<W>);
impl<W: Write> IoClient for Output<W> {
    type Request = Vec<u8>;
    type Response = ();
    type Error = io::Error;
    fn execute(&self, bytes: Vec<u8>) -> io::Result<()> {
        let mut w = self.0.borrow_mut();
        w.write_all(&bytes)?;
        w.flush()
    }
}
/// Args exclude executable name. The owner supplies the stop flag; this library
/// does not install signal handlers or start background threads.
pub fn run(
    args: impl IntoIterator<Item = OsString>,
    stop: Arc<AtomicBool>,
) -> Result<(), AppError> {
    forge_command::validate_descriptors(Catalog.descriptors()).map_err(crate::assembly)?;
    let invocation = parse(args)?;
    let out = Output(RefCell::new(io::stdout()));
    let err = Output(RefCell::new(io::stderr()));
    match invocation {
        Invocation::Help => out.execute(host::help()?).map_err(AppError::Output),
        Invocation::Client {
            config,
            node,
            project,
        } => out
            .execute(host::client(&config, &node, project)?)
            .map_err(AppError::Output),
        Invocation::Serve { config, max } => host::serve(&config, &stop, max, &out, &err),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(s: &[&str]) -> Vec<OsString> {
        s.iter().map(OsString::from).collect()
    }
    #[test]
    fn malformed_and_duplicate_options_are_rejected() {
        for s in [
            vec!["inspect"],
            vec!["node.info", "--config", "a", "--node", "b", "--node", "c"],
            vec!["serve", "--config", "a", "--max-connections", "0"],
            vec![
                "inspect",
                "--config",
                "a",
                "--node",
                "b",
                "--project",
                "../root",
            ],
            vec![
                "node.info",
                "--config",
                "a",
                "--node",
                "b",
                "--project",
                "p",
            ],
        ] {
            assert!(parse(args(&s)).is_err());
        }
    }
    #[test]
    fn help_is_described_without_configuration() {
        assert!(matches!(
            parse(args(&["inspect", "--help"])),
            Ok(Invocation::Help)
        ));
        assert!(String::from_utf8(host::help().unwrap())
            .unwrap()
            .contains("Server-owned project alias"));
    }
}
