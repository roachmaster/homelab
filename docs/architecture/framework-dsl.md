# Forge composition DSL — starter reference

**Provisional composition DSL; a small core subset is implemented.** This is a small shared vocabulary for the framework taking shape. The intended direction is a **Rust-embedded composition DSL** using ordinary typed functions/builders and statically linked modules. No parser, macro set, compiler, module/host-composition builder API or YAML/JSON manifest language is claimed. Names and spelling can evolve.

This page is the canonical reference for the three framework-specific skills. Use the existing [blueprint](forge-framework-blueprint.md), [module contract](module-authoring.md) and [DI design](dependency-injection.md) when more detail is needed. The [bootstrap/registrar reference](bootstrap-registrars.md) owns the next-slice staging, typed-storage and assembly design; it is not implemented syntax.

## Implemented subset

The [first Rust workspace](../../src/rust/README.md) exposes real `Envelope<P>`, `Header`, `Response<R>`, `Payload`, `AdmissionPolicy<C>`, `RegistryBuilder::freeze`, `Bridge::dispatch`, `Factory<T>`, `InstanceProvider<T>`, `Singleton`, `Transient`, `IoClient` and `Task` APIs. The runnable in-process examples demonstrate their actual syntax; the task/client example uses only in-memory doubles, not real external I/O. `Payload::kind` is an explicit, pure capability-owned variant mapping, including nested application kinds. This slice has no module DSL, runtime dependency resolution, provider pool, async host, production authentication, deadline/cancellation metadata or durable acceptance. The conceptual jobs notation below remains proposed, not a supported API.

## Label what exists

- **Implemented core:** actual public code and executed checks in the linked Rust workspace; only the documented subset.
- **Observed source pattern:** a mechanism found in a reviewed repository. Cite its report/source; inspected tests are not executed tests.
- **Proposed semantics / spelling:** the intended homelab behavior or illustrative syntax below, not an existing callable API.
- **Gap / unverified:** implementation, evidence or a decision still needed. Do not silently promote a design into working code.

## Shared vocabulary

| Term | Meaning and owner |
| --- | --- |
| Application / composition root | Selects concrete modules and dependencies; owns assembly/lifecycle, not business workflows |
| Capability | Cohesive policy/use cases, typed payload/result and consumer-owned ports; not necessarily a separate service |
| Module / starter | Optional cohesive capability/implementation package; owns its typed startup config, dependencies, factories and reusable registrar |
| Bootstrap / registrar | Application bootstrap configures and calls selected registrars; each registrar stages a complete cohesive factory group, without creating its clients |
| Startup configuration | Package-owned typed values/enums prepared at the application/environment boundary; separate from runtime command payloads |
| `Envelope<P>` | Canonical header plus a capability-owned Rust enum payload; JSON is only an optional external encoding |
| Route / binding | Header destination+version selects an allowed binding; it checks the expected variant before invoking the capability |
| `Bridge<P>` / operation registry | Shared admission and frozen allowed dispatch; not a domain switch or dependency service locator |
| Port | Consumer-owned behavior, outcomes and ownership contract |
| `IoClient` | Executes a preassembled typed request at an I/O boundary; may encode for that boundary, not assemble business content |
| `Task` | Pure computation or business orchestration through injected client contracts; owns request preparation, not concrete I/O mechanisms |
| Factory | Creates a typed value from explicit config and dependencies; errors remain visible |
| Provider / lease | Controls acquisition/reuse and valid resource use/release; scope is explicit, not inferred from a name |
| Assembly plan | Application-owned typed factory groups plus registration metadata; validated/frozen before explicit construction, not a heterogeneous service locator |
| Trusted context / outcome | Verified identity and execution metadata / typed result with explicit completion or acceptance meaning |

These are roles, not a requirement for a trait or crate per row. Keep coherent capability internals together and meaningful external boundaries separate.

## Core semantics

**Request path:** in-process caller or selected thin adapter → typed envelope → shared bridge → registered binding → capability → owned ports/fan-out. No transport is required. An optional boundary codec converts external data without effects, using the registered header/version mapping.

**Routing:** header → allowed destination/version → authorization/deadline admission → expected enum-variant check → handler. Never guess or repair the route from the payload. Direct typed callers receive the same checks. No internal JSON/value-map or `Any` fallback. All entries in one registry share `P`; genuinely heterogeneous ingress may use a finite **application-owned** sum of selected capability enums, never a framework-global business enum. Unify heterogeneous results explicitly too.

**Tasks and clients:** `Task::run` is a synchronous operation, not a scheduled/durable job. A task may compute without any I/O dependency or prepare requests and call injected typed `IoClient::execute` interfaces. Clients own the I/O and required boundary encoding, not business payload assembly. The task/client contracts do not depend on envelopes or each other; a composition-owned handler may adapt an admitted variant into a task input. Errors can follow earlier effects; neither trait adds retries or atomicity.

**Bootstrap and dependencies (next slice):** the application selects optional crates, loads typed config once, and invokes their registrars. Each stages a complete factory group, not running clients. Keep heterogeneous factories in typed application fields/tuples; metadata is for validation, not `Any` lookup. Validate config, duplicates, required selections and explicit construction order; consume/freeze, construct clients, establish provider lifetimes, bind dependencies into task factories, construct tasks and finalize the existing route registry. Inject ready ports by default. No partial application is exposed. Automatic graph execution, full managed lifecycle and broad graph policies remain deferred; see the [concrete next-core plan](../roadmap/forge-framework.md#next-core-slice-bootstrap-and-registrars).

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
  bootstrap: call selected registrars with package-owned startup config
  registration: stage typed store/task recipes; do not construct resources
  store binding: typed build_store(StoreConfig), host-owned provider
  input: in-process driver -> Envelope<JobsPayload> -> Bridge<P>
  assembly: validate -> freeze plan -> construct/inject -> ready -> admit
```

A submit header with a status variant is rejected before handler effects, not rerouted. A matching explicit store override suppresses only its default; two explicit providers conflict. A memory store can support unit tests but cannot justify durable acceptance. Only an actual durable handoff/commit can do that. The names above are illustrative, not existing exports.

## Source grounding and next use

The reviewed framework/Mission Control have typed-request and explicit registration/assembly seams, not a general discovered registrar/container or this exact capability-enum envelope. Their successful composition is being refined, not replaced wholesale; [bootstrap lineage](reuse-map.md#bootstrap-and-registrar-lineage) separates source behavior from the next contract. Older Forge contributes provider/factory experiments, not a verified general container. HPVS distinguishes factories, providers and leases, but its operation freeze does not validate service dependencies and its retained-state `RefCell` pool is not a concurrent async pool. See [payload lineage](reuse-map.md#typed-payload-lineage) and [provider lineage](reuse-map.md#providerfactory-lineage). These are adaptation inputs, not compatibility guarantees.

Use **[$model-forge-module](../../.agents/skills/model-forge-module/SKILL.md)** to explain/model assembly, **[$trace-forge-command](../../.agents/skills/trace-forge-command/SKILL.md)** for request flow, and **[$wire-forge-providers](../../.agents/skills/wire-forge-providers/SKILL.md)** for dependency/lifetime decisions. They are provisional repository-local starters, not runtime implementations. Keep them with this reference and refine them as real code and experience arrive.
