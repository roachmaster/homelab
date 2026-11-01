# Optional HTTPS node-info adapter

**Implemented local demonstration, not a deployed remote-management daemon.**
One unary read-only operation crosses real TCP, mutual TLS and HTTP/1.1, then the
same typed Forge bridge. No disk, IDE, job, process or mutation operation is
exposed. The server intentionally binds **loopback only** in this first slice.

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
   runtime. The server factory then binds its configured loopback socket; the
   client factory does **not** connect. Construction failure publishes no endpoint.
5. The host selects `Singleton` or `Transient` and injects the client into its
   task. A transient client gets a new owner/runtime on acquisition; a transient
   server rebinds and may fail if a fixed port is already occupied. There is no
   service locator or implicit pool. The example uses a singleton.

A separate Cargo consumer under `tests/consumer` imports these public exports;
its disabled feature omits the entire transport dependency tree. Existing local,
IDE and template consumer profiles also omit this package. TLS/HTTP/serde/Tokio
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

The only wire endpoint is `POST /forge/v1/node`, with versioned strict typed JSON
and `application/json`. Unknown fields/variants/versions fail; a body cannot choose
an unregistered handler or spoof an authenticated subject. There are no redirects,
retries, keep-alive request loops, compression, discovery or arbitrary URL paths.

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
and the [transport-neutral conversation](../forge-remote/README.md). A network
reachable deployment, stronger lifecycle management and any further operation
require their own explicit security/operational design.
