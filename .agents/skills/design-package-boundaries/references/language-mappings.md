# Optional language mappings

Use only the relevant section. These are design mappings, not current library/API instructions or mandates to adopt a framework. Inspect the repository's toolchain and conventions before choosing syntax or commands.

## Rust

- A module boundary can hide stable internals; a crate boundary can enforce dependency direction, limit dependencies, or support real independent consumers. Do not make one crate per struct. A workspace is organization, not automatic architectural enforcement.
- Keep the capability's application API and required traits/types in its crate when external adapters need them. Use private modules and the narrowest applicable visibility; review `pub use` as part of the public surface.
- A trait can describe an external need. Plain functions, concrete types, closures, enums, generics, or trait objects may each be appropriate. Choose based on ownership, dispatch, compilation, and actual substitution needs; do not require `dyn`, generics, or a trait for every collaborator.
- Keep vendor error types, raw pointers, and framework request types behind adapters unless they are deliberately part of the public contract. Use `Result` for recoverable failures and `Option` for meaningful absence; do not replace typed outcomes with panics to mimic exception-oriented examples.
- Preserve borrow/ownership semantics when extracting code. Specify who owns a worker, stream, callback, or buffer, how it is canceled, and what completes before destruction. Concurrency safety on one side of FFI does not validate the other side.
- Verify crate features and optional dependencies do not pull adapters back into the core. Test the real FFI/database/network adapter separately from trait fakes and inspect dependency direction at the crate and module levels.

## Java

- Start with capability packages and intentional public APIs; use package-private types for cohesive internals where practical. Java packages are namespaces/access boundaries, not automatically separate build artifacts or JPMS modules.
- Choose a separate build module or module-system boundary when it enforces dependency direction or supports an actual release/consumer need. Do not create a module per class or assume a subpackage has special access to its parent.
- Interfaces for external conversations belong to the consuming capability. Pass collaborators explicitly; constructor injection can suffice without a DI container. Keep framework controllers, persistence implementations, and vendor clients out of domain policy.
- Decide exception/result semantics from the application's caller needs and conventions. Translate low-level failures without losing useful causes; do not universally mandate checked or unchecked exceptions.
- Verify actual transaction scope, proxy/interceptor ordering, initialization failures, and resource closure. A framework annotation or successful mocked unit test does not establish cross-system atomicity or correct assembly.

## C

- There is no built-in package system. Map a capability to a cohesive source/header group or library with an intentional public header and private internals. Build/link rules and include checks help enforce dependencies; moving files alone does not.
- Opaque structures and operation-oriented functions can protect invariants. Plain data structures are also legitimate where exposing representation is intentional. Do not simulate a class hierarchy solely to appear object-oriented.
- For a real external boundary, a core-owned operations table plus explicit context pointer can invert dependencies. Direct functions or concrete modules may be simpler for stable local work. Avoid global callback registries and hidden service locators.
- Define ownership for every input/output pointer, allocation, handle, and callback: borrowed versus transferred, valid lifetime, mutability, thread-safety, destructor, and failure state. Keep ABI and unsafe conversion details in narrow adapters.
- Use explicit status/results and contextual errors according to the codebase's conventions. Ensure every acquisition has a cleanup path and that partial initialization is safe; one cleanup block can be clearer than copying a blanket ban on `goto`.
- Use the repository's compiler warnings, sanitizers, analyzers, and integration tests where applicable. Unit fakes do not establish ABI compatibility, allocator pairing, resource safety, or real external behavior.
