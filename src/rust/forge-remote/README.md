# Typed remote node conversation

**Implemented, narrow synchronous contract:** one read-only `node.info @ 1` call.
This optional capability owns no HTTP, TLS, JSON, async runtime or concrete kernel
client. The existing six execution contracts are unchanged.

- `Target` owns separately validated logical node and host-incarnation labels.
- `NodeCall` wraps the existing `Envelope<NodeCommand>` and an absolute expiry;
  `NodeCommand::Info` is the only supported payload variant.
- `RemoteNodeClient` fixes the existing `IoClient` request/response/error types.
  `ReadNodeTask<C>` prepares the canonical header and typed body from `ReadNodeInput`,
  invokes the injected client once, then checks the returned identity/metadata.
- `NodeReply` contains the responder and existing typed `Response<NodeInfo>`.
  `validate_reply` checks node/incarnation, route/version, request/correlation IDs,
  and bounded projection fields. Request IDs do **not** implement deduplication.
- `NodeInfoSource` is the destination's injected local observation port. The demo
  adapts `forge-kernel::node`; this contract does not import that implementation.
- `Destination<A>` registers only `node.info` in the **existing shared bridge**.
  It checks target, expiry, operation authorization and payload agreement before
  observing the source. Invalid projection/source failure is not a success.

## Identity, deadlines and ownership

`dispatch_verified` and `dispatch_verified_before` are **trusted in-process adapter
seams**, not authentication APIs. Their subject must come from a configured
identity verifier, never a request header/body. A malicious trusted Rust caller
can lie; this is not a private-constructor proof or a sandbox. The
[HTTPS adapter](../forge-remote-https/README.md) supplies only its TLS-verified,
explicitly mapped certificate subject. The injected `Authorize` policy separately
grants operation access; authentication does not imply permission.

Absolute Unix-millisecond expiry is carried unchanged. Cross-node evaluation
requires sufficiently synchronized clocks. At destination admission, remaining
wall time is converted to a local monotonic deadline. The optional host deadline
can shorten, never extend, that budget. Checks repeat after the policy callback
and observation, so a late result does not become an on-time success. A trusted
synchronous callback still cannot be preempted; blocking/panicking code is outside
these timing guarantees. Panics propagate, with no rollback or recovery promise.

The destination retains the bridge's single-owner, non-`Send`/non-`Sync` handler
model. It does not gain thread safety from network use. `RemoteError` distinguishes
controlled rejection from a deadline/connection failure whose
`may_have_executed` flag conservatively reports uncertainty. No retries, durable
acceptance, generic remote execution or remote jobs are implemented.

## Proof and extension

`tests/contracts.rs` covers denied/unknown/wrong-target/expired requests before
source calls, post-policy and post-observation timing, preserved headers, invalid
projections, and substituted clients with the unchanged task. Fake-client checks
are not TLS conformance; real endpoints are tested in the HTTPS package.

A second adapter implements this same port and owns its own wire codec/security
boundary. It must preserve these behavioral guarantees, not just compile against
the trait. See the [assembly and request design](../../../docs/architecture/remote-node-info.md).
