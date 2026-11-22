use forge_core::bootstrap::Registry;
use forge_core::io_client::IoClient;
use forge_core::provider::Factory;
use forge_jenkins::{
    register, Client, Config, Credentials, Error, HttpRequest, HttpResponse, Method, Request,
    Response, TransportError,
};
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
        reply(200, "", &[]),
        reply(202, "", &[]),
        reply(204, "", &[]),
        reply(302, "", &[]),
        reply(400, "", &[]),
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
        "https://example.com/a/..",
        "https://example.com/a/.",
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

#[test]
fn reconciliation_lists_encode_bounded_tree_and_only_project_selected_public_parameters() {
    let config = config()
        .with_observed_parameters(["FORGE_ACTION_ID", "PASSWORD"])
        .unwrap();
    let (client, calls) = configured(
        config,
        vec![
            reply(
                200,
                r#"{"items":[{"id":71,"task":{"name":"space & café?","url":"http://127.0.0.1:18084/jenkins/job/folder/job/space%20%26%20caf%C3%A9%3F/"},"actions":[{}, {"parameters":[{"_class":"hudson.model.StringParameterValue","name":"FORGE_ACTION_ID","value":"action-1"},{"_class":"hudson.model.PasswordParameterValue","name":"PASSWORD","value":"hidden-password"},{"name":"TOKEN","value":"private-token"},{"name":"OTHER","value":{"arbitrary":"ignored"}}]}],"executable":{"number":9}}]}"#,
                &[],
            ),
            reply(
                200,
                r#"{"builds":[{"number":9,"queueId":71,"building":false,"result":"SUCCESS","actions":[{"parameters":[{"name":"FORGE_ACTION_ID","value":"action-1"},{"name":"TOKEN","value":"private-token"}]}]},{"number":8,"queueId":-1,"building":true,"result":null,"actions":[]}] }"#,
                &[],
            ),
        ],
    );
    let Response::QueueList(list) = client.execute(Request::QueueList).unwrap() else {
        panic!()
    };
    assert_eq!(list.items[0].job, "folder/space & café?");
    assert_eq!(
        list.items[0].parameters,
        BTreeMap::from([("FORGE_ACTION_ID".into(), "action-1".into())])
    );
    assert_eq!(list.items[0].executable.as_ref().unwrap().number, 9);
    assert!(!list.truncated);
    assert!(!format!("{list:?}").contains("private-token"));
    assert!(!serde_json::to_string(&list)
        .unwrap()
        .contains("hidden-password"));
    let Response::RecentBuilds(builds) = client
        .execute(Request::RecentBuilds {
            job: "folder/space & café?".into(),
            limit: 1,
        })
        .unwrap()
    else {
        panic!()
    };
    assert!(builds.truncated);
    assert_eq!(builds.builds.len(), 1);
    assert_eq!(builds.builds[0].queue_id, Some(71));
    assert_eq!(builds.builds[0].parameters, list.items[0].parameters);
    let calls = calls.borrow();
    assert!(calls.iter().all(|call| call.method == Method::Get));
    let queue_url = url::Url::parse(&calls[0].url).unwrap();
    assert_eq!(queue_url.path(), "/jenkins/queue/api/json");
    assert_eq!(queue_url.query_pairs().collect::<Vec<_>>(), vec![("tree".into(), "items[id,task[name,url],actions[parameters[_class,name,value]],executable[number]]{0,51}".into())]);
    let build_url = url::Url::parse(&calls[1].url).unwrap();
    assert_eq!(
        build_url.path(),
        "/jenkins/job/folder/job/space%20%26%20caf%C3%A9%3F/api/json"
    );
    assert_eq!(
        build_url.query_pairs().next().unwrap().1,
        "builds[number,queueId,building,result,actions[parameters[_class,name,value]]]{0,2}"
    );
}

