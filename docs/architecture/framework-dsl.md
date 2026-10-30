# Forge composition DSL — starter reference

**Provisional design, not an implemented API.** This is a small shared vocabulary for the framework taking shape. The intended direction is a **Rust-embedded composition DSL** using ordinary typed functions/builders and statically linked modules. No parser, macro set, compiler, public builder API or YAML/JSON manifest language is claimed. Names and spelling can evolve.

This page is the canonical reference for the three framework-specific skills. Use the existing [blueprint](forge-framework-blueprint.md), [module contract](module-authoring.md) and [DI design](dependency-injection.md) when more detail is needed.

## Label what exists

- **Observed source pattern:** a mechanism found in a reviewed repository. Cite its report/source; inspected tests are not executed tests.
- **Proposed semantics / spelling:** the intended homelab behavior or illustrative syntax below, not an existing callable API.
- **Gap / unverified:** implementation, evidence or a decision still needed. Do not silently promote a design into working code.

## Shared vocabulary

| Term | Meaning and owner |
| --- | --- |
| Application / composition root | Selects concrete modules and dependencies; owns assembly/lifecycle, not business workflows |
| Capability | Cohesive policy/use cases, typed payload/result and consumer-owned ports; not necessarily a separate service |
| Module / starter | Opt-in declaration of requirements, provided bindings, config and lifecycle; a starter simplifies a real optional integration |
| `Envelope<P>` | Canonical header plus a capability-owned Rust enum payload; JSON is only an optional external encoding |
| Route / binding | Header destination+version selects an allowed binding; it checks the expected variant before invoking the capability |
| `Bridge<P>` / operation registry | Shared admission and frozen allowed dispatch; not a domain switch or dependency service locator |
| Port | Consumer-owned behavior, outcomes and ownership contract |
| Factory | Creates a typed value from explicit config and dependencies; errors remain visible |
| Provider / lease | Controls acquisition/reuse and valid resource use/release; scope is explicit, not inferred from a name |
| Assembly plan | Selected typed construction bindings plus dependency/lifetime diagnostics; distinct from operation routing |
| Trusted context / outcome | Verified identity and execution metadata / typed result with explicit completion or acceptance meaning |

These are roles, not a requirement for a trait or crate per row. Keep coherent capability internals together and meaningful external boundaries separate.

## Core semantics

**Request path:** in-process caller or selected thin adapter → typed envelope → shared bridge → registered binding → capability → owned ports/fan-out. No transport is required. An optional boundary codec converts external data without effects, using the registered header/version mapping.

**Routing:** header → allowed destination/version → authorization/deadline admission → expected enum-variant check → handler. Never guess or repair the route from the payload. Direct typed callers receive the same checks. No internal JSON/value-map or `Any` fallback. All entries in one registry share `P`; genuinely heterogeneous ingress may use a finite **application-owned** sum of selected capability enums, never a framework-global business enum. Unify heterogeneous results explicitly too.

**Dependencies:** factories create, providers acquire/reuse, bootstrap wires. Inject ready ports by default; inject a narrow provider only for actual runtime acquisition. Validate missing requirements, duplicate overrides, cycles and lifetime compatibility before factory effects/plan freeze. Construct in order, inject, finalize routing and check actual readiness before admission. Do not publish partial assembly.

**Lifetime:** host-shared is per application; transient is not request scope; OS-thread locality is not async request locality. Pools need explicit capacity, exclusivity, cancellation, retained/reset state and close policy. `Arc` does not make `RefCell` concurrent; generic borrowed leases are not automatically dyn-compatible; ordinary `Drop` cannot await cleanup. Keep borrowed versus owned resources and shutdown responsibility explicit. Details live in the [DI contract](dependency-injection.md).

## Tiny worked model

**Proposed notation only—not executable Rust or a file format.**

```text
Application reference_app: P = JobsPayload, result = JobsOutcome
  Module jobs owns JobsPayload, jobs policy and JobStore port
    requires store: JobStore
    handler: JobsHandler(store)                   # constructor injection
    route jobs.submit @ 1 -> expects Submit -> submit_binding
    route jobs.status @ 1 -> expects Status -> status_binding
  store binding: typed build_store(StoreConfig), host-owned provider
  input: in-process driver -> Envelope<JobsPayload> -> Bridge<P>
  assembly: validate -> freeze plan -> construct/inject -> ready -> admit
```

A submit header with a status variant is rejected before handler effects, not rerouted. A matching explicit store override suppresses only its default; two explicit providers conflict. A memory store can support unit tests but cannot justify durable acceptance. Only an actual durable handoff/commit can do that. The names above are illustrative, not existing exports.

## Source grounding and next use

The reviewed framework/Mission Control have typed-request seams but not this exact capability-enum envelope. Older Forge contributes provider/factory experiments, not a verified general container. HPVS distinguishes factories, providers and leases, but its operation freeze does not validate service dependencies and its retained-state `RefCell` pool is not a concurrent async pool. See [payload lineage](reuse-map.md#typed-payload-lineage) and [provider lineage](reuse-map.md#providerfactory-lineage). These are adaptation inputs, not compatibility guarantees.

Use **[$model-forge-module](../../.agents/skills/model-forge-module/SKILL.md)** to explain/model assembly, **[$trace-forge-command](../../.agents/skills/trace-forge-command/SKILL.md)** for request flow, and **[$wire-forge-providers](../../.agents/skills/wire-forge-providers/SKILL.md)** for dependency/lifetime decisions. They are provisional repository-local starters, not runtime implementations. Keep them with this reference and refine them as real code and experience arrive.
