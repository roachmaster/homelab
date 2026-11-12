# Optional HTTPS command gateway

**Implemented transport library, not a deployed remote-management service.**
The default profile retains the original unary `node.info` client/server. The
opt-in `ide` feature adds a typed `ide.inspect` client and private report codec;
[forge-agent](../forge-agent/README.md) composes these capabilities behind one
bridge and application policy. No job, process or filesystem-mutation operation
is exposed. The original `ServerConfig` remains loopback-only; the new gateway
config requires an explicit opt-in before binding a non-loopback address.

```bash
# repository root; fetch locked dependencies once
cargo fetch --manifest-path src/rust/Cargo.toml --locked
make remote
# equivalent:
cargo run --manifest-path src/rust/Cargo.toml --offline --locked \
  -p forge-remote-https --example loopback
```

The example creates ephemeral test identities **in memory**, assembles both
endpoints through public registrars/factories/providers, observes the configured
node once and exits. It changes no system trust store, device, firewall or
persistent credential. `make remote` deliberately has no endpoint/credential
`ARGS` interface; it is a fixed demonstration, not a generic shell or deployment
command. The existing Forge terminal remains local.

## Public integration, not example-only wiring

The library exports `ClientConfig::MutualTls`, `ServerConfig::LoopbackMutualTls`,
`Credentials`, `CertificateIdentity`, `Limits`, `register_client`,
`register_server`, `ClientFactory`, `ServerFactory`, `HttpsClient` and `HttpsServer`.

1. Bootstrap supplies explicit bounded DER certificate/key material, trust roots,
   target identity, peer mapping and budgets. No implicit system roots, environment
   reads or credential-file lookup occur in this library.
2. A registrar validates configuration shape and stages one complete factory group
   in `forge-bootstrap`. No TLS parsing, runtime creation or socket binding yet.
3. Consuming registration rejects conflicts without overwriting the original;
   freeze validates declared assembly metadata before construction.
4. `Factory::create` validates TLS material and creates one private current-thread
   runtime. The server factory then binds its configured socket; the
   client factory does **not** connect. Construction failure publishes no endpoint.
5. The host selects `Singleton` or `Transient` and injects the client into its
   task. A transient client gets a new owner/runtime on acquisition; a transient
   server rebinds and may fail if a fixed port is already occupied. There is no
   service locator or implicit pool. The example uses a singleton.

A separate Cargo consumer under `tests/consumer` imports these public exports;
its disabled feature omits the entire transport dependency tree. Existing local,
IDE and template local consumer profiles also omit this package. The default HTTPS
profile omits `forge-ide`; enabling `ide` adds only its remote capability profile,
not its CLI/YAML output or the agent application. TLS/HTTP/serde/Tokio
are normal dependencies **only of this selected adapter**. `rcgen`, the concrete
kernel and example task wiring are dev dependencies, not provisioning code shipped
as part of its normal library tree. JSON DTOs are private; typed enums/results
cross the application boundary, never `serde_json::Value`.

## Authenticated and authorized path

- Standard rustls verification checks the server chain/name and requires a valid
  client certificate chain. Trust roots and expected server name are explicit.
- After the handshake, an exact verified client leaf certificate must match one
  configured `CertificateIdentity`; its subject is not taken from wire claims.
- That subject reaches `Destination`'s separate injected operation policy. A mapped
  identity can still be forbidden. The server checks the target node/incarnation,
  operation/version, expiry and registered typed payload before source calls.
- The client checks the returned target/incarnation, operation/version and original
  request/correlation metadata before accepting the projection. Config maps the
  expected logical target to a network address and TLS server name; a returned
  node label alone is not authentication.

This is peer authentication, **not end-user delegation**, multi-tenant authorization
or a general policy engine. TLS session resumption/early data are disabled. No production certificate issuance, rotation, CRL/OCSP
revocation policy or secret storage is provided. Private-key input is kept in
caller-owned/adapter memory and redacted from Debug; no universal memory-zeroization
or protection from a compromised process is claimed. Trusted policy/source code
can still violate its contract.

## Bounded behavior and failure semantics

The single capability-neutral gateway is **`POST /forge/v1/dispatch`**. The path
identifies the gateway protocol, never a task/capability. This is an RPC-style
HTTP entry point, not a collection of resource-oriented REST endpoints.

### HTTP header projection

HTTP field names are case-insensitive. Every required field must occur exactly
once; the optional correlation field at most once. Duplicate field lines, including
case variants or comma-combined routing values, fail rather than picking a winner.
Values are bounded ASCII; this adapter rejects non-ASCII canonical metadata before
opening a client connection rather than inventing an encoding.

