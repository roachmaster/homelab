//! Application-owned sum and policy. Transport supplies verified context; no
//! arbitrary subject constructor is exposed by this application.
use crate::config::Grants;
use forge_bridge::{Bridge, DispatchError, RegistryBuilder};
use forge_ide::{
    assembly::ProjectAlias,
    model::ProjectReport,
    remote::{InspectCall, InspectCommand, InspectReply, ReportLimits, IDE_INSPECT},
};
use forge_ingress_contract::{
    AdmissionDenied, AdmissionPolicy, Envelope, Header, Payload, Response, RouteKey,
};
use forge_io_client::IoClient;
use forge_remote::{
    now_unix_ms, NodeCall, NodeInfo, NodeInfoSource, NodeReply, ReadNodeInfo, RemoteError, Target,
    NODE_INFO,
};
use forge_remote_https::{
    gateway::{GatewayCodec, NodeCodec, RequestMetadata, VerifiedContext},
    ide::IdeCodec,
};
use std::{cell::Cell, rc::Rc, time::Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Node,
    Inspect,
}
pub(crate) enum AgentCommand {
    NodeInfo,
    Inspect(ProjectAlias),
}
impl Payload for AgentCommand {
    type Kind = Kind;
    fn kind(&self) -> Kind {
        match self {
            Self::NodeInfo => Kind::Node,
            Self::Inspect(_) => Kind::Inspect,
        }
    }
}
pub(crate) struct AgentRequest {
    target: Target,
    request: Envelope<AgentCommand>,
    expires: u64,
}
enum AgentResult {
    Node(NodeInfo),
    Report(Box<ProjectReport>),
}
pub(crate) enum AgentReply {
    Node(NodeReply),
    Inspect(InspectReply),
}
pub(crate) struct Codec {
    ide: IdeCodec,
}
impl Codec {
    pub fn new(limits: ReportLimits) -> Result<Self, RemoteError> {
        Ok(Self {
            ide: IdeCodec::new(limits)?,
        })
    }
}
impl GatewayCodec for Codec {
    type Request = AgentRequest;
    type Response = AgentReply;
    fn decode(&self, meta: RequestMetadata, body: &[u8]) -> Result<AgentRequest, RemoteError> {
        if meta.header.route().version() != 1 {
            return Err(RemoteError::UnknownRoute);
        }
        match meta.header.route().destination() {
            NODE_INFO => {
                let NodeCall {
                    target,
                    request,
                    expires_unix_ms,
                } = NodeCodec.decode(meta, body)?;
                Ok(AgentRequest {
                    target,
                    request: Envelope::new(request.header, AgentCommand::NodeInfo),
                    expires: expires_unix_ms,
                })
            }
            IDE_INSPECT => {
                let InspectCall {
                    target,
                    request,
                    expires_unix_ms,
                } = self.ide.decode(meta, body)?;
                let InspectCommand::Project(project) = request.payload;
                Ok(AgentRequest {
                    target,
                    request: Envelope::new(request.header, AgentCommand::Inspect(project)),
                    expires: expires_unix_ms,
                })
            }
            _ => Err(RemoteError::UnknownRoute),
        }
    }
    fn encode(
        &self,
        result: Result<AgentReply, RemoteError>,
        max: usize,
    ) -> Result<Vec<u8>, RemoteError> {
        match result {
            Ok(AgentReply::Node(reply)) => NodeCodec.encode(Ok(reply), max),
            Ok(AgentReply::Inspect(reply)) => self.ide.encode(Ok(reply), max),
            Err(error) => NodeCodec.encode(Err(error), max),
        }
    }
}
struct Context {
    subject: String,
    header: Header,
    expires: u64,
    deadline: Instant,
    invoked: Cell<bool>,
}
impl Context {
    fn live(&self) -> Result<(), RemoteError> {
        if Instant::now() >= self.deadline || now_unix_ms()? >= self.expires {
            Err(RemoteError::Deadline {
                may_have_executed: self.invoked.get(),
            })
        } else {
            Ok(())
        }
    }
}
struct Admission(Rc<Grants>);
impl AdmissionPolicy<Context> for Admission {
    fn admit(&self, header: &Header, c: &Context) -> Result<(), AdmissionDenied> {
        c.live().map_err(|_| AdmissionDenied)?;
        if header != &c.header {
            return Err(AdmissionDenied);
        }
        let g = self.0.get(&c.subject).ok_or(AdmissionDenied)?;
        let permitted = match header.route().destination() {
            NODE_INFO => g.node_info,
            IDE_INSPECT => !g.projects.is_empty(),
            _ => false,
        };
        if !permitted {
            return Err(AdmissionDenied);
        }
        c.live().map_err(|_| AdmissionDenied)
    }
}
pub(crate) struct Service {
    target: Target,
    bridge: Bridge<'static, AgentCommand, Context, AgentResult, RemoteError, Admission>,
}
impl Service {
    pub fn new<N, P>(
        target: Target,
        node: N,
        projects: P,
        grants: Grants,
    ) -> Result<Self, RemoteError>
    where
        N: NodeInfoSource + 'static,
        P: IoClient<Request = ProjectAlias, Response = ProjectReport, Error = RemoteError>
            + 'static,
    {
        let grants = Rc::new(grants);
        let mut routes = RegistryBuilder::new();
        routes
            .register(
                RouteKey::new(NODE_INFO, 1).expect("static route"),
                Kind::Node,
                move |_: AgentCommand, c: &Context| {
                    c.live()?;
                    c.invoked.set(true);
                    let value = node.execute(ReadNodeInfo)?;
                    value.validate()?;
                    c.live()?;
                    Ok(AgentResult::Node(value))
                },
            )
            .map_err(|_| RemoteError::InvalidRequest)?;
        let project_grants = Rc::clone(&grants);
        routes
            .register(
                RouteKey::new(IDE_INSPECT, 1).expect("static route"),
                Kind::Inspect,
                move |command: AgentCommand, c: &Context| {
                    let AgentCommand::Inspect(alias) = command else {
                        return Err(RemoteError::InvalidRequest);
                    };
                    if !project_grants
                        .get(&c.subject)
                        .is_some_and(|g| g.projects.contains(&alias))
                    {
                        return Err(RemoteError::Forbidden);
                    }
                    c.live()?;
                    c.invoked.set(true);
                    let report = projects.execute(alias)?;
                    c.live()?;
                    Ok(AgentResult::Report(Box::new(report)))
                },
            )
            .map_err(|_| RemoteError::InvalidRequest)?;
        Ok(Self {
            target,
            bridge: Bridge::new(routes.freeze(), Admission(grants)),
        })
    }
    pub fn dispatch(
        &self,
        call: AgentRequest,
        peer: &VerifiedContext,
    ) -> Result<AgentReply, RemoteError> {
        self.dispatch_inner(call, peer.subject(), peer.deadline())
    }
    fn dispatch_inner(
        &self,
        call: AgentRequest,
        subject: &str,
        deadline: Instant,
    ) -> Result<AgentReply, RemoteError> {
        if call.target != self.target {
            return Err(RemoteError::WrongTarget);
        }
        let c = Context {
            subject: subject.to_owned(),
            header: call.request.header.clone(),
            expires: call.expires,
            deadline,
            invoked: Cell::new(false),
        };
        c.live()?;
        let result = self.bridge.dispatch(call.request, &c);
        c.live()?;
        let response = result.map_err(|e| match e {
            DispatchError::UnknownRoute(_) => RemoteError::UnknownRoute,
            DispatchError::Admission(_) => RemoteError::Forbidden,
            DispatchError::PayloadMismatch(_) => RemoteError::InvalidRequest,
            DispatchError::Handler(e) => e,
        })?;
        match response.payload {
            AgentResult::Node(value) => {
                if value.node != self.target.node() {
                    return Err(RemoteError::InvalidResponse);
                }
                Ok(AgentReply::Node(NodeReply {
                    responder: self.target.clone(),
                    response: Response {
                        header: response.header,
                        payload: value,
                    },
                }))
            }
            AgentResult::Report(value) => Ok(AgentReply::Inspect(InspectReply {
                responder: self.target.clone(),
                response: Response {
                    header: response.header,
                    payload: *value,
                },
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Grant;
    use forge_ide::model::{Entry, Name, TreeNode};
    use std::{
        collections::{BTreeMap, BTreeSet},
        time::Duration,
    };
    struct Nodes(Rc<Cell<usize>>);
    impl IoClient for Nodes {
        type Request = ReadNodeInfo;
        type Response = NodeInfo;
        type Error = RemoteError;
        fn execute(&self, _: ReadNodeInfo) -> Result<NodeInfo, RemoteError> {
            self.0.set(self.0.get() + 1);
            Ok(NodeInfo {
                node: "node".into(),
                target_os: "test".into(),
                target_arch: "test".into(),
            })
        }
    }
    struct Projects(Rc<Cell<usize>>);
    impl IoClient for Projects {
        type Request = ProjectAlias;
        type Response = ProjectReport;
        type Error = RemoteError;
        fn execute(&self, _: ProjectAlias) -> Result<ProjectReport, RemoteError> {
            self.0.set(self.0.get() + 1);
            Ok(ProjectReport {
                format_version: 1,
                complete: true,
                entries: 1,
                files: 0,
                bytes_read: 0,
                tree: TreeNode {
                    name: Name::Utf8(".".into()),
                    detail: Entry::Directory { children: vec![] },
                },
            })
        }
    }
    fn call(route: &str, payload: AgentCommand) -> AgentRequest {
        AgentRequest {
            target: Target::new("node", "run").unwrap(),
            request: Envelope::new(
                Header::new(RouteKey::new(route, 1).unwrap(), "test").unwrap(),
                payload,
            ),
            expires: now_unix_ms().unwrap() + 1000,
        }
    }
    #[test]
    fn direct_gate_and_resource_policy_precede_effects() {
        let n = Rc::new(Cell::new(0));
        let p = Rc::new(Cell::new(0));
        let mut grants = BTreeMap::new();
        grants.insert(
            "reader".into(),
            Grant {
                node_info: true,
                projects: BTreeSet::from([ProjectAlias::new("good").unwrap()]),
            },
        );
        let service = Service::new(
            Target::new("node", "run").unwrap(),
            Nodes(n.clone()),
            Projects(p.clone()),
            grants,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        assert!(matches!(
            service.dispatch_inner(
                call(
                    NODE_INFO,
                    AgentCommand::Inspect(ProjectAlias::new("good").unwrap())
                ),
                "reader",
                deadline
            ),
            Err(RemoteError::InvalidRequest)
        ));
        assert!(matches!(
            service.dispatch_inner(
                call(
                    IDE_INSPECT,
                    AgentCommand::Inspect(ProjectAlias::new("denied").unwrap())
                ),
                "reader",
                deadline
            ),
            Err(RemoteError::Forbidden)
        ));
        assert!(matches!(
            service.dispatch_inner(call(NODE_INFO, AgentCommand::NodeInfo), "forged", deadline),
            Err(RemoteError::Forbidden)
        ));
        assert!(matches!(
            service.dispatch_inner(call("unknown", AgentCommand::NodeInfo), "reader", deadline),
            Err(RemoteError::UnknownRoute)
        ));
        assert_eq!((n.get(), p.get()), (0, 0));
        service
            .dispatch_inner(
                call(
                    IDE_INSPECT,
                    AgentCommand::Inspect(ProjectAlias::new("good").unwrap()),
                ),
                "reader",
                deadline,
            )
            .unwrap();
        assert_eq!((n.get(), p.get()), (0, 1));
    }
}
