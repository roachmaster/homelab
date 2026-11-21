# Forge Jenkins I/O client

`forge_jenkins` is an optional synchronous `forge_io_client::IoClient` capability.
It requires Rust1.89 or newer, matching existing optional workspace capabilities.
Use an installed supported toolchain explicitly, for example `cargo +1.97.1`.
Its default features contain no native HTTP runtime. Feature `http` adds the
blocking Ureq3.5.2 HTTP/TLS transport. Core Forge I/O/provider/bootstrap contracts
do not import this capability. Hosts select and inject their own transport recipe
through `register`; they choose `Singleton` or `Transient` scope explicitly.

Public integration: `Config`, `Credentials`, `Client<T>`, `ClientFactory<F,T>`,
`register`, and feature-gated `http::{factory, HttpFactory, HttpTransport}` plus
`register_http`, `HttpClient`, `HttpClientFactory`. Register stages a validated,
effect-free group; freeze the assembly before calling `Factory::create` and
acquiring the selected provider. HTTP construction performs no network calls.
Custom transport factories implement `Factory<T, Error=forge_jenkins::Error>`;
their transport implements the typed `IoClient<HttpRequest,HttpResponse,...>`
associated-type contract with `TransportError`. No global lookup or runtime
dependency registry is used.

```rust,no_run
# #[cfg(feature="http")]
# fn example() -> Result<(), Box<dyn std::error::Error>> {
use forge_bootstrap::Registry;
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_io_client::IoClient;
use forge_jenkins::{register_http, Config, Credentials, Request};
let group = register_http(
    Config::new("http://127.0.0.1:18084/")?,
    Credentials::new("mission-control", "injected-api-token")?,
)?;
let ((), factory) = Registry::new().register(group)?.freeze()?.into_factories();
let client = Singleton::new(factory.create()?);
let controller = client.acquire()?.execute(Request::Controller)?;
# let _ = controller;
# Ok(()) }
```

Supported prepared requests are controller information, job listing, build
submission with optional parameters, queue inspection, build inspection and queue
cancellation. `Request::validate` is pure and reusable by an application Task
before executing an injected client. Slash-separated job names represent Jenkins
folders; each segment is encoded separately. Empty/dot segments, zero IDs and
oversized parameters are rejected before I/O. JSON responses project typed DTOs;
missing queue flags remain `None`, and an unknown legacy `queueId=-1` remains -1.

Jenkins's native authentication is username + API token using preemptive HTTP
Basic authentication, rather than Bearer. API-token calls are CSRF-exempt; this
adapter does not disable Jenkins CSRF or request a password login/crumb. Hosts
load and protect credentials outside this library. Credentials and wire Debug
output redact authentication and parameter bodies; errors do not include raw
server/transport bodies or headers. The token's server-side permissions remain
those of its Jenkins account. `allow_mutations=false` is a local client policy,
not token scoping or server authorization.

Mutation calls are disabled by default. Explicit `Config::with_mutations(true)`
permits them. HTTP201 plus a same-origin, exact-context canonical queue Location
means **accepted into Jenkins's queue**, not a finished build. `CancelRequested`
means the cancellation request was accepted, not proof that a running build
stopped. Build-submit timeouts, server failures, lost/invalid acceptance receipts
and cancellation uncertainty return `MutationUncertain`; reconcile instead of
blindly retrying. Neither this adapter nor Ureq3 performs implicit retries.

Only numeric loopback HTTP endpoints are accepted for local Mission Control.
Other endpoints require HTTPS with the native transport's standard trusted roots.
Userinfo, fragments, query strings and ambiguous context paths are rejected in
startup configuration. Defaults: total10-second I/O deadline, 1MiB response-body
limit, 32KiB response-header limit. The native transport disables ambient proxies
and all redirects, including credential forwarding; JSON-returned URLs are never
followed. Queue acceptance Locations are validated and reduced to a positive ID.
The blocking deadline does not preempt host computation or an injected transport.
Native factory instances own the Ureq agent/pool; a cloned client shares that
agent's owned pool, and dropping all clones releases it. No daemon is started.

Verification includes injected transport boundary tests, real owned-loopback HTTP
tests and the separate Cargo consumer under `tests/consumer`, which imports public
library exports and proves register→freeze→create→provider→execute, dependency
preservation, duplicate rejection, deferred construction and typed errors. Fakes
do not certify remote Jenkins/TLS behavior. Loopback tests use only fake tokens
and owned temporary listeners. Live controller qualification is recorded by the
host separately. No build is submitted by the test suite or examples.

References: [Jenkins scripted-client authentication](https://www.jenkins.io/doc/book/system-administration/authenticating-scripted-clients/),
[CSRF and API tokens](https://www.jenkins.io/doc/book/security/csrf-protection/),
[remote access API](https://www.jenkins.io/doc/book/using/remote-access-api/).