| Header | Meaning / bound |
| --- | --- |
| `Forge-Envelope-Version` | Canonical unsigned decimal `1`, matching `/forge/v1/dispatch` |
| `Forge-Destination` | Canonical Header destination, `node.info`, or opt-in `ide.inspect`; 1–256 bytes |
| `Forge-Contract-Version` | Nonzero canonical unsigned decimal u16, currently registered `1` |
| `Forge-Target-Node` | Configured logical target; 1–128 ASCII token bytes |
| `Forge-Target-Incarnation` | Expected host incarnation; same token bound |
| `Forge-Request-Id` | Original request ID; 1–256 bytes, no whitespace/control |
| `Forge-Correlation-Id` (optional) | Original correlation ID; same bound |
| `Forge-Expires-Unix-Ms` | Original absolute expiry; canonical unsigned decimal u64, at most 20 digits |

The node `application/json` body is only `{"payload":"info"}`. It cannot provide or
override destination, target, version, deadline, request IDs or identity. Strict
body decoding rejects unknown fields and payload variants. The adapter projects
headers into the existing typed Header/NodeCall; the destination bridge remains
the authoritative route/version/authorization/variant gate. Payload contents never
select a destination. The authenticated principal still comes solely from verified
TLS peer mapping—not from any HTTP header. The successful response retains the
existing private, versioned DTO and checked request/target metadata.

Only POST invocation is implemented. Other methods on this path return **405**
with **`Allow: POST`**, without executing a task; other paths return **404**.
GET, PUT, PATCH and DELETE are not fake aliases. A future GET operation must be
read-only and use no request-body convention; replacement/update/deletion methods
need an actual operation contract and independent authorization. Such methods can
share this gateway path without changing header-selected dispatch. There are no
redirects, retries, keep-alive request loops, compression, discovery or per-task URLs.

**Wire compatibility correction:** the first experimental `/forge/v1/node` path
and body-carried routing metadata are removed, without a silent fallback. Existing
Rust config/client/task APIs and `make remote` remain unchanged; raw HTTP callers
must switch to this path and header projection. Request-envelope and task-contract
versions are separate; an unsupported envelope version is rejected, not inferred
from a body or repaired from the URL.

Default body cap is 4096 bytes; configuration permits 256–65536 bytes. Incoming
bodies are counted while collected. HTTP parsing uses hyper with 16 headers and an
8192-byte connection buffer cap; TLS has its own protocol buffers, so this is not
a universal whole-process memory quota. Certificate chains/keys/mappings and time
budgets are also bounded by registration validation. Default connection budget is
5 seconds, configurable above zero through 30 seconds. `serve_one` processes at
most one connection/request and includes accept/handshake/headers/body/response in
its wait budget. No unbounded worker/task spawning is introduced.

An absolute caller expiry is never reset on transmission. Local monotonic wait
budgets additionally cap client and server work; clocks still need adequate
cross-node synchronization. Synchronous policy/observation code cannot be
preempted, but expiry is checked again before observation and before accepting a
result. A timeout or lost reply can mean the read occurred; `may_have_executed`
reports that uncertainty, not cancellation or exactly-once execution. Invalid/oversized
responses can also follow observation; other error variants are not universal proof
of non-execution. Error
categories omit credentials and native diagnostic strings.

Owners are blocking, single-owner objects, not async services. Construction and
calls reject an ambient Tokio runtime; use a separately owned blocking boundary
when embedding later in an async host. This is not permission to spawn an unbounded
thread per request. The destination is created on and remains on its owner thread;
no `Send`/`Sync` retrofit to the core bridge is implied. Idle adapter destruction
uses nonblocking runtime shutdown (no adapter-spawned work); it does not interrupt
an already-blocked trusted callback.

See [design, limits and primary API sources](../../../docs/architecture/remote-node-info.md)
and the [transport-neutral conversation](../forge-remote/README.md). The new gateway supports deliberate network binding, but deployment, certificate
operations and production hardening still require explicit operational design.
Only local-loopback tests are claimed, not two physical machines.

## Reusable agent gateway and IDE integration

`gateway::{GatewayServerConfig, GatewayLimits, register_gateway_server,
GatewayServerFactory, GatewayServer}` follows the same stage → freeze → create →
provider lifecycle. Registration does not parse credentials, bind or dispatch.
A duplicate group is rejected without replacing the original. Existing public
`ServerConfig`/`ClientConfig`, `register_server`/`register_client` and `serve_one`
remain compatible, rather than extending an exhaustive config enum silently.

