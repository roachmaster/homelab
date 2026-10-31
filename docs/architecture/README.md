# Forge framework architecture

**Design direction:** a small Rust application-assembly framework with explicit starters, one shared header-routed ingress, and capability-owned handlers. Reuse successful boundaries from the existing projects without merging their domains into a new monolith.

The framework is a library family. No particular transport is required or selected; a broker and a network API are optional integrations, not baseline components. A separate reference application proves configuration, registration, lifecycle, adapter substitutability, durable acceptance, and extension ergonomics. The diagrams show that application—not crates disguised as deployed services.

**Implementation now started:** the [first Rust core](../../src/rust/README.md) implements typed contracts, synchronous routing, factory/provider/lease primitives and independent task/I/O-client contracts. The full assembly/DSL and operational diagrams below remain target designs, not claims that every component exists.

## Read in this order

1. [Repository reuse map](reuse-map.md) — where all 15 reviewed repositories fit; what to retain, adapt, keep optional, or exclude from the kernel.
2. [Framework blueprint](forge-framework-blueprint.md) — the target shape, package boundaries, C4 views, request sequence, and reliability contract.
3. [DSL semantic reference and framework skills](framework-dsl.md) — shared vocabulary, status labels, a worked composition model and task-specific workflows.
4. [Module and starter authoring](module-authoring.md) — how a developer plugs in one cohesive capability without editing the bridge.
5. [Bootstrap and registrars](bootstrap-registrars.md) — package-owned typed configuration, cohesive factory groups and the concrete next core slice.
6. [Provider/factory dependency injection](dependency-injection.md) — typed construction, acquisition scopes, leases, and bootstrap ownership.
7. [Architecture decisions](decisions.md) — tradeoffs and explicit non-goals.
8. [Delivery roadmap](../roadmap/forge-framework.md) — the smallest useful slice and evidence required before expanding.

[All diagrams and editable Graphviz sources](diagrams/README.md) · [Per-repository reports](../reviews/forge/README.md)

## The central path

```text
Selected thin input adapter, or an in-process caller
  -> canonical header + capability-owned typed payload enum + trusted context
  -> shared admission bridge
  -> one registered, authorized capability binding
  -> domain/application handler
  -> owned outbound ports and explicit fan-out
```

The shared envelope/bridge is generic over the capability payload enum. Header metadata selects the registered destination, and header/variant mismatches are rejected before effects. JSON is only an optional external serialization format, never the internal business payload.

Factories construct typed implementations; providers govern acquisition and reuse. Application bootstrap configures and calls selected registrars, stages their cohesive factory groups, validates/freezes the typed plan, then constructs dependencies and injects narrow ports into tasks/handlers. This next assembly layer is proposed; providers are not a request-time service locator.

“Plug and play” means adding a linked starter/capability, supplying typed configuration, and getting deterministic assembly or an actionable failure. It does not mean dynamic code loading, a universal service locator, mandatory microservices, or automatic exactly-once effects.

These are proposed architecture and extraction decisions based on read-only source review. They are not an implemented framework or production certification. Code and implementation evidence belong in the repository's implementation areas when built, separately from these design documents and reusable agent skills.
