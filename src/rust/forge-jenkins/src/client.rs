use crate::observation::{self, QueueListing, RecentBuildListing};
use base64::{engine::general_purpose::STANDARD, Engine};
use forge_bootstrap::{PreparedGroup, RegistrationError};
use forge_io_client::IoClient;
use forge_provider::Factory;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    error, fmt,
    marker::PhantomData,
    time::Duration,
};
use url::{Host, Url};

/// Complete prepared operation. Build parameters are caller-owned business data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Controller,
    Jobs,
    /// Bounded queue snapshot, including explicitly selected public parameters.
    QueueList,
    /// Bounded newest-build snapshot; `limit` must be between 1 and 50.
    RecentBuilds {
        job: String,
        limit: u8,
    },
    /// Request graceful cancellation of one known build, without polling.
    StopBuild {
        job: String,
        number: u64,
    },
    Build {
        job: String,
        parameters: BTreeMap<String, String>,
    },
    Queue {
        id: u64,
    },
    BuildStatus {
        job: String,
        number: u64,
    },
    CancelQueue {
        id: u64,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    Controller(ControllerInfo),
    Jobs(Vec<JobSummary>),
    QueueList(QueueListing),
    RecentBuilds(RecentBuildListing),
    /// Jenkins accepted the request; the build may still be running.
    StopRequested,
    Queued(QueueReceipt),
    Queue(QueueItem),
    Build(BuildInfo),
    /// Jenkins accepted a cancellation request, not proof that a build stopped.
    CancelRequested,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControllerInfo {
    pub version: Option<String>,
    pub use_security: bool,
    pub num_executors: u32,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct JobSummary {
    pub name: String,
    pub color: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueReceipt {
    pub id: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Executable {
    pub number: u64,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct QueueItem {
    pub id: u64,
    pub cancelled: Option<bool>,
    pub blocked: Option<bool>,
    pub buildable: Option<bool>,
    pub why: Option<String>,
    pub executable: Option<Executable>,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct BuildInfo {
    pub number: u64,
    pub building: bool,
    pub result: Option<String>,
    #[serde(rename = "duration")]
    pub duration_ms: u64,
    #[serde(rename = "queueId")]
    pub queue_id: Option<i64>,
}

/// Errors never contain credentials, raw HTTP bodies, headers or backend errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Configuration,
    InvalidRequest,
    MutationDenied,
    Unauthorized,
    NotFound,
    UnexpectedStatus(u16),
    InvalidResponse,
    BodyLimit,
    Timeout,
    Transport,
    /// A write may have reached Jenkins. Do not blindly retry it.
    MutationUncertain,
    Registration(RegistrationError),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration => f.write_str("invalid Jenkins configuration"),
            Self::InvalidRequest => f.write_str("invalid Jenkins request"),
            Self::MutationDenied => f.write_str("Jenkins mutations are disabled for this client"),
            Self::Unauthorized => f.write_str("Jenkins denied authentication or permission"),
            Self::NotFound => f.write_str("Jenkins resource was not found"),
            Self::UnexpectedStatus(status) => write!(f, "unexpected Jenkins HTTP status {status}"),
            Self::InvalidResponse => f.write_str("invalid Jenkins response"),
            Self::BodyLimit => f.write_str("Jenkins response exceeded its byte limit"),
            Self::Timeout => f.write_str("Jenkins request deadline expired"),
            Self::Transport => f.write_str("Jenkins transport failed"),
            Self::MutationUncertain => {
                f.write_str("Jenkins write outcome is uncertain; reconcile before retrying")
            }
            Self::Registration(error) => error.fmt(f),
        }
    }
}
impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        if let Self::Registration(error) = self {
            Some(error)
        } else {
            None
        }
    }
}

#[derive(Clone)]
pub struct Credentials {
    authorization: String,
}
impl Credentials {
    pub fn new(username: impl Into<String>, api_token: impl Into<String>) -> Result<Self, Error> {
        let username = username.into();
        let token = api_token.into();
        if username.is_empty()
            || username.len() > 128
            || username.contains(':')
            || token.is_empty()
            || token.len() > 4096
            || username.chars().any(char::is_control)
            || token.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(Error::Configuration);
        }
        Ok(Self {
            authorization: format!("Basic {}", STANDARD.encode(format!("{username}:{token}"))),
        })
    }
}
impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub timeout: Duration,
    pub max_response_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            max_response_bytes: 1024 * 1024,
        }
    }
}
impl Limits {
    pub(crate) fn validate(self) -> Result<Self, Error> {
        if self.timeout.is_zero()
            || self.timeout > Duration::from_secs(120)
            || !(256..=1024 * 1024).contains(&self.max_response_bytes)
        {
            return Err(Error::Configuration);
        }
        Ok(self)
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    endpoint: Url,
    allow_mutations: bool,
    observed_parameters: BTreeSet<String>,
    limits: Limits,
}
impl Config {
    pub fn new(endpoint: impl Into<String>) -> Result<Self, Error> {
        let input = endpoint.into();
        if input.len() > 2048 || input.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(Error::Configuration);
        }
        let mut endpoint = Url::parse(&input).map_err(|_| Error::Configuration)?;
        let host = endpoint.host().ok_or(Error::Configuration)?;
        let loopback = match host {
            Host::Ipv4(ip) => ip.is_loopback(),
            Host::Ipv6(ip) => {
                ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|ip| ip.is_loopback())
            }
            _ => false,
        };
        if !matches!(endpoint.scheme(), "https" | "http")
            || (endpoint.scheme() == "http" && !loopback)
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.path().split('/').any(|part| {
                !part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
                    || matches!(part, "." | "..")
            })
            || input.contains('%')
            || input.split('/').any(|part| matches!(part, "." | ".."))
            || input.contains('\\')
            || input.contains("/../")
            || input.contains("/./")
        {
            return Err(Error::Configuration);
        }
        let path = endpoint.path().to_owned();
        if !path.ends_with('/') {
            endpoint.set_path(&(path + "/"));
        }
        Ok(Self {
            endpoint,
            allow_mutations: false,
            observed_parameters: BTreeSet::new(),
            limits: Limits::default(),
        })
    }
    pub fn with_mutations(mut self, enabled: bool) -> Self {
        self.allow_mutations = enabled;
        self
    }
    /// Select public correlation parameters that list operations may return.
    /// Empty by default. Do not select credentials or secret parameter names.
    /// Password parameter types are never returned, even if explicitly selected.
    pub fn with_observed_parameters(
        mut self,
        names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, Error> {
        let mut selected = BTreeSet::new();
        for name in names {
            let name = name.into();
            if name.is_empty()
                || name.len() > 128
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || selected.len() >= 64
                || !selected.insert(name)
            {
                return Err(Error::Configuration);
            }
        }
        self.observed_parameters = selected;
        Ok(self)
    }
    pub fn with_limits(
        mut self,
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<Self, Error> {
        self.limits = Limits {
            timeout,
            max_response_bytes,
        }
        .validate()?;
        Ok(self)
    }
    pub fn endpoint(&self) -> &str {
        self.endpoint.as_str()
    }
    pub fn allow_mutations(&self) -> bool {
        self.allow_mutations
    }
    pub fn limits(&self) -> Limits {
        self.limits
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}
/// The wire request is prepared by the typed adapter. Debug hides authorization
/// and parameter bytes. A selected transport may inspect them to perform I/O.
#[derive(Clone)]
pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    pub body: Vec<u8>,
    pub content_type: Option<&'static str>,
    pub limits: Limits,
    authorization: String,
}
impl HttpRequest {
    pub fn authorization(&self) -> &str {
        &self.authorization
    }
}
impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("body_bytes", &self.body.len())
            .field("authorization", &"<redacted>")
            .finish()
    }
}
#[derive(Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
impl fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("body_bytes", &self.body.len())
            .finish()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
    Timeout,
    BodyLimit,
    InvalidResponse,
    Unavailable,
}

