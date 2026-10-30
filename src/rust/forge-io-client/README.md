# forge-io-client

`IoClient` is a small synchronous I/O boundary with associated `Request`,
`Response` and `Error` types and `execute(&self, request)`. It has no dependencies
on tasks, envelopes, providers, serialization or a transport. Requests may be
owned, non-Clone capability types; the framework supplies no universal payload.

**The caller/task assembles the business request; the client performs the I/O.**
An eventual API/file/publisher adapter may encode the prepared request, apply its
configured connection/authentication details and map responses/errors. It must
not invent business content, orchestrate a workflow or route the inbound command.
Such adapters are examples of future implementations, not current dependencies.

With associated types fixed, clients can be constructor-injected through
`dyn IoClient<...>` or generic bounds. The trait imposes no automatic retry,
timeout, transaction, durability or concurrency guarantee. Implementations may
block and panic; an error may follow partial I/O. Their actual semantics must be
specified and tested against the real dependency when implemented. Current tests
use in-memory doubles only, including an unchanged non-Clone prepared payload.

From **`src/rust/`**, using the [workspace toolchain](../README.md):

```bash
cargo fmt --all -- --check
cargo test -p forge-io-client --offline --locked
cargo clippy -p forge-io-client --all-targets --offline --locked -- -D warnings
```

The [task/client example](../forge-bridge/examples/task_clients.rs) composes the
contract without network/disk operations. This package owns the execution shape;
consumer capabilities still own their concrete request/result/error vocabulary.
