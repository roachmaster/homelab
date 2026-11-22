# forge-core

The Forge contract layer in one crate: typed construction, I/O, commands,
ingress, tasks, registration, dispatch, and admission. One module per
contract, no external dependencies, no features. The eight crates this
merges (`forge-provider`, `forge-io-client`, `forge-command`,
`forge-ingress-contract`, `forge-task`, `forge-bootstrap`, `forge-bridge`,
`forge-access`) are gone; their modules preserve the layering the crate
boundaries used to enforce, and `tests/layering.rs` fails the build if a
module imports one above it:

- `task` may use `io_client` and `provider`.
- `bridge` may use `ingress`, `io_client`, `provider`, and `task`.
- `access` may use `ingress`.
- `provider`, `io_client`, `command`, `ingress`, and `bootstrap` use no
  other `forge-core` module.

Two modules export a `Registry`, and they are different types:
`bootstrap::Registry` is the typed factory-group registry;
`bridge::Registry` is the frozen route registry. Keep them qualified.

From **`src/rust`**, using the [workspace toolchain](../README.md):

```bash
cargo fmt --all -- --check
cargo test -p forge-core --offline --locked
cargo clippy -p forge-core --all-targets --offline --locked -- -D warnings
```


## provider

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
cargo test -p forge-core --offline --locked
cargo clippy -p forge-core --all-targets --offline --locked -- -D warnings
```

Tests cover reuse and isolation, transient creation/error/drop behavior,
non-static borrowing and typed substitution. A compile-fail doctest protects the
borrowed singleton lifetime. See [workspace checks](../README.md) and the
[longer proposed DI design](../../../docs/architecture/dependency-injection.md).

## io_client

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
cargo test -p forge-core --offline --locked
cargo clippy -p forge-core --all-targets --offline --locked -- -D warnings
```

The [task/client example](../forge-core/examples/task_clients.rs) composes the
contract without network/disk operations. This package owns the execution shape;
consumer capabilities still own their concrete request/result/error vocabulary.

## command

Static, transport-neutral command descriptions with **zero normal dependencies**.
`DescribeCommand::descriptors(&self)` returns a static slice of operation names,
help text and named arguments. Each `ArgumentDescriptor` declares help, semantic
`DataType` and required/optional `Presence`, including an optional display default.
`DataType` supports booleans, text, paths, signed/unsigned 64-bit integers, 64-bit
floating-point values and named capability-owned concepts.

Description and execution are independent traits. A `Task` can implement
`DescribeCommand` when exposed as a command, but pure tasks need no command
metadata and non-task providers can describe commands. Neither module depends on
the other. An enum-multiplexing capability returns **one descriptor per exposed
operation**, with that operation's arguments—not one flattened list of every enum
variant's fields. No framework-global business enum or metadata supertrait is
required.

Implementations return static values without running tasks, invoking clients,
acquiring resources or performing live discovery. Rust cannot enforce this purity
contract. Composition selects metadata for successfully registered bindings; this
crate provides no execution, registration, authorization, parser or serializer.

`CommandDescriptor::validate` checks nonblank names, help and custom type labels,
and duplicate argument names. `validate_descriptors` additionally rejects an
empty set or duplicate operation names. Empty argument lists are valid. Blank
means empty or entirely Unicode whitespace; duplicate identity is exact and
case-sensitive. Names are not normalized and no transport-specific grammar is
imposed. Validation is explicit, allocation-free, deterministic and quadratic
in catalog size, intended for small static declarations at assembly time.

These are **declarations, not Rust input reflection or a complete schema**.
Validation cannot prove agreement with a capability's typed input, route, parser,
resource permissions or business rules; consumer tests must do that. A default
is descriptive text, not parsed, applied or checked against `DataType`; even an
empty text default is legitimate. A `Path` is not inherently UTF-8 and does not
promise a path exists. Adapters still own conversions and presentation escaping.

Help text on each operation/argument is business metadata. An adapter's `--help`
and `-h` argument definition belongs to the adapter separately (it may reuse
`ArgumentDescriptor`), not a mandatory help field in every business input. The
metadata does not prescribe positional arguments, flags or wire formats.