`GatewayCodec` is a static generic port with associated `Request`/`Response` types,
not a dynamic service locator or framework-global business enum. The application
supplies its own finite sum and composes `NodeCodec` plus `ide::IdeCodec`. Codec
selection uses the already validated header destination/version. The adapter
accepts the same single `POST /forge/v1/dispatch`; it does not add `/ide` or
project-specific endpoints. Node clients also work through this generic gateway.

After mTLS leaf mapping, `serve_next(&codec, dispatch)` passes a **privately
constructed** `VerifiedContext` exposing only `subject()` and `deadline()`.
Applications still must check target/incarnation, authorize operation **and project**,
and use their shared typed bridge before reading a project. Possession of a mapped
peer certificate does not itself grant project access. A codec/dispatcher is
trusted host code; the generic API cannot stop a malicious implementation from
ignoring these obligations, fabricating data, allocating, blocking or panicking.
No headers or body fields can manufacture a `VerifiedContext`.

The IDE request body is `{"payload":{"kind":"project","alias":"demo"}}`.
It carries a validated `ProjectAlias`, not a filesystem path, and cannot override
header fields. The agent maps an allowlisted alias to a preconfigured inspection
handle. `ide::{IdeClientConfig, register_ide_client, IdeClientFactory,
HttpsIdeClient}` supplies a reusable provider-ready client implementing the typed
`RemoteIdeClient` contract through its `IoClient` associated types. Reports remain
`forge_ide::model::ProjectReport`; private strict DTOs preserve native name encodings,
hierarchical declarations/members/spans/counts and incomplete/error entries.

Responses are collected and serialized with explicit byte limits, then validated
against `ReportLimits`, exact responder/header/request/correlation metadata and
the original deadline **after decoding and structural validation**. Unknown fields,
unknown enum cases, invalid counts/hierarchy, and oversized responses fail closed.
Serialization limits are checked as bytes are written; no partial report is exposed
as success. JSON deserialization also retains serde_json's recursion limit. An independent,
string-aware `MAX_JSON_NESTING=96` container cap applies before decoding and before
returning encoded success. Combined tree/syntax nesting can exceed that wire cap
even when each domain-depth bound is valid; this returns `Limit`, not an
unreadable successful response. This
is bounded protocol validation, not a guarantee of truthful source or a whole-process
memory/CPU quota: typed tree construction/conversion has overhead, and trusted
synchronous syntax parsing cannot be preempted. Reports can reveal native names,
type spelling and diagnostics; there is no secret-redaction guarantee.

`GatewayLimits` separates request bytes (default 4KiB; 256..64KiB) from response
bytes (default 4MiB; 256 bytes..16MiB), connection timeout (>0..30s, default 5s) and
idle accept polling (>0..1s, default 100ms). `IdeClientConfig` separately caps received
response bytes with the same 16MiB ceiling. `IdeCodec` rejects request input above 64KiB
and reply input above 16MiB even when called directly. A lower application's budget
may reject a report the producer otherwise considers valid.

`ServeOutcome::Idle` means no connection arrived during `accept_poll`;
`Handled` means one connection/request completed (including typed rejection).
An error terminates that connection, not a hidden accept/retry loop. The host owns
its bounded serving loop and stop policy. Accept polling does **not** preempt an
active callback; shutdown needs admission rechecks before effects and waits for
bounded in-flight work, or a host failure policy. One current-thread runtime and
one connection are used at a time: no per-request runtime/thread spawning and no
new `Send`/`Sync` requirement for core bridges. Runtime construction/use stay on a
synchronous owner; ambient Tokio calls fail. Idle Drop closes the socket and uses
nonblocking runtime shutdown, not forced callback cancellation.

Tests include the external consumer's `https` / `ide` / omitted profiles,
original node endpoint regressions, real ephemeral-mTLS IDE round trips, policy
rejection, response caps, expiry/uncertain results, strict codec negatives, idle
polling and listener cleanup. Test keys exist only transiently; the normal library
never generates credentials or modifies trust stores.

## Source adapter (`ide` feature)

`source::{register_source_client, SourceClientFactory, HttpsSourceClient, SourceCodec}`
provides the typed `ide.source@1` adapter. Registration validates explicit transport
configuration without I/O; factory creation builds the retained mTLS client. Calls
carry an authorized project alias, bounded native-name components and the digest
from inspection. Responses preserve target, request/correlation headers and exact
UTF-8 bytes. The adapter verifies both the expected digest and the returned text's
SHA-256 before publishing a result, with a 1 MiB source limit and bounded JSON.
It does not open files, select projects or retry failed/changed reads.
