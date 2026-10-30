# homelab

Homelab tools and capabilities, grouped by implementation language and then by
purpose. Add source sets as real needs emerge; this repository does not yet
contain runnable applications.

## Source sets

- [Source-set guide](src/README.md): boundaries, supporting files, and how to add a tool.
- [Rust](src/rust/README.md): capability crates and internal modules.
- [Python](src/python/README.md): purpose-oriented projects and import packages.
- [Bash](src/bash/README.md): executable tools and reusable shell libraries.

## Skills

Repository skills live separately from application code in `.agents/skills/`.

- [Design Package Boundaries](.agents/skills/design-package-boundaries/SKILL.md): capability-first packages and practical hexagonal boundaries, without unnecessary layers.

Example: `Use $design-package-boundaries to review this feature and propose cohesive packages, dependency rules, and a safe migration plan.`
