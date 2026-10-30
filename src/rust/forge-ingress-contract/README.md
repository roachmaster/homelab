# forge-ingress-contract

Std-only typed contracts, independent of dispatch, providers and business policy.
Public entry points: `RouteKey`, `Header`, `Envelope<P>`, `ResponseHeader`,
`Response<R>`, `Payload`, `AdmissionPolicy<C>`, and controlled metadata/admission
errors. No serialization or default authentication policy is supplied.

The consumer owns its enum and `Payload::Kind`. Implement `kind` as a pure,
faithful variant classification, including inner kinds for a composed application
sum. It does not select the route. A single shared registry uses one `P` and one
result type; multiple capabilities require explicit application-owned sums or
separate typed entry points. Headers carry operation version, request ID and
optional correlation ID; deadline/idempotency/envelope-schema versioning are not
implemented in this slice.

No deployment configuration or infrastructure is required. From **`src/rust/`**:

```bash
cargo fmt --all -- --check
cargo test -p forge-ingress-contract --offline --locked
cargo clippy -p forge-ingress-contract --all-targets --offline --locked -- -D warnings
```

See [workspace commands and limitations](../README.md) for toolchain and build/run
checks. Header tests establish metadata validation and correlation preservation,
not trust in a caller's identity.
