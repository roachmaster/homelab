# Bootstrap and cohesive factory registrars

**Status: small bootstrap subset implemented.** The five earlier [Rust crates](../../src/rust/README.md) retain their public contracts. A sixth core package, [`forge-bootstrap`](../../src/rust/forge-bootstrap/README.md), and three explicitly labeled demo packages now implement atomic typed group registration, consuming validation/freeze and optional in-process assembly. Automatic graph execution, production clients, UI and managed lifecycle remain deferred. This refines the explicit assembly found in Mission Control and Forge-framework; it is not a new transport, parser, dynamic plugin loader or universal DI container. [Canonical vocabulary](framework-dsl.md) · [DI ownership](dependency-injection.md) · [Implementation checkpoint](../roadmap/forge-framework.md#next-core-slice-bootstrap-and-registrars)

## The developer-facing contract

The application chooses optional capability/implementation crates and supplies typed configuration. **Bootstrap configures and invokes their registrars; each registrar stages a cohesive group of configured factories.** Registration stores construction recipes, not constructed clients or running tasks. After complete validation and freeze, application assembly constructs selected dependencies in an explicit order, establishes provider ownership, constructs tasks, and binds the existing shared bridge.

A reusable implementation crate owns its concrete client, implementation dependencies, typed configuration (an enum when selecting supported modes), factories and registrar. It depends on the narrow contracts it implements; core never imports it. A caller needing another I/O mechanism selects another crate, not an extra dependency hidden in core. Pure computations need no I/O crate. Contract-only crates do not need meaningless registrars, and small coherent internals can remain modules.

The goal is a simple application front door to installed capabilities, including tools that may eventually be remote—not merely argument parsing. CLI, HTTP, IPC and filesystem access are examples, not required integrations. No real transport/client implementation is included; the optional implementation package is an observable in-memory fake.

## Configuration is not a command payload

Each implementation owns a validated startup type, such as a package-local configuration enum; there is no framework-wide configuration enum. Application/environment loading happens once at the composition boundary, which maps raw settings to those types and reports missing/invalid values. Registrars and factories receive explicit values: they do not independently read environment variables. Defaults and override precedence are application policy, with secret-safe diagnostics.

Startup configuration selects implementation behavior and lifecycle. Runtime capability enums remain the `Envelope<P>` payload checked by bridge bindings and adapted into task inputs; `Task` itself remains envelope-independent. Neither is a JSON/value-map escape hatch, and callers cannot choose providers by placing implementation names in a request header.

## Minimal Rust-feasible registration

The implemented **`forge-bootstrap` crate** is std-only with no normal dependencies. `PreparedGroup<F>` stores one package-owned factory/recipe struct plus validated group metadata. `Registry<G>` owns the index and all typed groups together. Successful registration appends to a nested tuple `(G, F)`; rejected registration returns the original registry and incoming group unchanged through `Rejected::into_parts()`.

This **consuming, typed builder replaces the earlier separately borrowed slot/metadata proposal**. Owning both together prevents mismatched slot/index ownership without a second container protocol. The tuple type changes when a group is added; the consuming application explicitly chooses the registration calls. No `Any`/downcasts, JSON, global business enum, heterogeneous factory map or dyn-GAT provider is used. The core does not impose a universal registrar trait: packages export ordinary typed registrar functions.

`freeze` checks that all declared external requirements are provided by earlier selected groups. Only the resulting `Frozen<G>` exposes `into_factories()`. The application explicitly binds resolved client handles into a dependent task recipe, creates the task and registers its bridge binding. Metadata checks do not execute an automatic graph or prove arbitrary recipe behavior.

A configuration enum can select only compatible implementations: fixed request/response/error types, a package-owned adapter enum, or explicit typed application branches. A configuration tag cannot make unrelated Rust output types interchangeable. Startup errors remain typed; diagnostic IDs are not service-lookup keys.

### Actual implemented composition

The [demo bootstrap](../../src/rust/examples/forge-bootstrap-demo/src/messages.rs) calls two reusable registrars: [message capability](../../src/rust/examples/forge-demo-messages/src/lib.rs) and [optional memory client](../../src/rust/examples/forge-demo-memory-client/src/lib.rs). Reduced from that executable path (application error mapping and the pure group omitted):

```rust,ignore
let clients = memory::register(client_config, client_probe)?;
let tasks = capability::register(task_config, task_probe)?;
let registry = Registry::new().register(clients)?.register(tasks)?;
let (((), clients), tasks) = registry.freeze()?.into_factories();
// Construct the chosen client/provider, bind its handle to tasks.greeting,
// create the task and register its typed bridge closure before publication.
```

This fragment names real APIs but is not a standalone program; error mapping and lifetime selection are in the linked source. The default demo consumer contains only pure arithmetic and has **no normal dependency on either message package, `forge-io-client` or `forge-provider`**. Its opt-in Cargo `messages` feature adds the selected packages; tests/normal-dependency trees prove actual inclusion/omission, not just a runtime `if`.

No client/task is created during staging or freeze. The complete group's collisions are checked before metadata or typed values move into the next builder. Preparation errors occur before registration; registration errors retain the prior plan; construction errors return no application. The sample composes only owned in-memory resources and makes no remote/durable/authentication guarantee.

## Construction and provider lifetime

For a client-using task the explicit order is:

1. For a singleton client, invoke the selected, already configured `Factory<T>::create()` once and give the result to `Singleton`/its owner.
2. For a transient client instead, give the configured factory directly to `Transient` without creating an unused initial instance; construction happens on acquisition. Both policies are explicit, not inferred from the registrar name.
3. Acquire a valid ready client handle for the task. A dependent task recipe binds that handle to a constructor/closure, **then** yields a task factory if repeatable construction is useful. Existing `Factory<T>::create(&self)` takes no dependency argument; this design does not silently change it.
4. Construct the task, optionally retain it in a task provider, and register a typed bridge binding that calls `Task::run`. The task still prepares requests; `IoClient::execute` only executes prepared requests and required boundary encoding.
5. Freeze the route registry and expose the fully assembled application. Header selection, admission and variant checks remain on the existing path.

Factories and dependent recipes are typed values stored during registration; creating a factory object is not creating its client. A shared handle must satisfy actual Rust bounds. The demo uses owned `Rc` client/provider handles in a single-threaded application. For another returned application, prefer owned handles or externally owned scoped backing; do not build a self-referential struct whose tasks borrow movable sibling provider fields. `Arc` alone does not confer thread safety, and the current bridge remains synchronous/non-`Send`.

A singleton task must not accidentally retain a short operation/pool lease for its entire lifetime. Keep per-call acquisition private to the client when possible, or inject a purposeful narrow provider when the workflow needs a session. Pools, async factories and awaited cleanup are deferred. A pure task has no client recipe or provider dependency.

## Validate, freeze, assemble; do not publish partial state

- **Prepare:** validate typed config and group-local identities; stage all factory recipes together. No client construction, network/file operations, worker starts or route exposure. This is a contract for trusted registrar code, verified with effect counters—not a sandbox that proves arbitrary Rust code pure.
- **Validate the selected application:** reject duplicate groups/bindings and missing selected requirements before factory effects. The demo resolves conflicting explicit/default choices before calling registrars. Runtime route collisions remain the existing bridge builder's responsibility, checked before application publication. Check the explicitly declared construction order: required groups must precede consumers. Reject invalid/self/forward dependencies rather than inventing runtime resolution. Typed constructors additionally enforce actual dependency types/lifetimes; string metadata does not replace them.
- **Freeze:** consume the pending application plan, keeping its typed groups and validated metadata together. No additions or implicit replacements afterward. The implemented metadata describes assembly bindings, not a route-descriptor catalog. Existing bridge registration rejects route collisions, and the demo tests its known routes; generic declared-route coverage validation remains deferred.
- **Assemble:** application-owned code calls factories in that declared order and injects dependencies. It is not automatic heterogeneous graph execution. Tests compare actual creation order with the declared order. If construction fails, release already-owned synchronous resources in reverse dependency order (using explicit ownership scopes/drop order), preserve the original error, and return no application. Core registration errors describe metadata; application-owned typed error variants preserve specific factory/provider causes instead of adding every client failure to a framework-global enum. Effects outside memory are not rolled back by `Drop`; future adapters need explicit close/cleanup contracts before claiming such guarantees.

An omitted required binding/configuration is an assembly error. The implemented registry checks declared order and preserves failed builders; broader configuration/override policy remains application-owned. A full auto-wiring graph, arbitrary cycle resolution/diagnostics, dynamic registration and managed startup/shutdown are later work, not promises supplied by these metadata checks.

## Defaults, replacement and extension

Select an explicit implementation before invoking its default registrar; an unused default factory must never run. Two explicit choices conflict. Replacement must name a documented extension point—not depend on insertion order or a duplicate-error side effect.

A future generic home can be the default presentation provider. **Replace** selects another home implementation; **extend** contributes uniquely identified entries to an explicit extension surface rather than another competing home handler. Keep navigation entries, command routes and DI bindings distinct. Validate application-action targets against registered commands; menu visibility is not authorization. Home rendering and remote front ends are follow-on capabilities, not prerequisites for bootstrap core.

## Continuity with the reviewed base

Mission Control already composes domain-owned handler lists from injected ports and shares one command-port instance across selected inbounds. Its typed registry rejects duplicates before insertion; its separate domain router does not. Forge-framework supplies explicit handler → registry → dispatcher → API composition, but its duplicate handler insertion also mutates before reporting failure. Both supply useful seams, not the complete registrar contract above. Mission Control's UI catalog is hardcoded, not an existing default/replace/extend system. Preserve the successful pattern while moving implementation details into selected packages and making registration failure-atomic. [Mission Control review](../reviews/forge/mission-control.md) · [Framework review](../reviews/forge/forge-framework.md)
