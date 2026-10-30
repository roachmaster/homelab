# forge-bridge

A synchronous shared entry path depending only on `forge-ingress-contract`.
Create `RegistryBuilder`, register exact route/version keys and expected payload
kinds with typed handler closures, consume it with `freeze`, then construct
`Bridge::new(registry, policy)`. `Bridge::dispatch` is the only public execution
path. Routes are selected from headers, never payloads or dependency names.

The builder preserves an old binding on duplicate failure. Freeze fixes routing
membership; it is not a service-graph validator. Dispatch resolves the header,
asks the mandatory host policy, checks the variant, invokes the handler and
preserves response metadata. `DispatchError<E>` distinguishes unknown route/version,
admission denial, mismatch and original handler error. No automatic retries or
rollback are implied. Closures may borrow dependencies; their erased storage lacks `Send`/`Sync`, so
this bridge cannot be shared across threads unchanged. This is not an async host.
Admission, payload classification/equality and handlers are trusted caller code;
panics propagate without rollback. Handlers receive payload/context, not headers;
header-dependent business metadata requires a future explicit contract.

No deployment config is needed. From **`src/rust/`**:

```bash
cargo fmt --all -- --check
cargo test -p forge-bridge --offline --locked
cargo clippy -p forge-bridge --all-targets --offline --locked -- -D warnings
cargo run -p forge-bridge --example in_process --offline --locked
```

The examples use `forge-provider`, `forge-task` and `forge-io-client` only as dev
dependencies and explicitly inject capability-owned ports. `task_clients` wraps
a task behind an admitted binding and uses only in-memory client doubles. Run it
with `cargo run -p forge-bridge --example task_clients --offline --locked`. The demo contexts are not production authentication.
See the [workspace guide](../README.md) for complete checks and deferred features.
