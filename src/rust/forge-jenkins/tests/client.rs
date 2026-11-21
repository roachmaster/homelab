use forge_bootstrap::Registry;
use forge_io_client::IoClient;
use forge_jenkins::{
    register, Client, Config, Credentials, Error, HttpRequest, HttpResponse, Method, Request,
    Response, TransportError,
};
use forge_provider::Factory;
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    time::Duration,
};

#[derive(Clone)]
struct Fake {
    calls: Rc<RefCell<Vec<HttpRequest>>>,
    replies: Rc<RefCell<VecDeque<Result<HttpResponse, TransportError>>>>,
}
impl IoClient for Fake {
    type Request = HttpRequest;
    type Response = HttpResponse;
    type Error = TransportError;
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.calls.borrow_mut().push(request);
        self.replies
            .borrow_mut()
            .pop_front()
            .expect("configured fixture")
    }
}
fn reply(
    status: u16,
    body: &str,
    headers: &[(&str, &str)],
) -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse {
        status,
        body: body.as_bytes().to_vec(),
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    })
}
fn config() -> Config {
    Config::new("http://127.0.0.1:18084/jenkins/").unwrap()
}
fn configured(
    config: Config,
    replies: Vec<Result<HttpResponse, TransportError>>,
) -> (Client<Fake>, Rc<RefCell<Vec<HttpRequest>>>) {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let fake = Fake {
        calls: calls.clone(),
        replies: Rc::new(RefCell::new(replies.into())),
    };
    let ((), factory) = Registry::new()
        .register(
            register(
                config,
                Credentials::new("operator", "fixture-token").unwrap(),
                move || Ok(fake.clone()),
            )
            .unwrap(),
        )
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    (factory.create().unwrap(), calls)
}

