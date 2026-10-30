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

#[test]
fn preassembled_non_clone_request_crosses_dynamic_client_boundary_unchanged() {
    let client = RecordingClient {
        calls: RefCell::new(Vec::new()),
        fail: false,
    };
    let port: &dyn IoClient<Request = PreparedPublish, Response = usize, Error = ClientError> =
        &client;
    assert_eq!(port.execute(prepared()), Ok(4));
    assert_eq!(client.calls.borrow().as_slice(), &[prepared()]);
}

#[test]
fn typed_error_does_not_erase_attempted_operation_or_trigger_retry() {
    let client = RecordingClient {
        calls: RefCell::new(Vec::new()),
        fail: true,
    };
    assert_eq!(client.execute(prepared()), Err(ClientError("send failed")));
    assert_eq!(client.calls.borrow().as_slice(), &[prepared()]);
}