#[test]
fn parameter_projection_defaults_empty_and_rejects_ambiguous_or_oversized_selected_data() {
    let body = r#"{"builds":[{"number":1,"building":false,"actions":[{"parameters":[{"name":"TOKEN","value":"never-observed"}]}]}]}"#;
    let (client, _) = configured(config(), vec![reply(200, body, &[])]);
    let Response::RecentBuilds(list) = client
        .execute(Request::RecentBuilds {
            job: "suite".into(),
            limit: 50,
        })
        .unwrap()
    else {
        panic!()
    };
    assert!(list.builds[0].parameters.is_empty());
    assert_eq!(list.builds[0].queue_id, None);
    for parameters in [
        serde_json::json!([{"name":"PUBLIC","value":9}]),
        serde_json::json!([{"name":"PUBLIC","value":"a"},{"name":"PUBLIC","value":"b"}]),
        serde_json::json!([{"name":"PUBLIC","value":"x".repeat(4097)}]),
        serde_json::json!([{"name":"PUBLIC","value":"unsafe\nline"}]),
        serde_json::json!((0..65)
            .map(|i| serde_json::json!({"name": format!("NAME{i}"),"value":"a"}))
            .collect::<Vec<_>>()),
    ] {
        let body = serde_json::json!({"builds":[{"number":1,"building":false,"actions":[{"parameters":parameters}]}]}).to_string();
        let (client, _) = configured(
            config().with_observed_parameters(["PUBLIC"]).unwrap(),
            vec![reply(200, &body, &[])],
        );
        assert_eq!(
            client.execute(Request::RecentBuilds {
                job: "suite".into(),
                limit: 50
            }),
            Err(Error::InvalidResponse)
        );
    }
    for names in [
        vec![""],
        vec!["PUBLIC", "PUBLIC"],
        vec!["bad\nname"],
        vec!["bad-name"],
    ] {
        assert!(config().with_observed_parameters(names).is_err());
    }
}

#[test]
fn queue_identity_context_and_snapshot_limits_are_enforced() {
    for input in [
        "https://other.example/jenkins/job/suite/",
        "http://127.0.0.1:18084/job/suite/",
        "http://user:pass@127.0.0.1:18084/jenkins/job/suite/",
        "http://127.0.0.1:18084/jenkins/job/suite/?token=x",
        "http://127.0.0.1:18084/jenkins/job/one%2Ftwo/",
        "http://127.0.0.1:18084/jenkins/queue/item/1/",
    ] {
        let body = serde_json::json!({"items":[{"id":1,"task":{"name":"suite","url":input},"actions":[]}]}).to_string();
        let (client, _) = configured(config(), vec![reply(200, &body, &[])]);
        assert_eq!(
            client.execute(Request::QueueList),
            Err(Error::InvalidResponse)
        );
    }
    for count in [50, 51, 52] {
        let items: Vec<_> = (1..=count).map(|id| serde_json::json!({"id":id,"task":{"name":"suite","url":"http://127.0.0.1:18084/jenkins/job/suite/"},"actions":[]})).collect();
        let body = serde_json::json!({"items":items}).to_string();
        let (client, _) = configured(config(), vec![reply(200, &body, &[])]);
        let result = client.execute(Request::QueueList);
        if count == 52 {
            assert_eq!(result, Err(Error::InvalidResponse));
        } else {
            let Response::QueueList(list) = result.unwrap() else {
                panic!()
            };
            assert_eq!(list.items.len(), 50);
            assert_eq!(list.truncated, count == 51);
        }
    }
    let (client, calls) = configured(config(), vec![]);
    for limit in [0, 51, 255] {
        assert_eq!(
            client.execute(Request::RecentBuilds {
                job: "suite".into(),
                limit
            }),
            Err(Error::InvalidRequest)
        );
    }
    assert!(calls.borrow().is_empty());
}

#[test]
fn stop_build_requires_mutation_policy_and_never_claims_execution_stopped_or_retries() {
    let request = || Request::StopBuild {
        job: "team/suite".into(),
        number: 8,
    };
    let (client, calls) = configured(config(), vec![]);
    assert_eq!(client.execute(request()), Err(Error::MutationDenied));
    assert!(calls.borrow().is_empty());
    for status in [200, 201, 202, 204, 302] {
        let (client, calls) = configured(
            config().with_mutations(true),
            vec![reply(
                status,
                "",
                &[("location", "/jenkins/job/team/job/suite/8/")],
            )],
        );
        assert_eq!(client.execute(request()), Ok(Response::StopRequested));
        assert_eq!(calls.borrow().len(), 1);
        assert_eq!(calls.borrow()[0].method, Method::Post);
        assert!(calls.borrow()[0]
            .url
            .ends_with("/job/team/job/suite/8/stop"));
    }
    for response in [
        Err(TransportError::Timeout),
        Err(TransportError::Unavailable),
        reply(503, "internal-secret", &[]),
        reply(302, "", &[("location", "/login")]),
        reply(
            302,
            "",
            &[("location", "https://other.example/job/team/job/suite/8/")],
        ),
        reply(302, "", &[("location", "/jenkins/job/team/job/suite/9/")]),
        reply(
            302,
            "",
            &[("location", "/jenkins/job/team/job/suite/8/?secret=x")],
        ),
    ] {
        let (client, calls) = configured(config().with_mutations(true), vec![response]);
        let error = client.execute(request()).unwrap_err();
        assert_eq!(error, Error::MutationUncertain);
        assert!(!format!("{error:?}").contains("secret"));
        assert_eq!(calls.borrow().len(), 1);
    }
}

