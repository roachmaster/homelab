//! All clients here are in-memory fakes; no network or disk operation occurs.
use forge_bridge::{Bridge, RegistryBuilder};
use forge_ingress_contract::{AdmissionDenied, Envelope, Header, Payload, RouteKey};
use forge_io_client::IoClient;
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_task::Task;
use std::{cell::RefCell, convert::Infallible, error::Error};

// The example capability owns these request/result types, not the framework.
struct LookupName {
    id: String,
}
struct Name(String);
#[derive(Debug, PartialEq, Eq)]
struct PreparedMessage {
    destination: String,
    body: String,
}
struct MessageInput {
    name_id: String,
    destination: String,
}

struct FakeNames;
impl IoClient for FakeNames {
    type Request = LookupName;
    type Response = Name;
    type Error = Infallible;
    fn execute(&self, request: LookupName) -> Result<Name, Infallible> {
        Ok(Name(request.id)) // An in-memory stand-in for a lookup operation.
    }
}
#[derive(Default)]
struct RecordingPublisher {
    sent: RefCell<Vec<PreparedMessage>>,
}
impl IoClient for RecordingPublisher {
    type Request = PreparedMessage;
    type Response = usize;
    type Error = Infallible;
    fn execute(&self, prepared: PreparedMessage) -> Result<usize, Infallible> {
        // The client records exactly what it receives; it assembles no message.
        let mut sent = self.sent.borrow_mut();
        sent.push(prepared);
        Ok(sent.len()) // A local count, NOT a durable-acceptance receipt.
    }
}
struct MessageTask<'a, F, S> {
    names: &'a F,
    publisher: &'a S,
}
impl<F, S> Task for MessageTask<'_, F, S>
where
    F: IoClient<Request = LookupName, Response = Name, Error = Infallible>,
    S: IoClient<Request = PreparedMessage, Response = usize, Error = Infallible>,
{
    type Input = MessageInput;
    type Output = usize;
    type Error = Infallible;
    fn run(&self, input: MessageInput) -> Result<usize, Infallible> {
        let name = self.names.execute(LookupName { id: input.name_id })?;
        let prepared = PreparedMessage {
            destination: input.destination,
            body: format!("Hello, {}!", name.0),
        };
        self.publisher.execute(prepared)
    }
}
enum Command {
    Send(MessageInput),
}
#[derive(PartialEq, Eq)]
enum Kind {
    Send,
}
impl Payload for Command {
    type Kind = Kind;
    fn kind(&self) -> Kind {
        match self {
            Self::Send(_) => Kind::Send,
        }
    }
}
// Explicit local demonstration policy, not an authentication implementation.
fn demo_policy(_: &Header, allowed: &bool) -> Result<(), AdmissionDenied> {
    if *allowed {
        Ok(())
    } else {
        Err(AdmissionDenied)
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let names_factory = || Ok::<_, Infallible>(FakeNames);
    let publisher_factory = || Ok::<_, Infallible>(RecordingPublisher::default());
    let names = Singleton::new(names_factory.create()?);
    let publisher = Singleton::new(publisher_factory.create()?);
    let task = MessageTask {
        names: names.acquire()?,
        publisher: publisher.acquire()?,
    };

    let key = RouteKey::new("messages.send", 1)?;
    let mut builder = RegistryBuilder::new();
    builder.register(key.clone(), Kind::Send, |command, _: &bool| {
        let Command::Send(input) = command;
        task.run(input)
    })?;
    let bridge = Bridge::new(builder.freeze(), demo_policy);
    let header = Header::new(key, "request-2")?.with_correlation_id("conversation-2")?;
    let request = Command::Send(MessageInput {
        name_id: "Forge".into(),
        destination: "demo".into(),
    });
    let reply = bridge.dispatch(Envelope::new(header, request), &true)?;
    assert_eq!(reply.payload, 1);
    assert_eq!(reply.header.correlation_id(), Some("conversation-2"));
    assert_eq!(
        publisher.acquire()?.sent.borrow().as_slice(),
        &[PreparedMessage {
            destination: "demo".into(),
            body: "Hello, Forge!".into()
        }]
    );
    println!(
        "Task completed through typed bridge; {} prepared message recorded in memory.",
        reply.payload
    );
    Ok(())
}
