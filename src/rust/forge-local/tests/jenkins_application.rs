#![cfg(feature = "jenkins")]
use forge_ingress_contract::RouteKey;
use forge_io_client::IoClient;
use forge_jenkins::{ControllerInfo, Error, QueueReceipt, Request, Response};
use forge_local::{jenkins::JenkinsCommand, Application, Command, LocalConfig, Output};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

struct Selected {
    calls: Rc<RefCell<Vec<Request>>>,
    drops: Rc<Cell<usize>>,
    wrong_response: bool,
}
impl IoClient for Selected {
    type Request = Request;
    type Response = Response;
    type Error = Error;
    fn execute(&self, request: Request) -> Result<Response, Error> {
        self.calls.borrow_mut().push(request.clone());
        Ok(match request {
            Request::Jobs if !self.wrong_response => Response::Jobs(vec![]),
            Request::Build { .. } => Response::Queued(QueueReceipt { id: 17 }),
            Request::CancelQueue { .. } => Response::CancelRequested,
            _ => Response::Controller(ControllerInfo {
                version: Some("selected-instance".into()),
                use_security: true,
                num_executors: 0,
            }),
        })
    }
}
impl Drop for Selected {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}
fn route(name: &str) -> RouteKey {
    RouteKey::new(name, 1).unwrap()
}

#[test]
fn selected_shared_port_runs_only_after_admitted_matching_validated_command() {
    let calls = Rc::new(RefCell::new(vec![]));
    let drops = Rc::new(Cell::new(0));
    let app = Application::bootstrap_with_jenkins(
        LocalConfig::default(),
        Selected {
            calls: calls.clone(),
            drops: drops.clone(),
            wrong_response: false,
        },
    )
    .unwrap();
    let commands = app.commands().unwrap();
    assert_eq!(
        commands
            .iter()
            .filter(|c| c.route.destination().starts_with("jenkins."))
            .count(),
        6
    );
    assert!(calls.borrow().is_empty());
    assert!(app
        .invoke(
            route("jenkins.jobs"),
            Command::Jenkins(JenkinsCommand::Status)
        )
        .is_err());
    assert!(app
        .invoke(
            RouteKey::new("jenkins.status", 2).unwrap(),
            Command::Jenkins(JenkinsCommand::Status)
        )
        .is_err());
    assert!(app
        .invoke(
            route("jenkins.missing"),
            Command::Jenkins(JenkinsCommand::Status)
        )
        .is_err());
    assert!(calls.borrow().is_empty());
    for (name, command) in [
        ("jenkins.queue", JenkinsCommand::Queue { id: 0 }),
        ("jenkins.cancel", JenkinsCommand::Cancel { id: 0 }),
        (
            "jenkins.build-status",
            JenkinsCommand::BuildStatus {
                job: "demo".into(),
                number: 0,
            },
        ),
        (
            "jenkins.build",
            JenkinsCommand::Build {
                job: "../outside".into(),
                parameters: BTreeMap::new(),
            },
        ),
    ] {
        assert!(app.invoke(route(name), Command::Jenkins(command)).is_err());
    }
    assert!(calls.borrow().is_empty());
    assert!(
        matches!(app.invoke(route("jenkins.status"), Command::Jenkins(JenkinsCommand::Status)).unwrap(),
        Output::Jenkins(Response::Controller(ControllerInfo { version: Some(ref value), .. })) if value=="selected-instance")
    );
    assert_eq!(
        app.invoke(
            route("jenkins.jobs"),
            Command::Jenkins(JenkinsCommand::Jobs)
        )
        .unwrap(),
        Output::Jenkins(Response::Jobs(vec![]))
    );
    let parameters = BTreeMap::from([("TARGET".to_owned(), "release candidate".to_owned())]);
    assert_eq!(
        app.invoke(
            route("jenkins.build"),
            Command::Jenkins(JenkinsCommand::Build {
                job: "team/pipeline".into(),
                parameters: parameters.clone(),
            })
        )
        .unwrap(),
        Output::Jenkins(Response::Queued(QueueReceipt { id: 17 }))
    );
    assert_eq!(
        &*calls.borrow(),
        &[
            Request::Controller,
            Request::Jobs,
            Request::Build {
                job: "team/pipeline".into(),
                parameters
            },
        ]
    );
    assert_eq!(drops.get(), 0);
    drop(app);
    assert_eq!(
        drops.get(),
        1,
        "all bindings shared one host-owned selected port"
    );
}

#[test]
fn unexpected_response_and_preserved_native_error_are_explicit() {
    let app = Application::bootstrap_with_jenkins(
        LocalConfig::default(),
        Selected {
            calls: Rc::new(RefCell::new(vec![])),
            drops: Rc::new(Cell::new(0)),
            wrong_response: true,
        },
    )
    .unwrap();
    let error = app
        .invoke(
            route("jenkins.jobs"),
            Command::Jenkins(JenkinsCommand::Jobs),
        )
        .unwrap_err();
    assert!(error.to_string().contains("unexpected response"));
    struct Denied;
    impl IoClient for Denied {
        type Request = Request;
        type Response = Response;
        type Error = Error;
        fn execute(&self, _: Request) -> Result<Response, Error> {
            Err(Error::Unauthorized)
        }
    }
    let app = Application::bootstrap_with_jenkins(LocalConfig::default(), Denied).unwrap();
    let error = app
        .invoke(
            route("jenkins.status"),
            Command::Jenkins(JenkinsCommand::Status),
        )
        .unwrap_err();
    assert!(error.to_string().contains("denied"));
}

#[test]
fn ordinary_application_leaves_jenkins_unbound() {
    let app = Application::bootstrap(LocalConfig::default()).unwrap();
    assert!(!app
        .commands()
        .unwrap()
        .iter()
        .any(|c| c.route.destination().starts_with("jenkins.")));
}
