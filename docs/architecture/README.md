# Forge framework architecture

**Design direction:** a small Rust application-assembly framework with explicit starters, one shared header-routed ingress, and capability-owned handlers. Reuse successful boundaries from the existing projects without merging their domains into a new monolith.

The framework is a library family. A separate reference application proves configuration, registration, lifecycle, transport parity, durable acceptance, and extension ergonomics. The diagrams show that application—not crates disguised as deployed services.

## Read in this order

1. [Repository reuse map](reuse-map.md) — where all 15 reviewed repositories fit; what to retain, adapt, keep optional, or exclude from the kernel.
2. [Framework blueprint](forge-framework-blueprint.md) — the target shape, package boundaries, C4 views, request sequence, and reliability contract.
3. [Module and starter authoring](module-authoring.md) — how a developer plugs in one cohesive capability without editing the bridge.
4. [Architecture decisions](decisions.md) — tradeoffs and explicit non-goals.
5. [Delivery roadmap](../roadmap/forge-framework.md) — the smallest useful slice and evidence required before expanding.

[All diagrams and editable Graphviz sources](diagrams/README.md) · [Per-repository reports](../reviews/forge/README.md)

## The central path

```text
Thin REST / Kafka adapters
  -> canonical header + body + trusted context
  -> shared admission bridge
  -> one registered, authorized capability binding
  -> domain/application handler
  -> owned outbound ports and explicit fan-out
```

“Plug and play” means adding a linked starter/capability, supplying typed configuration, and getting deterministic assembly or an actionable failure. It does not mean dynamic code loading, a universal service locator, mandatory microservices, or automatic exactly-once effects.

These are proposed architecture and extraction decisions based on read-only source review. They are not an implemented framework or production certification. Code and implementation evidence belong in the repository's implementation areas when built, separately from these design documents and reusable agent skills.
