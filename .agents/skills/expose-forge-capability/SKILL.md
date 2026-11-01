---
name: expose-forge-capability
description: Implement or review a reusable Forge capability's public typed configuration, factory and registrar integration, including proof from an independent consumer. Use when delivering a plug-in capability, not to require empty registrars for contract-only or standalone utility crates.
---

# Expose a Forge capability

Deliver an **importable capability**, not merely an example of how a consumer could
reimplement its assembly. Read the [shared DSL](../../../docs/architecture/framework-dsl.md)
for vocabulary and [provider wiring](../wire-forge-providers/SKILL.md) for scopes.

## Workflow

1. Inspect the library root/re-exports, Cargo dependency kinds and a real external
   caller. `pub` inside `examples/`, tests or a binary does **not** export a library
   API. Record which names consumers can actually import.
2. Put cohesive typed startup configuration, configured factories and a reusable
   registrar in the optional capability package's public API. Keep the algorithm
   or client modules independent of assembly internals. Use only the necessary
   core dependencies; zero normal dependencies is not a substitute for delivering
   requested integration. State any feature gate and its default explicitly.
3. Bootstrap supplies config and explicit dependencies, then calls the registrar.
   Stage a complete factory group without creating instances, executing callbacks
   or performing I/O. Validate configuration and registry conflicts; freeze before
   explicit construction. Preserve typed errors. Singleton/transient selection
   belongs to the host, and shared callback state is not reset by fresh instances.
4. Keep application-owned Task inputs, route sums and business preparation outside
   a generic rendering/IO capability. No hardcoded sample names, global lookup,
   `Any`, mandatory transport or provider-GAT type erasure. Example code should
   **consume public integration**, not secretly own the only implementation.
5. Prove it from a separate Cargo consumer: import normal library exports, register
   → freeze → create/acquire → use. Do not `include!` or `#[path]` an example/private
   module as the public-API proof. Test custom dependency preservation, duplicate
   rejection without overwrite, config/construction/runtime failures and deferred
   creation. Build with the optional capability omitted and inspect its normal
   dependency tree; an unused runtime branch is not compile-time omission.

## Completion gate

Report exact public import paths, dependency/feature policy, construction and
acquisition ownership, external-consumer checks and remaining limits. Separate source inspection, compiled consumer proof and
executed assertions. State which substitution boundary was tested: injected method
registries or fake Task/provider tests do not prove the concrete rendering service
is replaceable without changing its consumer. Add a port only when that requirement
actually exists. Match docs
and diagrams to actual exports—not architectural intentions. Keep example-specific
bridge/admission tests, but do not mistake them for reusable integration acceptance.
For the implemented reference, inspect
[`forge_template::assembly`](../../../src/rust/forge-template/src/assembly.rs) and
its [standalone consumer](../../../src/rust/forge-template/tests/consumer/Cargo.toml).
This skill does not authorize publication or unrelated restructuring.
