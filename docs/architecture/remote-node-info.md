# First remote boundary: authenticated node.info

**Implemented narrow slice:** typed node-info contracts and an optional HTTPS
adapter, exercised over real loopback TCP. **Not implemented:** a network-reachable
agent deployment, generic remote jobs, remote IDE/file access, credential operations,
end-user delegation, gRPC or Bitwave. This is a replaceable transport boundary, not
an OS or a promise of location-transparent calls.

## Why this first choice

For one unary read, HTTPS avoids protobuf generation and an async public service
model. Hyper supplies HTTP/1 parsing and rustls supplies TLS authentication; a
private current-thread Tokio runtime drives those adapters. This is **our scoped
design decision**, not a claim that HTTPS is inherently better than gRPC. A later
gRPC adapter can implement the same typed port without changing the task, core
bridge or local node observation. Different failure/security guarantees would
still require explicit conformance work; matching method signatures is insufficient.

## Package and ownership map

| Package/module | Owns | Imports / must not import |
| --- | --- | --- |
| `forge-remote` | `Target`, typed `NodeCall`/`NodeReply`, `RemoteNodeClient`, `ReadNodeTask`, injected observation/policy seams, destination bridge | Existing ingress/bridge/task/I/O contracts; no TLS, HTTP, serde, kernel or runtime |
| `forge-remote-https::assembly` | Public typed config, DER credentials, bounded peer mappings, staged factories/registrars | Bootstrap/provider and selected transport libraries; no runtime service lookup |
| `forge-remote-https::transport` | Single-owner HTTPS client/server, standard certificate verification, peer mapping, I/O budgets | Typed remote port; no direct kernel/business-handler calls |
| `forge-remote-https::wire` (private) | Strict versioned request/reply DTOs and bounded JSON conversion | No JSON/value-tree fallback escapes into task or bridge |
| `examples/loopback.rs` | Concrete kernel-to-observation adapter, operation policy, chosen providers and ephemeral demo trust | Composition only; no reusable integration hidden in the example |

The second crate is justified by an actual replaceable network dependency boundary,
not one crate per type. Core crates and existing local/template/IDE profiles do
not import either network implementation. The separate consumer fixture proves
public registrar imports and whole-package omission. The dependency guard rejects
TLS/codec leakage inward and verifies real resolved consumer trees.

## Bootstrap, then call

The host prepares `ClientConfig::MutualTls` and
`ServerConfig::LoopbackMutualTls` with explicit DER credentials/trust roots and
limits. Public `register_client` / `register_server` stage configured factories
without parsing TLS or opening sockets. Host registration/freeze precedes explicit
`Factory::create`. TLS/key errors occur at construction; client construction does
not connect, server construction binds only a loopback address. Providers remain
host-selected; failure returns no completed host and dropping earlier owned
resources closes them. There is no automatic graph executor or universal DI container.

At runtime:

1. `ReadNodeTask<C>` prepares `node.info @ 1`, the typed `Info` variant, original
   request/correlation IDs, target node/incarnation and caller expiry.
2. The HTTPS client checks its configured target, projects canonical routing metadata
   into bounded HTTP headers, encodes the typed payload body and performs
   one deadline-bounded call. Server trust/name checking and client-certificate
   authentication use rustls, never a custom cryptographic verifier.
3. The server requires an exact configured mapping for the **verified** client leaf.
   Wire/header subject claims are not accepted. That mapped subject is passed only
   through the trusted in-process `Destination::dispatch_verified_before` seam.
4. Destination checks target/expiry. The **existing shared bridge** selects the
   registered header route/version, invokes the independent operation policy,
   rechecks time after policy, checks payload-kind agreement, then calls the injected
   local source. Only `Info` is representable; other wire payload variants fail
   decoding. Unknown versions/routes never become handler names or DI keys.
5. The source adapts the existing kernel node observation. It reports configured
   node ID and compile-target OS/architecture, not uptime/health/hardware probes.
6. Reply validation checks exact responder/incarnation, route/version,
   request/correlation and bounded fields. Final task validation protects against
   a substituted client returning mismatched typed data too.

The trusted subject seam is intentionally explicit: it is not a sealed proof that
arbitrary Rust callers authenticated a peer. The actual HTTPS boundary only supplies
TLS-derived subjects. A future broader identity package can replace this small seam;
no generic access/delegation framework is claimed here. Authentication grants no
default operation access: even a mapped peer can be denied by destination policy.

## Wire and security limits

The only entry path is **`/forge/v1/dispatch`**, not a task/capability URL.
Current invocation uses POST. This is a single RPC-style HTTP gateway, not a claim
of resource-oriented REST. Unsupported methods return 405 with `Allow: POST`;
unknown paths return 404. Neither response executes a business task. Future GET,
PUT, PATCH or DELETE support requires an actual operation and method semantics;
GET must remain read-only and have no request-body convention. No file deletion
or other new capability follows from naming an HTTP verb.

