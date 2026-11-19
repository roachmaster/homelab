# Forge documentation

Start with the [current implementation reference](design/implementation-status.md). It distinguishes implemented repository capabilities, configured live services, remaining work and historical proposals. The live portal exposes the same reference at `/design/implementation`.

## Current architecture and operation

- [Framework implementation inventory](architecture/forge-framework-blueprint.md#current-implementation): typed assembly, factories/providers, durable jobs, access and managed writes.
- [Implementation roadmap](roadmap/forge-framework.md): current slices and remaining acceptance work.
- [Service inventory](architecture/forge-ide-system-inventory.yaml): local deployment checkpoint with earlier remote observations dated separately.
- [Generated UI boundary](architecture/proposals/forge-ide-ui.md), [component compositions](design/forge-ide-components/README.md), [transitions](design/forge-ide-components/transitions.md) and [current layout captures](design/forge-ide-wireframes/README.md).
- [Native host](../src/rust/forge-ide-web-host/README.md), [source agent](../src/rust/forge-agent/README.md) and [Workbench](../src/rust/forge-ide-web/WORKBENCH.md): configuration and capability limits.
- [Chat/Mail runner](../deploy/mission-control/agent-runner/README.md) and [PostgreSQL dispatch](architecture/forge-postgres-inference-dispatch.md): durable queue/events, saved conversations, recall and polling.
- [Managed inline editing](architecture/proposals/forge-ide-inline-edit.md): staged review, revision fencing, receipts and reconciliation.
- [Portal Home](architecture/proposals/forge-portal-landing.md): generated launchers and documentation navigation.

## Game and Studio

[Scout character creation](https://portal.thedevex.com/design/characters) showcases the approved greybox, pixel finish and 4× illustrated skin with the complete animation set. [Editable recipes and tool](../src/python/pixel-art/README.md).

- [Game runtime and Studio](architecture/proposals/forge-game-engine.md).
- [Snapshots](architecture/proposals/forge-game-snapshot.md), [debug/trace](architecture/proposals/forge-game-debug.md), [deterministic replay](architecture/proposals/forge-game-debug.md#full-replay-and-historical-assets).
- [Physics](architecture/proposals/forge-game-physics.md), [assets](architecture/proposals/forge-game-asset-pipeline.md), [worker ownership](architecture/proposals/forge-worker-ownership.md).
- [Studio service](../deploy/mission-control/studio/README.md): implemented static-web profile; a host relay must be configured before the portal can use it.

## Forge authoring and diagrams

Begin capability work with [Wire Forge Providers](../.agents/skills/wire-forge-providers/SKILL.md). Use [bootstrap registrars](architecture/bootstrap-registrars.md), [provider/factory lifetimes](architecture/dependency-injection.md), [module authoring](architecture/module-authoring.md) and the [DSL reference](architecture/framework-dsl.md).

The [Mermaid gallery](architecture/diagrams/mermaid/README.md) identifies current implementation views and retained historical diagrams. [C4/DOT diagrams](architecture/diagrams/README.md) preserve broader architecture context. Diagram/API sketches remain labeled where they are illustrative; a proposed name is not automatically a Rust export.

## Historical evidence

The [16 repository reviews](reviews/forge/README.md), original proposal PDFs and earlier light wireframes are retained provenance. They are not current operating instructions or fresh build/test certifications. Current source, configured capability gates and release evidence supersede outdated future-only labels.

[Homelab](../README.md) · [Architecture index](architecture/README.md)