#[derive(Clone)]
pub struct Client<T> {
    config: Config,
    credentials: Credentials,
    transport: T,
}
impl<T> fmt::Debug for Client<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JenkinsClient")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}
pub struct ClientFactory<F, T> {
    config: Config,
    credentials: Credentials,
    transport_factory: F,
    marker: PhantomData<fn() -> T>,
}
impl<F, T> ClientFactory<F, T> {
    pub fn new(
        config: Config,
        credentials: Credentials,
        transport_factory: F,
    ) -> Result<Self, Error> {
        config.limits.validate()?;
        Ok(Self {
            config,
            credentials,
            transport_factory,
            marker: PhantomData,
        })
    }
}
impl<F, T> Factory<Client<T>> for ClientFactory<F, T>
where
    F: Factory<T, Error = Error>,
{
    type Error = Error;
    fn create(&self) -> Result<Client<T>, Error> {
        Ok(Client {
            config: self.config.clone(),
            credentials: self.credentials.clone(),
            transport: self.transport_factory.create()?,
        })
    }
}
/// Validate and stage the selected transport recipe. Does not create/call it.
pub fn register<F, T>(
    config: Config,
    credentials: Credentials,
    transport_factory: F,
) -> Result<PreparedGroup<ClientFactory<F, T>>, Error>
where
    F: Factory<T, Error = Error>,
    T: IoClient<Request = HttpRequest, Response = HttpResponse, Error = TransportError>,
{
    PreparedGroup::new(
        "forge.jenkins",
        &["jenkins.client"],
        &[],
        ClientFactory::new(config, credentials, transport_factory)?,
    )
    .map_err(Error::Registration)
}

