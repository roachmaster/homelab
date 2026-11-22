use forge_core::access::{
    AuthenticatedBoundary, AuthenticationError, Context, Grant, GrantError, IdentityFacts,
    RoutePolicy, Verifier,
};
use forge_core::ingress::{AdmissionDenied, AdmissionPolicy, Header, RouteKey};
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

struct FixedVerifier {
    result: Result<(String, Instant), &'static str>,
}

impl Verifier<&str> for FixedVerifier {
    type Error = &'static str;
    fn verify(&self, _evidence: &str) -> Result<IdentityFacts, &'static str> {
        match &self.result {
            Ok((subject, valid_until)) => Ok(IdentityFacts {
                subject: subject.clone(),
                valid_until: *valid_until,
            }),
            Err(error) => Err(*error),
        }
    }
}

struct RecordingVerifier {
    evidence: Rc<RefCell<Vec<String>>>,
    valid_until: Instant,
}

impl Verifier<String> for RecordingVerifier {
    type Error = &'static str;
    fn verify(&self, evidence: String) -> Result<IdentityFacts, &'static str> {
        self.evidence.borrow_mut().push(evidence);
        Ok(IdentityFacts {
            subject: "alice".into(),
            valid_until: self.valid_until,
        })
    }
}

fn route(name: &str) -> RouteKey {
    RouteKey::new(name, 1).unwrap()
}

fn header(name: &str) -> Header {
    Header::new(route(name), "r1").unwrap()
}

fn context(boundary: &AuthenticatedBoundary<FixedVerifier>) -> Context {
    boundary
        .authenticate("token", Duration::from_secs(60))
        .unwrap()
}

#[test]
fn verifier_records_the_exact_evidence_in_order() {
    let evidence = Rc::new(RefCell::new(Vec::new()));
    let verifier = RecordingVerifier {
        evidence: evidence.clone(),
        valid_until: Instant::now() + Duration::from_secs(60),
    };
    let boundary = AuthenticatedBoundary::new(verifier, Duration::from_secs(5));
    boundary
        .authenticate("token-a".to_string(), Duration::from_secs(60))
        .unwrap();
    boundary
        .authenticate("token-b".to_string(), Duration::from_secs(60))
        .unwrap();
    assert_eq!(
        *evidence.borrow(),
        vec!["token-a".to_string(), "token-b".to_string()]
    );
}

#[test]
fn authenticate_issues_context_bounded_by_identity_expiry() {
    let valid_until = Instant::now() + Duration::from_secs(10);
    let boundary = AuthenticatedBoundary::new(
        FixedVerifier {
            result: Ok(("alice".into(), valid_until)),
        },
        Duration::from_secs(30),
    );
    let context = context(&boundary);
    assert_eq!(context.subject(), "alice");
    assert_eq!(context.deadline(), valid_until);
}

#[test]
fn authenticate_rejects_invalid_identity_verification_failure_and_expiry() {
    let boundary = AuthenticatedBoundary::new(
        FixedVerifier {
            result: Ok((
                "bad subject".into(),
                Instant::now() + Duration::from_secs(60),
            )),
        },
        Duration::from_secs(5),
    );
    assert!(matches!(
        boundary.authenticate("token", Duration::from_secs(60)),
        Err(AuthenticationError::InvalidIdentity)
    ));
    let boundary = AuthenticatedBoundary::new(
        FixedVerifier {
            result: Err("verifier exploded"),
        },
        Duration::from_secs(5),
    );
    assert!(matches!(
        boundary.authenticate("token", Duration::from_secs(60)),
        Err(AuthenticationError::Verification("verifier exploded"))
    ));
    let boundary = AuthenticatedBoundary::new(
        FixedVerifier {
            result: Ok(("alice".into(), Instant::now() - Duration::from_secs(1))),
        },
        Duration::from_secs(5),
    );
    assert!(matches!(
        boundary.authenticate("token", Duration::from_secs(60)),
        Err(AuthenticationError::Expired)
    ));
    let boundary = AuthenticatedBoundary::new(
        FixedVerifier {
            result: Ok(("alice".into(), Instant::now() + Duration::from_secs(60))),
        },
        Duration::from_secs(5),
    );
    assert!(matches!(
        boundary.authenticate("token", Duration::from_secs(0)),
        Err(AuthenticationError::Expired)
    ));
}

