# Forge architecture

The [current implementation reference](../design/implementation-status.md) and [framework checkpoint](forge-framework-blueprint.md#current-implementation) are the starting points. Updated 23 November 2026; distinguish source capabilities from what a live host enables. The last qualified deployment is `95651a5`, ABI13/schema26; later review fixes are not a deployment.

## Current design references

- [Jenkins I/O client and Forge CLI](forge-jenkins.md): optional typed adapter, factory/provider wiring, token authentication and bounded operations.
- [Generated browser boundary](proposals/forge-ide-ui.md), [portal Home](proposals/forge-portal-landing.md), [managed editing](proposals/forge-ide-inline-edit.md).
- [Chat/Mail PostgreSQL dispatch](forge-postgres-inference-dispatch.md), [service inventory](forge-ide-system-inventory.yaml).
- [Game framework](proposals/forge-game-engine.md), [physics](proposals/forge-game-physics.md), [asset pipeline](proposals/forge-game-asset-pipeline.md), [snapshot](proposals/forge-game-snapshot.md), [debug/trace](proposals/forge-game-debug.md), [replay](proposals/forge-game-debug.md#full-replay-and-historical-assets), [worker ownership](proposals/forge-worker-ownership.md).
- [Implementation roadmap](../roadmap/forge-framework.md), [source component and transition contracts](../design/forge-ide-components/README.md), [current layouts](../design/forge-ide-wireframes/README.md).

## Coding-agent dispatch and usage

[Source content and verification identity](content-identity.md) separates task
UUIDs, canonical artifact digests and frozen policy/generation snapshots. Its
[portal reader](https://portal.thedevex.com/design/content-identity) includes
data, component and native/external sequence views. Content-v2 is selected
explicitly; historical v1 work and exact-candidate CI/publication remain intact.

[Task correlation and event envelopes](proposals/forge-task-event-envelope.md)
documents implemented M21 trace identities and the ABI12 bounded task reader.
The full correlation and every typed event are available through explicit paging;
live activation remains release-controlled.

[External author and native reviewer SDLC](external-agent-workflow.md)
documents the implemented M22 Muse database → native Codex review → Jenkins →
trusted merge flow, including bounded revision, cancellation and lost-ack recovery.

[Current coding-agent workflow](coding-admission.md) covers the implemented Agents UI, typed Work destinations, immutable policy/recipe, isolated native workers, scoped execute-only database roles, result paging and legacy explicit review/resume plus configured automatic workflows. Work/Usage remain independently opt-in; this source checkpoint does not claim live deployment.

[Dispatch sequences](proposals/forge-coding-agent-dispatch.md) retain ownership/lifecycle/completion/takeover diagrams with current source mapping. [Agent usage](proposals/forge-agent-usage.md) separates supported provider allowance/account activity from Forge project token/runtime totals. Codex metadata is supported; Antigravity headless allowance is explicitly unsupported. Refresh performs metadata only and never automatically resumes a quota-blocked task.

## Contracts and construction

[Bootstrap/registrars](bootstrap-registrars.md) · [Dependency injection](dependency-injection.md) · [Module authoring](module-authoring.md) · [Foundation contracts](foundation-contracts.md) · [Foundation interface sketches](foundation-interfaces.md) · [DSL semantics](framework-dsl.md) · [Analysis/design method](analysis-and-design.md) · [Reuse map](reuse-map.md).

Current synchronous core, native/remote adapters, authenticated durable job capability and managed file operations have concrete implementations. Broader federation, generic remote scheduling, full DSL automation and bootable-kernel ambitions remain proposals. The Rust durable jobs store and the Python/PostgreSQL inference/build queues are different implementations with different ownership.

The [PostgreSQL/queue split](proposals/postgres-queue-split.md) is also an
unimplemented proposal. No broker or orchestration microservice is enabled by
that document; existing database leases, committed state and scoped coordinator
admissions remain the current delivery boundary.

## Evidence and historical plans

[Mermaid implementation/proposal gallery](diagrams/mermaid/README.md) · [C4/DOT architecture views](diagrams/README.md) · [Remote node proof](remote-node-info.md) · [Remote IDE record](proposals/remote-ide.md) · [Hosted system plan](forge-system-plan.md) · [Source ledger](forge-system-sources.md) · [Decisions](decisions.md).

Retained PDFs and original schematic views are dated records. Current source/API guides, source/configuration audits and fresh validation records establish present behavior. A plan, picture or successful localhost fixture does not establish production enablement.
