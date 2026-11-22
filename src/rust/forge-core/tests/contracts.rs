use forge_core::ingress::{
    AdmissionDenied, AdmissionPolicy, Envelope, Header, HeaderError, Payload, ResponseHeader,
    RouteKey, MAX_TOKEN_BYTES,
};
use std::cell::RefCell;

#[test]
fn reject_invalid_metadata_without_silent_normalization() {
    for destination in ["", "jobs submit", " jobs.submit", "jobs.submit\n"] {
        assert_eq!(
            RouteKey::new(destination, 1),
            Err(HeaderError::InvalidDestination)
        );
    }
    assert_eq!(
        RouteKey::new("x".repeat(MAX_TOKEN_BYTES + 1), 1),
        Err(HeaderError::InvalidDestination)
    );
    assert_eq!(
        RouteKey::new("jobs.submit", 0),
        Err(HeaderError::InvalidVersion)
    );
    let route = RouteKey::new("jobs.submit", 1).unwrap();
    assert_eq!(
        Header::new(route.clone(), ""),
        Err(HeaderError::InvalidRequestId)
    );
    assert_eq!(
        Header::new(route, "r1")
            .unwrap()
            .with_correlation_id("bad id"),
        Err(HeaderError::InvalidCorrelationId)
    );
}

#[test]
fn response_preserves_exact_route_and_correlation() {
    let route = RouteKey::new("example.echo", 7).unwrap();
    let header = Header::new(route.clone(), "request-17")
        .unwrap()
        .with_correlation_id("parent-4")
        .unwrap();
    let response = ResponseHeader::from(&header);
    assert_eq!(response.route(), &route);
    assert_eq!(response.request_id(), "request-17");
    assert_eq!(response.correlation_id(), Some("parent-4"));
    assert_eq!(route.destination(), "example.echo");
    assert_eq!(route.version(), 7);
}

#[test]
fn payload_stays_owned_and_typed_without_serialization() {
    #[derive(Debug, PartialEq)]
    enum Request {
        Echo(String),
    }
    let header = Header::new(RouteKey::new("example.echo", 1).unwrap(), "r1").unwrap();
    let envelope = Envelope::new(header, Request::Echo("typed".into()));
    assert_eq!(envelope.payload, Request::Echo("typed".into()));
    assert_eq!(
        ResponseHeader::from(&envelope.header).correlation_id(),
        None
    );
}

#[test]
fn admission_policy_records_exact_headers_and_denial_is_recorded() {
    struct RecordingPolicy {
        seen: RefCell<Vec<(String, String)>>,
        deny: bool,
    }
    impl RecordingPolicy {
        fn assert_seen(&self, expected: &[(&str, &str)]) {
            let seen = self.seen.borrow();
            let actual: Vec<(&str, &str)> =
                seen.iter().map(|(d, r)| (d.as_str(), r.as_str())).collect();
            assert_eq!(actual, expected);
        }
    }
    impl AdmissionPolicy<()> for RecordingPolicy {
        fn admit(&self, header: &Header, _context: &()) -> Result<(), AdmissionDenied> {
            self.seen.borrow_mut().push((
                header.route().destination().to_string(),
                header.request_id().to_string(),
            ));
            if self.deny {
                Err(AdmissionDenied)
            } else {
                Ok(())
            }
        }
    }
    fn header(destination: &str, request_id: &str) -> Header {
        Header::new(RouteKey::new(destination, 1).unwrap(), request_id).unwrap()
    }
    let policy = RecordingPolicy {
        seen: RefCell::new(Vec::new()),
        deny: false,
    };
    assert_eq!(policy.admit(&header("jobs.submit", "r1"), &()), Ok(()));
    assert_eq!(policy.admit(&header("jobs.status", "r2"), &()), Ok(()));
    policy.assert_seen(&[("jobs.submit", "r1"), ("jobs.status", "r2")]);
    let denying = RecordingPolicy {
        seen: RefCell::new(Vec::new()),
        deny: true,
    };
    assert_eq!(
        denying.admit(&header("jobs.submit", "r9"), &()),
        Err(AdmissionDenied)
    );
    denying.assert_seen(&[("jobs.submit", "r9")]);
    // The closure form of the policy admits through the same seam.
    let closure = |_header: &Header, _context: &()| Ok::<(), AdmissionDenied>(());
    assert_eq!(closure.admit(&header("jobs.submit", "r1"), &()), Ok(()));
}

#[test]
fn payload_kind_classifies_each_variant_exactly() {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Kind {
        Echo,
        Submit,
    }
    #[derive(Debug)]
    enum Request {
        Echo(String),
        Submit { job: String },
    }
    impl Payload for Request {
        type Kind = Kind;
        fn kind(&self) -> Kind {
            match self {
                Request::Echo(_) => Kind::Echo,
                Request::Submit { .. } => Kind::Submit,
            }
        }
    }
    let echo = Request::Echo("hi".into());
    assert_eq!(echo.kind(), Kind::Echo);
    assert!(matches!(&echo, Request::Echo(text) if text == "hi"));
    let submit = Request::Submit { job: "j1".into() };
    assert_eq!(submit.kind(), Kind::Submit);
    assert!(matches!(&submit, Request::Submit { job } if job == "j1"));
}

#[test]
fn route_key_display_ordering_and_token_boundary_are_exact() {
    let key = RouteKey::new("example.echo", 7).unwrap();
    assert_eq!(key.to_string(), "example.echo@7");
    let mut keys = vec![
        RouteKey::new("jobs.submit", 2).unwrap(),
        RouteKey::new("jobs.submit", 1).unwrap(),
        RouteKey::new("jobs.status", 1).unwrap(),
    ];
    keys.sort();
    let rendered: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
    assert_eq!(
        rendered,
        vec!["jobs.status@1", "jobs.submit@1", "jobs.submit@2"]
    );
    assert_eq!(keys, keys.clone());
    let at_limit = "x".repeat(MAX_TOKEN_BYTES);
    assert!(RouteKey::new(at_limit, 1).is_ok());
    assert_eq!(
        Header::new(
            RouteKey::new("jobs.submit", 1).unwrap(),
            "y".repeat(MAX_TOKEN_BYTES)
        ),
        Header::new(
            RouteKey::new("jobs.submit", 1).unwrap(),
            "y".repeat(MAX_TOKEN_BYTES)
        )
    );
}
