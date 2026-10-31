# Bootstrap and cohesive factory registrars

**Status: practical next core slice, not implemented.** The five existing [Rust crates](../../src/rust/README.md) remain unchanged. This refines the explicit assembly found in Mission Control and Forge-framework; it is not a new transport, parser, dynamic plugin loader or universal DI container. [Canonical vocabulary](framework-dsl.md) · [DI ownership](dependency-injection.md) · [Implementation sequence](../roadmap/forge-framework.md#next-core-slice-bootstrap-and-registrars)

## The developer-facing contract

The application chooses optional capability/implementation crates and supplies typed configuration. **Bootstrap configures and invokes their registrars; each registrar stages a cohesive group of configured factories.** Registration stores construction recipes, not constructed clients or running tasks. After complete validation and freeze, application assembly constructs selected dependencies in an explicit order, establishes provider ownership, constructs tasks, and binds the existing shared bridge.

A reusable implementation crate owns its concrete client, implementation dependencies, typed configuration (an enum when selecting supported modes), factories and registrar. It depends on the narrow contracts it implements; core never imports it. A caller needing another I/O mechanism selects another crate, not an extra dependency hidden in core. Pure computations need no I/O crate. Contract-only crates do not need meaningless registrars, and small coherent internals can remain modules.

The goal is a simple application front door to installed capabilities, including tools that may eventually be remote—not merely argument parsing. CLI, HTTP, IPC and filesystem access are examples, not required integrations. No concrete transport/client implementation is part of this next slice.

## Configuration is not a command payload

Each implementation owns a validated startup type, such as a package-local configuration enum; there is no framework-wide configuration enum. Application/environment loading happens once at the composition boundary, which maps raw settings to those types and reports missing/invalid values. Registrars and factories receive explicit values: they do not independently read environment variables. Defaults and override precedence are application policy, with secret-safe diagnostics.

Startup configuration selects implementation behavior and lifecycle. Runtime capability enums remain the `Envelope<P>` payload checked by bridge bindings and adapted into task inputs; `Task` itself remains envelope-independent. Neither is a JSON/value-map escape hatch, and callers cannot choose providers by placing implementation names in a request header.

## Minimal Rust-feasible registration

Propose **one new `forge-bootstrap` crate**, initially std-only with no normal dependency on a concrete implementation, transport, task or bridge. It provides small prepared-group/validation/freeze helpers and controlled assembly errors—not an all-purpose runtime. Reuse `forge-provider` for actual factory/provider behavior; bootstrap examples can depend on the existing crates as dev dependencies.

Keep heterogeneous factory values in an **application-owned typed struct or tuple**, with fields for the selected registrars' distinct factory-group types. A narrow typed registration target borrows one application slot plus the common metadata index; its commit checks the complete group for collisions before changing either. The index records binding identities, requirements and diagnostics, not erased services. A registrar produces its metadata and typed factory group together. Bootstrap stages the whole group, not a sequence of independently published registrations.

Different concrete factory output types therefore never have to inhabit `HashMap<Key, dyn Factory<...>>`. No `Any`/downcasts, JSON, global business enum, `Box<dyn InstanceProvider<...>>` with an unsupported GAT promise, or heterogeneous array of unrelated registrars is needed. A configuration enum may choose implementations only through a compatible exported contract: fixed associated request/response/error types, a package-owned adapter enum, or explicitly typed application branches. A config tag cannot make unrelated Rust output types interchangeable. Ordinary typed functions are sufficient registrar contracts initially; a trait is useful only if real callers share its type parameters. Adding an optional crate can add a typed field/call in application composition without editing framework dispatch.

**Illustrative shape only; these names/APIs do not exist and this is not a compiled example.** Here `ApplicationRegistry` and its typed slots belong to the consuming application, while each selected package owns its registrar and factory-group type:

```rust,ignore
let config: AppConfig = load_at_application_boundary()?;
let mut registry = ApplicationRegistry::new();

lookup_package::register(config.lookup, registry.lookup_target())?;
message_package::register(config.messages, registry.messages_target())?;
// Each call stages a complete typed factory group; neither executes I/O.

let plan = registry.validate_and_freeze()?;
let application = plan.assemble()?; // explicit typed constructor calls, below
// Only the completed application's front door is exposed to callers.
```

These packages are placeholders for a controlled in-memory demonstration, not commitments to production clients. Both typed slots and cross-group metadata are validated before assembly. A failed registrar leaves its target and earlier accepted groups unchanged; a later assembly failure discards the entire unpublished application.

## Construction and provider lifetime

For a client-using task the explicit order is:

1. For a singleton client, invoke the selected, already configured `Factory<T>::create()` once and give the result to `Singleton`/its owner.
2. For a transient client instead, give the configured factory directly to `Transient` without creating an unused initial instance; construction happens on acquisition. Both policies are explicit, not inferred from the registrar name.
3. Acquire a valid ready client handle for the task. A dependent task recipe binds that handle to a constructor/closure, **then** yields a task factory if repeatable construction is useful. Existing `Factory<T>::create(&self)` takes no dependency argument; this design does not silently change it.
4. Construct the task, optionally retain it in a task provider, and register a typed bridge binding that calls `Task::run`. The task still prepares requests; `IoClient::execute` only executes prepared requests and required boundary encoding.
5. Freeze the route registry and expose the fully assembled application. Header selection, admission and variant checks remain on the existing path.

Factories and dependent recipes are typed values stored during registration; creating a factory object is not creating its client. A shared handle must satisfy actual Rust bounds. For a returned application, prefer owned handles or externally owned scoped backing; do not build a self-referential struct whose tasks borrow movable sibling provider fields. `Arc` alone does not confer thread safety, and the current bridge remains synchronous/non-`Send`.

A singleton task must not accidentally retain a short operation/pool lease for its entire lifetime. Keep per-call acquisition private to the client when possible, or inject a purposeful narrow provider when the workflow needs a session. Pools, async factories and awaited cleanup are deferred. A pure task has no client recipe or provider dependency.

## Validate, freeze, assemble; do not publish partial state

- **Prepare:** validate typed config and group-local identities; stage all factory recipes together. No client construction, network/file operations, worker starts or route exposure. This is a contract for trusted registrar code, verified with effect counters—not a sandbox that proves arbitrary Rust code pure.
- **Validate the selected application:** reject occupied slots, duplicate binding/route identities, missing selected requirements and conflicting overrides before factory effects. Check the explicitly declared construction order: required groups must precede consumers. Reject invalid/self/forward dependencies rather than inventing runtime resolution. Typed constructors additionally enforce actual dependency types/lifetimes; string metadata does not replace them.
- **Freeze:** consume the pending application plan, keeping its typed groups and validated metadata together. No additions or implicit replacements afterward. Route descriptors and later bindings must agree; verify coverage during assembly.
- **Assemble:** application-owned code calls factories in that declared order and injects dependencies. It is not automatic heterogeneous graph execution. Tests compare actual creation order with the declared order. If construction fails, release already-owned synchronous resources in reverse dependency order (using explicit ownership scopes/drop order), preserve the original error, and return no application. Core registration errors describe metadata; application-owned typed error variants preserve specific factory/provider causes instead of adding every client failure to a framework-global enum. Effects outside memory are not rolled back by `Drop`; future adapters need explicit close/cleanup contracts before claiming such guarantees.

For the next slice, an omitted required typed slot/configuration is an assembly error. A full auto-wiring graph, arbitrary cycle resolution/diagnostics, dynamic registration and managed startup/shutdown are later work, not promises supplied by these metadata checks.

## Defaults, replacement and extension

Select an explicit implementation before invoking its default registrar; an unused default factory must never run. Two explicit choices conflict. Replacement must name a documented extension point—not depend on insertion order or a duplicate-error side effect.

A future generic home can be the default presentation provider. **Replace** selects another home implementation; **extend** contributes uniquely identified entries to an explicit extension surface rather than another competing home handler. Keep navigation entries, command routes and DI bindings distinct. Validate application-action targets against registered commands; menu visibility is not authorization. Home rendering and remote front ends are follow-on capabilities, not prerequisites for bootstrap core.

## Continuity with the reviewed base

Mission Control already composes domain-owned handler lists from injected ports and shares one command-port instance across selected inbounds. Its typed registry rejects duplicates before insertion; its separate domain router does not. Forge-framework supplies explicit handler → registry → dispatcher → API composition, but its duplicate handler insertion also mutates before reporting failure. Both supply useful seams, not the complete registrar contract above. Mission Control's UI catalog is hardcoded, not an existing default/replace/extend system. Preserve the successful pattern while moving implementation details into selected packages and making registration failure-atomic. [Mission Control review](../reviews/forge/mission-control.md) · [Framework review](../reviews/forge/forge-framework.md)
