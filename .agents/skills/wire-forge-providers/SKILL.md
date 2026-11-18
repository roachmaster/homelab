---
name: wire-forge-providers
description: Start here for Forge plug-and-play. Use before designing, implementing, or reviewing a Forge capability, module, service, adapter, worker, or runtime dependency. Select implementations through typed registrars, wire factories and scoped providers into the normal host, and verify substitution and lifecycle.
---

# Wire Forge providers

Use this skill first for Forge plug-and-play work. Start at the actual executable,
host factory, or browser bootstrap. Trace the dependency to its consumer before
changing the wiring. A host should be able to select another implementation of
the same typed port without changing business logic or request routing.

## Wire the normal application path

1. **Choose the port and scope.** Use the consumer-owned contract and record who
   configures, constructs, acquires, and releases the dependency. Prefer an
   existing typed port such as `IoClient`. Keep pure computation, domain values,
   and contract-only crates free of provisioning. `Singleton` shares an instance
   within one owner. `Transient` creates on acquisition and may share captured
   dependencies. Define request or worker ownership explicitly when needed.
2. **Stage the selected factory.** Bootstrap reads configuration and chooses
   the implementation or explicit override. A registrar stages a cohesive
   group of typed config and factory recipes. Registration may validate values
   and identity metadata. File access, network binds, process starts, and worker
   starts happen only after the startup plan validates and freezes. Choose the
   override before construction so an unused default has zero effects.
3. **Create and inject.** Call the selected factories in dependency order and
   inject ready ports into consumers. Bind ready collaborators before creating
   factories that need them. Pass a narrow factory or provider when the consumer
   genuinely creates or acquires resources at runtime. Each worker gets its
   owned dependencies or an explicit message port. Keep registries in bootstrap.
4. **Own the lifetime.** A factory cleans up partial acquisition until it
   transfers ownership. Establish readiness before opening admission. On
   shutdown, stop admission and quiesce users before releasing their resources.
   Define how outstanding leases, cancellation, and cleanup errors are handled.
   Preserve typed request admission and routing when replacing a dependency.

Use the [implemented registrar APIs](../../../docs/architecture/bootstrap-registrars.md)
for typed registration, duplicate and required binding checks, and consuming
freeze. Composition owns the explicit construction order and lifetime checks.
Use the [DI reference](../../../docs/architecture/dependency-injection.md) for
pooling, request scope, lease constraints, and failure recovery. The
[DSL reference](../../../docs/architecture/framework-dsl.md) defines Forge terms.

## Follow a working example

- **Per-project instance** follows the [Agent assembly](../../../src/rust/forge-agent/src/host.rs).
  It creates one managed writer per writable project after freeze.
- **Host-scoped dependency** follows the [Portal host factory](../../../src/rust/forge-ide-web-host/src/service.rs).
  It owns the selected asset source and relay instances.
- **Runtime lease** follows the [model runtime provider](../../../src/rust/forge-llama/src/model_runtime.rs).
  It holds a GPU reservation across model replacement and creates the selected child.
- **Shared store and owned worker** follow the [durable jobs executable](../../../src/rust/forge-terminal/src/bin/forge-jobs.rs).
  It selects the verifier, store, and worker recipe through explicit startup wiring.
- **Browser I/O** follows the [Playtest wiring](../../../src/rust/forge-ide-web-host/README.md).
  Rust issues and verifies typed resource requests. The page-scoped JavaScript
  worker executes I/O and returns correlated results.

## Verify the wiring

Follow the normal entry point through registration, freeze, creation, and use.
Prove the selected instance is used and that rejection occurs before the
relevant effect. Where substitution or scope matters, exercise a second
implementation or acquisition and check identity, reuse, and cleanup at the
observable boundary. Use `make check-deps` when changing package or feature
edges and a focused test for runtime behavior. Report the port, factory inputs,
scope, owner, normal entry point, and checks actually run.

Use [Expose Forge Capability](../expose-forge-capability/SKILL.md) when shipping
public config, factory, and registrar exports. Use [Model Forge Modules](../model-forge-module/SKILL.md)
for capability design, [Trace Forge Commands](../trace-forge-command/SKILL.md)
for admitted request flow, and [Forge UML](../document-forge-uml/SKILL.md) when
the task calls for an ownership or construction diagram.
