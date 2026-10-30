use forge_ingress_contract::{
    Envelope, Header, HeaderError, ResponseHeader, RouteKey, MAX_TOKEN_BYTES,
};

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
