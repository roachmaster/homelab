use forge_bootstrap::PreparedGroup;
use forge_ingress_contract::{Envelope, Header, Payload, RouteKey};
use forge_io_client::IoClient;
use forge_local::*;
use forge_provider::Factory;
use std::{cell::Cell, error::Error, rc::Rc, time::Duration};

fn route(name: &str) -> RouteKey {
    RouteKey::new(name, 1).unwrap()
}
#[derive(Clone)]
struct Counts {
    creates: Rc<Cell<usize>>,
    calls: Rc<Cell<usize>>,
    drops: Rc<Cell<usize>>,
}
impl Counts {
    fn new() -> Self {
        Self {
            creates: Rc::new(Cell::new(0)),
            calls: Rc::new(Cell::new(0)),
            drops: Rc::new(Cell::new(0)),
        }
    }
}
struct FakeFactory {
    counts: Counts,
    id: String,
    fail_create: bool,
    fail_call: bool,
}
struct FakeNode {
    counts: Counts,
    id: String,
    fail_call: bool,
}
impl Factory<FakeNode> for FakeFactory {
    type Error = NodeError;
    fn create(&self) -> Result<FakeNode, NodeError> {
        self.counts.creates.set(self.counts.creates.get() + 1);
        if self.fail_create {
            return Err(NodeError::Unavailable("factory failed".into()));
        }
        Ok(FakeNode {
            counts: self.counts.clone(),
            id: self.id.clone(),
            fail_call: self.fail_call,
        })
    }
}
impl Drop for FakeNode {
    fn drop(&mut self) {
        self.counts.drops.set(self.counts.drops.get() + 1);
    }
}
impl IoClient for FakeNode {
    type Request = NodeRequest;
    type Response = NodeResponse;
    type Error = NodeError;
    fn execute(&self, request: NodeRequest) -> Result<NodeResponse, NodeError> {
        self.counts.calls.set(self.counts.calls.get() + 1);
        if self.fail_call {
            return Err(NodeError::Unavailable("collector failed".into()));
        }
        Ok(match request {
            NodeRequest::Info => NodeResponse::Info(NodeInfo {
                id: self.id.clone(),
                target_os: "fake-os",
                target_arch: "fake-arch",
            }),
            NodeRequest::Status => NodeResponse::Status(NodeStatus {
                id: self.id.clone(),
                state: NodeState::Responding,
                instance_uptime: Duration::from_secs(42),
            }),
        })
    }
}
fn fake_group(
    config: NodeConfig,
    counts: Counts,
    fail_create: bool,
    fail_call: bool,
) -> Result<PreparedGroup<NodeFactories<FakeFactory>>, NodeConfigError> {
    let NodeConfig::Local { id } = config;
    PreparedGroup::new(
        "test.node",
        &["kernel.node.client"],
        &[],
        NodeFactories {
            client: FakeFactory {
                counts,
                id,
                fail_create,
                fail_call,
            },
        },
    )
    .map_err(NodeConfigError::Registration)
}
fn fake_app(counts: Counts, fail_call: bool) -> Application {
    Application::bootstrap_with_node_registrar(LocalConfig::default(), move |c| {
        fake_group(c, counts, false, fail_call)
    })
    .unwrap()
}
#[test]
fn native_node_reports_configured_identity_and_its_own_observation() {
    let app = Application::bootstrap(LocalConfig {
        node_id: "office-1".into(),
        ..LocalConfig::default()
    })
    .unwrap();
    let Output::Node(NodeResponse::Info(info)) = app
        .invoke(route("node.info"), Command::Node(NodeCommand::Info))
        .unwrap()
    else {
        panic!("info")
    };
    assert_eq!(info.id, "office-1");
    assert_eq!(info.target_os, std::env::consts::OS);
    let Output::Node(NodeResponse::Status(status)) = app
        .invoke(route("node.status"), Command::Node(NodeCommand::Status))
        .unwrap()
    else {
        panic!("status")
    };
    assert_eq!(status.id, "office-1");
    assert_eq!(status.state, NodeState::Responding);
}
#[test]
fn selected_routes_are_sorted_and_discovery_is_a_checked_command() {
    let app = Application::bootstrap(LocalConfig::default()).unwrap();
    let commands = app.commands().unwrap();
    assert_eq!(
        commands
            .iter()
            .map(|x| x.route.destination())
            .collect::<Vec<_>>(),
        vec!["node.info", "node.status", "system.commands"]
    );
    for c in &commands {
        assert_eq!(c.route.version(), 1);
        assert!(!c.usage.is_empty());
        assert!(!c.summary.is_empty());
    }
    let bad = app
        .invoke(route("system.commands"), Command::Node(NodeCommand::Info))
        .unwrap_err();
    assert!(matches!(
        bad,
        AppError::Dispatch(forge_bridge::DispatchError::PayloadMismatch(_))
    ));
}
#[test]
fn header_route_and_nested_variants_gate_before_injected_client_calls() {
    let counts = Counts::new();
    let app = fake_app(counts.clone(), false);
    assert_eq!(counts.creates.get(), 1);
    for r in [route("unknown"), RouteKey::new("node.info", 2).unwrap()] {
        assert!(matches!(
            app.invoke(r, Command::Node(NodeCommand::Info)),
            Err(AppError::Dispatch(
                forge_bridge::DispatchError::UnknownRoute(_)
            ))
        ));
    }
    assert!(matches!(
        app.invoke(route("node.info"), Command::Node(NodeCommand::Status)),
        Err(AppError::Dispatch(
            forge_bridge::DispatchError::PayloadMismatch(_)
        ))
    ));
    assert_eq!(counts.calls.get(), 0);
    let header = Header::new(route("node.info"), "external-1")
        .unwrap()
        .with_correlation_id("corr-1")
        .unwrap();
    let response = app
        .dispatch(Envelope::new(header, Command::Node(NodeCommand::Info)))
        .unwrap();
    assert_eq!(response.header.request_id(), "external-1");
    assert_eq!(response.header.correlation_id(), Some("corr-1"));
    assert_eq!(counts.calls.get(), 1);
    assert_ne!(
        Command::Node(NodeCommand::Info).kind(),
        Command::Node(NodeCommand::Status).kind()
    );
}
#[test]
fn substitution_uses_same_tasks_and_scope_is_one_application() {
    let counts = Counts::new();
    let app = fake_app(counts.clone(), false);
    let Output::Node(NodeResponse::Status(status)) = app
        .invoke(route("node.status"), Command::Node(NodeCommand::Status))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(status.instance_uptime, Duration::from_secs(42));
    let Output::Node(NodeResponse::Info(info)) = app
        .invoke(route("node.info"), Command::Node(NodeCommand::Info))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(info.target_os, "fake-os");
    assert_eq!(counts.creates.get(), 1);
    let other = fake_app(counts.clone(), false);
    assert_eq!(counts.creates.get(), 2);
    drop(app);
    assert_eq!(counts.drops.get(), 1);
    drop(other);
    assert_eq!(counts.drops.get(), 2);
}
#[test]
fn factory_and_client_errors_are_visible_without_retry() {
    let counts = Counts::new();
    let bad = Application::bootstrap_with_node_registrar(LocalConfig::default(), |c| {
        fake_group(c, counts.clone(), true, false)
    })
    .unwrap_err();
    assert!(matches!(bad, BootstrapError::NodeFactory(_)));
    assert!(bad.source().is_some());
    assert_eq!(counts.creates.get(), 1);
    let app = fake_app(counts.clone(), true);
    let bad = app
        .invoke(route("node.status"), Command::Node(NodeCommand::Status))
        .unwrap_err();
    assert!(
        matches!(&bad,AppError::Dispatch(forge_bridge::DispatchError::Handler(RunError::Node(NodeError::Unavailable(reason)))) if reason=="collector failed")
    );
    assert!(bad.source().unwrap().source().is_some());
    assert_eq!(counts.calls.get(), 1);
}
#[test]
fn validation_precedes_factory_effects() {
    let counts = Counts::new();
    let result = Application::bootstrap_with_node_registrar(LocalConfig::default(), |_| {
        PreparedGroup::new(
            "test.node",
            &["kernel.node.client"],
            &["missing"],
            NodeFactories {
                client: FakeFactory {
                    counts: counts.clone(),
                    id: "x".into(),
                    fail_create: false,
                    fail_call: false,
                },
            },
        )
        .map_err(NodeConfigError::Registration)
    });
    assert!(matches!(result, Err(BootstrapError::Validation(_))));
    assert_eq!(counts.creates.get(), 0);
    assert!(Application::bootstrap(LocalConfig {
        node_id: "bad id".into(),
        ..LocalConfig::default()
    })
    .unwrap_err()
    .is_configuration());
}
#[test]
fn omitted_disk_is_not_advertised_or_routable() {
    let app = Application::bootstrap(LocalConfig::default()).unwrap();
    assert!(app
        .commands()
        .unwrap()
        .iter()
        .all(|c| !c.route.destination().starts_with("disk.")));
    assert!(matches!(
        app.invoke(route("disk.pwd"), Command::Commands),
        Err(AppError::Dispatch(
            forge_bridge::DispatchError::UnknownRoute(_)
        ))
    ));
}

