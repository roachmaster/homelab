use forge_bootstrap::Registry;
use forge_io_client::IoClient;
use forge_jenkins::{
    register, Config, Credentials, Error, HttpRequest, HttpResponse, Request, Response,
    TransportError,
};
use forge_provider::{Factory, InstanceProvider, Singleton, Transient};
use std::{cell::Cell, collections::BTreeMap, rc::Rc};

#[derive(Clone)]
struct SelectedTransport {
    calls: Rc<Cell<usize>>,
    fail: bool,
}
impl IoClient for SelectedTransport {
    type Request = HttpRequest;
    type Response = HttpResponse;
    type Error = TransportError;
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.calls.set(self.calls.get() + 1);
        assert!(request
            .url
            .starts_with("https://configured.example/jenkins/"));
        if self.fail {
            return Err(TransportError::Timeout);
        }
        Ok(HttpResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: if request.url.contains("queue/api/json") { br#"{"items":[]}"#.to_vec() } else { br#"{"jobs":[]}"#.to_vec() },
        })
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let creates = Rc::new(Cell::new(0));
    let calls = Rc::new(Cell::new(0));
    let count = creates.clone();
    let state = calls.clone();
    let group = register(
        Config::new("https://configured.example/jenkins/")?,
        Credentials::new("caller", "fixture-token")?,
        move || {
            count.set(count.get() + 1);
            Ok(SelectedTransport {
                calls: state.clone(),
                fail: false,
            })
        },
    )?;
    let pending = Registry::new().register(group)?;
    assert_eq!(creates.get(), 0);
    assert_eq!(calls.get(), 0);
    let duplicate = register(
        Config::new("https://other.example/")?,
        Credentials::new("caller", "fixture-token")?,
        || Err::<SelectedTransport, _>(Error::Transport),
    )?;
    let rejected = pending.register(duplicate).unwrap_err();
    let (original, _, _) = rejected.into_parts();
    let ((), factory) = original.freeze()?.into_factories();
    assert_eq!(creates.get(), 0);
    let singleton = Singleton::new(factory.create()?);
    assert_eq!(creates.get(), 1);
    assert!(std::ptr::eq(singleton.acquire()?, singleton.acquire()?));
    assert_eq!(
        singleton.acquire()?.execute(Request::Jobs)?,
        Response::Jobs(vec![])
    );
    let transient = Transient::new(factory);
    let first = transient.acquire()?;
    let second = transient.acquire()?;
    assert_eq!(creates.get(), 3);
    assert!(!std::ptr::eq(&*first, &*second));
    assert_eq!(first.execute(Request::Jobs)?, Response::Jobs(vec![]));
    assert_eq!(calls.get(), 2);
    let Response::QueueList(snapshot) = first.execute(Request::QueueList)? else { panic!("typed queue snapshot") };
    assert!(snapshot.items.is_empty());
    assert!(!snapshot.truncated);
    assert_eq!(calls.get(), 3);
    let group = register(
        Config::new("https://configured.example/jenkins/")?,
        Credentials::new("caller", "fixture-token")?,
        || Err::<SelectedTransport, _>(Error::Transport),
    )?;
    let ((), factory) = Registry::new().register(group)?.freeze()?.into_factories();
    assert!(matches!(factory.create(), Err(Error::Transport)));
    let runtime_calls = Rc::new(Cell::new(0));
    let state = runtime_calls.clone();
    let group = register(
        Config::new("https://configured.example/jenkins/")?,
        Credentials::new("caller", "fixture-token")?,
        move || {
            Ok(SelectedTransport {
                calls: state.clone(),
                fail: true,
            })
        },
    )?;
    let ((), factory) = Registry::new().register(group)?.freeze()?.into_factories();
    assert_eq!(
        factory.create()?.execute(Request::Jobs),
        Err(Error::Timeout)
    );
    assert_eq!(runtime_calls.get(), 1);
    assert!(Config::new("http://untrusted.example/").is_err());
    println!("Jenkins independent consumer: registration, injection, providers and typed failures passed");
    Ok(())
}
