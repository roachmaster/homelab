//! Original local composition example; no authentication or durable-storage claim.
use forge_bridge::{Bridge, RegistryBuilder};
use forge_ingress_contract::{AdmissionDenied, Envelope, Header, Payload, RouteKey};
use forge_provider::{Factory, InstanceProvider, Singleton};
use std::{convert::Infallible, error::Error, sync::Arc};

// This illustrative capability owns its enum, results and consumed port.
enum GreetingRequest {
    Greet(String),
}
#[derive(PartialEq, Eq)]
enum GreetingKind {
    Greet,
}
impl Payload for GreetingRequest {
    type Kind = GreetingKind;
    fn kind(&self) -> GreetingKind {
        match self {
            Self::Greet(_) => GreetingKind::Greet,
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
enum GreetingReply {
    Message(String),
}
trait Greeting {
    fn greet(&self, name: &str) -> String;
}
struct LocalGreeting {
    prefix: String,
}
impl Greeting for LocalGreeting {
    fn greet(&self, name: &str) -> String {
        format!("{}, {name}!", self.prefix)
    }
}
struct GreetingHandler {
    greeting: Arc<dyn Greeting>,
}
impl GreetingHandler {
    fn handle(&self, request: GreetingRequest) -> GreetingReply {
        match request {
            GreetingRequest::Greet(name) => GreetingReply::Message(self.greeting.greet(&name)),
        }
    }
}

// Only a demonstration context for this in-process executable. An external host
// must establish verified identity; a caller-supplied bool is not authentication.
struct DemoContext {
    admitted: bool,
}
fn local_policy(_: &Header, context: &DemoContext) -> Result<(), AdmissionDenied> {
    if context.admitted {
        Ok(())
    } else {
        Err(AdmissionDenied)
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let prefix = String::from("Hello");
    let factory = || {
        Ok::<Arc<dyn Greeting>, Infallible>(Arc::new(LocalGreeting {
            prefix: prefix.clone(),
        }))
    };
    let provider = Singleton::new(factory.create()?);
    let handler = GreetingHandler {
        greeting: Arc::clone(provider.acquire()?),
    };
    let route = RouteKey::new("greeting.greet", 1)?;
    let mut registry = RegistryBuilder::new();
    registry.register(
        route.clone(),
        GreetingKind::Greet,
        move |request, _: &DemoContext| Ok::<_, Infallible>(handler.handle(request)),
    )?;
    let bridge = Bridge::new(registry.freeze(), local_policy);
    let header = Header::new(route, "request-1")?.with_correlation_id("conversation-1")?;
    let reply = bridge.dispatch(
        Envelope::new(header, GreetingRequest::Greet("Forge".into())),
        &DemoContext { admitted: true },
    )?;
    assert_eq!(
        reply.payload,
        GreetingReply::Message("Hello, Forge!".into())
    );
    assert_eq!(reply.header.correlation_id(), Some("conversation-1"));
    println!(
        "{:?} [request={}, correlation={}]",
        reply.payload,
        reply.header.request_id(),
        reply.header.correlation_id().unwrap_or("none")
    );
    Ok(())
}
