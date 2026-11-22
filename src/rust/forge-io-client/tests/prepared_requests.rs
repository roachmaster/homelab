use forge_io_client::IoClient;
use std::cell::RefCell;

// Capability-owned prepared data; deliberately not Clone or an ingress envelope.
#[derive(Debug, PartialEq, Eq)]
struct PreparedPublish {
    destination: String,
    content_type: String,
    body: Box<[u8]>,
}
#[derive(Debug, PartialEq, Eq)]
struct ClientError(&'static str);
struct RecordingClient {
    calls: RefCell<Vec<PreparedPublish>>,
    fail: bool,
}
impl RecordingClient {
    fn new(fail: bool) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            fail,
        }
    }

    /// The fake's observation method: tests assert the exact recorded vector
    /// through here, never by reaching into the calls field. The request type
    /// is deliberately not Clone, so the observation takes the expected slice
    /// and asserts equality instead of returning a cloned vector.
    fn assert_recorded(&self, expected: &[PreparedPublish]) {
        assert_eq!(self.calls.borrow().as_slice(), expected);
    }
}
impl IoClient for RecordingClient {
    type Request = PreparedPublish;
    type Response = usize;
    type Error = ClientError;
    fn execute(&self, request: PreparedPublish) -> Result<usize, ClientError> {
        let size = request.body.len();
        self.calls.borrow_mut().push(request);
        if self.fail {
            Err(ClientError("send failed"))
        } else {
            Ok(size)
        }
    }
}
fn prepared() -> PreparedPublish {
    PreparedPublish {
        destination: "capability.events".into(),
        content_type: "application/octet-stream".into(),
        body: vec![0, 1, 255, 7].into_boxed_slice(),
    }
}
fn prepared_other() -> PreparedPublish {
    PreparedPublish {
        destination: "capability.audit".into(),
        content_type: "application/json".into(),
        body: vec![9, 8].into_boxed_slice(),
    }
}

/// A consumer written against the generic bound, not the dynamic trait object.
fn publish<C>(client: &C, request: PreparedPublish) -> Result<usize, ClientError>
where
    C: IoClient<Request = PreparedPublish, Response = usize, Error = ClientError>,
{
    client.execute(request)
}

#[test]
fn preassembled_non_clone_request_crosses_dynamic_client_boundary_unchanged() {
    let client = RecordingClient::new(false);
    let port: &dyn IoClient<Request = PreparedPublish, Response = usize, Error = ClientError> =
        &client;
    assert_eq!(port.execute(prepared()), Ok(4));
    client.assert_recorded(&[prepared()]);
}

#[test]
fn typed_error_does_not_erase_attempted_operation_or_trigger_retry() {
    let client = RecordingClient::new(true);
    assert_eq!(client.execute(prepared()), Err(ClientError("send failed")));
    client.assert_recorded(&[prepared()]);
}

#[test]
fn multiple_requests_record_in_exact_order() {
    let client = RecordingClient::new(false);
    assert_eq!(client.execute(prepared()), Ok(4));
    assert_eq!(client.execute(prepared_other()), Ok(2));
    assert_eq!(client.execute(prepared()), Ok(4));
    client.assert_recorded(&[prepared(), prepared_other(), prepared()]);
}

#[test]
fn preassembled_non_clone_request_crosses_static_generic_boundary_unchanged() {
    let client = RecordingClient::new(false);
    assert_eq!(publish(&client, prepared()), Ok(4));
    client.assert_recorded(&[prepared()]);
}

#[test]
fn repeated_flow_records_identical_requests() {
    fn run_flow() -> RecordingClient {
        let client = RecordingClient::new(false);
        client.execute(prepared()).unwrap();
        client.execute(prepared_other()).unwrap();
        client
    }
    let first = run_flow();
    let second = run_flow();
    first.assert_recorded(&[prepared(), prepared_other()]);
    second.assert_recorded(&[prepared(), prepared_other()]);
}