#[cfg(feature = "disk")]
mod native_disk {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "forge-local-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            fs::create_dir(root.join("child with spaces")).unwrap();
            fs::write(root.join("item"), b"test").unwrap();
            Self(root)
        }
        fn config(&self) -> LocalConfig {
            LocalConfig {
                node_id: "fixture".into(),
                disk_directory: Some(self.0.clone()),
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn pwd(app: &Application) -> PathBuf {
        let Output::Disk(DiskResponse::WorkingDirectory(path)) = app
            .invoke(route("disk.pwd"), Command::Disk(DiskCommand::Pwd))
            .unwrap()
        else {
            panic!()
        };
        path
    }
    #[test]
    fn session_cd_is_isolated_and_failed_or_rejected_cd_preserves_it() {
        let f = Fixture::new();
        let ambient = std::env::current_dir().unwrap();
        let app = Application::bootstrap(f.config()).unwrap();
        let other = Application::bootstrap(f.config()).unwrap();
        assert_eq!(app.commands().unwrap().len(), 7);
        let start = pwd(&app);
        let cd = Command::Disk(DiskCommand::ChangeDirectory {
            path: "child with spaces".into(),
        });
        assert!(app.invoke(route("disk.pwd"), cd.clone()).is_err());
        assert_eq!(pwd(&app), start);
        app.invoke(route("disk.cd"), cd).unwrap();
        assert_eq!(pwd(&app), start.join("child with spaces"));
        assert_eq!(pwd(&other), start);
        assert!(app
            .invoke(
                route("disk.cd"),
                Command::Disk(DiskCommand::ChangeDirectory {
                    path: "missing".into()
                })
            )
            .is_err());
        assert_eq!(pwd(&app), start.join("child with spaces"));
        assert_eq!(std::env::current_dir().unwrap(), ambient);
    }
    #[test]
    fn selected_catalog_entries_match_actual_native_operations() {
        let f = Fixture::new();
        let app = Application::bootstrap(f.config()).unwrap();
        for c in app.commands().unwrap() {
            let command = match c.kind {
                CommandKind::Commands => Command::Commands,
                CommandKind::NodeInfo => Command::Node(NodeCommand::Info),
                CommandKind::NodeStatus => Command::Node(NodeCommand::Status),
                CommandKind::DiskPwd => Command::Disk(DiskCommand::Pwd),
                CommandKind::DiskList => Command::Disk(DiskCommand::List { path: ".".into() }),
                CommandKind::DiskCd => {
                    Command::Disk(DiskCommand::ChangeDirectory { path: ".".into() })
                }
                CommandKind::DiskPermissions => Command::Disk(DiskCommand::Permissions {
                    path: "item".into(),
                    follow_symlinks: false,
                }),
            };
            assert_eq!(command.kind(), c.kind);
            assert!(app.invoke(c.route, command).is_ok());
        }
    }
    #[test]
    fn cohesive_conflict_rejects_before_factory_creation() {
        let f = Fixture::new();
        let counts = Counts::new();
        let result = Application::bootstrap_with_node_registrar(f.config(), |_| {
            PreparedGroup::new(
                "conflicting.node",
                &["kernel.node.client", "kernel.disk.client"],
                &[],
                NodeFactories {
                    client: FakeFactory {
                        counts: counts.clone(),
                        id: "x".into(),
                        fail_create: false,
                        fail_call: false,
                    },
                },
            )
            .map_err(NodeConfigError::Registration)
        });
        assert!(matches!(result, Err(BootstrapError::Registration(_))));
        assert_eq!(counts.creates.get(), 0);
    }
    #[test]
    fn later_disk_creation_failure_drops_earlier_node_without_publishing_host() {
        let f = Fixture::new();
        let counts = Counts::new();
        let config = LocalConfig {
            disk_directory: Some(f.0.join("missing")),
            ..f.config()
        };
        let result = Application::bootstrap_with_node_registrar(config, |c| {
            fake_group(c, counts.clone(), false, false)
        });
        assert!(matches!(result, Err(BootstrapError::DiskFactory(_))));
        assert_eq!(counts.creates.get(), 1);
        assert_eq!(counts.drops.get(), 1);
        assert_eq!(counts.calls.get(), 0);
    }
}