The [exact HTTP header projection](../../src/rust/forge-remote-https/README.md#http-header-projection)
puts destination/contract version, target/incarnation, request/correlation IDs,
absolute expiry and envelope version in bounded, uniquely occurring `Forge-*`
headers. It creates the unchanged typed Header/NodeCall. The body contains only
`{"payload":"info"}`; it cannot override routing or identity. Missing, duplicate
(including case variants), malformed or over-bound required metadata fails closed.
Unsupported envelope versions fail rather than silently disagreeing with the v1
path. Only the registered header route selects the destination; a typed body must
still agree and never supplies a fallback route.

The Rust client/config/port APIs remain compatible. **The experimental wire format
changes deliberately:** `/forge/v1/node` and body-carried routing metadata are no
longer accepted. Raw HTTP callers must migrate together with this first slice;
there is no legacy alias that restores body-driven routing. Successful reply DTOs
and their exact metadata checks remain unchanged. There is no arbitrary command,
filesystem path, global business enum or reflective dispatch field.

Configured CA roots plus the expected server name establish server trust; bootstrap
maps that endpoint to a logical target. Client chain verification plus an exact leaf
allowlist establishes the peer subject. A matching certificate still needs an
operation grant. TLS resumption/early data are disabled for this one-shot slice.
Certificate issuance/rotation/revocation policy, remote trust deployment, end-user
delegation and production secret storage are outside this implementation. The demo
uses ephemeral in-memory identities only; it does not install trust or create
operational credentials. Key-bearing Debug output is redacted, but process-memory
secrecy/zeroization is not universally guaranteed.

Default body and wait caps are 4096 bytes and 5 seconds; configured ranges are
256–65536 bytes and greater than zero through 30 seconds. Headers are capped at
16 with an 8192-byte Hyper connection buffer. Bodies are bounded during collection,
and projections/metadata/certificates/mappings have explicit field/count caps.
These are application/protocol limits, not a whole-process/TLS allocator proof.
`serve_one` admits one connection/request at a time with no worker/task spawning;
its total wait includes accept, TLS, headers, body and response. The server is
loopback-only by policy, so a reachable daemon requires a separately reviewed change.

Absolute expiry travels unchanged, requiring adequately synchronized wall clocks
across hosts. Each receiving owner converts remaining time into monotonic waits;
the server may shorten the budget with its connection deadline. It never gives a
retry a fresh deadline. Checks after policy prevent delayed authorization from
starting an observation after the local budget. Synchronous trusted callbacks are
not preemptible; their panics propagate and a stalled callback can stall its owner.
Late observation results are rejected rather than reported as timely success.

`Deadline`/`Transport` errors carry conservative `may_have_executed` information.
Malformed/oversized replies can also follow completed observation; do **not** infer
non-execution from every other error category. There are no automatic retries,
deduplication, durable acceptance or cancellation acknowledgments. A failed wait
does not undo work, even though this first operation is read-only.

## Compatibility and next gates

Existing core APIs and local application enums are unchanged. No mandatory transport
or runtime is added to Task, IoClient or Bridge. The new task's client and destination
source/policy are substituted through typed injection. The private runtime is an
adapter implementation detail, not permission to call blocking methods inside an
async runtime. Constructors/calls reject that context; idle runtime cleanup is
nonblocking and does not rescue a currently blocked trusted callback.

Before remote deployment: decide endpoint exposure, credential issuance/rotation
and revocation, host-incarnation lifecycle, observability/redaction and shutdown,
rate/capacity policy, clock assumptions and the deployment trust boundary. Before
another operation: define its authorization/resource rules, codec/versioning and
uncertain-outcome semantics, then add conformance tests. Do not expose disk/IDE or
jobs merely because a network channel exists. Bitwave remains a separate future
research idea, not a dependency or compatibility requirement of this slice.

## Verification and sources

Run `make ci` for workspace build, debug/release tests, strict Clippy/rustdoc,
minimal profiles, helper tests and dependency guards. `make remote` is the real
loopback proof; fake clients are tested separately. The package tests exercise
actual mutual TLS, denied/unmapped peers, wrong server trust/name/target, malformed
wire input, bounded reads, expiry and public consumer assembly. Consult current
check output rather than treating this document as a stored CI result.

Primary API references (facts about protocols/libraries, not endorsements of our design):

- HTTP defines method semantics; 405 responses advertise supported methods through
  Allow. Our choice is POST-only invocation now, not fabricated support for every
  verb. [HTTP semantics and 405](https://www.rfc-editor.org/rfc/rfc9110.html#section-15.5.6).

- rustls config separates server verification and client-certificate presentation;
  built-in root verification does not itself configure revocation. [rustls ConfigBuilder](https://docs.rs/rustls/latest/rustls/struct.ConfigBuilder.html).
- Client-certificate verification can require a chain rooted in explicit trust. [rustls WebPkiClientVerifier](https://docs.rs/rustls/latest/rustls/server/struct.WebPkiClientVerifier.html).
- Hyper exposes HTTP/1 header-count, connection-buffer and header-read-time controls. [Hyper HTTP/1 server builder](https://docs.rs/hyper/latest/hyper/server/conn/http1/struct.Builder.html).
- A current-thread Tokio runtime is driven by its owner; a timeout cannot preempt
  non-yielding synchronous work. [Tokio runtime builder](https://docs.rs/tokio/latest/tokio/runtime/struct.Builder.html), [Tokio timeout](https://docs.rs/tokio/latest/tokio/time/fn.timeout.html).

[Typed contract](../../src/rust/forge-remote/README.md) ·
[Adapter API and usage](../../src/rust/forge-remote-https/README.md) ·
[Mermaid source](diagrams/mermaid/m12-remote-node-info.mmd) ·
[Rendered SVG](diagrams/mermaid/images/m12-remote-node-info.svg).
