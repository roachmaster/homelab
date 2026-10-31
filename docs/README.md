# Portfolio documentation

## Forge: from experiments to a composable Rust framework

**Observed:** 15 repositories were reviewed read-only for architecture, contracts, boundaries, and implementation evidence. This is not a fresh build/test certification.

**Implemented core slices:** [six std-only Rust core packages](../src/rust/README.md), synchronous typed in-process examples and focused tests, including prepared-request I/O clients, pure/client-using tasks and typed registrar/freeze helpers. Three optional-demo packages prove configuration-driven assembly and compile-time dependency omission. Optional [forge-kernel disk access](../src/rust/forge-kernel/README.md) now performs real native filesystem inspection and client-local navigation. It is separate from core and is not a bootable kernel. This does not implement the full DSL, durable jobs or host lifecycle.

**Proposed:** a small, Spring Boot-inspired Rust assembly model with explicit capability modules, thin ingress adapters, a shared header-routed bridge, and deliberate lifecycle ownership. The blueprint is a design, not a deployed or completed framework.

- [Repository reviews — 15 reports](reviews/forge/README.md)
- [Architecture overview](architecture/README.md)
- [Cross-repository reuse map](architecture/reuse-map.md)
- [Framework blueprint](architecture/forge-framework-blueprint.md)
- [Hosted kernel research and implementation plan](architecture/forge-kernel-plan.md) · [primary-source ledger](architecture/forge-kernel-sources.md)
- [C4 and supporting diagrams](architecture/diagrams/README.md)
- [Architecture decisions](architecture/decisions.md)
- [Module-authoring contract](architecture/module-authoring.md)
- [Bootstrap, typed configuration and cohesive registrars](architecture/bootstrap-registrars.md)
- [Provider/factory dependency injection](architecture/dependency-injection.md)
- [Forge DSL semantic reference and framework skills](architecture/framework-dsl.md)
- [Implementation roadmap and acceptance gates](roadmap/forge-framework.md)

Keep review evidence in `reviews/`, target designs in `architecture/`, delivery gates in `roadmap/`, and implementations in the existing language-oriented `src/` tree. Do not duplicate source projects under documentation.

[Back to Homelab](../README.md)
