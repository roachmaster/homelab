use forge_bridge::{Bridge, DispatchError, RegistryBuilder};
use forge_ingress_contract::{AdmissionDenied, Envelope, Header, Payload, RouteKey};
use forge_io_client::IoClient;
use forge_task::Task;
use std::{cell::RefCell, error::Error, fmt};

#[derive(Debug, PartialEq, Eq)]
struct Prepared(String);
#[derive(Debug, PartialEq, Eq)]
struct ClientFailure(&'static str);
impl fmt::Display for ClientFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl Error for ClientFailure {}
struct Client {
    received: RefCell<Vec<Prepared>>,
    fail: bool,
}
impl IoClient for Client {
    type Request = Prepared;
    type Response = usize;
    type Error = ClientFailure;
    fn execute(&self, request: Prepared) -> Result<usize, ClientFailure> {
        let mut received = self.received.borrow_mut();
        received.push(request);
        if self.fail {
            Err(ClientFailure("attempt failed"))
        } else {
            Ok(received.len())
        }
    }
}
struct SendTask<'a, C: ?Sized> {
    client: &'a C,
}
impl<C> Task for SendTask<'_, C>
where
    C: IoClient<Request = Prepared, Response = usize, Error = ClientFailure> + ?Sized,
{
    type Input = String;
    type Output = usize;
    type Error = ClientFailure;
    fn run(&self, input: String) -> Result<usize, ClientFailure> {
        self.client.execute(Prepared(format!("prepared:{input}")))
    }
}
enum Command {
    Send(String),
    Other,
}
#[derive(PartialEq, Eq)]
enum Kind {
    Send,
    Other,
}
impl Payload for Command {
    type Kind = Kind;
    fn kind(&self) -> Kind {
        match self {
            Self::Send(_) => Kind::Send,
            Self::Other => Kind::Other,
        }
    }
}
fn key(version: u16) -> RouteKey {
    RouteKey::new("messages.send", version).unwrap()
}
fn request(version: u16, payload: Command) -> Envelope<Command> {
    Envelope::new(Header::new(key(version), "r1").unwrap(), payload)
}
fn policy(_: &Header, allowed: &bool) -> Result<(), AdmissionDenied> {
    if *allowed {
        Ok(())
    } else {
        Err(AdmissionDenied)
    }
}

#[test]
fn admitted_binding_calls_injected_task_and_all_rejections_prevent_client_effects() {
    let client = Client {
        received: RefCell::new(vec![]),
        fail: false,
    };
    let task = SendTask { client: &client };
    let mut builder = RegistryBuilder::new();
    builder
        .register(key(1), Kind::Send, |payload, _: &bool| match payload {
            Command::Send(input) => task.run(input),
            Command::Other => Err(ClientFailure("invalid binding")),
        })
        .unwrap();
    let bridge = Bridge::new(builder.freeze(), policy);
    assert_eq!(
        bridge.dispatch(request(1, Command::Other), &true),
        Err(DispatchError::PayloadMismatch(key(1)))
    );
    assert_eq!(
        bridge.dispatch(request(1, Command::Send("denied".into())), &false),
        Err(DispatchError::Admission(AdmissionDenied))
    );
    assert_eq!(
        bridge.dispatch(request(2, Command::Send("wrong-version".into())), &true),
        Err(DispatchError::UnknownRoute(key(2)))
    );
    assert!(client.received.borrow().is_empty());
    assert_eq!(
        bridge
            .dispatch(request(1, Command::Send("ok".into())), &true)
            .unwrap()
            .payload,
        1
    );
    assert_eq!(
        client.received.borrow().as_slice(),
        &[Prepared("prepared:ok".into())]
    );
}

#[test]
fn task_client_error_reaches_bridge_caller_without_erasure_or_retry() {
    let client = Client {
        received: RefCell::new(vec![]),
        fail: true,
    };
    let task = SendTask { client: &client };
    let mut builder = RegistryBuilder::new();
    builder
        .register(key(1), Kind::Send, |payload, _: &bool| match payload {
            Command::Send(input) => task.run(input),
            Command::Other => Err(ClientFailure("invalid binding")),
        })
        .unwrap();
    let bridge = Bridge::new(builder.freeze(), policy);
    let error = bridge
        .dispatch(request(1, Command::Send("x".into())), &true)
        .unwrap_err();
    assert_eq!(error.source().unwrap().to_string(), "attempt failed");
    assert_eq!(
        error,
        DispatchError::Handler(ClientFailure("attempt failed"))
    );
    assert_eq!(
        client.received.borrow().as_slice(),
        &[Prepared("prepared:x".into())]
    );
}
