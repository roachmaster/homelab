//! Disk use-case preparation is outside the concrete filesystem client.
use super::{descriptor_for, Bindings, BootstrapError, Command, CommandKind, Output, RunError};
use forge_command::{ArgumentDescriptor, CommandDescriptor, DataType, DescribeCommand, Presence};
use forge_io_client::IoClient;
use forge_kernel::disk::{DiskError, DiskRequest, DiskResponse};
use forge_task::Task;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskCommand {
    Pwd,
    List {
        path: PathBuf,
    },
    ChangeDirectory {
        path: PathBuf,
    },
    Permissions {
        path: PathBuf,
        follow_symlinks: bool,
    },
}
impl DiskCommand {
    pub(super) fn kind(&self) -> CommandKind {
        match self {
            Self::Pwd => CommandKind::DiskPwd,
            Self::List { .. } => CommandKind::DiskList,
            Self::ChangeDirectory { .. } => CommandKind::DiskCd,
            Self::Permissions { .. } => CommandKind::DiskPermissions,
        }
    }
}
struct DiskTask<C> {
    client: C,
}
impl<C> DescribeCommand for DiskTask<C> {
    fn descriptors(&self) -> &'static [CommandDescriptor] {
        &[
            CommandDescriptor {
                name: "disk.pwd",
                help: "Current client-local directory",
                arguments: &[],
            },
            CommandDescriptor {
                name: "disk.list",
                help: "List native entries in deterministic native-name order",
                arguments: &[ArgumentDescriptor {
                    name: "path",
                    help: "Directory to inspect, relative to the client-local cwd or absolute",
                    data_type: DataType::Path,
                    presence: Presence::Optional { default: Some(".") },
                }],
            },
            CommandDescriptor {
                name: "disk.cd",
                help: "Change this application session's directory, not process cwd",
                arguments: &[ArgumentDescriptor {
                    name: "path",
                    help: "Directory to select, relative to the client-local cwd or absolute",
                    data_type: DataType::Path,
                    presence: Presence::Required,
                }],
            },
            CommandDescriptor {
                name: "disk.permissions",
                help: "Read permission metadata, not effective access",
                arguments: &[
                    ArgumentDescriptor {
                        name: "path",
                        help: "Native entry to inspect",
                        data_type: DataType::Path,
                        presence: Presence::Required,
                    },
                    ArgumentDescriptor {
                        name: "follow_symlinks",
                        help: "Inspect the target rather than the link itself",
                        data_type: DataType::Bool,
                        presence: Presence::Optional {
                            default: Some("true"),
                        },
                    },
                ],
            },
        ]
    }
}
impl<C: IoClient<Request = DiskRequest, Response = DiskResponse, Error = DiskError>> Task
    for DiskTask<C>
{
    type Input = DiskCommand;
    type Output = DiskResponse;
    type Error = RunError;
    fn run(&self, command: DiskCommand) -> Result<DiskResponse, RunError> {
        let expected = command.kind();
        let request = match command {
            DiskCommand::Pwd => DiskRequest::Pwd,
            DiskCommand::List { path } => DiskRequest::List { path },
            DiskCommand::ChangeDirectory { path } => DiskRequest::ChangeDirectory { path },
            DiskCommand::Permissions {
                path,
                follow_symlinks,
            } => DiskRequest::Permissions {
                path,
                follow_symlinks,
            },
        };
        let response = self.client.execute(request).map_err(RunError::Disk)?;
        let matches = matches!(
            (&response, expected),
            (DiskResponse::WorkingDirectory(_), CommandKind::DiskPwd)
                | (DiskResponse::Listed(_), CommandKind::DiskList)
                | (DiskResponse::DirectoryChanged(_), CommandKind::DiskCd)
                | (DiskResponse::Permissions(_), CommandKind::DiskPermissions)
        );
        if matches {
            Ok(response)
        } else {
            Err(RunError::UnexpectedResponse)
        }
    }
}
pub(super) fn bind<C>(bindings: &mut Bindings, client: C) -> Result<(), BootstrapError>
where
    C: IoClient<Request = DiskRequest, Response = DiskResponse, Error = DiskError>
        + Clone
        + 'static,
{
    for (name, kind) in [
        ("disk.pwd", CommandKind::DiskPwd),
        ("disk.list", CommandKind::DiskList),
        ("disk.cd", CommandKind::DiskCd),
        ("disk.permissions", CommandKind::DiskPermissions),
    ] {
        let task = DiskTask {
            client: client.clone(),
        };
        bindings.register(
            descriptor_for(&task, name)?,
            kind,
            move |command, _| match command {
                Command::Disk(command) => task.run(command).map(Output::Disk),
                _ => Err(RunError::UnexpectedCommand),
            },
        )?;
    }
    Ok(())
}
