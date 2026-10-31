//! Disk use-case preparation is outside the concrete filesystem client.
use super::{Bindings, BootstrapError, Command, CommandKind, Output, RunError};
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
    for (destination, kind, usage, summary) in [
        (
            "disk.pwd",
            CommandKind::DiskPwd,
            "disk.pwd",
            "Current client-local directory",
        ),
        (
            "disk.list",
            CommandKind::DiskList,
            "disk.list [PATH]",
            "List native entries in deterministic native-name order",
        ),
        (
            "disk.cd",
            CommandKind::DiskCd,
            "disk.cd PATH",
            "Change this application session's directory, not process cwd",
        ),
        (
            "disk.permissions",
            CommandKind::DiskPermissions,
            "disk.permissions [--no-follow] PATH",
            "Read permission metadata, not effective access",
        ),
    ] {
        let task = DiskTask {
            client: client.clone(),
        };
        bindings.register(
            destination,
            kind,
            usage,
            summary,
            move |command, _| match command {
                Command::Disk(command) => task.run(command).map(Output::Disk),
                _ => Err(RunError::UnexpectedCommand),
            },
        )?;
    }
    Ok(())
}