#[test]
fn typed_reads_parse_identity_fields_and_preserve_absence() {
    let (client, calls) = configured(
        config(),
        vec![
            reply(
                200,
                r#"{"useSecurity":true,"numExecutors":0}"#,
                &[("x-jenkins", "fixture-version")],
            ),
            reply(200, r#"{"jobs":[{"name":"folder","color":null}]}"#, &[]),
            reply(200, r#"{"id":7,"why":"waiting"}"#, &[]),
            reply(
                200,
                r#"{"number":9,"building":false,"result":"SUCCESS","duration":123,"queueId":-1}"#,
                &[],
            ),
        ],
    );
    let Response::Controller(info) = client.execute(Request::Controller).unwrap() else {
        panic!()
    };
    assert!(info.use_security);
    assert_eq!(info.num_executors, 0);
    assert_eq!(info.version.as_deref(), Some("fixture-version"));
    let Response::Jobs(jobs) = client.execute(Request::Jobs).unwrap() else {
        panic!()
    };
    assert_eq!(jobs[0].name, "folder");
    let Response::Queue(queue) = client.execute(Request::Queue { id: 7 }).unwrap() else {
        panic!()
    };
    assert_eq!(queue.cancelled, None);
    assert_eq!(queue.executable, None);
    let Response::Build(build) = client
        .execute(Request::BuildStatus {
            job: "folder/suite".into(),
            number: 9,
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(build.queue_id, Some(-1));
    assert_eq!(calls.borrow().len(), 4);
    assert!(calls.borrow()[3]
        .url
        .ends_with("/job/folder/job/suite/9/api/json"));
}

#[test]
fn mutation_policy_and_preparation_reject_before_transport() {
    let (client, calls) = configured(config(), vec![]);
    assert_eq!(
        client.execute(Request::Build {
            job: "suite".into(),
            parameters: BTreeMap::new()
        }),
        Err(Error::MutationDenied)
    );
    assert_eq!(
        client.execute(Request::CancelQueue { id: 1 }),
        Err(Error::MutationDenied)
    );
    for job in [
        "",
        ".",
        "..",
        "folder/../suite",
        "folder//suite",
        "/suite",
        "suite/",
        "line\nbreak",
        "a\\b",
    ] {
        assert_eq!(
            client.execute(Request::BuildStatus {
                job: job.into(),
                number: 1
            }),
            Err(Error::InvalidRequest)
        );
    }
    assert_eq!(
        client.execute(Request::Queue { id: 0 }),
        Err(Error::InvalidRequest)
    );
    assert_eq!(
        client.execute(Request::BuildStatus {
            job: "suite".into(),
            number: 0
        }),
        Err(Error::InvalidRequest)
    );
    assert!(calls.borrow().is_empty());
}

#[test]
fn folder_segments_and_form_are_encoded_without_changing_meaning() {
    let (client, calls) = configured(
        config().with_mutations(true),
        vec![reply(201, "", &[("location", "/jenkins/queue/item/7/")])],
    );
    let result = client
        .execute(Request::Build {
            job: "folder/space & café?".into(),
            parameters: BTreeMap::from([("NAME".into(), "a b&x=1\nnext".into())]),
        })
        .unwrap();
    assert_eq!(
        result,
        Response::Queued(forge_jenkins::QueueReceipt { id: 7 })
    );
    let calls = calls.borrow();
    let wire = &calls[0];
    assert_eq!(wire.method, Method::Post);
    assert_eq!(wire.url,"http://127.0.0.1:18084/jenkins/job/folder/job/space%20%26%20caf%C3%A9%3F/buildWithParameters");
    assert_eq!(
        String::from_utf8(wire.body.clone()).unwrap(),
        "NAME=a+b%26x%3D1%0Anext"
    );
    assert_eq!(
        wire.authorization(),
        "Basic b3BlcmF0b3I6Zml4dHVyZS10b2tlbg=="
    );
    let debug = format!("{wire:?}");
    assert!(!debug.contains("b3BlcmF0b3I"));
    assert!(!debug.contains("a b&x"));
}

#[test]
fn queue_receipt_is_exact_origin_context_and_canonical_positive_id() {
    for location in [
        "https://other.example/jenkins/queue/item/7/",
        "//other.example/jenkins/queue/item/7/",
        "/queue/item/7/",
        "/jenkins/queue/item/0/",
        "/jenkins/queue/item/07/",
        "/jenkins/queue/item/7/?token=x",
        "/jenkins/queue/item/7/#x",
        "http://user:pass@127.0.0.1:18084/jenkins/queue/item/7/",
    ] {
        let (client, calls) = configured(
            config().with_mutations(true),
            vec![reply(201, "", &[("location", location)])],
        );
        assert_eq!(
            client.execute(Request::Build {
                job: "suite".into(),
                parameters: BTreeMap::new()
            }),
            Err(Error::MutationUncertain)
        );
        assert_eq!(calls.borrow().len(), 1, "no retry or follow");
    }
}

#[test]
fn cancel_acceptance_is_a_request_and_login_redirect_is_not_success() {
    let (client, _) = configured(
        config().with_mutations(true),
        vec![reply(302, "", &[("location", "/jenkins/queue/")])],
    );
    assert_eq!(
        client.execute(Request::CancelQueue { id: 7 }),
        Ok(Response::CancelRequested)
    );
    for location in [
        "/login",
        "https://other.example/queue/",
        "/jenkins/queue/?redirect=x",
    ] {
        let (client, _) = configured(
            config().with_mutations(true),
            vec![reply(302, "", &[("location", location)])],
        );
        assert_eq!(
            client.execute(Request::CancelQueue { id: 7 }),
            Err(Error::MutationUncertain)
        );
    }
}

#[test]
fn failures_and_malformed_success_preserve_write_uncertainty_without_retries() {
    for (response, expected) in [
        (Err(TransportError::Timeout), Error::Timeout),
        (Err(TransportError::Unavailable), Error::Transport),
        (reply(403, "private body", &[]), Error::Unauthorized),
        (reply(404, "", &[]), Error::NotFound),
        (reply(200, "not json", &[]), Error::InvalidResponse),
    ] {
        let (client, calls) = configured(config(), vec![response]);
        assert_eq!(client.execute(Request::Controller), Err(expected));
        assert_eq!(calls.borrow().len(), 1);
    }
    for response in [
        Err(TransportError::Timeout),
        reply(503, "", &[]),
        reply(201, "", &[]),
    ] {
        let (client, calls) = configured(config().with_mutations(true), vec![response]);
        assert_eq!(
            client.execute(Request::Build {
                job: "suite".into(),
                parameters: BTreeMap::new()
            }),
            Err(Error::MutationUncertain)
        );
        assert_eq!(calls.borrow().len(), 1);
    }
    let (client, _) = configured(
        config().with_limits(Duration::from_secs(1), 256).unwrap(),
        vec![reply(200, &"x".repeat(257), &[])],
    );
    assert_eq!(client.execute(Request::Controller), Err(Error::BodyLimit));
}

#[test]
fn wrong_queue_or_build_identity_is_not_accepted() {
    let (client, _) = configured(
        config(),
        vec![
            reply(200, r#"{"id":8}"#, &[]),
            reply(
                200,
                r#"{"number":10,"building":true,"result":null,"duration":0}"#,
                &[],
            ),
        ],
    );
    assert_eq!(
        client.execute(Request::Queue { id: 7 }),
        Err(Error::InvalidResponse)
    );
    assert_eq!(
        client.execute(Request::BuildStatus {
            job: "suite".into(),
            number: 9
        }),
        Err(Error::InvalidResponse)
    );
}

#[test]
fn configuration_is_bounded_and_secrets_debug_redacted() {
    for endpoint in [
        "http://example.com/",
        "http://localhost:18084/",
        "https://user:secret@example.com/",
        "https://example.com/?token=x",
        "https://example.com/#x",
        "https://example.com/a/../",
        "https://example.com/%2e%2e/",
        "file:///tmp/jenkins",
        "https://example.com/a\\b",
    ] {
        assert!(Config::new(endpoint).is_err(), "{endpoint}");
    }
    assert!(Config::new("https://jenkins.example/context/").is_ok());
    assert!(Config::new("http://[::1]:18084/").is_ok());
    assert!(config().with_limits(Duration::ZERO, 1024).is_err());
    assert!(config()
        .with_limits(Duration::from_secs(1), 1024 * 1024 + 1)
        .is_err());
    for (user, token) in [("bad:user", "token"), ("user", "token\n"), ("user", "")] {
        assert!(Credentials::new(user, token).is_err());
    }
    assert_eq!(
        format!("{:?}", Credentials::new("user", "fixture-token").unwrap()),
        "Credentials(<redacted>)"
    );
}

#[test]
fn registrar_preserves_dependency_defers_creation_and_rejects_duplicate_without_replacing() {
    let creates = Rc::new(Cell::new(0));
    let calls = Rc::new(RefCell::new(Vec::new()));
    let fake = Fake {
        calls: calls.clone(),
        replies: Rc::new(RefCell::new(vec![reply(200, r#"{"jobs":[]}"#, &[])].into())),
    };
    let observed = creates.clone();
    let original = Registry::new()
        .register(
            register(
                config(),
                Credentials::new("user", "fixture-token").unwrap(),
                move || {
                    observed.set(observed.get() + 1);
                    Ok(fake.clone())
                },
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(creates.get(), 0);
    let duplicate = register(
        config(),
        Credentials::new("user", "fixture-token").unwrap(),
        || Err::<Fake, _>(Error::Transport),
    )
    .unwrap();
    let rejected = original.register(duplicate).unwrap_err();
    let (original, _, _) = rejected.into_parts();
    let ((), factory) = original.freeze().unwrap().into_factories();
    assert_eq!(creates.get(), 0);
    let client = factory.create().unwrap();
    assert_eq!(creates.get(), 1);
    assert_eq!(client.execute(Request::Jobs), Ok(Response::Jobs(vec![])));
    assert_eq!(calls.borrow().len(), 1);
}
