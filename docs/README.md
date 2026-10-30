# Portfolio documentation

## Forge: from experiments to a composable Rust framework

**Observed:** 15 repositories were reviewed read-only for architecture, contracts, boundaries, and implementation evidence. This is not a fresh build/test certification.

**Proposed:** a small, Spring Boot-inspired Rust assembly model with explicit capability modules, thin ingress adapters, a shared header-routed bridge, and deliberate lifecycle ownership. The blueprint is a design, not a deployed or completed framework.

- [Repository reviews — 15 reports](reviews/forge/README.md)
- [Architecture overview](architecture/README.md)
- [Cross-repository reuse map](architecture/reuse-map.md)
- [Framework blueprint](architecture/forge-framework-blueprint.md)
- [C4 and supporting diagrams](architecture/diagrams/README.md)
- [Architecture decisions](architecture/decisions.md)
- [Module-authoring contract](architecture/module-authoring.md)
- [Implementation roadmap and acceptance gates](roadmap/forge-framework.md)

Keep review evidence in `reviews/`, target designs in `architecture/`, delivery gates in `roadmap/`, and implementations in the existing language-oriented `src/` tree. Do not duplicate source projects under documentation.

[Back to Homelab](../README.md)