#[test]
fn route_policy_rejects_invalid_subject_and_duplicate_routes() {
    assert!(matches!(
        RoutePolicy::new([Grant {
            subject: "bad subject".into(),
            route: route("jobs.submit"),
            discoverable: true,
        }]),
        Err(GrantError::InvalidIdentity)
    ));
    assert!(matches!(
        RoutePolicy::new([
            Grant {
                subject: "alice".into(),
                route: route("jobs.submit"),
                discoverable: true,
            },
            Grant {
                subject: "alice".into(),
                route: route("jobs.submit"),
                discoverable: false,
            },
        ]),
        Err(GrantError::DuplicateRoute)
    ));
    assert!(RoutePolicy::new([
        Grant {
            subject: "alice".into(),
            route: route("jobs.submit"),
            discoverable: true,
        },
        Grant {
            subject: "bob".into(),
            route: route("jobs.submit"),
            discoverable: false,
        },
    ])
    .is_ok());
}

#[test]
fn route_policy_admits_only_granted_routes_for_the_context_subject() {
    let policy = RoutePolicy::new([
        Grant {
            subject: "alice".into(),
            route: route("jobs.submit"),
            discoverable: true,
        },
        Grant {
            subject: "alice".into(),
            route: route("jobs.secret"),
            discoverable: false,
        },
    ])
    .unwrap();
    let boundary = AuthenticatedBoundary::new(
        FixedVerifier {
            result: Ok(("alice".into(), Instant::now() + Duration::from_secs(60))),
        },
        Duration::from_secs(5),
    );
    let alice = context(&boundary);
    let boundary = AuthenticatedBoundary::new(
        FixedVerifier {
            result: Ok(("bob".into(), Instant::now() + Duration::from_secs(60))),
        },
        Duration::from_secs(5),
    );
    let bob = context(&boundary);
    assert_eq!(policy.admit(&header("jobs.submit"), &alice), Ok(()));
    assert_eq!(policy.admit(&header("jobs.secret"), &alice), Ok(()));
    assert_eq!(
        policy.admit(&header("jobs.submit"), &bob),
        Err(AdmissionDenied)
    );
    assert_eq!(
        policy.admit(&header("jobs.other"), &alice),
        Err(AdmissionDenied)
    );
    assert!(policy.visible(&route("jobs.submit"), &alice));
    assert!(!policy.visible(&route("jobs.secret"), &alice));
    assert!(!policy.visible(&route("jobs.submit"), &bob));
    assert!(!policy.visible(&route("jobs.other"), &alice));
}

#[test]
fn expired_context_admits_nothing_and_is_not_visible() {
    let policy = RoutePolicy::new([Grant {
        subject: "alice".into(),
        route: route("jobs.submit"),
        discoverable: true,
    }])
    .unwrap();
    let boundary = AuthenticatedBoundary::new(
        FixedVerifier {
            result: Ok(("alice".into(), Instant::now() + Duration::from_millis(1))),
        },
        Duration::from_secs(5),
    );
    let context = context(&boundary);
    assert_eq!(policy.admit(&header("jobs.submit"), &context), Ok(()));
    std::thread::sleep(Duration::from_millis(10));
    assert_eq!(
        policy.admit(&header("jobs.submit"), &context),
        Err(AdmissionDenied)
    );
    assert!(!policy.visible(&route("jobs.submit"), &context));
}

#[test]
fn identical_policies_admit_identically() {
    fn build() -> RoutePolicy {
        RoutePolicy::new([Grant {
            subject: "alice".into(),
            route: route("jobs.submit"),
            discoverable: true,
        }])
        .unwrap()
    }
    let boundary = AuthenticatedBoundary::new(
        FixedVerifier {
            result: Ok(("alice".into(), Instant::now() + Duration::from_secs(60))),
        },
        Duration::from_secs(5),
    );
    let context = context(&boundary);
    let first = build();
    let second = build();
    assert_eq!(
        first.admit(&header("jobs.submit"), &context),
        second.admit(&header("jobs.submit"), &context)
    );
    assert_eq!(
        first.visible(&route("jobs.submit"), &context),
        second.visible(&route("jobs.submit"), &context)
    );
}
