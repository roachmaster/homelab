use crate::help::HELP_OPTION;
#[cfg(feature = "disk")]
use forge_local::DiskCommand;
use forge_local::{Command, CommandInfo, CommandKind, LocalConfig, NodeCommand};
use std::ffi::{OsStr, OsString};
#[cfg(feature = "disk")]
use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    Help,
    Session,
    Command(Vec<OsString>),
}
#[derive(Debug)]
pub struct Options {
    pub node_id: String,
    pub mode: Mode,
    #[cfg(feature = "disk")]
    pub directory: Option<PathBuf>,
    #[cfg(feature = "disk")]
    pub disk: bool,
}

impl Options {
    /// Only the adapter resolves an explicit relative startup path/current cwd.
    /// Modules and registrars receive a typed absolute directory afterwards.
    pub fn into_config(self) -> std::io::Result<(LocalConfig, Mode)> {
        #[cfg(feature = "disk")]
        let disk_directory = if self.disk {
            let selected = self.directory.unwrap_or_else(|| PathBuf::from("."));
            Some(if selected.is_absolute() {
                selected
            } else {
                std::env::current_dir()?.join(selected)
            })
        } else {
            None
        };
        Ok((
            LocalConfig {
                node_id: self.node_id,
                #[cfg(feature = "disk")]
                disk_directory,
            },
            self.mode,
        ))
    }
}

pub fn parse_options(args: Vec<OsString>) -> Result<Options, String> {
    let mut args = args.into_iter();
    let mut options = Options {
        node_id: "local".into(),
        mode: Mode::Help,
        #[cfg(feature = "disk")]
        directory: None,
        #[cfg(feature = "disk")]
        disk: true,
    };
    let mut node_seen = false;
    #[cfg(feature = "disk")]
    let mut disk_seen = false;
    while let Some(argument) = args.next() {
        if argument == "--node-id" {
            if node_seen {
                return Err("--node-id was supplied twice".into());
            }
            node_seen = true;
            options.node_id = args
                .next()
                .ok_or("--node-id needs a value")?
                .into_string()
                .map_err(|_| "--node-id must be UTF-8")?;
        } else if argument == "--cwd" {
            #[cfg(feature = "disk")]
            {
                if options.directory.is_some() {
                    return Err("--cwd was supplied twice".into());
                }
                if !options.disk {
                    return Err("--cwd conflicts with --no-disk".into());
                }
                options.directory = Some(PathBuf::from(args.next().ok_or("--cwd needs a path")?));
            }
            #[cfg(not(feature = "disk"))]
            return Err("--cwd is unavailable: this binary was built without disk".into());
        } else if argument == "--no-disk" {
            #[cfg(feature = "disk")]
            {
                if disk_seen {
                    return Err("--no-disk was supplied twice".into());
                }
                if options.directory.is_some() {
                    return Err("--no-disk conflicts with --cwd".into());
                }
                disk_seen = true;
                options.disk = false;
            }
            // A node-only build already has no disk; this switch is idempotent.
        } else if argument == "--session" || HELP_OPTION.matches(argument.as_os_str()) {
            options.mode = if argument == "--session" {
                Mode::Session
            } else {
                Mode::Help
            };
            if args.next().is_some() {
                return Err("--help/-h/--session must end the startup options".into());
            }
            return Ok(options);
        } else {
            if argument.as_encoded_bytes().first() == Some(&b'-') {
                return Err("unknown startup option; use --help".into());
            }
            let mut command = vec![argument];
            command.extend(args);
            options.mode = Mode::Command(command);
            return Ok(options);
        }
    }
    Ok(options)
}