pub(crate) fn job_path(job: &str) -> Result<String, Error> {
    if job.is_empty() || job.len() > 512 || job.chars().any(|c| c.is_control() || c == '\\') {
        return Err(Error::InvalidRequest);
    }
    let parts: Vec<_> = job.split('/').collect();
    if parts.len() > 16
        || parts
            .iter()
            .any(|p| p.is_empty() || matches!(*p, "." | ".."))
    {
        return Err(Error::InvalidRequest);
    }
    Ok(parts
        .into_iter()
        .map(|part| format!("job/{}/", percent(part)))
        .collect())
}
fn percent(value: &str) -> String {
    let mut encoded = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            encoded.push(b as char);
        } else {
            use fmt::Write;
            let _ = write!(encoded, "%{b:02X}");
        }
    }
    encoded
}
fn positive(value: u64) -> Result<u64, Error> {
    if value == 0 {
        Err(Error::InvalidRequest)
    } else {
        Ok(value)
    }
}

impl Request {
    /// Pure preparation checks shared by Tasks and concrete adapters. No I/O.
    pub fn validate(&self) -> Result<(), Error> {
        self.prepare().map(|_| ())
    }
    fn prepare(&self) -> Result<(Method, String, Vec<u8>), Error> {
        let mut body = Vec::new();
        let (method, path) = match self {
            Request::Controller => (Method::Get, "api/json?tree=useSecurity,numExecutors".into()),
            Request::Jobs => (Method::Get, "api/json?tree=jobs[name,color]".into()),
            Request::QueueList => (
                Method::Get,
                observation::tree_path("queue/api/json", "items[id,task[name,url],actions[parameters[_class,name,value]],executable[number]]{0,51}"),
            ),
            Request::RecentBuilds { job, limit } => {
                if !(1..=50).contains(limit) { return Err(Error::InvalidRequest); }
                (Method::Get, observation::tree_path(
                    &(job_path(job)? + "api/json"),
                    &format!("builds[number,queueId,building,result,actions[parameters[_class,name,value]]]{{0,{}}}", u16::from(*limit) + 1),
                ))
            }
            Request::StopBuild { job, number } => (
                Method::Post, format!("{}{}/stop", job_path(job)?, positive(*number)?),
            ),
            Request::Build { job, parameters } => {
                let job = job_path(job)?;
                if parameters.len() > 64
                    || parameters.iter().any(|(key, value)| {
                        key.is_empty()
                            || key.len() > 128
                            || value.len() > 4096
                            || key.chars().any(char::is_control)
                    })
                {
                    return Err(Error::InvalidRequest);
                }
                body = url::form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(parameters)
                    .finish()
                    .into_bytes();
                if body.len() > 65536 {
                    return Err(Error::InvalidRequest);
                }
                (
                    Method::Post,
                    job + if parameters.is_empty() {
                        "build"
                    } else {
                        "buildWithParameters"
                    },
                )
            }
            Request::Queue { id } => (
                Method::Get,
                format!("queue/item/{}/api/json", positive(*id)?),
            ),
            Request::BuildStatus { job, number } => (
                Method::Get,
                format!("{}{}/api/json", job_path(job)?, positive(*number)?),
            ),
            Request::CancelQueue { id } => (
                Method::Post,
                format!("queue/cancelItem?id={}", positive(*id)?),
            ),
        };
        Ok((method, path, body))
    }
}
impl<T> IoClient for Client<T>
where
    T: IoClient<Request = HttpRequest, Response = HttpResponse, Error = TransportError>,
{
    type Request = Request;
    type Response = Response;
    type Error = Error;
    fn execute(&self, request: Request) -> Result<Response, Error> {
        let (method, path, body) = request.prepare()?;
        let write = method == Method::Post;
        if write && !self.config.allow_mutations {
            return Err(Error::MutationDenied);
        }
        let url = self
            .config
            .endpoint
            .join(&path)
            .map_err(|_| Error::InvalidRequest)?;
        let response = self
            .transport
            .execute(HttpRequest {
                method,
                url: url.into(),
                body,
                content_type: write.then_some("application/x-www-form-urlencoded"),
                limits: self.config.limits,
                authorization: self.credentials.authorization.clone(),
            })
            .map_err(|error| {
                if write {
                    Error::MutationUncertain
                } else {
                    match error {
                        TransportError::Timeout => Error::Timeout,
                        TransportError::BodyLimit => Error::BodyLimit,
                        TransportError::InvalidResponse => Error::InvalidResponse,
                        TransportError::Unavailable => Error::Transport,
                    }
                }
            })?;
        if response.body.len() > self.config.limits.max_response_bytes {
            return Err(if write {
                Error::MutationUncertain
            } else {
                Error::BodyLimit
            });
        }
        if matches!(response.status, 401 | 403) {
            return Err(Error::Unauthorized);
        }
        if response.status == 404 {
            return Err(Error::NotFound);
        }
        if response.status >= 500 && write {
            return Err(Error::MutationUncertain);
        }
        match request {
            Request::Build { .. } => {
                if response.status != 201 {
                    return Err(Error::MutationUncertain);
                }
                let location = response
                    .headers
                    .get("location")
                    .ok_or(Error::MutationUncertain)?;
                let id = self
                    .queue_location(location)
                    .ok_or(Error::MutationUncertain)?;
                Ok(Response::Queued(QueueReceipt { id }))
            }
            Request::StopBuild { job, number } => {
                // Jenkins aborts asynchronously. Never follow a redirect or
                // turn an acknowledgement into proof that execution stopped.
                if response.status == 302 {
                    let location = response
                        .headers
                        .get("location")
                        .ok_or(Error::MutationUncertain)?;
                    let build_url = self
                        .config
                        .endpoint
                        .join(&format!("{}{}/", job_path(&job)?, number))
                        .map_err(|_| Error::MutationUncertain)?;
                    if location.is_empty()
                        || location.len() > 4096
                        || location.chars().any(char::is_control)
                        || build_url.join(location).ok().as_ref() != Some(&build_url)
                    {
                        return Err(Error::MutationUncertain);
                    }
                } else if !matches!(response.status, 200 | 201 | 202 | 204) {
                    return Err(Error::MutationUncertain);
                }
                Ok(Response::StopRequested)
            }
            Request::CancelQueue { .. } => {
                // Jenkins redirects successful cancellation to the queue page.
                // The redirect is never followed and grants no target authority.
                if response.status == 302 {
                    let location = response
                        .headers
                        .get("location")
                        .ok_or(Error::MutationUncertain)?;
                    if location.is_empty()
                        || location.len() > 4096
                        || location.chars().any(char::is_control)
                    {
                        return Err(Error::MutationUncertain);
                    }
                    let target = self
                        .config
                        .endpoint
                        .join(location)
                        .map_err(|_| Error::MutationUncertain)?;
                    if target.origin() != self.config.endpoint.origin()
                        || !target.username().is_empty()
                        || target.password().is_some()
                        || target.query().is_some()
                        || target.fragment().is_some()
                        || (target.path() != self.config.endpoint.path()
                            && target.path() != format!("{}queue/", self.config.endpoint.path()))
                    {
                        return Err(Error::MutationUncertain);
                    }
                    Ok(Response::CancelRequested)
                } else if matches!(response.status, 200 | 201 | 204) {
                    Ok(Response::CancelRequested)
                } else {
                    Err(Error::MutationUncertain)
                }
            }
            request => {
                if response.status != 200 {
                    return Err(Error::UnexpectedStatus(response.status));
                }
                match request {
                    Request::Controller => {
                        #[derive(Deserialize)]
                        struct Info {
                            #[serde(rename = "useSecurity")]
                            use_security: bool,
                            #[serde(rename = "numExecutors")]
                            num_executors: u32,
                        }
                        let info: Info = serde_json::from_slice(&response.body)
                            .map_err(|_| Error::InvalidResponse)?;
                        let version = response
                            .headers
                            .get("x-jenkins")
                            .filter(|value| {
                                value.len() <= 128 && !value.chars().any(char::is_control)
                            })
                            .cloned();
                        Ok(Response::Controller(ControllerInfo {
                            version,
                            use_security: info.use_security,
                            num_executors: info.num_executors,
                        }))
                    }
                    Request::Jobs => {
                        #[derive(Deserialize)]
                        struct Jobs {
                            jobs: Vec<JobSummary>,
                        }
                        let jobs: Jobs = serde_json::from_slice(&response.body)
                            .map_err(|_| Error::InvalidResponse)?;
                        Ok(Response::Jobs(jobs.jobs))
                    }
                    Request::QueueList => observation::queue_listing(
                        &response.body,
                        &self.config.endpoint,
                        &self.config.observed_parameters,
                    )
                    .map(Response::QueueList),
                    Request::RecentBuilds { job, limit } => observation::recent_builds(
                        &response.body,
                        job,
                        limit,
                        &self.config.observed_parameters,
                    )
                    .map(Response::RecentBuilds),
                    Request::Queue { id } => {
                        let queue: QueueItem = serde_json::from_slice(&response.body)
                            .map_err(|_| Error::InvalidResponse)?;
                        if queue.id != id
                            || queue.executable.as_ref().is_some_and(|e| e.number == 0)
                        {
                            return Err(Error::InvalidResponse);
                        }
                        Ok(Response::Queue(queue))
                    }
                    Request::BuildStatus { number, .. } => {
                        let build: BuildInfo = serde_json::from_slice(&response.body)
                            .map_err(|_| Error::InvalidResponse)?;
                        if build.number != number {
                            return Err(Error::InvalidResponse);
                        }
                        Ok(Response::Build(build))
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
}
impl<T> Client<T> {
    fn queue_location(&self, location: &str) -> Option<u64> {
        if location.len() > 4096 || location.chars().any(char::is_control) {
            return None;
        }
        let url = self.config.endpoint.join(location).ok()?;
        if url.origin() != self.config.endpoint.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return None;
        }
        let expected = format!("{}queue/item/", self.config.endpoint.path());
        let id = url.path().strip_prefix(&expected)?.strip_suffix('/')?;
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) || id.starts_with('0') {
            return None;
        }
        let parsed = id.parse::<u64>().ok()?;
        (parsed > 0).then_some(parsed)
    }
}
