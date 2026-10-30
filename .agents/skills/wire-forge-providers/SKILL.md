---
name: wire-forge-providers
description: Model or review Forge factory/provider dependency wiring, constructor injection, scopes, leases and startup/shutdown ownership. Keep provisioning separate from command routing; use for substitution, pooled resources and assembly failure questions.
---

# Wire Forge providers

**Provisional starter.** Read the [canonical DSL reference](../../../docs/architecture/framework-dsl.md); consult the [DI design](../../../docs/architecture/dependency-injection.md) for scope/Rust details. These repository-local documents distinguish existing source lessons from proposed APIs; report missing context rather than inventing syntax.

## Workflow

1. Start with the consumer-owned port and actual requirement. Inspect supplied constructors/providers; names, fresh wrappers and one-call tests do not prove lifetime behavior.
2. Separate factory creation, provider acquisition/reuse, lease ownership and bootstrap wiring. Prefer injecting a ready port. Inject a narrow factory/provider only for genuine runtime creation or acquisition; no handler service locator.
3. Specify typed config/dependencies, default/override, scope, error and ownership transfer/borrowing. Host-shared is per assembly; transient may share captured state and is not request scope; OS-thread locality is not async request locality.
4. Validate missing dependencies, duplicates, cycles and lifetime compatibility before factory effects/freeze. Construct in order, inject ports, finalize routing and establish actual readiness; never publish a partial host or silently fall back after construction failure.
5. Assign cleanup: factory owns partial resources until transfer; host closes resources it owns after users stop. Preserve cleanup failures, handle outstanding leases and state whether admitted work finishes acquisition or is canceled. Grace exhaustion is not success.

For pools, define capacity/exhaustion, exclusive/shared access, cancellation, retained/reset state, quarantine and close policy. Do not copy HPVS's `RefCell` pool into a concurrent async host. `Arc` does not fix that; borrowed generic leases are not automatically dyn-compatible; ordinary `Drop` cannot await reset/rollback. Use the detailed DI reference when these constraints matter.

Deliver **port → factory inputs → provider scope/lease → owner; assembly order/overrides; failures/cleanup; checks and gaps**. Keep roles as modules unless a real boundary justifies another crate.

## Small example

For explicitly requested pooled rendering sessions, decide whether pooling stays private to the rendering adapter or a use case needs a session port. A failed render can contaminate retained state: require reset/validation or quarantine and explicit async cleanup ownership if needed. Test exhaustion, reuse state, cancellation and close—not just successful acquisition.

Older Forge factories and HPVS operation freeze are source patterns, not a general DI container or service-graph validator. Keep typed payload/header routing unchanged by provider substitution. No runtime implementation or remote action is authorized by this skill alone.
