//! Application-owned Jenkins actions, request preparation and admitted bindings.
use super::{descriptor_for, Bindings, BootstrapError, Command, CommandKind, Output, RunError};
use forge_command::{ArgumentDescriptor, CommandDescriptor, DataType, DescribeCommand, Presence};
use forge_io_client::IoClient;
use forge_jenkins::{Error, Request, Response};
use forge_task::Task;
use std::{collections::BTreeMap, rc::Rc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JenkinsCommand {
    Status,
    Jobs,
    QueueList,
    RecentBuilds {
        job: String,
        limit: u8,
    },
    StopBuild {
        job: String,
        number: u64,
    },
    Build {
        job: String,
        parameters: BTreeMap<String, String>,
    },
    Queue {
        id: u64,
    },
    BuildStatus {
        job: String,
        number: u64,
    },
    Cancel {
        id: u64,
    },
}

impl JenkinsCommand {
    pub(super) fn kind(&self) -> CommandKind {
        match self {
            Self::Status => CommandKind::JenkinsStatus,
            Self::Jobs => CommandKind::JenkinsJobs,
            Self::QueueList => CommandKind::JenkinsQueueList,
            Self::RecentBuilds { .. } => CommandKind::JenkinsRecentBuilds,
            Self::StopBuild { .. } => CommandKind::JenkinsStopBuild,
            Self::Build { .. } => CommandKind::JenkinsBuild,
            Self::Queue { .. } => CommandKind::JenkinsQueue,
            Self::BuildStatus { .. } => CommandKind::JenkinsBuildStatus,
            Self::Cancel { .. } => CommandKind::JenkinsCancel,
        }
    }

    /// Pure preparation validation also protects callers using an injected port.
    pub fn prepared(self) -> Result<Request, Error> {
        let request = match self {
            Self::Status => Request::Controller,
            Self::Jobs => Request::Jobs,
            Self::QueueList => Request::QueueList,
            Self::RecentBuilds { job, limit } => Request::RecentBuilds { job, limit },
            Self::StopBuild { job, number } => Request::StopBuild { job, number },
            Self::Build { job, parameters } => Request::Build { job, parameters },
            Self::Queue { id } => Request::Queue { id },
            Self::BuildStatus { job, number } => Request::BuildStatus { job, number },
            Self::Cancel { id } => Request::CancelQueue { id },
        };
        request.validate()?;
        Ok(request)
    }
}

type JenkinsPort = dyn IoClient<Request = Request, Response = Response, Error = Error>;

/// Explicit per-application shared handle; it cannot discover another provider.
#[derive(Clone)]
pub(crate) struct JenkinsHandle(Rc<JenkinsPort>);
impl JenkinsHandle {
    pub(crate) fn new<C>(client: C) -> Self
    where
        C: IoClient<Request = Request, Response = Response, Error = Error> + 'static,
    {
        Self(Rc::new(client))
    }
}
impl IoClient for JenkinsHandle {
    type Request = Request;
    type Response = Response;
    type Error = Error;
    fn execute(&self, request: Request) -> Result<Response, Error> {
        self.0.execute(request)
    }
}

struct JenkinsTask<C> {
    client: C,
}
impl<C> DescribeCommand for JenkinsTask<C> {
    fn descriptors(&self) -> &'static [CommandDescriptor] {
        &[
            CommandDescriptor {
                name: "jenkins.status",
                help: "Read the configured Jenkins controller",
                arguments: &[],
            },
            CommandDescriptor {
                name: "jenkins.jobs",
                help: "List jobs visible to the configured Jenkins account",
                arguments: &[],
            },
            CommandDescriptor {
                name: "jenkins.queue-list",
                help: "Read up to 50 queued builds and selected public parameters as JSON",
                arguments: &[],
            },
            CommandDescriptor {
                name: "jenkins.recent-builds",
                help: "Read recent builds and selected public parameters as JSON; never resubmit automatically",
                arguments: &[
                    ArgumentDescriptor {
                        name: "job",
                        help: "Job name, with folders separated by /",
                        data_type: DataType::Text,
                        presence: Presence::Required,
                    },
                    ArgumentDescriptor {
                        name: "limit",
                        help: "Maximum build count, 1 through 50",
                        data_type: DataType::U64,
                        presence: Presence::Optional { default: Some("50") },
                    },
                ],
            },
            CommandDescriptor {
                name: "jenkins.stop-build",
                help: "Request graceful build stop; acceptance does not prove the build stopped",
                arguments: &[
                    ArgumentDescriptor {
                        name: "job",
                        help: "Job name, with folders separated by /",
                        data_type: DataType::Text,
                        presence: Presence::Required,
                    },
                    ArgumentDescriptor {
                        name: "number",
                        help: "Positive build number",
                        data_type: DataType::U64,
                        presence: Presence::Required,
                    },
                ],
            },
            CommandDescriptor {
                name: "jenkins.build",
                help: "Request a build; acceptance returns a queue item, not build completion",
                arguments: &[
                    ArgumentDescriptor {
                        name: "job",
                        help: "Job name, with folders separated by /",
                        data_type: DataType::Text,
                        presence: Presence::Required,
                    },
                    ArgumentDescriptor {
                        name: "parameters",
                        help: "Optional NAME=VALUE pairs; no shell expansion",
                        data_type: DataType::Custom("NameValuePairs"),
                        presence: Presence::Optional { default: None },
                    },
                ],
            },
            CommandDescriptor {
                name: "jenkins.queue",
                help: "Inspect a queue item without automatically polling",
                arguments: &[ArgumentDescriptor {
                    name: "id",
                    help: "Positive queue item number",
                    data_type: DataType::U64,
                    presence: Presence::Required,
                }],
            },
            CommandDescriptor {
                name: "jenkins.build-status",
                help: "Inspect one build without automatically polling",
                arguments: &[
                    ArgumentDescriptor {
                        name: "job",
                        help: "Job name, with folders separated by /",
                        data_type: DataType::Text,
                        presence: Presence::Required,
                    },
                    ArgumentDescriptor {
                        name: "number",
                        help: "Positive build number",
                        data_type: DataType::U64,
                        presence: Presence::Required,
                    },
                ],
            },
            CommandDescriptor {
                name: "jenkins.cancel",
                help: "Request queue cancellation; this does not prove a running build stopped",
                arguments: &[ArgumentDescriptor {
                    name: "id",
                    help: "Positive queue item number",
                    data_type: DataType::U64,
                    presence: Presence::Required,
                }],
            },
        ]
    }
}
impl<C: IoClient<Request = Request, Response = Response, Error = Error>> Task for JenkinsTask<C> {
    type Input = JenkinsCommand;
    type Output = Response;
    type Error = RunError;
    fn run(&self, command: JenkinsCommand) -> Result<Response, RunError> {
        let expected = command.kind();
        let response = self
            .client
            .execute(command.prepared().map_err(RunError::Jenkins)?)
            .map_err(RunError::Jenkins)?;
        if matches!(
            (&response, expected),
            (Response::Controller(_), CommandKind::JenkinsStatus)
                | (Response::Jobs(_), CommandKind::JenkinsJobs)
                | (Response::QueueList(_), CommandKind::JenkinsQueueList)
                | (Response::RecentBuilds(_), CommandKind::JenkinsRecentBuilds)
                | (Response::StopRequested, CommandKind::JenkinsStopBuild)
                | (Response::Queued(_), CommandKind::JenkinsBuild)
                | (Response::Queue(_), CommandKind::JenkinsQueue)
                | (Response::Build(_), CommandKind::JenkinsBuildStatus)
                | (Response::CancelRequested, CommandKind::JenkinsCancel)
        ) {
            Ok(response)
        } else {
            Err(RunError::UnexpectedResponse)
        }
    }
}

pub(super) fn bind<C>(bindings: &mut Bindings, client: C) -> Result<(), BootstrapError>
where
    C: IoClient<Request = Request, Response = Response, Error = Error> + Clone + 'static,
{
    for (name, kind) in [
        ("jenkins.status", CommandKind::JenkinsStatus),
        ("jenkins.jobs", CommandKind::JenkinsJobs),
        ("jenkins.queue-list", CommandKind::JenkinsQueueList),
        ("jenkins.recent-builds", CommandKind::JenkinsRecentBuilds),
        ("jenkins.stop-build", CommandKind::JenkinsStopBuild),
        ("jenkins.build", CommandKind::JenkinsBuild),
        ("jenkins.queue", CommandKind::JenkinsQueue),
        ("jenkins.build-status", CommandKind::JenkinsBuildStatus),
        ("jenkins.cancel", CommandKind::JenkinsCancel),
    ] {
        let task = JenkinsTask {
            client: client.clone(),
        };
        bindings.register(
            descriptor_for(&task, name)?,
            kind,
            move |command, _| match command {
                Command::Jenkins(command) => task.run(command).map(Output::Jenkins),
                _ => Err(RunError::UnexpectedCommand),
            },
        )?;
    }
    Ok(())
}
