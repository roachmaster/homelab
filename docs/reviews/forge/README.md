# Forge repository reviews

**Transport scope:** the proposed framework is transport-neutral. No Kafka, REST, or other particular transport is required. Historical transport capabilities and absences below remain source findings, not a mandate to add those technologies. The first framework proof can use an in-process ingress driver.

Read-only architectural reviews of 16 related repositories: 15 Forge experiments inspected on 30 October 2026, plus Pi Cluster Control inspected on 31 October 2026. These are original technical assessments, not source releases. Source observations describe the reviewed snapshots; reuse recommendations and the target framework are proposals. No fresh builds or tests were executed for these reviews.

## Reports

| Repository | Report |
| --- | --- |
| `forge-agent-runtime` | [forge-agent-runtime — reference behavior for capabilities and orchestration](forge-agent-runtime.md) |
| `forge-bitwave-system` | [forge-bitwave-system — the header-and-command idea is here, but the platform is unfinished](forge-bitwave-system.md) |
| `forge-codegen` | [forge-codegen — optional, explicit source-materialization tooling](forge-codegen.md) |
| `forge-framework` | [forge-framework — reusable command foundation, not an application runtime](forge-framework.md) |
| `forge-fsm` | [forge-fsm — small, optional state-transition capability](forge-fsm.md) |
| `forge-game-framework` | [forge-game-framework — focused session capability, not the application kernel](forge-game-framework.md) |
| `forge-hpvs` | [forge-hpvs — extract the composition discipline, not the whole machine](forge-hpvs.md) |
| `forge-ios-sdk` | [forge-ios-sdk — a useful optional client, not the Rust runtime](forge-ios-sdk.md) |
| `forge-llm` | [forge-llm — a demanding framework consumer, not the framework core](forge-llm.md) |
| `forge-os` | [forge-os — a numerical experiment, not an operating or application runtime](forge-os.md) |
| `forge-oss` | [forge-oss — useful generation ideas, unfinished application routing](forge-oss.md) |
| `forge-ui-framework` | [forge-ui-framework — optional presentation contracts and platform adapters](forge-ui-framework.md) |
| `forge` | [forge — a useful execution substrate, not yet the application framework](forge.md) |
| `forge_rust` | [forge_rust — historical experiments, not the runtime baseline](forge_rust.md) |
| `mission-control` | [Mission Control: recover the seams, not the whole assembly](mission-control.md) |
| `pi-cluster-control` | [Pi Cluster Control — a cluster use-case blueprint, not an implemented control plane](pi-cluster-control.md) |

## From evidence to assembly

See the [cross-repository reuse map](../../architecture/reuse-map.md) for consolidated dispositions. Start with the [framework blueprint](../../architecture/forge-framework-blueprint.md), then explore the [diagrams](../../architecture/diagrams/README.md), [architecture decisions](../../architecture/decisions.md), [module-authoring contract](../../architecture/module-authoring.md), and [phased roadmap](../../roadmap/forge-framework.md).

Repository-relative source locators in the reports identify reviewed evidence; they are not links to source files in Homelab. Review coverage and limitations are stated per repository.

[Documentation index](../../README.md)
