use crate::{
    chat::ChatRelay, config::https_authority, service::Rejection, ChatConfig, HostError, NodeConfig,
};
use forge_ingress_contract::Header;
use hyper::body::Bytes;
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, path::PathBuf, time::Instant};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StudioConfig {
    pub endpoint: SocketAddr,
    pub token_file: PathBuf,
    pub preview_origin: String,
}

impl StudioConfig {
    pub(crate) fn validate(&self) -> Result<(), HostError> {
        https_authority(&self.preview_origin)?;
        if !self.endpoint.ip().is_loopback()
            || self.endpoint.port() == 0
            || !self.token_file.is_absolute()
        {
            return Err(HostError::Configuration(
                "Studio requires a loopback endpoint and an absolute token path",
            ));
        }
        Ok(())
    }
}

pub(crate) struct StudioRelay {
    relay: ChatRelay,
    preview_origin: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Project {
    node: String,
    project: String,
}

impl Project {
    fn admitted(&self, nodes: &[NodeConfig]) -> bool {
        nodes
            .iter()
            .any(|node| node.key == self.node && node.projects.contains(&self.project))
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Request {
    #[serde(rename = "game_project_list")]
    ProjectList { request_id: String },
    #[serde(rename = "game_project_get")]
    ProjectGet {
        request_id: String,
        project: Project,
    },
    #[serde(rename = "game_build_submit")]
    BuildSubmit {
        request_id: String,
        project: Project,
        revision: String,
        profile: String,
        idempotency_key: String,
    },
    #[serde(rename = "game_build_get")]
    BuildGet {
        request_id: String,
        project: Project,
        build_id: String,
        after_sequence: Option<u64>,
    },
    #[serde(rename = "game_build_cancel")]
    BuildCancel {
        request_id: String,
        project: Project,
        build_id: String,
    },
    #[serde(rename = "game_artifact_get")]
    ArtifactGet {
        request_id: String,
        project: Project,
        build_id: String,
    },
}

impl Request {
    fn bindings(
        &self,
    ) -> (
        &str,
        &'static str,
        &'static str,
        Option<&Project>,
        Option<&str>,
    ) {
        match self {
            Self::ProjectList { request_id } => (
                request_id,
                "game.project.list",
                "game_projects_listed",
                None,
                None,
            ),
            Self::ProjectGet {
                request_id,
                project,
            } => (
                request_id,
                "game.project.get",
                "game_project_opened",
                Some(project),
                None,
            ),
            Self::BuildSubmit {
                request_id,
                project,
                ..
            } => (
                request_id,
                "game.build.submit",
                "game_build_submitted",
                Some(project),
                None,
            ),
            Self::BuildGet {
                request_id,
                project,
                build_id,
                ..
            } => (
                request_id,
                "game.build.get",
                "game_build_status",
                Some(project),
                Some(build_id),
            ),
            Self::BuildCancel {
                request_id,
                project,
                build_id,
            } => (
                request_id,
                "game.build.cancel",
                "game_build_cancelled",
                Some(project),
                Some(build_id),
            ),
            Self::ArtifactGet {
                request_id,
                project,
                build_id,
            } => (
                request_id,
                "game.artifact.get",
                "game_artifact_opened",
                Some(project),
                Some(build_id),
            ),
        }
    }

    fn is_valid(&self) -> bool {
        match self {
            Self::BuildSubmit {
                revision,
                profile,
                idempotency_key,
                ..
            } => is_revision(revision) && is_name(profile, 64) && is_name(idempotency_key, 128),
            Self::BuildGet { after_sequence, .. } => {
                after_sequence.is_none_or(|sequence| sequence <= i64::MAX as u64)
            }
            _ => true,
        }
    }
}

#[derive(Deserialize, Serialize)]
struct ProjectView {
    #[serde(flatten)]
    key: Project,
    revision: String,
    profiles: Vec<String>,
}

impl ProjectView {
    fn is_valid(&self) -> bool {
        is_revision(&self.revision)
            && self.profiles.len() <= 32
            && self.profiles.iter().all(|value| is_name(value, 64))
    }
}

#[derive(Deserialize, Serialize)]
struct Artifact {
    sha256: String,
    bytes: u64,
    preview_url: String,
}

#[derive(Deserialize, Serialize)]
struct Build {
    build_id: String,
    project: Project,
    revision: String,
    profile: String,
    state: String,
    events: Vec<BuildEvent>,
    artifact: Option<Artifact>,
}

#[derive(Deserialize, Serialize)]
struct BuildEvent {
    sequence: u64,
    state: String,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind")]
enum Reply {
    #[serde(rename = "game_projects_listed")]
    ProjectsListed {
        request_id: String,
        projects: Vec<ProjectView>,
    },
    #[serde(rename = "game_project_opened")]
    ProjectOpened {
        request_id: String,
        project: ProjectView,
    },
    #[serde(rename = "game_build_submitted")]
    BuildSubmitted { request_id: String, build: Build },
    #[serde(rename = "game_build_status")]
    BuildStatus { request_id: String, build: Build },
    #[serde(rename = "game_build_cancelled")]
    BuildCancelled { request_id: String, build: Build },
    #[serde(rename = "game_artifact_opened")]
    ArtifactOpened {
        request_id: String,
        artifact: Artifact,
    },
    #[serde(rename = "game_failed")]
    Failed { request_id: String, code: String },
}

impl Reply {
    fn kind(&self) -> &'static str {
        match self {
            Self::ProjectsListed { .. } => "game_projects_listed",
            Self::ProjectOpened { .. } => "game_project_opened",
            Self::BuildSubmitted { .. } => "game_build_submitted",
            Self::BuildStatus { .. } => "game_build_status",
            Self::BuildCancelled { .. } => "game_build_cancelled",
            Self::ArtifactOpened { .. } => "game_artifact_opened",
            Self::Failed { .. } => "game_failed",
        }
    }
}

fn is_name(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
}

fn is_digest(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_revision(value: &str) -> bool {
    value.len() == 40 && is_digest(value)
}

fn is_build_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

pub(crate) fn is_destination(destination: &str) -> bool {
    matches!(
        destination,
        "game.project.list"
            | "game.project.get"
            | "game.build.submit"
            | "game.build.get"
            | "game.build.cancel"
            | "game.artifact.get"
    )
}

impl StudioRelay {
    pub(crate) fn new(config: &StudioConfig) -> Result<Self, HostError> {
        Ok(Self {
            relay: ChatRelay::new(&ChatConfig {
                endpoint: config.endpoint,
                token_file: config.token_file.clone(),
                workspace_persistence: false,
                async_dispatch: false,
                mail_enabled: false,
            })?,
            preview_origin: config.preview_origin.clone(),
        })
    }

    fn valid_artifact(&self, artifact: &Artifact) -> bool {
        artifact.sha256.len() == 64
            && is_digest(&artifact.sha256)
            && artifact.bytes > 0
            && artifact.preview_url.len() <= 2048
            && artifact
                .preview_url
                .strip_prefix(&self.preview_origin)
                .is_some_and(|path| {
                    path.starts_with('/')
                        && !path.starts_with("//")
                        && !path.contains('\\')
                        && path.bytes().all(|byte| byte.is_ascii_graphic())
                })
    }

    pub(crate) fn dispatch(
        &self,
        header: &Header,
        body: &[u8],
        nodes: &[NodeConfig],
        deadline: Instant,
    ) -> Result<(u16, Bytes), Rejection> {
        if body.len() > 4096 {
            return Err(Rejection::Limit);
        }
        let request: Request = serde_json::from_slice(body).map_err(|_| Rejection::Invalid)?;
        let (id, destination, expected, project, build_id) = request.bindings();
        if id != header.request_id()
            || destination != header.route().destination()
            || !request.is_valid()
            || build_id.is_some_and(|id| !is_build_id(id))
        {
            return Err(Rejection::Invalid);
        }
        if project.is_some_and(|project| !project.admitted(nodes)) {
            return Err(Rejection::Denied);
        }
        let (status, response) = self
            .relay
            .dispatch(destination, id, body.to_vec(), deadline)?;
        let mut reply: Reply =
            serde_json::from_slice(&response).map_err(|_| Rejection::Unavailable)?;
        let kind = reply.kind();
        let (response_id, valid) = match &mut reply {
            Reply::ProjectsListed {
                request_id,
                projects,
            } => {
                projects.retain(|project| project.key.admitted(nodes));
                (
                    request_id,
                    projects.len() <= 100 && projects.iter().all(ProjectView::is_valid),
                )
            }
            Reply::ProjectOpened {
                request_id,
                project: view,
            } => (request_id, Some(&view.key) == project && view.is_valid()),
            Reply::BuildSubmitted { request_id, build }
            | Reply::BuildStatus { request_id, build }
            | Reply::BuildCancelled { request_id, build } => {
                let valid = Some(&build.project) == project
                    && is_revision(&build.revision)
                    && is_build_id(&build.build_id)
                    && build_id.is_none_or(|id| id == build.build_id)
                    && is_name(&build.profile, 64)
                    && is_name(&build.state, 48)
                    && build.events.len() <= 100
                    && build
                        .events
                        .iter()
                        .all(|event| event.sequence > 0 && is_name(&event.state, 48))
                    && build
                        .events
                        .windows(2)
                        .all(|pair| pair[0].sequence < pair[1].sequence)
                    && build
                        .artifact
                        .as_ref()
                        .is_none_or(|artifact| self.valid_artifact(artifact));
                let matches_intent = match &request {
                    Request::BuildSubmit {
                        revision, profile, ..
                    } => build.revision == *revision && build.profile == *profile,
                    _ => true,
                };
                (request_id, valid && matches_intent)
            }
            Reply::ArtifactOpened {
                request_id,
                artifact,
            } => (request_id, self.valid_artifact(artifact)),
            Reply::Failed { request_id, code } => (request_id, is_name(code, 64)),
        };
        let status = status.as_u16();
        let expected_status = if destination == "game.build.submit" {
            202
        } else {
            200
        };
        if response_id != id
            || !valid
            || !(kind == expected && status == expected_status
                || kind == "game_failed"
                    && matches!(status, 400 | 401 | 403 | 404 | 409 | 413 | 422 | 429 | 503))
        {
            return Err(Rejection::Unavailable);
        }
        let clean = serde_json::to_vec(&reply).map_err(|_| Rejection::Unavailable)?;
        Ok((status, Bytes::from(clean)))
    }
}
