# Rust source sets — first Forge core

This directory contains **six core packages, one optional hosted system package, a local application/terminal pair and three
clearly separated demo packages**, all original, std-only and synchronous. Typed registration/freeze and
optional-package assembly, local node observations, admitted command discovery and native disk inspection/navigation now work; the broader managed framework/durable reference
application remains a target, not a completed product.

| Package | Responsibility | Normal dependencies |
| --- | --- | --- |
| [forge-ingress-contract](forge-ingress-contract/README.md) | Validated route/header, generic typed request/response, payload classification and admission contracts | Standard library only |
| [forge-bridge](forge-bridge/README.md) | Local route builder, frozen membership, header-selected admission/variant-checked dispatch | `forge-ingress-contract` |
| [forge-provider](forge-provider/README.md) | Typed factories, borrow-aware provider/lease contract, host-owned singleton and transient acquisition | Standard library only |
| [forge-io-client](forge-io-client/README.md) | Executes an already prepared typed request at a narrow I/O boundary | Standard library only |
| [forge-task](forge-task/README.md) | Typed pure computation or orchestration through injected client contracts | Standard library only |
| [forge-bootstrap](forge-bootstrap/README.md) | Atomic typed factory-group registration, declared-order validation, consuming freeze and tuple extraction | Standard library only |

The bridge's **dev-only** provider/client/task dependencies compose examples and
contract tests. Task's client/provider dependencies are also dev-only; pure task
consumers have no normal I/O dependency, and client adapters need no task model.
Production routing imports none of these abstractions. Factories, providers and leases stay in
one cohesive crate; they are not a class-per-package hierarchy. Capability enums
and ports live in consumers, not this framework. There is no JSON/value-map
payload fallback, transport library, global enum, global registry or service locator.

## Optional native system clients

[`forge-kernel`](forge-kernel/README.md) groups hosted low-level clients; `disk` is the first module. Its real native filesystem operations are list, pwd, client-local cd and permission inspection. It imports bootstrap/provider/I/O-client contracts; core never imports it. No shell wrapper, process cwd mutation, file mutation API, bootable OS or Raspberry Pi image is included. The opt-in `node` module adds configured identity, compile-target facts and client-instance uptime; it does not claim machine health. `disk` remains enabled by default for existing consumers. New local consumers select kernel features explicitly.

## Runnable local system

- [`forge-local`](forge-local/README.md) owns explicit node/disk composition, per-application providers, typed command/result sums and registered command descriptors. It imports the six core contracts and selected kernel modules; no core imports it.
- [`forge-terminal`](forge-terminal/README.md) is the std-only terminal adapter and `forge` binary. It imports only the local application contract and I/O-client contract. Console I/O is an injected typed client, not business policy.

```bash
cargo run -p forge-terminal -- --help
cargo run -p forge-terminal -- --node-id desk node.info
cargo run -p forge-terminal -- --session
cargo run -p forge-terminal --no-default-features -- node.status
```

The default terminal enables native disk; `--no-disk` omits it at assembly. `--no-default-features` omits its code/module from the terminal's kernel feature graph. The node module still requires the kernel package; the older pure demo below remains the proof of whole-package omission. Discovery/help and commands all pass the same bridge. Status means the node client responded, not that all resources are healthy. No process commands, network, jobs or destructive file operations are added.

## Optional consumer proof

The demo packages live under `examples/`, not inside core:

- [forge-demo-messages](examples/forge-demo-messages/README.md): capability-owned input/prepared request/result, injected-client task, typed config and registrar.
- [forge-demo-memory-client](examples/forge-demo-memory-client/README.md): fake client, its config/factories/registrar and explicit singleton/transient handle.
- [forge-bootstrap-demo](examples/forge-bootstrap-demo/README.md): pure-default application; the Cargo `messages` feature opts in to the two packages above. Its default normal dependency graph excludes both, `forge-kernel`, `forge-io-client` and `forge-provider`. A separate opt-in `disk` feature selects the native kernel module without importing the message packages.

The application selects configurations/registrars before constructing resources.
`Registry<G>` owns metadata and typed groups together; a rejected complete group
returns the original builder unchanged. `Frozen<G>` yields the nested tuple only
after dependency-order checks. Construction stays explicit; dependent task recipes
bind client handles without changing `Factory<T>::create`. No global service map,
forced client for pure tasks or concrete transport is introduced.

