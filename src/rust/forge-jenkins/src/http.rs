//! Optional native synchronous HTTP/TLS transport. No redirects, ambient proxy,
//! cookies or implicit retries. Construction performs no network operations.
use crate::{
    register, Client, ClientFactory, Config, Credentials, Error, HttpRequest, HttpResponse, Limits,
    Method, TransportError,
};
use forge_bootstrap::PreparedGroup;
use forge_io_client::IoClient;
use forge_provider::Factory;
use std::{collections::BTreeMap, io::Read};

pub type HttpClient = Client<HttpTransport>;
pub type HttpClientFactory = ClientFactory<HttpFactory, HttpTransport>;

#[derive(Clone)]
pub struct HttpFactory {
    limits: Limits,
}
impl HttpFactory {
    pub fn new(limits: Limits) -> Result<Self, Error> {
        Ok(Self {
            limits: limits.validate()?,
        })
    }
}
impl Factory<HttpTransport> for HttpFactory {
    type Error = Error;
    fn create(&self) -> Result<HttpTransport, Error> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(self.limits.timeout))
            .max_redirects(0)
            .max_response_header_size(32768)
            .http_status_as_error(false)
            .proxy(None)
            .build()
            .into();
        Ok(HttpTransport {
            agent,
            limits: self.limits,
        })
    }
}
#[derive(Clone)]
pub struct HttpTransport {
    agent: ureq::Agent,
    limits: Limits,
}

pub fn factory(config: Config, credentials: Credentials) -> Result<HttpClientFactory, Error> {
    let transport = HttpFactory::new(config.limits())?;
    ClientFactory::new(config, credentials, transport)
}
pub fn register_http(
    config: Config,
    credentials: Credentials,
) -> Result<PreparedGroup<HttpClientFactory>, Error> {
    let transport = HttpFactory::new(config.limits())?;
    register(config, credentials, transport)
}

fn classify(error: ureq::Error) -> TransportError {
    match error {
        ureq::Error::Timeout(_) => TransportError::Timeout,
        _ => TransportError::Unavailable,
    }
}
impl IoClient for HttpTransport {
    type Request = HttpRequest;
    type Response = HttpResponse;
    type Error = TransportError;
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let timeout = request.limits.timeout.min(self.limits.timeout);
        let max_body = request
            .limits
            .max_response_bytes
            .min(self.limits.max_response_bytes);
        let mut response = match request.method {
            Method::Get => self
                .agent
                .get(&request.url)
                .config()
                .timeout_global(Some(timeout))
                .build()
                .header("Authorization", request.authorization())
                .header("Accept", "application/json")
                .call(),
            Method::Post => self
                .agent
                .post(&request.url)
                .config()
                .timeout_global(Some(timeout))
                .build()
                .header("Authorization", request.authorization())
                .header("Accept", "application/json")
                .header(
                    "Content-Type",
                    request
                        .content_type
                        .unwrap_or("application/x-www-form-urlencoded"),
                )
                .send(request.body.as_slice()),
        }
        .map_err(classify)?;
        let status = response.status().as_u16();
        let mut headers = BTreeMap::new();
        let mut header_bytes = 0;
        for (name, value) in response.headers() {
            header_bytes += name.as_str().len() + value.len();
            if header_bytes > 32768 {
                return Err(TransportError::InvalidResponse);
            }
            if matches!(name.as_str(), "location" | "x-jenkins")
                && headers
                    .insert(
                        name.as_str().to_owned(),
                        value
                            .to_str()
                            .map_err(|_| TransportError::InvalidResponse)?
                            .to_owned(),
                    )
                    .is_some()
            {
                return Err(TransportError::InvalidResponse);
            }
        }
        let mut body = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(max_body as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::TimedOut
                    || error
                        .get_ref()
                        .and_then(|error| error.downcast_ref::<ureq::Error>())
                        .is_some_and(|error| matches!(error, ureq::Error::Timeout(_)))
                {
                    TransportError::Timeout
                } else {
                    TransportError::Unavailable
                }
            })?;
        if body.len() > max_body {
            return Err(TransportError::BodyLimit);
        }
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}
