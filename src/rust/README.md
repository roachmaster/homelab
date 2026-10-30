# Rust source sets — first Forge core

This directory is now a real **five-package Cargo workspace**, with an original,
std-only synchronous core and runnable in-process examples. It is a narrow first
slice, not the complete framework/reference application described in the designs.

| Package | Responsibility | Normal dependencies |
| --- | --- | --- |
| [forge-ingress-contract](forge-ingress-contract/README.md) | Validated route/header, generic typed request/response, payload classification and admission contracts | Standard library only |
| [forge-bridge](forge-bridge/README.md) | Local route builder, frozen membership, header-selected admission/variant-checked dispatch | `forge-ingress-contract` |
| [forge-provider](forge-provider/README.md) | Typed factories, borrow-aware provider/lease contract, host-owned singleton and transient acquisition | Standard library only |
| [forge-io-client](forge-io-client/README.md) | Executes an already prepared typed request at a narrow I/O boundary | Standard library only |
| [forge-task](forge-task/README.md) | Typed pure computation or orchestration through injected client contracts | Standard library only |

The bridge's **dev-only** provider/client/task dependencies compose examples and
contract tests. Task's client/provider dependencies are also dev-only; pure task
consumers have no normal I/O dependency, and client adapters need no task model.
Production routing imports none of these abstractions. Factories, providers and leases stay in
one cohesive crate; they are not a class-per-package hierarchy. Capability enums
and ports live in consumers, not this framework. There is no JSON/value-map
payload fallback, transport library, global enum, global registry or service locator.

## Run and check

Working directory: **`src/rust/`**. Requires a current stable Rust toolchain with
`rustfmt` and `clippy`; the dependency guard additionally uses Python 3's standard
library. No credentials, network service or third-party Rust crate is required.
Cargo.lock is committed; build output is ignored.

```bash
cargo fmt --all -- --check
cargo build --workspace --all-targets --offline --locked
cargo test --workspace --offline --locked
cargo clippy --workspace --all-targets --offline --locked -- -D warnings
cargo run -p forge-bridge --example in_process --offline --locked
cargo run -p forge-bridge --example task_clients --offline --locked
python3 scripts/check_dependencies.py
```

The example constructs a capability-owned greeting enum, factory and host-owned
provider, injects the ready port into a handler, registers it and invokes the same
typed bridge entry point an adapter would use. Its correlation ID survives into
the response. The second example wraps a `Task` behind the bridge: injected fake
clients fetch data and record a request prepared by the task, without network or
disk operations. Neither example claims durable acceptance. The explicit demo
policies are **not authentication**.

These checks ran with Rust/Cargo **1.99.1** on Linux. The suite has 30
integration tests plus one compile-fail lifetime doctest; it covers invalid
metadata, typed success/correlation, unknown route/version, denied admission,
header/variant mismatch, preserved duplicate binding, typed error propagation,
nested application sums, factory substitution, scope identity and lease cleanup.
Client/task tests add pure computation, unchanged non-Clone prepared requests,
fetch/send sequencing, typed error causes, client substitution and no client
effects after rejected bridge admission.
No minimum-supported-Rust-version guarantee or production certification is claimed.

## Task/client boundary

`Task::run` owns computation or business orchestration and prepares typed client
requests. `IoClient::execute` takes a prepared request and owns I/O execution; a
real adapter may perform required boundary encoding, not business payload
assembly. Capability-owned inputs/results/errors keep both traits generic.
Clients are injected interfaces, never arbitrary services looked up during a
request. This separation is a behavioral contract, not a compiler proof that an
implementation is pure or that a future client respects its semantics.

## Implemented invariants and limits

- `Header` and `RouteKey` constructors reject empty/whitespace/control-containing
  or over-256-byte tokens, and zero contract versions. Identity is not a header
  field. Trusted context arrives separately from the embedding host.
- The header selects an exact route/version, then the mandatory injected policy
  runs, then the registered expected kind is compared with `Payload::kind`, before
  invoking the handler. `kind` is a capability-owned **pure, faithful variant
  classifier**. Test it exhaustively, including nested application variants;
  the framework cannot verify arbitrary trait implementations or authenticate a
  caller-created context. Admission callbacks, classification/equality and
  handlers are trusted application code; their panics propagate, with no catch
  boundary or rollback guarantee.
- Duplicate registration rejects without replacing the old handler or expected
  kind. Consuming `freeze` fixes operation membership only. No public handler
  lookup or alternate unchecked dispatch API is exposed.
- Handler closures are stored without `Send`/`Sync`; the bridge cannot be shared
  across threads unchanged. Handlers receive payload and context, not the header;
  header-dependent business metadata is a future explicit API decision.
- Typed handler errors are preserved. A handler failure can follow effects; the
  bridge does not roll them back. Response metadata is copied on success; callers
  retain their own request metadata when handling an error.
- `Singleton` shares a prebuilt value per provider; `Transient` constructs on
  acquisition. Borrowed leases cannot outlive their owner. Owned transient values
  may still share captured backing state. Ordinary destruction is synchronous.

**Not implemented:** a module/DSL parser or assembly container, dependency
resolution/override validation, provider pools, async dispatch or `Send`/`Sync`
handler hosting, real authentication, deadline/cancellation/idempotency metadata
and enforcement, durable acceptance, storage, transports, workers or supervised
startup/shutdown. The dependency guard checks the **compile-time crate graph**,
not a runtime service graph. These remain planned rather than silently stubbed.

## Continue without growing a catch-all core

Keep each source set at `src/rust/<purpose>/`, public contracts narrow, and smaller
internals in modules. Add another crate only for a real consumer or enforceable
dependency boundary. Test consumer policy with its owned enum/ports; test a real
adapter against its actual dependency when one is introduced. Do not turn this
example into the durable jobs application or algorithm platform by default.

See the [DSL semantic reference](../../docs/architecture/framework-dsl.md) for the
larger proposed model and the [source-set guide](../README.md) for organization.