From `src/rust/` with the workspace toolchain:

```bash
cargo test -p forge-core --offline --locked
cargo clippy -p forge-core --all-targets --offline --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p forge-core --no-deps --offline --locked
```

The runnable crate example and integration tests cover non-task description,
multiple operations, optional/default metadata, all type labels and validation
failures. They do not claim to exercise an executable parser or live I/O.

## ingress

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
cargo test -p forge-core --offline --locked
cargo clippy -p forge-core --all-targets --offline --locked -- -D warnings
```

See [workspace commands and limitations](../README.md) for toolchain and build/run
checks. Header tests establish metadata validation and correlation preservation,
not trust in a caller's identity.

## task

`Task` describes one synchronous capability operation through associated `Input`,
`Output`, `Error` and `run(&self, input)`. It has **no normal dependencies**, including
no I/O-client, bridge or provider dependency. A pure task therefore needs neither
an I/O library nor an ingress envelope. This is not a scheduler or durable job.

A client-using task implementation in a consumer capability imports the narrow
client contract it needs and receives clients by constructor injection. It owns
business request assembly, calculations, sequencing and any failure policy;
concrete clients own I/O execution and necessary boundary encoding. Generic bounds
or trait objects with fixed associated types allow replacement without changing
the task. Do not inject a service registry or put transport-specific calls inside
its policy. Pure computation is an implementation property, not enforced by the
trait or a ban on all client-using tasks.

The test-only dependencies demonstrate pure tasks, fetch-then-send orchestration,
client substitution, exact prepared payloads, original typed error causes and
existing factory/provider injection. They do not imply real I/O conformance.
Sequential calls are not an atomic transaction; errors/panics may follow earlier
effects. No async runtime, automatic retry, cancellation or worker lifecycle is
provided. A composition-owned bridge binding can extract a validated enum variant
and call `run`; the task itself need not know the header or bridge.

From **`src/rust/`**, using the [workspace toolchain](../README.md):

```bash
cargo fmt --all -- --check
cargo test -p forge-core --offline --locked
cargo clippy -p forge-core --all-targets --offline --locked -- -D warnings
cargo run -p forge-core --example task_clients --offline --locked
```

The two tiny contract modules stay separate to preserve independent consumers:
client adapters need no task model, and pure tasks need no I/O model. This is an
original interface, not a compatibility facade over older Forge task executors.

## Optional command description

A task is one possible command execution stereotype, not the universal command
model. The separate `command` module below owns
`DescribeCommand` and typed argument/help descriptors. A consumer may implement
both traits when exposing a task as a command; `Task` has no metadata supertrait
or dependency, so existing pure tasks and trait objects remain unchanged. A
non-Task handler may implement the same description trait. Descriptors are
semantic declarations, not reflection over `Task::Input` or automatic decoding.

## bootstrap

Implemented std-only registration helpers for **configured factory groups**, not a
service locator or automatic DI executor. No normal crate dependencies.

## Public protocol

1. A package registrar validates its typed startup configuration and returns
   `PreparedGroup<F>` containing one cohesive typed factory/recipe group and its
   metadata. No `create`, `acquire`, environment read or I/O belongs in this phase.
2. `Registry<G>::register(group)` checks every claim before committing the whole
   group. Success owns `(G, F)`, preserving heterogeneous types without erasure.
   On conflict, `Rejected::into_parts()` returns the **unchanged original
   registry**, rejected group and typed reason. Neither a late collision in a
   group nor a duplicate group ID partially inserts anything.
3. Consuming `freeze` validates that required bindings are supplied by earlier
   groups. Missing, self and forward dependencies fail; no order is invented.
   `FreezeRejected` retains the unchanged pending registry and diagnostic.
4. Only `Frozen<G>::into_factories()` releases the nested tuple. Application code
   then performs explicit typed construction, chooses providers, injects tasks
   and binds the existing bridge. Publish the application only after completion.

A registrar is an ordinary typed function. It can return several factories in one
package-owned struct. No common erased registrar/factory trait, arbitrary type
lookup, framework-global business enum or dyn-GAT provider is needed. The registry
owns both metadata and values; separately stored mutable slots cannot drift from
its index. This is the concrete refinement of the earlier slot-target proposal.

```rust
use forge_core::bootstrap::{PreparedGroup, Registry};

