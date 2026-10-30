# forge-provider

Std-only construction and acquisition, with no dependency on routing or business
types. `Factory<T>` supports explicit configured closures. `InstanceProvider<T>`
returns a lifetime-aware associated lease that dereferences to `T`.

Implemented scopes:

- `Singleton<T>` owns one prebuilt value and lends shared references; singleton
  means per provider, not process-global.
- `Transient<F>` invokes its factory on each acquisition and returns an
  `OwnedLease<T>`. The lease drops its owned value or transfers it through
  `into_inner`; it is not a pool lease. Captured backing can still be shared.

The GAT provider trait is not dyn-compatible. These wrappers add no concurrency
safety beyond Rust's contained-type bounds. There is no pool, request scope,
service discovery, async reset, automatic fallback or host shutdown implementation.
A factory must clean partial internal acquisition on error before ownership is
transferred. `Drop` cannot await cleanup or reverse external effects.

No runtime config is read implicitly. From **`src/rust/`**:

```bash
cargo fmt --all -- --check
cargo test -p forge-provider --offline --locked
cargo clippy -p forge-provider --all-targets --offline --locked -- -D warnings
```

Tests cover reuse and isolation, transient creation/error/drop behavior,
non-static borrowing and typed substitution. A compile-fail doctest protects the
borrowed singleton lifetime. See [workspace checks](../README.md) and the
[longer proposed DI design](../../../docs/architecture/dependency-injection.md).
