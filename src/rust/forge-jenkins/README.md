# Forge Jenkins I/O client

`forge_jenkins` is an optional synchronous `forge_core::io_client::IoClient` capability.
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
use forge_core::bootstrap::Registry;
use forge_core::provider::{Factory, InstanceProvider, Singleton};
use forge_core::io_client::IoClient;
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
submission with optional parameters, queue inspection, build inspection, bounded
queue/recent-build snapshots, queue cancellation and graceful build-stop requests. `Request::validate` is pure and reusable by an application Task
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
stopped. `StopRequested` likewise acknowledges only a graceful `POST .../stop`
request; callers must observe `building=false` separately. No `/term` or `/kill`
endpoint is used. Build-submit timeouts, server failures, lost/invalid acceptance receipts
and cancellation uncertainty return `MutationUncertain`; reconcile instead of
blindly retrying. Neither this adapter nor Ureq3 performs implicit retries.
Unexpected write response statuses, including unverified 2xx/3xx responses,
remain uncertain; explicit 401/403 and 404 rejections retain their typed errors.

Only numeric loopback HTTP endpoints are accepted for local Mission Control.
Other endpoints require HTTPS with the native transport's standard trusted roots.
Userinfo, fragments, query strings and ambiguous context paths are rejected in
startup configuration. Defaults: total10-second I/O deadline, 1MiB response-body
limit, 32KiB response-header limit. The native transport disables ambient proxies
and all redirects, including credential forwarding; JSON-returned URLs are never
followed. Queue acceptance Locations are validated and reduced to a positive ID.
When a custom native factory and typed client specify different budgets, each
request uses the shorter deadline and smaller response limit of the two.
The blocking deadline does not preempt host computation or an injected transport.
Native factory instances own the Ureq agent/pool; a cloned client shares that
agent's owned pool, and dropping all clones releases it. No daemon is started.

For reconciliation after an uncertain submission, `QueueList` reads at most50
items and `RecentBuilds { job, limit }` reads 1–50 recent builds. Both request one
extra item through Jenkins's `tree` range syntax and return `truncated` rather
than claiming the bounded snapshot is complete. A truncated or failed read does
not prove that a submitted action is absent. The host owns action identity,
matching, subsequent observations and retry decisions; this adapter never
resubmits or polls.

`BuildPage { job, offset, limit }` is the additive read-only history operation.
It accepts a zero-based offset from 0 through `MAX_BUILD_PAGE_OFFSET` (100000)
and a limit of 1–50. One GET requests the selected interval and one extra
sentinel using Jenkins's [inclusive/exclusive tree ranges](https://www.jenkins.io/doc/book/using/remote-access-api/).
`BuildPageListing` returns `job`, `offset`, `builds`, `truncated` and `next_offset`.
The next offset is present only when that sentinel proves more history exists;
an empty or short terminal page returns `null`. Ordering must be strictly
newest-first, including the sentinel; duplicate or reversed build IDs fail.
Parameter privacy and transport/response budgets are unchanged.

The host must bound the entire scan and reject page-budget exhaustion rather
than interpreting it as absence. Offset pages are not a transaction: a host
must also detect repeated IDs, changed history and incomplete queue observations
before admitting or reconciling a submission. This client performs no automatic
pagination, restart, polling or mutation. `RecentBuilds` remains unchanged.

Snapshot `parameters` are empty by default. Select **public** correlation names
using `Config::with_observed_parameters(["MY_ACTION_ID"])`; at most64 unique ASCII
identifier names are accepted. Only selected string values of at most4096bytes,
without control characters, are returned. Password/secret parameter classes are
omitted, and ambiguous duplicate selected parameters fail closed. Each response
has the configured byte limit; list counts, action/parameter counts, job URLs and
identities are validated. Queue job URLs must identify a canonical job below the
configured same-origin Jenkins context and are never followed. No credential or
arbitrary unselected parameter is included in the projected DTO's Debug/JSON.

Verification includes injected transport boundary tests, real owned-loopback HTTP
tests and the separate Cargo consumer under `tests/consumer`, which imports public
library exports and proves register→freeze→create→provider→execute, dependency
preservation, duplicate rejection, deferred construction and typed errors. Fakes
do not certify remote Jenkins/TLS behavior. Loopback tests use only fake tokens
and owned temporary listeners. Live controller qualification is recorded by the
host separately. No build is submitted by the test suite or examples.

References: [Jenkins scripted-client authentication](https://www.jenkins.io/doc/book/system-administration/authenticating-scripted-clients/),
[CSRF and API tokens](https://www.jenkins.io/doc/book/security/csrf-protection/),
[remote access API](https://www.jenkins.io/doc/book/using/remote-access-api/),
[asynchronous build abort](https://www.jenkins.io/doc/book/using/aborting-a-build/),
and [official tree-range parser](https://github.com/jenkinsci/stapler/blob/master/core/src/main/java/org/kohsuke/stapler/export/NamedPathPruner.java).

### Native v4 policy

`forge_jenkins::native_policy` is a pure shared contract for the native verifier,
publication producer, review worker and release actor. `native_phases_for_scope`,
`native_gate_contract`, `native_coverage`, `native_stage_evidence`,
`native_policy_descriptor`, `native_policy_sha256` and `native_protects` perform
no acquisition or execution. The profile is `forge-sdlc-jenkins-v4`, with numeric
public-spool/protected-result version 4 and gate identity `<profile>:<scope>`.
Persisted DB/binding requests keep their existing identity fields without a numeric
version; the normal producer converts them to the distinct public spool envelope. Valid scopes are
`main`, `detached`, or the closed `branch/<slug>` form.

The 45 branch / 46 main phases retain the feature matrix, development quality
checks and real-browser journey. Their backend tail is `make backend-suites
SCOPE=branch` or `SCOPE=main`; the latter requires actual disposable PostgreSQL
with zero silently skipped database coverage. Repository build/test Python tools
remain development commands; the installed verifier and its root-owned policy,
process supervision and artifact export are native Rust.

The descriptor has version 1 and contains `verification_profile`, closed source
protection rules, branch/main phase lists and branch/main coverage. Its checksum
is SHA256 of compact recursively key-sorted JSON without a trailing newline.
Installed v4 delivery configuration binds an absolute `policy_path` to adjacent
immutable `policy.json`, `policy_sha256`, and separately `verifier_sha256`.
Historical profiles retain their existing command and receipt acceptance rules;
the new profile never substitutes Rust commands inside historical receipts.
