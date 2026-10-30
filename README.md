# Homelab

A professional engineering portfolio: lessons from the Forge experiments, a proposed composable Rust framework, and reusable design practices.

## Forge framework portfolio

- [Documentation index](docs/README.md): observed repository findings versus proposed architecture.
- [15 repository reviews](docs/reviews/forge/README.md): reusable ideas, risks, and boundaries.
- [Framework blueprint and assembly model](docs/architecture/forge-framework-blueprint.md).
- [C4 and supporting diagrams](docs/architecture/diagrams/README.md).
- [Implementation roadmap](docs/roadmap/forge-framework.md).

The framework is currently a design proposal backed by read-only source reviews, not a completed runtime or a fresh test certification. Existing source sets remain organized by language and purpose; this repository does not yet contain runnable applications.

## Source sets

- [Source-set guide](src/README.md): boundaries, supporting files, and how to add a tool.
- [Rust](src/rust/README.md): capability crates and internal modules.
- [Python](src/python/README.md): purpose-oriented projects and import packages.
- [Bash](src/bash/README.md): executable tools and reusable shell libraries.

## Skills

Repository skills live separately from application code in `.agents/skills/`.

- [Design Package Boundaries](.agents/skills/design-package-boundaries/SKILL.md): capability-first packages and practical hexagonal boundaries, without unnecessary layers.

Example: `Use $design-package-boundaries to review this feature and propose cohesive packages, dependency rules, and a safe migration plan.`