## Run and check

Working directory: **`src/rust/`**. Requires a current stable Rust toolchain with
`rustfmt` and `clippy`; the dependency guard additionally uses Python 3's standard
library. No credentials, network service or third-party Rust crate is required.
Cargo.lock is committed; build output is ignored.

```bash
cargo fmt --all -- --check
cargo build --workspace --all-targets --all-features --offline --locked
cargo test --workspace --all-features --offline --locked
cargo test --workspace --all-features --release --offline --locked
cargo test -p forge-bootstrap-demo --no-default-features --offline --locked
cargo test -p forge-bootstrap-demo --no-default-features --features disk --offline --locked
cargo clippy --workspace --all-targets --all-features --offline --locked -- -D warnings
cargo clippy -p forge-bootstrap-demo --all-targets --no-default-features --offline --locked -- -D warnings
cargo test -p forge-local -p forge-terminal --no-default-features --offline --locked
cargo clippy -p forge-terminal --all-targets --no-default-features --offline --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --offline --locked
cargo run -p forge-bootstrap-demo --no-default-features --offline --locked
cargo run -p forge-bootstrap-demo --features messages --offline --locked
cargo run -p forge-bootstrap-demo --example disk_access --no-default-features --features disk --offline --locked
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

These checks ran with Rust/Cargo **1.99.1** on Linux. The all-feature suite now passes **138 unit/integration tests and five doctests** (four compile-fail, one runnable), including the local application and terminal; the pure and node-only
consumers are additionally checked separately. The suite covers invalid
metadata, typed success/correlation, unknown route/version, denied admission,
header/variant mismatch, preserved duplicate binding, typed error propagation,
nested application sums, factory substitution, scope identity and lease cleanup.
Client/task tests add pure computation, unchanged non-Clone prepared requests,
fetch/send sequencing, typed error causes, client substitution and no client
effects after rejected bridge admission. Bootstrap tests add late multi-binding
conflict preservation, typed recovery after rejection, missing/self/forward
requirements, freeze restrictions, config/default selection, lazy transient
creation, startup failure cleanup, and actual optional dependency omission.
Local system tests add native/configured node observations, substituted provider failures, catalog/binding consistency, actual terminal process commands and stateful sessions, bounded input and escaped native names. Native disk tests add real temporary-fixture listing/navigation/permission checks, non-UTF8/symlink cases on Unix, isolated sessions and admission-before-cd; fake substitution tests remain separate evidence. Windows-specific cases are present but not executed on this Linux host.
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

`forge-bootstrap` validates assembly binding metadata, not a route-descriptor
catalog. The local application now registers its presentation descriptors together with checked bindings and tests that every advertised command is invokable; this is an application-specific guarantee, not a generic core catalog. Existing bridge registration still rejects route duplicates; the demo
tests its known route set. Metadata cannot prove arbitrary factory code follows
its declarations. Registrar purity and construction/drop order are tested behavior,
not a sandbox or automatic teardown guarantee. Runtime effects are never rolled back.

**Not implemented:** a module/DSL parser, universal container, automatic dependency
resolution, generic override/default engine, route-descriptor coverage validator,
provider pools, async dispatch or `Send`/`Sync`
handler hosting, real authentication, deadline/cancellation/idempotency metadata
and enforcement, durable acceptance, persistent content stores, remote transports, workers or supervised
startup/shutdown. The dependency guard checks the **compile-time crate graph**,
and actual pure/messages/disk/combined consumer dependency trees; bootstrap separately checks
its declared assembly order, not an auto-wired runtime service graph. Deferred
features remain planned rather than silently stubbed.

## Continue without growing a catch-all core

Keep each source set at `src/rust/<purpose>/`, public contracts narrow, and smaller
internals in modules. Add another crate only for a real consumer or enforceable
dependency boundary. Test consumer policy with its owned enum/ports; test a real
adapter against its actual dependency when one is introduced. Do not turn this
example into the durable jobs application or algorithm platform by default.

See the [DSL semantic reference](../../docs/architecture/framework-dsl.md) for the
larger proposed model and the [source-set guide](../README.md) for organization.
