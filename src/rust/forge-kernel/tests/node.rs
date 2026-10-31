use forge_bootstrap::{RegistrationError, Registry};
use forge_io_client::IoClient;
use forge_kernel::node::*;
use forge_provider::{Factory, InstanceProvider, Singleton, Transient};
use forge_task::Task;
use std::{cell::RefCell, rc::Rc, time::Duration};

fn factory(id: &str) -> NodeClientFactory {
    let group = register(NodeConfig::Local { id: id.into() }).unwrap();
    let ((), factories) = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    factories.client
}
fn info(id: &str) -> NodeResponse {
    NodeResponse::Info(NodeInfo {
        id: id.into(),
        target_os: "test-os",
        target_arch: "test-arch",
    })
}
fn status(id: &str) -> NodeResponse {
    NodeResponse::Status(NodeStatus {
        id: id.into(),
        state: NodeState::Responding,
        instance_uptime: Duration::from_secs(7),
    })
}

#[test]
fn config_is_explicit_bounded_and_safe_for_presentation() {
    for id in ["", "with space", "a/b", "a\nb", "\u{1b}[31m", "nödé"] {
        assert_eq!(
            register(NodeConfig::Local { id: id.into() }).unwrap_err(),
            NodeConfigError::InvalidId
        );
    }
    assert_eq!(
        register(NodeConfig::Local {
            id: "x".repeat(MAX_NODE_ID_BYTES + 1)
        })
        .unwrap_err(),
        NodeConfigError::InvalidId
    );
    for id in ["node-1.local_A".to_owned(), "x".repeat(MAX_NODE_ID_BYTES)] {
        assert!(register(NodeConfig::Local { id }).is_ok());
    }
}

#[test]
fn registrar_stages_only_a_typed_group_and_duplicates_preserve_original() {
    let group = register(NodeConfig::Local { id: "first".into() }).unwrap();
    assert_eq!(group.info().id(), "kernel.node");
    assert_eq!(group.info().provides(), &[CLIENT_BINDING]);
    assert!(group.info().requires().is_empty());
    let registry = Registry::new().register(group).unwrap();
    let rejection = registry
        .register(
            register(NodeConfig::Local {
                id: "replacement".into(),
            })
            .unwrap(),
        )
        .unwrap_err();
    assert_eq!(
        rejection.reason(),
        &RegistrationError::DuplicateGroup("kernel.node".into())
    );
    let (original, _incoming, _) = rejection.into_parts();
    let ((), factories) = original.freeze().unwrap().into_factories();
    let client = factories.client.create().unwrap();
    match client.execute(NodeRequest::Info).unwrap() {
        NodeResponse::Info(value) => assert_eq!(value.id, "first"),
        other => panic!("unexpected response: {other:?}"),
    }
}

#[test]
fn native_client_reports_configured_label_and_compile_target_not_machine_identity() {
    let client = factory("local-one").create().unwrap();
    assert_eq!(
        client.execute(NodeRequest::Info).unwrap(),
        NodeResponse::Info(NodeInfo {
            id: "local-one".into(),
            target_os: std::env::consts::OS,
            target_arch: std::env::consts::ARCH,
        })
    );
    let first = match client.execute(NodeRequest::Status).unwrap() {
        NodeResponse::Status(value) => value,
        other => panic!("unexpected response: {other:?}"),
    };
    let second = match client.execute(NodeRequest::Status).unwrap() {
        NodeResponse::Status(value) => value,
        other => panic!("unexpected response: {other:?}"),
    };
    assert_eq!(first.id, "local-one");
    assert_eq!(first.state, NodeState::Responding);
    assert!(second.instance_uptime >= first.instance_uptime);
}

#[test]
fn independent_assemblies_and_provider_scopes_do_not_share_a_global_node() {
    let first = Singleton::new(factory("first").create().unwrap());
    let second = Singleton::new(factory("second").create().unwrap());
    let first_lease = first.acquire().unwrap();
    assert!(std::ptr::eq(first_lease, first.acquire().unwrap()));
    assert!(!std::ptr::eq(first_lease, second.acquire().unwrap()));
    let transient = Transient::new(factory("fresh"));
    let a = transient.acquire().unwrap();
    let b = transient.acquire().unwrap();
    assert!(!std::ptr::eq(&*a, &*b));
    for (client, id) in [
        (first_lease, "first"),
        (second.acquire().unwrap(), "second"),
        (&*a, "fresh"),
        (&*b, "fresh"),
    ] {
        match client.execute(NodeRequest::Info).unwrap() {
            NodeResponse::Info(value) => assert_eq!(value.id, id),
            other => panic!("unexpected response: {other:?}"),
        }
    }
}

struct FakeClient {
    seen: Rc<RefCell<Vec<NodeRequest>>>,
    response: Result<NodeResponse, NodeError>,
}
impl IoClient for FakeClient {
    type Request = NodeRequest;
    type Response = NodeResponse;
    type Error = NodeError;
    fn execute(&self, request: NodeRequest) -> Result<NodeResponse, NodeError> {
        self.seen.borrow_mut().push(request);
        self.response.clone()
    }
}

#[test]
fn task_prepares_exact_typed_request_and_works_with_substituted_observations() {
    for (command, expected, response) in [
        (NodeCommand::Info, NodeRequest::Info, info("injected")),
        (NodeCommand::Status, NodeRequest::Status, status("injected")),
    ] {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let task = NodeTask::new(FakeClient {
            seen: seen.clone(),
            response: Ok(response.clone()),
        });
        assert_eq!(task.run(command).unwrap(), response);
        assert_eq!(&*seen.borrow(), &[expected]);
    }
    let task = NodeTask::new(factory("native").create().unwrap());
    assert!(matches!(
        task.run(NodeCommand::Info),
        Ok(NodeResponse::Info(_))
    ));
    assert!(matches!(
        task.run(NodeCommand::Status),
        Ok(NodeResponse::Status(_))
    ));
}

#[test]
fn task_propagates_observation_failure_without_retry_or_fake_health() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let error = NodeError::Unavailable("collector not available".into());
    let task = NodeTask::new(FakeClient {
        seen: seen.clone(),
        response: Err(error.clone()),
    });
    assert_eq!(task.run(NodeCommand::Status), Err(error));
    assert_eq!(&*seen.borrow(), &[NodeRequest::Status]);
}

#[test]
fn task_rejects_a_substituted_clients_wrong_response_kind() {
    for (command, response) in [
        (NodeCommand::Info, status("wrong")),
        (NodeCommand::Status, info("wrong")),
    ] {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let task = NodeTask::new(FakeClient {
            seen: seen.clone(),
            response: Ok(response),
        });
        assert_eq!(task.run(command), Err(NodeError::UnexpectedResponse));
        assert_eq!(seen.borrow().len(), 1);
    }
}

#[test]
fn command_description_is_optional_separate_and_has_no_client_bound_or_execution() {
    use forge_command::{validate_descriptors, DescribeCommand};
    // () is not a node client: reading metadata cannot require executing one.
    let task = forge_kernel::node::NodeTask::new(());
    let descriptions = task.descriptors();
    validate_descriptors(descriptions).unwrap();
    assert_eq!(
        descriptions.iter().map(|d| d.name).collect::<Vec<_>>(),
        ["node.info", "node.status"]
    );
    assert!(descriptions.iter().all(|d| d.arguments.is_empty()));
}
