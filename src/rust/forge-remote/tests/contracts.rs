use forge_ingress_contract::{Envelope, Header, Response, ResponseHeader, RouteKey};
use forge_io_client::IoClient;
use forge_remote::*;
use forge_task::Task;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};
fn target() -> Target {
    Target::new("node-a", "boot-1").unwrap()
}
fn call(route: &str) -> NodeCall {
    NodeCall {
        target: target(),
        request: Envelope::new(
            Header::new(RouteKey::new(route, 1).unwrap(), "request-1")
                .unwrap()
                .with_correlation_id("parent-1")
                .unwrap(),
            NodeCommand::Info,
        ),
        expires_unix_ms: now_unix_ms().unwrap() + 5_000,
    }
}
fn info() -> NodeInfo {
    NodeInfo {
        node: "node-a".into(),
        target_os: "test".into(),
        target_arch: "fixture".into(),
    }
}
struct Source(Rc<Cell<usize>>, Result<NodeInfo, RemoteError>);
impl IoClient for Source {
    type Request = ReadNodeInfo;
    type Response = NodeInfo;
    type Error = RemoteError;
    fn execute(&self, _: ReadNodeInfo) -> Result<NodeInfo, RemoteError> {
        self.0.set(self.0.get() + 1);
        self.1.clone()
    }
}
#[test]
fn destination_preserves_header_and_uses_injected_source() {
    let calls = Rc::new(Cell::new(0));
    let destination = Destination::new(
        target(),
        Source(calls.clone(), Ok(info())),
        |subject: &str, _: &Target, _: &Header| subject == "reader",
    );
    let request = call(NODE_INFO);
    let header = ResponseHeader::from(&request.request.header);
    let reply = destination.dispatch_verified(request, "reader").unwrap();
    assert_eq!(reply.responder, target());
    assert_eq!(reply.response.header, header);
    assert_eq!(reply.response.payload, info());
    assert_eq!(calls.get(), 1);
}
#[test]
fn auth_route_target_and_expiry_fail_before_observation() {
    let calls = Rc::new(Cell::new(0));
    let destination = Destination::new(
        target(),
        Source(calls.clone(), Ok(info())),
        |subject: &str, _: &Target, _: &Header| subject == "reader",
    );
    assert_eq!(
        destination.dispatch_verified(call(NODE_INFO), "other"),
        Err(RemoteError::Forbidden)
    );
    assert_eq!(
        destination.dispatch_verified(call(NODE_INFO), ""),
        Err(RemoteError::Unauthenticated)
    );
    assert_eq!(
        destination.dispatch_verified(call("node.status"), "reader"),
        Err(RemoteError::UnknownRoute)
    );
    let mut request = call(NODE_INFO);
    request.target = Target::new("other", "boot-1").unwrap();
    assert_eq!(
        destination.dispatch_verified(request, "reader"),
        Err(RemoteError::WrongTarget)
    );
    let mut request = call(NODE_INFO);
    request.expires_unix_ms = 0;
    assert_eq!(
        destination.dispatch_verified(request, "reader"),
        Err(RemoteError::Deadline {
            may_have_executed: false
        })
    );
    assert_eq!(calls.get(), 0);
}
#[test]
fn expiry_is_rechecked_after_policy_callback() {
    let calls = Rc::new(Cell::new(0));
    let destination = Destination::new(
        target(),
        Source(calls.clone(), Ok(info())),
        |_: &str, _: &Target, _: &Header| {
            std::thread::sleep(Duration::from_millis(30));
            true
        },
    );
    let mut request = call(NODE_INFO);
    request.expires_unix_ms = now_unix_ms().unwrap() + 10;
    assert_eq!(
        destination.dispatch_verified(request, "reader"),
        Err(RemoteError::Deadline {
            may_have_executed: false
        })
    );
    assert_eq!(calls.get(), 0);
}
#[test]
fn source_failure_and_invalid_or_wrong_node_projection_are_not_success() {
    for expected in [
        Err(RemoteError::Unavailable),
        Ok(NodeInfo {
            node: "other".into(),
            ..info()
        }),
        Ok(NodeInfo {
            target_os: "".into(),
            ..info()
        }),
    ] {
        let calls = Rc::new(Cell::new(0));
        let destination = Destination::new(
            target(),
            Source(calls.clone(), expected.clone()),
            |_: &str, _: &Target, _: &Header| true,
        );
        let error = destination
            .dispatch_verified(call(NODE_INFO), "reader")
            .unwrap_err();
        assert_eq!(
            error,
            expected.err().unwrap_or(RemoteError::InvalidResponse)
        );
        assert_eq!(calls.get(), 1);
    }
}
struct FakeClient(Rc<RefCell<Vec<NodeCall>>>, Result<NodeInfo, RemoteError>);
impl IoClient for FakeClient {
    type Request = NodeCall;
    type Response = NodeReply;
    type Error = RemoteError;
    fn execute(&self, request: NodeCall) -> Result<NodeReply, RemoteError> {
        self.0.borrow_mut().push(request.clone());
        let response = Response {
            header: ResponseHeader::from(&request.request.header),
            payload: self.1.clone()?,
        };
        Ok(NodeReply {
            responder: request.target,
            response,
        })
    }
}
#[test]
fn task_builds_typed_call_and_substitutes_clients_without_io() {
    for result in [Ok(info()), Err(RemoteError::Unavailable)] {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let task = ReadNodeTask::new(FakeClient(calls.clone(), result.clone()));
        let request = call(NODE_INFO);
        let output = task.run(ReadNodeInput {
            target: request.target.clone(),
            request_id: "request-1".into(),
            correlation_id: Some("parent-1".into()),
            expires_unix_ms: request.expires_unix_ms,
        });
        assert_eq!(calls.borrow().len(), 1);
        assert_eq!(&calls.borrow()[0], &request);
        if let Err(error) = result {
            assert_eq!(output, Err(error));
        }
    }
}
#[test]
fn invalid_task_metadata_never_calls_client() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let task = ReadNodeTask::new(FakeClient(calls.clone(), Ok(info())));
    assert_eq!(
        task.run(ReadNodeInput {
            target: target(),
            request_id: "bad id".into(),
            correlation_id: None,
            expires_unix_ms: 1
        }),
        Err(RemoteError::InvalidRequest)
    );
    assert!(calls.borrow().is_empty());
}
#[test]
fn bounded_address_and_projection_tokens() {
    for id in ["", "has space", "a/b", &"a".repeat(129)] {
        assert_eq!(Target::new(id, "one"), Err(RemoteError::InvalidRequest));
    }
    assert!(Target::new("node-a_1.example", "boot-1").is_ok());
}
struct SlowSource;
impl IoClient for SlowSource {
    type Request = ReadNodeInfo;
    type Response = NodeInfo;
    type Error = RemoteError;
    fn execute(&self, _: ReadNodeInfo) -> Result<NodeInfo, RemoteError> {
        std::thread::sleep(Duration::from_millis(80));
        Ok(info())
    }
}
#[test]
fn completed_observation_after_expiry_has_unknown_to_caller_outcome() {
    let destination =
        Destination::new(target(), SlowSource, |_: &str, _: &Target, _: &Header| true);
    let mut request = call(NODE_INFO);
    request.expires_unix_ms = now_unix_ms().unwrap() + 50;
    assert_eq!(
        destination.dispatch_verified(request, "reader"),
        Err(RemoteError::Deadline {
            may_have_executed: true
        })
    );
}
#[test]
fn exact_reply_identity_header_and_projection_are_required() {
    let request = call(NODE_INFO);
    let reply = NodeReply {
        responder: target(),
        response: Response {
            header: ResponseHeader::from(&request.request.header),
            payload: info(),
        },
    };
    assert_eq!(validate_reply(&request, &reply), Ok(()));
    let mut wrong = reply.clone();
    wrong.responder = Target::new("node-a", "different-boot").unwrap();
    assert_eq!(
        validate_reply(&request, &wrong),
        Err(RemoteError::InvalidResponse)
    );
    let mut wrong = reply.clone();
    wrong.response.header = ResponseHeader::from(
        &Header::new(RouteKey::new(NODE_INFO, 1).unwrap(), "different-request").unwrap(),
    );
    assert_eq!(
        validate_reply(&request, &wrong),
        Err(RemoteError::InvalidResponse)
    );
    let mut wrong = reply;
    wrong.response.payload.node = "different-node".into();
    assert_eq!(
        validate_reply(&request, &wrong),
        Err(RemoteError::InvalidResponse)
    );
}
#[test]
fn local_connection_budget_can_shorten_but_not_extend_call_deadline() {
    let calls = Rc::new(Cell::new(0));
    let destination = Destination::new(
        target(),
        Source(calls.clone(), Ok(info())),
        |_: &str, _: &Target, _: &Header| {
            std::thread::sleep(Duration::from_millis(30));
            true
        },
    );
    assert_eq!(
        destination.dispatch_verified_before(
            call(NODE_INFO),
            "reader",
            std::time::Instant::now() + Duration::from_millis(10)
        ),
        Err(RemoteError::Deadline {
            may_have_executed: false
        })
    );
    assert_eq!(calls.get(), 0);
    let mut expired = call(NODE_INFO);
    expired.expires_unix_ms = 0;
    assert_eq!(
        destination.dispatch_verified_before(
            expired,
            "reader",
            std::time::Instant::now() + Duration::from_secs(5)
        ),
        Err(RemoteError::Deadline {
            may_have_executed: false
        })
    );
}
