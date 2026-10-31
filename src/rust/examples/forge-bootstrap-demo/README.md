# Typed bootstrap demo

Original, std-only application composition with a genuinely optional client
path. The default package builds and runs a pure checked-sum `Task` through the
existing mandatory-admission, header-selected, variant-checked `Bridge`.
**Its default normal dependency graph contains neither demo messages crate,
`forge-io-client`, nor `forge-provider`.** No transport is mandatory.

The opt-in Cargo feature `messages` selects the two optional demo packages and
provider primitives. Compile-time `#[cfg]` branches remove their config,
command/result variants, bootstrap wiring and dependencies from the baseline.
Enabling the feature permits installation; `AppConfig.messages = None` still
leaves that route uninstalled at runtime. The runnable binary installs the
sample capability when compiled with the feature.

## Run and verify

From `src/rust/`:

```bash
# Actual pure-only consumer; do not replace this with only --all-features checks.
cargo build -p forge-bootstrap-demo --no-default-features --offline --locked
cargo test -p forge-bootstrap-demo --no-default-features --offline --locked
cargo run -p forge-bootstrap-demo --no-default-features --offline --locked
cargo tree -p forge-bootstrap-demo --no-default-features -e normal --offline --locked

# Optional in-memory client path.
cargo test -p forge-bootstrap-demo --features messages --offline --locked
cargo run -p forge-bootstrap-demo --features messages --offline --locked
cargo clippy -p forge-bootstrap-demo --all-targets --all-features --offline --locked -- -D warnings
```

The executables only compute, update the selected in-memory fake and print
results. They do not make network calls, read configuration from the environment,
or write application files. Cargo itself of course writes its build outputs.
The caller-controlled demo admission flag is **not authentication**.

## Composition, not a service locator

`bootstrap(AppConfig)` is the typed application entry point. For observations,
`messages::bootstrap_messages(PureConfig, MessagesBootstrapConfig, Observations)`
uses the same optional assembly path. Startup config and instrumentation are
separate from runtime `Command` payloads.

1. Select the client configuration: zero explicit choices uses the optional
   default; exactly one explicit choice suppresses the default *before invoking
   its registrar*; two explicit choices fail. A missing selected client fails.
2. Invoke the selected client registrar and capability registrar; stage complete
   typed groups, never clients. Invalid configuration produces zero factory
   calls, including task configuration rejected after the client recipe stages.
3. Register pure → client → message-task groups. The consuming registry owns
   metadata and a heterogeneous nested tuple together. Validate declared
   requirements/order and consume/freeze before extracting factories. Binding
   IDs are diagnostics, **not** runtime service-lookup or route keys.
4. Construct the singleton client once, or retain the transient factory without
   a discarded eager instance. Bind an owned `MemoryClientHandle` into the
   dependent greeting recipe, then invoke the task factory. There is no
   self-referential borrowing of movable sibling provider fields.
5. Bind the pure and message tasks into the existing bridge, freeze routes, and
   return the complete application. The application's only front door is
   `Application::dispatch`; no raw handler/unchecked dispatch is exposed.

The app owns its local command/result sums; adding a selected capability never
requires a framework-global business enum. `GreetingTask` assembles the exact
text and destination; the concrete client executes that prepared request.
Factory selection/provider lifetime cannot be changed with an ingress header.
The pure task remains independent of all I/O abstractions even in a mixed host.

## Failure and lifetime evidence

Focused tests establish default/explicit selection, duplicate rejection, invalid
config with zero creates, omitted required clients, order validation, exact
prepared messages, substitutable fake modes, typed creation/execution errors,
zero acquisitions after rejected ingress, separate host isolation, singleton
reuse, transient independent instance state and acquisition-only construction.

A singleton factory failure creates no task and returns no app. A later task
construction failure synchronously drops the already-owned client and returns
no app. Successful host destruction logs task destruction before its owned
client. Temporary factories do not keep hidden extra resource owners alive.
Transient execution drops each short-lived client; transient construction errors
appear at the first admitted acquisition, not during bootstrap. Startup readiness
for transient mode means the configured acquisition path is installed, **not**
that future acquisitions are guaranteed to succeed. No default fallback or retry
conceals those failures.

## Deliberate limits

This is a fake-only, synchronous, single-threaded demonstration, not a production
host/container or managed lifecycle. It adds no network transport, disk effects,
authentication, pools, workers, async runtime/cleanup, concurrency guarantee,
durable delivery, graph auto-execution, rollback, idempotency or automatic retry.
Registrars, factories, admission and handlers remain trusted Rust code. The
metadata checks do not prove arbitrary constructor purity or type semantics;
typed constructors supply the actual dependency compatibility. Ordinary `Drop`
only releases owned local state; it cannot undo outside-world effects or await
cleanup. Separate transport implementations would need their own real contract
and lifecycle tests.
