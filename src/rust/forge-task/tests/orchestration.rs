use forge_io_client::IoClient;
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_task::Task;
use std::{cell::RefCell, convert::Infallible, error::Error, fmt};

#[derive(Debug, PartialEq, Eq)]
struct Fetch {
    key: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    title: String,
}
#[derive(Debug, PartialEq, Eq)]
struct Publish {
    destination: String,
    body: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Receipt(u64);
#[derive(Debug, Clone, PartialEq, Eq)]
struct FetchError(&'static str);
#[derive(Debug, Clone, PartialEq, Eq)]
struct PublishError(&'static str);
impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl Error for FetchError {}
impl Error for PublishError {}
#[derive(Debug, PartialEq, Eq)]
enum TaskError {
    Fetch(FetchError),
    Publish(PublishError),
}
impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetch(e) => write!(f, "fetch: {e}"),
            Self::Publish(e) => write!(f, "publish: {e}"),
        }
    }
}
impl Error for TaskError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Fetch(e) => Some(e),
            Self::Publish(e) => Some(e),
        }
    }
}
struct Input {
    key: String,
    destination: String,
}
type FetchPort = dyn IoClient<Request = Fetch, Response = Record, Error = FetchError>;
type PublishPort = dyn IoClient<Request = Publish, Response = Receipt, Error = PublishError>;
struct PublishTask<'a> {
    fetch: &'a FetchPort,
    publish: &'a PublishPort,
}
impl Task for PublishTask<'_> {
    type Input = Input;
    type Output = Receipt;
    type Error = TaskError;
    fn run(&self, input: Input) -> Result<Receipt, TaskError> {
        let record = self
            .fetch
            .execute(Fetch { key: input.key })
            .map_err(TaskError::Fetch)?;
        // Business payload assembly lives here, not in either client.
        let prepared = Publish {
            destination: input.destination,
            body: format!("Title: {}", record.title),
        };
        self.publish.execute(prepared).map_err(TaskError::Publish)
    }
}
struct RecordingFetch {
    calls: RefCell<Vec<Fetch>>,
    result: Result<Record, FetchError>,
}
impl IoClient for RecordingFetch {
    type Request = Fetch;
    type Response = Record;
    type Error = FetchError;
    fn execute(&self, request: Fetch) -> Result<Record, FetchError> {
        self.calls.borrow_mut().push(request);
        self.result.clone()
    }
}
struct AlternateFetch;
impl IoClient for AlternateFetch {
    type Request = Fetch;
    type Response = Record;
    type Error = FetchError;
    fn execute(&self, request: Fetch) -> Result<Record, FetchError> {
        assert_eq!(request.key, "document-1");
        Ok(Record {
            title: "same".into(),
        })
    }
}
struct RecordingPublish {
    calls: RefCell<Vec<Publish>>,
    result: Result<Receipt, PublishError>,
}
impl IoClient for RecordingPublish {
    type Request = Publish;
    type Response = Receipt;
    type Error = PublishError;
    fn execute(&self, request: Publish) -> Result<Receipt, PublishError> {
        self.calls.borrow_mut().push(request);
        self.result.clone()
    }
}
fn input() -> Input {
    Input {
        key: "document-1".into(),
        destination: "announcements".into(),
    }
}
fn source() -> RecordingFetch {
    RecordingFetch {
        calls: RefCell::new(vec![]),
        result: Ok(Record {
            title: "same".into(),
        }),
    }
}
fn sink() -> RecordingPublish {
    RecordingPublish {
        calls: RefCell::new(vec![]),
        result: Ok(Receipt(42)),
    }
}
fn expected() -> Publish {
    Publish {
        destination: "announcements".into(),
        body: "Title: same".into(),
    }
}

#[test]
fn task_fetches_then_prepares_exact_send_payload_using_injected_clients() {
    let fetch = source();
    let publish = sink();
    let task = PublishTask {
        fetch: &fetch,
        publish: &publish,
    };
    assert_eq!(task.run(input()), Ok(Receipt(42)));
    assert_eq!(
        fetch.calls.borrow().as_slice(),
        &[Fetch {
            key: "document-1".into()
        }]
    );
    assert_eq!(publish.calls.borrow().as_slice(), &[expected()]);
}

#[test]
fn distinct_client_implementations_substitute_without_changing_task() {
    let fetch = source();
    let alternative = AlternateFetch;
    let publish = sink();
    for client in [&fetch as &FetchPort, &alternative as &FetchPort] {
        assert_eq!(
            PublishTask {
                fetch: client,
                publish: &publish
            }
            .run(input()),
            Ok(Receipt(42))
        );
    }
    assert_eq!(publish.calls.borrow().as_slice(), &[expected(), expected()]);
}

#[test]
fn fetch_error_is_preserved_and_prevents_send() {
    let fetch = RecordingFetch {
        calls: RefCell::new(vec![]),
        result: Err(FetchError("source unavailable")),
    };
    let publish = sink();
    let error = PublishTask {
        fetch: &fetch,
        publish: &publish,
    }
    .run(input())
    .unwrap_err();
    assert_eq!(error.source().unwrap().to_string(), "source unavailable");
    assert_eq!(error, TaskError::Fetch(FetchError("source unavailable")));
    assert_eq!(fetch.calls.borrow().len(), 1);
    assert!(publish.calls.borrow().is_empty());
}

#[test]
fn send_failure_preserves_cause_after_fetch_without_automatic_retry_or_rollback() {
    let fetch = source();
    let publish = RecordingPublish {
        calls: RefCell::new(vec![]),
        result: Err(PublishError("ambiguous send")),
    };
    let error = PublishTask {
        fetch: &fetch,
        publish: &publish,
    }
    .run(input())
    .unwrap_err();
    assert_eq!(error.source().unwrap().to_string(), "ambiguous send");
    assert_eq!(error, TaskError::Publish(PublishError("ambiguous send")));
    assert_eq!(fetch.calls.borrow().len(), 1);
    assert_eq!(publish.calls.borrow().as_slice(), &[expected()]);
}

#[test]
fn factory_and_provider_compose_clients_without_a_task_service_locator() {
    let fetch_factory = || Ok::<_, Infallible>(source());
    let publish_factory = || Ok::<_, Infallible>(sink());
    let fetch_provider = Singleton::new(fetch_factory.create().unwrap());
    let publish_provider = Singleton::new(publish_factory.create().unwrap());
    let task = PublishTask {
        fetch: fetch_provider.acquire().unwrap(),
        publish: publish_provider.acquire().unwrap(),
    };
    assert_eq!(task.run(input()), Ok(Receipt(42)));
    assert_eq!(
        publish_provider
            .acquire()
            .unwrap()
            .calls
            .borrow()
            .as_slice(),
        &[expected()]
    );
}
