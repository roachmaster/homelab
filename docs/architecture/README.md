# Forge architecture

The [current implementation reference](../design/implementation-status.md) and [framework checkpoint](forge-framework-blueprint.md#current-implementation) are the starting points. Updated 21 November 2026; distinguish source capabilities from what a live host enables.

## Current design references

- [Jenkins I/O client and Forge CLI](forge-jenkins.md): optional typed adapter, factory/provider wiring, token authentication and bounded operations.
- [Generated browser boundary](proposals/forge-ide-ui.md), [portal Home](proposals/forge-portal-landing.md), [managed editing](proposals/forge-ide-inline-edit.md).
- [Chat/Mail PostgreSQL dispatch](forge-postgres-inference-dispatch.md), [service inventory](forge-ide-system-inventory.yaml).
- [Game framework](proposals/forge-game-engine.md), [physics](proposals/forge-game-physics.md), [asset pipeline](proposals/forge-game-asset-pipeline.md), [snapshot](proposals/forge-game-snapshot.md), [debug/trace](proposals/forge-game-debug.md), [replay](proposals/forge-game-debug.md#full-replay-and-historical-assets), [worker ownership](proposals/forge-worker-ownership.md).
- [Implementation roadmap](../roadmap/forge-framework.md), [source component and transition contracts](../design/forge-ide-components/README.md), [current layouts](../design/forge-ide-wireframes/README.md).

## Coding-agent dispatch and usage

[Task correlation and event envelopes](proposals/forge-task-event-envelope.md)
imports Muse's reviewed plan for per-task trace IDs, generated event IDs, derived
event correlations and bounded history. Implementation and rollout are pending.

[Current coding-agent workflow](coding-admission.md) covers the implemented Agents UI, nine Work destinations, immutable policy/recipe, isolated native workers, scoped execute-only database roles, result paging and explicit review/resume. Work/Usage remain independently opt-in; this source checkpoint does not claim live deployment.

[Dispatch sequences](proposals/forge-coding-agent-dispatch.md) retain ownership/lifecycle/completion/takeover diagrams with current source mapping. [Agent usage](proposals/forge-agent-usage.md) separates supported provider allowance/account activity from Forge project token/runtime totals. Codex metadata is supported; Antigravity headless allowance is explicitly unsupported. Refresh performs metadata only and never automatically resumes a quota-blocked task.

## Contracts and construction

[Bootstrap/registrars](bootstrap-registrars.md) · [Dependency injection](dependency-injection.md) · [Module authoring](module-authoring.md) · [Foundation contracts](foundation-contracts.md) · [Foundation interface sketches](foundation-interfaces.md) · [DSL semantics](framework-dsl.md) · [Analysis/design method](analysis-and-design.md) · [Reuse map](reuse-map.md).

Current synchronous core, native/remote adapters, authenticated durable job capability and managed file operations have concrete implementations. Broader federation, generic remote scheduling, full DSL automation and bootable-kernel ambitions remain proposals. The Rust durable jobs store and the Python/PostgreSQL inference/build queues are different implementations with different ownership.

## Evidence and historical plans

[Mermaid implementation/proposal gallery](diagrams/mermaid/README.md) · [C4/DOT architecture views](diagrams/README.md) · [Remote node proof](remote-node-info.md) · [Remote IDE record](proposals/remote-ide.md) · [Hosted kernel plan](forge-kernel-plan.md) · [Source ledger](forge-kernel-sources.md) · [Decisions](decisions.md).

Retained PDFs and original schematic views are dated records. Current source/API guides, source/configuration audits and fresh validation records establish present behavior. A plan, picture or successful localhost fixture does not establish production enablement.