/// Decode only the operation selected by registered header metadata. This pure
/// mapping does not execute a task/client; the application bridge checks again.
pub fn decode(info: &CommandInfo, args: &[OsString]) -> Result<Command, String> {
    match info.kind {
        CommandKind::Commands => {
            no_arguments(args)?;
            Ok(Command::Commands)
        }
        CommandKind::NodeInfo => {
            no_arguments(args)?;
            Ok(Command::Node(NodeCommand::Info))
        }
        CommandKind::NodeStatus => {
            no_arguments(args)?;
            Ok(Command::Node(NodeCommand::Status))
        }
        #[cfg(feature = "disk")]
        CommandKind::DiskPwd => {
            no_arguments(args)?;
            Ok(Command::Disk(DiskCommand::Pwd))
        }
        #[cfg(feature = "disk")]
        CommandKind::DiskList => {
            if args.len() > 1 {
                return Err("disk.list takes zero or one path".into());
            }
            let path = args
                .first()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            nonempty_path(&path)?;
            Ok(Command::Disk(DiskCommand::List { path }))
        }
        #[cfg(feature = "disk")]
        CommandKind::DiskCd => Ok(Command::Disk(DiskCommand::ChangeDirectory {
            path: one_path(args)?,
        })),
        #[cfg(feature = "disk")]
        CommandKind::DiskPermissions => {
            let (follow_symlinks, paths) = if args.first().is_some_and(|arg| arg == "--no-follow") {
                (false, &args[1..])
            } else {
                (true, args)
            };
            Ok(Command::Disk(DiskCommand::Permissions {
                path: one_path(paths)?,
                follow_symlinks,
            }))
        }
    }
}
fn no_arguments(args: &[OsString]) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err("this command takes no arguments".into())
    }
}
#[cfg(feature = "disk")]
fn one_path(args: &[OsString]) -> Result<PathBuf, String> {
    if args.len() != 1 {
        return Err("this command needs exactly one path".into());
    }
    let path = PathBuf::from(&args[0]);
    nonempty_path(&path)?;
    Ok(path)
}
#[cfg(feature = "disk")]
fn nonempty_path(path: &std::path::Path) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        Err("path must not be empty".into())
    } else if path.as_os_str().as_encoded_bytes().first() == Some(&b'-') {
        Err("unknown option or flag-looking path; prefix a literal path with ./".into())
    } else {
        Ok(())
    }
}

/// A deliberately non-shell session grammar. Command then literal remainder.
/// Leading/trailing whitespace is trimmed; quotes/backslashes are path bytes.
pub fn session_arguments(line: &str) -> Vec<OsString> {
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }
    let Some((name, rest)) = line.split_once(char::is_whitespace) else {
        return vec![OsString::from(line)];
    };
    let rest = rest.trim();
    let mut args = vec![OsString::from(name)];
    if rest.is_empty() {
        return args;
    }
    args.push(OsString::from(rest));
    args
}

/// Session-only option splitting follows the selected registered kind, not a
/// separate destination catalog. All other remainders stay literal path text.
pub fn decode_session(info: &CommandInfo, args: &[OsString]) -> Result<Command, String> {
    #[cfg(feature = "disk")]
    if info.kind == CommandKind::DiskPermissions && args.len() == 1 {
        if let Some(text) = args[0].to_str() {
            if let Some((flag, path)) = text.split_once(char::is_whitespace) {
                if flag == "--no-follow" {
                    return decode(
                        info,
                        &[OsString::from(flag), OsString::from(path.trim_start())],
                    );
                }
            }
        }
    }
    decode(info, args)
}

/// Escape native encoded bytes, without lossy UTF-8 conversion or terminal control.
pub fn quote_os(value: &OsStr) -> String {
    let mut output = String::from("\"");
    for byte in value.as_encoded_bytes() {
        match byte {
            b'"' => output.push_str("\\\""),
            b'\\' => output.push_str("\\\\"),
            0x20..=0x7e => output.push(char::from(*byte)),
            _ => {
                use std::fmt::Write;
                let _ = write!(output, "\\x{byte:02x}");
            }
        }
    }
    output.push('"');
    output
}

pub fn safe_text(text: &str) -> String {
    text.chars().flat_map(char::escape_default).collect()
}
