# Forge architecture

The [current implementation reference](../design/implementation-status.md) and [framework checkpoint](forge-framework-blueprint.md#current-implementation) are the starting points. Reviewed 18 November 2026; distinguish source capabilities from what a live host enables.

## Current design references

- [Generated browser boundary](proposals/forge-ide-ui.md), [portal Home](proposals/forge-portal-landing.md), [managed editing](proposals/forge-ide-inline-edit.md).
- [Chat/Mail PostgreSQL dispatch](forge-postgres-inference-dispatch.md), [service inventory](forge-ide-system-inventory.yaml).
- [Game framework](proposals/forge-game-engine.md), [physics](proposals/forge-game-physics.md), [asset pipeline](proposals/forge-game-asset-pipeline.md), [snapshot](proposals/forge-game-snapshot.md), [debug/trace](proposals/forge-game-debug.md), [replay](proposals/forge-game-debug.md#full-replay-and-historical-assets), [worker ownership](proposals/forge-worker-ownership.md).
- [Implementation roadmap](../roadmap/forge-framework.md), [source component and transition contracts](../design/forge-ide-components/README.md), [current layouts](../design/forge-ide-wireframes/README.md).

## Proposed coding-agent dispatch

[Assignment/event/CLI/result workflow](proposals/forge-coding-agent-dispatch.md): provider and DAO boundaries, PostgreSQL task/attempt/event persistence, CLI process ownership and sequence diagrams for completion and reviewed takeover. This is a design proposal, not an installed queue or worker.

[Agent usage and portal visibility](proposals/forge-agent-usage.md): no inherited llama.cpp token budget for coding CLIs; per-agent allowance, reset, task tokens and cost provenance, with provider/collector ownership and a collection/read/refresh sequence. The Agents screen is proposed, not deployed.

## Contracts and construction

[Bootstrap/registrars](bootstrap-registrars.md) · [Dependency injection](dependency-injection.md) · [Module authoring](module-authoring.md) · [Foundation contracts](foundation-contracts.md) · [Foundation interface sketches](foundation-interfaces.md) · [DSL semantics](framework-dsl.md) · [Analysis/design method](analysis-and-design.md) · [Reuse map](reuse-map.md).

Current synchronous core, native/remote adapters, authenticated durable job capability and managed file operations have concrete implementations. Broader federation, generic remote scheduling, full DSL automation and bootable-kernel ambitions remain proposals. The Rust durable jobs store and the Python/PostgreSQL inference/build queues are different implementations with different ownership.

## Evidence and historical plans

[Mermaid implementation/proposal gallery](diagrams/mermaid/README.md) · [C4/DOT architecture views](diagrams/README.md) · [Remote node proof](remote-node-info.md) · [Remote IDE record](proposals/remote-ide.md) · [Hosted kernel plan](forge-kernel-plan.md) · [Source ledger](forge-kernel-sources.md) · [Decisions](decisions.md).

Retained PDFs and original schematic views are dated records. Current source/API guides, source/configuration audits and fresh validation records establish present behavior. A plan, picture or successful localhost fixture does not establish production enablement.