let first = PreparedGroup::new("client", &["client.port"], &[], || 7)?;
let second = PreparedGroup::new("task", &["task.run"], &["client.port"], || "ready")?;
let plan = Registry::new().register(first)?.register(second)?.freeze()?;
let (((), client_recipe), task_recipe) = plan.into_factories();
// Call typed constructors and inject ready dependencies here, not in registration.
# Ok::<(), Box<dyn std::error::Error>>(())
```

The closures above only illustrate typed storage. The [optional consumer](../examples/forge-bootstrap-demo/README.md)
and its [capability](../examples/forge-demo-messages/README.md) /
[implementation](../examples/forge-demo-memory-client/README.md) registrars demonstrate
actual existing `Factory`, `Singleton`, lazy `Transient`, `IoClient`, `Task` and
bridge composition with fake-only effects.

## Exact scope

Identifiers are nonempty, at most 256 ASCII bytes, and use letters, digits, `.`,
`_` or `-`. Each group provides at least one binding. Repeated provided/required
identifiers are rejected. Requirements name *external* bindings from earlier
groups; dependencies among factories inside one group are its typed recipe's job.
An empty overall registry can freeze. IDs are assembly diagnostics, not runtime
request destinations. Actual routes remain controlled by the `bridge` module.

Freeze proves the declared metadata/order checks only. It cannot prove that a
factory obeys its metadata, that a caller follows the order after extraction,
that a constructor is pure, or that a client is production-ready. Registrars and
recipes are trusted Rust code; panic/allocation failure and external effects are
not rolled back. Tests use counters and typed construction to establish the
selected behavior. Factory/debug/Clone/`'static` bounds are not imposed on groups.

The registry never creates resources: normal registrars store factories/config,
not running clients. Supplied recipe values may still own external handles and
have arbitrary `Drop` behavior; callers own that contract. The application owns
construction failures and resource teardown. RAII does not reverse external effects; awaited cleanup,
pooling, hot registration, graph auto-wiring and managed host lifecycle are not
implemented. The example explicitly models singleton versus transient timing and
uses owned handles rather than self-referential provider/task fields.

## Verify

From `src/rust`:

```bash
cargo test -p forge-core --offline --locked
cargo run -p forge-core-demo --no-default-features --offline --locked
cargo run -p forge-core-demo --features messages --offline --locked
python3 scripts/check_dependencies.py
```

Twelve integration tests cover typed/non-static groups, late multi-claim conflict,
original binding preservation and recovery, group-local validation, missing/self/
forward/cyclic declared dependencies, no construction during staging, and error
causes. Four doctests include typed tuple extraction and three compile-fail
checks for premature extraction, post-freeze registration and moved-builder reuse.

## bridge

A synchronous shared entry path depending only on the `ingress` module.
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
cargo test -p forge-core --offline --locked
cargo clippy -p forge-core --all-targets --offline --locked -- -D warnings
cargo run -p forge-core --example in_process --offline --locked
```

The examples use the `provider`, `task` and `io_client` modules
and explicitly inject capability-owned ports. `task_clients` wraps
a task behind an admitted binding and uses only in-memory client doubles. Run it
with `cargo run -p forge-core --example task_clients --offline --locked`. The demo contexts are not production authentication.
See the [workspace guide](../README.md) for complete checks and deferred features.

## access

Verify evidence at ingress and admit only explicitly permitted routes.
`Verifier` turns evidence into `IdentityFacts`; `AuthenticatedBoundary`
authenticates it into a `Context` the caller can inspect but cannot mint
(subject + deadline, bounded by the boundary's max call budget and the
identity's validity). `RoutePolicy` holds explicit per-subject route grants
and implements `AdmissionPolicy`, so a route is admitted only when the
subject holds a grant for it and the context has not expired. Subjects are
1–64 ASCII letters, digits, `.`, `_`, or `-`; duplicate route grants are
rejected at construction.
