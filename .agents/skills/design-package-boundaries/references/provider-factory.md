# Provider/factory boundaries

Read when reviewing construction, dependency injection, scoped acquisition or resource reuse. This is a design convention, not a mandate to introduce a DI framework, provider layer for every type, or a new crate for every factory.

## Distinguish the mechanisms

- **Port:** consumer-owned behavior and failure/ownership semantics.
- **Factory:** creates a typed instance from explicit configuration and dependencies; construction errors remain visible. A normal function or closure can be sufficient.
- **Provider:** governs acquisition and reuse of a narrow dependency: ready shared handle, new owned value, or scoped lease. It is not an unrestricted service locator.
- **Lease:** establishes how long an acquired resource is valid, whether access is shared/exclusive, and what happens on release or failure.
- **Composition root:** chooses factories/providers, validates dependencies, constructs and injects ports, and owns startup/shutdown. Keep dependency selection separate from the header-directed operation registry.

Normally inject the ready port directly into the handler. Inject a narrow factory/provider only if the consumer really needs runtime creation, per-operation sessions or scoped acquisition. Hide an adapter's internal connection pool behind its capability port unless session ownership is meaningful to the use case. Do not let handlers look up arbitrary services by strings, types or request headers.

## Specify the chosen scope

- **Host/application shared:** one selected instance per assembled application, not a process-global static. Shared ownership does not establish thread safety or exclusive access.
- **Transient:** a fresh outer instance per successful creation/acquisition. Cloned dependencies may still share internal state; transient does not mean request scope.
- **Request/operation scoped:** an explicit owner/context bounds values to an operation. OS-thread locality is not async request locality.
- **Pooled:** state capacity/exhaustion, waiting/deadline/cancellation policy, shared versus exclusive access, return, reset/validation/quarantine and close behavior. Lease return does not imply a clean resource; retained state may be intentional.

Use only scopes that a real consumer needs. Keep configuration and concrete constructors at the boundary; avoid hardcoded zero-argument construction, hidden global caches and silent fallback after creation failure.

## Assembly and lifecycle

Validate selected defaults/overrides, missing dependencies, cycles and lifetime compatibility before factory side effects or plan freeze. Duplicate registration must not replace a valid binding. Declarative keys support diagnostics; they do not replace typed constructor compatibility. Freeze the validated plan, construct in dependency order, inject ports, finalize operation bindings, then open admission only after actual readiness.

Record ownership transfer versus borrowing; only close resources owned by this assembly. Each factory cleans its partially acquired internals until successful ownership transfer. Failure must leave no published partial host. Release acquired resources in reverse dependency order; preserve the original failure and cleanup errors. Stop admission and new acquisitions before draining work. Release operation leases before closing their providers, and report uncooperative tasks or outstanding leases honestly.

In Rust, a generic associated lease can express borrowed ownership, but it does not automatically permit a `dyn` provider. Choose a generic/concrete provider or a genuinely object-safe capability-specific contract. `Arc` is not a thread-safety adapter for `RefCell`; native async trait syntax does not by itself make a trait dyn-compatible. Inspect actual bounds and executor requirements. Ordinary `Drop` cannot await reset/rollback/flush; define explicit asynchronous cleanup plus a safe fallback when needed. Avoid claiming RAII guarantees cleanup after process abort, leaked guards or already committed remote effects.

## Configured registrar boundary, when needed

An optional implementation package can own its concrete client, dependencies, typed startup config, factories and reusable registrar. Application bootstrap loads environment/configuration once and invokes selected registrars; registration stages complete recipes, not live resources. Keep unrelated factory types in typed application groups rather than an erased service map. Validate group/selection conflicts before construction and do not publish partial assembly. A default is skipped by explicit selection, not overwritten by a failing duplicate. This is a boundary pattern, not a requirement that every contract crate implement a registrar or that all projects adopt a DI container.

## Verify behavior, not labels

Test invalid dependency plans before construction side effects; non-mutating duplicate rejection; explicit override precedence; construction failure without publication; host isolation and reuse identity; transient state independence where promised; lease exhaustion/return/reset or deliberate retention; partial-start cleanup; cancellation and provider close with outstanding work. Distinguish wrapper allocation from shared backend state. Handler tests should inject the port directly; real adapter tests prove the selected provider's behavior. Confirm replacing the provider leaves typed payload, header admission and domain contracts unchanged.

When borrowing patterns from an older repository, inspect executed paths and tests. Do not infer a general DI container from factory names, a frozen operation registry, generated templates or tests that call a provider only once. Separate observed behavior from proposed validation and async lifecycle additions.