#[test]
fn error_display_names_each_failure_exactly_without_secrets() {
    use forge_core::bootstrap::RegistrationError;
    let cases = [
        (Error::Configuration, "invalid Jenkins configuration"),
        (Error::InvalidRequest, "invalid Jenkins request"),
        (
            Error::MutationDenied,
            "Jenkins mutations are disabled for this client",
        ),
        (
            Error::Unauthorized,
            "Jenkins denied authentication or permission",
        ),
        (Error::NotFound, "Jenkins resource was not found"),
        (
            Error::UnexpectedStatus(302),
            "unexpected Jenkins HTTP status 302",
        ),
        (Error::InvalidResponse, "invalid Jenkins response"),
        (Error::BodyLimit, "Jenkins response exceeded its byte limit"),
        (Error::Timeout, "Jenkins request deadline expired"),
        (Error::Transport, "Jenkins transport failed"),
        (
            Error::MutationUncertain,
            "Jenkins write outcome is uncertain; reconcile before retrying",
        ),
        (
            Error::Registration(RegistrationError::DuplicateGroup("g".into())),
            "factory group g is already registered",
        ),
    ];
    for (error, message) in cases {
        assert_eq!(error.to_string(), message);
        assert!(!error.to_string().contains("fixture-token"));
    }
}

#[test]
fn request_validate_checks_preparation_without_transport() {
    for request in [
        Request::Controller,
        Request::Jobs,
        Request::QueueList,
        Request::RecentBuilds {
            job: "suite".into(),
            limit: 50,
        },
        Request::StopBuild {
            job: "suite".into(),
            number: 1,
        },
        Request::Build {
            job: "suite".into(),
            parameters: BTreeMap::new(),
        },
        Request::Queue { id: 1 },
        Request::BuildStatus {
            job: "suite".into(),
            number: 1,
        },
        Request::CancelQueue { id: 1 },
    ] {
        assert_eq!(request.validate(), Ok(()));
    }
    for request in [
        Request::RecentBuilds {
            job: "suite".into(),
            limit: 0,
        },
        Request::RecentBuilds {
            job: "suite".into(),
            limit: 51,
        },
        Request::StopBuild {
            job: "suite".into(),
            number: 0,
        },
        Request::Queue { id: 0 },
        Request::BuildStatus {
            job: "".into(),
            number: 1,
        },
        Request::BuildStatus {
            job: "suite".into(),
            number: 0,
        },
        Request::CancelQueue { id: 0 },
        Request::Build {
            job: "suite".into(),
            parameters: BTreeMap::from([("".into(), "v".into())]),
        },
    ] {
        assert_eq!(request.validate(), Err(Error::InvalidRequest));
    }
}

#[test]
fn identical_request_sequences_produce_identical_wire_vectors_and_responses() {
    fn run() -> (Vec<(Method, String)>, Vec<Response>) {
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
        let responses = vec![
            client.execute(Request::Controller).unwrap(),
            client.execute(Request::Jobs).unwrap(),
            client.execute(Request::Queue { id: 7 }).unwrap(),
            client
                .execute(Request::BuildStatus {
                    job: "folder/suite".into(),
                    number: 9,
                })
                .unwrap(),
        ];
        let wire = calls
            .borrow()
            .iter()
            .map(|call| (call.method, call.url.clone()))
            .collect();
        (wire, responses)
    }
    let first = run();
    let second = run();
    assert_eq!(first, second);
    assert_eq!(
        first
            .0
            .iter()
            .map(|(method, url)| (*method, url.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (
                Method::Get,
                "http://127.0.0.1:18084/jenkins/api/json?tree=useSecurity,numExecutors"
            ),
            (
                Method::Get,
                "http://127.0.0.1:18084/jenkins/api/json?tree=jobs[name,color]"
            ),
            (
                Method::Get,
                "http://127.0.0.1:18084/jenkins/queue/item/7/api/json"
            ),
            (
                Method::Get,
                "http://127.0.0.1:18084/jenkins/job/folder/job/suite/9/api/json"
            ),
        ]
    );
}
