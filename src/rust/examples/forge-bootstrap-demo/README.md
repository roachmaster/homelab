# Typed bootstrap demo

Original, std-only application composition with a genuinely optional client
path. The default package builds and runs a pure checked-sum `Task` through the
existing mandatory-admission, header-selected, variant-checked `Bridge`.
**Its default normal dependency graph contains neither demo messages crate,
`forge-io-client`, `forge-provider`, nor `forge-kernel`.** No transport or disk
client is mandatory.

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

The default and `messages` main-binary paths only compute, update the selected
in-memory fake and print results. They do not make network calls, read
configuration from the environment, or write application files. The separate
optional disk example below explicitly owns its temporary filesystem fixture.
Cargo itself of course writes its build outputs.
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

## Deliberate limits of pure/messages paths

These paths are fake-only, synchronous, single-threaded demonstrations, not a
production host/container or managed lifecycle. They add no network transport, disk effects,
authentication, pools, workers, async runtime/cleanup, concurrency guarantee,
durable delivery, graph auto-execution, rollback, idempotency or automatic retry.
Registrars, factories, admission and handlers remain trusted Rust code. The
metadata checks do not prove arbitrary constructor purity or type semantics;
typed constructors supply the actual dependency compatibility. Ordinary `Drop`
only releases owned local state; it cannot undo outside-world effects or await
cleanup. Separate transport implementations would need their own real contract
and lifecycle tests.


## Optional native disk consumer

The independent `disk` feature selects the optional hosted
[`forge-kernel::disk`](../../forge-kernel/README.md) module, `forge-io-client`, and
provider primitives. It neither enables `messages` nor changes the existing
`AppConfig`, main binary, command/result sums or pure-only build. The kernel name
is a cohesive home for system-facing clients, **not** a bootable operating system,
`no_std` kernel, Raspberry Pi image, driver or bare-metal implementation.

```bash
# From src/rust/: feature-selected native fixture tests and runnable example.
cargo test -p forge-bootstrap-demo --features disk --test disk --offline --locked
cargo run -p forge-bootstrap-demo --features disk --example disk_access --offline --locked
```

`disk::bootstrap_disk(DiskConfig)` stages the reusable native registrar's complete
factory group, validates/freezes `forge-bootstrap::Registry`, then creates one
client and installs an owned `Rc<Singleton<DiskClient>>` handle. A generic
`ListCurrentTask<C>` prepares exactly `DiskRequest::List { path: "." }` for its
injected `IoClient`, checks for a listing response, and preserves typed errors
without retry. Tests substitute a fake client without changing that task.

The separate application-owned `DiskCommand` classifier and four explicit routes
cover pwd, list-current, change-directory and permission inspection. Thin
bindings for the other three operations call that same host-owned client after
the existing bridge's header lookup, mandatory admission and variant checks.
The only exposed application entry path is `DiskApplication::dispatch`; there is
no unchecked handler/native-client accessor or request-time service lookup.
Native OS errors retain their `std::io::Error` cause, not a string or success-shaped
fallback. Failed startup returns no application.

Each assembled host has its own client-local navigation state. `ChangeDirectory`
changes that session's directory, **never** the process working directory; later
pwd/list/permission bindings see that same session. Other hosts remain separate.
Directory and filename values remain `PathBuf`/`OsString`, including non-UTF-8
names. Explicit `follow_symlinks` controls final-component permission inspection;
reported permission metadata does not prove effective user access.

The runnable example creates a unique temporary directory and sample files with
native Rust APIs, operates only on that owned fixture, then explicitly cleans it
up. It accepts no CLI/environment-supplied browsing path. `temp_dir()` is only the
fixture allocation base, not the directory being inspected. Test fixtures follow
the same ownership rule; no existing user files are mutated or removed. These
fixture helpers are **not** native-client create/delete operations: the client
currently exposes only inspection and session navigation.

Native integration tests verify listing and metadata, shared host-local state,
independent hosts, preserved process cwd, original native errors, non-UTF-8 names
and symlink flags where supported. Denied, unknown, unsupported-version and
header/variant-mismatched change-directory commands leave session cwd unchanged.
Those tests use owned native filesystem fixtures; fake substitution tests prove
task preparation separately and are not claimed as native-I/O evidence.

### Native-path limits

This is synchronous hosted `std::fs`/metadata access, with no shell commands or
process spawning. The bridge/`Rc` demo host remains single-threaded. No file
creation/deletion API, content reading/writing API, recursive scan, permission
mutation, pool, async runtime, OS boot, automatic retry or rollback is supplied.
Paths are **not sandboxed**: absolute paths, `..` and symlinks can access other
locations allowed by the host process. The starting directory is not a security
root, and the caller-controlled demo admission flag is not authentication or a
production authorization policy. Filesystem state can change between native
calls; listings are not atomic snapshots. The client owns a directory path, not
a directory-handle capability or a promise to track outside renames/removals.
