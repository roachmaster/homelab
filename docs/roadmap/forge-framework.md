# Forge framework delivery roadmap

**Status:** Proposed sequencing based on the complete repository synthesis. This is a plan, not completed implementation or authority to modify source repositories.

## Current implementation checkpoint

The [first std-only Rust workspace](../../src/rust/README.md) establishes part of Stage 1: validated headers, generic typed request/response, explicit payload-kind agreement, frozen duplicate-safe routes and an injected admission seam. Separate factory/provider/lease primitives support singleton/transient injection. The in-process greeting example proves composition without a transport. Independent `IoClient` and `Task` contracts now also support prepared-request execution, pure computation and injected-client orchestration with fake-only examples. A synchronous `Task` is not the supervised/durable worker in Stage 4. It does **not** complete Stage 1 deadline/security requirements, the provider graph validator, or the durable jobs slice below. The workspace documents exact executed checks and deferred behavior.

## Next core slice: bootstrap and registrars

**Do this next, before the durable jobs application.** Keep the five implemented libraries unchanged unless a concrete compatibility issue is demonstrated. Add one small proposed `src/rust/forge-bootstrap` package; no CLI, HTTP, IPC, filesystem adapter, remote execution, home renderer or new async runtime is required. The [bootstrap reference](../architecture/bootstrap-registrars.md) defines the single illustrative design and Rust type/ownership constraints.

| Patch | Concrete implementation boundary | Acceptance evidence |
| --- | --- | --- |
| A — Registration groups | Std-only `forge-bootstrap`: cohesive prepared-group metadata, typed group/slot wrappers, controlled duplicate/missing-selection errors and consuming freeze. An application-owned typed struct/tuple retains actual factory groups; metadata never becomes a service locator. | A registrar stages recipes without calling `create`/`acquire`; duplicate within a group, occupied slot or cross-group identity conflict leaves all prior registrations intact. Failed whole validation exposes no frozen plan. Freeze prevents further registration. |
| B — Typed bootstrap demonstration | Application/example module calls two package-style registrars with typed config enums/structs; all environment mapping stays at the boundary. Use fixture modules initially, not new production client crates. Validate selected requirements and explicit order; no automatic graph executor. | Missing selection, conflicting explicit choices, invalid config and self/forward dependencies fail before factory effects. Declared order matches recorded constructor order. Disabling an optional fixture does not construct its factories or add a mandatory I/O requirement to pure tasks. |
| C — Factory/provider/task assembly | Explicit application code consumes the frozen groups: singleton creation or lazy transient acquisition, dependent task recipe binds ready client handles, then task construction and the existing bridge binding. Preserve typed factory/provider errors. | Singleton reuse and host isolation; transient creation only on acquire; construction failure at each stage releases owned earlier values and publishes no application. Fake client substitution leaves task policy unchanged; denied/mismatched/unknown commands still cause no client effects. |
| D — Small consumer proof and safeguards | Runnable in-process front door plus a second configured group/default substitution. Update dependency guard, API docs and skills to mark only the verified subset implemented. No new transport. | Clean consumer builds; no normal core → implementation dependencies; no `Any`/JSON erasure or framework-global business enum; factory counters prove an overridden default never runs; duplicate is not implicit replacement; fmt, debug/release tests, strict Clippy/rustdoc and dependency checks pass. |

A/B describe proposed helper responsibilities, not an already frozen API. Choose the smallest generic wrappers that preserve typed ownership; do not invent a trait for every role. Package registrars configure/store cohesive factory groups, while application bootstrap chooses the packages and concrete assembly order. Every group's metadata and recipes originate together; check route coverage when converting the plan into the existing registry. Tests must exercise failed *multi-factory* groups, not only a single duplicate.

**Ready when:** a small consumer can choose implementation packages, pass typed startup config, run bootstrap and obtain a working typed in-process application—or a specific error with no partial publication. A client-using task prepares requests; injected clients execute them. A pure task remains independent of clients. This establishes the plug-and-play core, not a complete operational host.

**Later, only when needed:** automatic graph ordering/diagnostics, runtime registration, pooled/request-scoped providers, async factories/cleanup, managed workers/readiness/shutdown, concrete remote/local clients and presentation/navigation adapters. A generic home can later expose explicit default/replace/extend contracts; do not implement a UI to prove factory registration. The intended application front door can support remote tools, but it is not synonymous with a CLI parser and no specific transport is selected.

## Later vertical slice: one useful workflow, not a new platform

After the bootstrap core is proven and a durable consumer is selected, build the illustrative `jobs.submit` → `jobs.status` workflow. The first route validates a bounded work request and returns a stable durable receipt; the second makes that acceptance observable. Use a trivial, explicitly documented job kind. Execution, network/broker integrations, scheduling, remote model calls, and deployment orchestration are not prerequisites for proving the first admission/storage seam.

Start with an in-process contract driver, the shared bridge, one capability, and a real local durable-store adapter. No input transport is selected or required. A thin CLI or network adapter can be considered later for an actual consumer; neither is a prerequisite. A memory adapter is useful for unit tests but cannot satisfy the durability gate. This is intentionally more meaningful than a health endpoint and smaller than recreating Mission Control.

## Milestones and exit evidence

| Stage | Scope | Exit evidence | Do not add yet |
| --- | --- | --- | --- |
| 0 — Select the salvage | Complete the repository reuse map; identify minimal contracts and licensing/ownership constraints before code copying | Every candidate has a source report, concrete extraction boundary, known limitations, and a validation plan | Wholesale repository merges or renamed application scaffolds |
| 1 — Admission kernel | Generic `Envelope<P>`/`Bridge<P>`, capability payload enum, immutable registry, header/version/variant checks, trusted-context authorization | Duplicate registration is non-mutating; every entry point validates; denied/unknown/conflicting routes and header/variant mismatches have zero handler effects; no internal JSON fallback | Transport libraries in the kernel; generic global service lookup |
| 1b — Bootstrap core (next) | Selected typed registrars stage cohesive factory groups; validate/freeze; explicit construction/provider ownership and task binding | Patches A–D above; zero factory effects during registration, atomic group failure, no partial application, correct optional/default selection | Concrete transports, automatic heterogeneous graph execution, home UI |
| 2 — Smallest durable vertical slice | Typed-enum in-process submit/status, jobs policy, real store, typed factory plus one host-owned store provider, constructor-injected ports, stable receipts | Integration tests cover success, same-key replay, changed-body conflict, concurrent duplicate submissions, restart persistence, store failure, and lost-response recovery | Worker execution, remote side effects, extra domains |
| 3 — Prove lifecycle and substitution | Validate/freeze the provider dependency plan before construction; explicit overrides, supervised startup/shutdown and replaceable provider; add an input adapter only for a real consumer | Missing/cyclic/invalid-scope bindings fail before factory effects; host instances are isolated; default/override conflicts fail without mutation; factory failure cleans partial assembly; replacing the provider preserves ingress/domain contracts; actual selected adapters conform; shutdown reports grace exhaustion | Mandatory second transport, dynamic plugin loading or distributed host orchestration |
| 4 — Execute one job type | Supervised worker, durable claim/completion, cancellation policy, retry budget | Crash/reclaim tests; no lost accepted jobs; state transitions and concurrency controls explicit; status reflects reality | General workflow DSL or unbounded fan-out |
| 5 — Prove extension ergonomics | Add a second small capability and one custom provider override without modifying bridge internals | Small composition-only integration; no mandatory unrelated dependencies; absent/default/override/conflict test matrix passes | Broad starter catalog before the extension contract stabilizes |
| 6 — Package for reuse | Version public contracts, document compatibility/support, publish runnable examples and measured limits | Clean consumer build, contract tests, dependency checks, reproducible demo and release checks | Production claims unsupported by operational evidence |

A stage can be split into small reversible patches. Its exit gate—not a date or amount of generated code—determines readiness for the next stage.

## Verification strategy

**Fast:** typed factory inputs/errors, provider selection and scope compatibility, no side effects on invalid assembly, capability enum contracts, header/variant mismatch rejection including direct typed callers, optional edge-codec conversion, admission rejection without effects, config merge/validation, registry transactionality, override conflict detection, dependency-graph checks.

**Real boundaries:** storage concurrency, cleanup, restart persistence, and ambiguity recovery; parsing, outcome mapping and lifecycle of each input adapter only if selected. Test retry, ordering and acknowledgment semantics only when that adapter actually has them. Keep test doubles, local real-dependency tests, and deployed-system tests visibly distinct.

**Lifecycle:** one shared instance per host with isolation across hosts; factory failure and start failure after each acquisition step; stop during active requests and poll loops; bounded drain and cancellation; all workers joined before resource destruction. Canceling a caller must not falsely report that an already committed job vanished.

The next typed registration index is not a runtime provider lookup catalog. Do not implement a pool, request scope or async factory abstraction just to check a DI feature box. Begin with configured recipes, explicit typed constructors and existing singleton/transient behavior; the later durable slice adds its host-owned store provider. Add scoped acquisition only for a real consumer, with lease-return, retained/reset state, capacity and cancellation tests; do not substitute an OS-thread cache for request scope. See [provider/factory DI](../architecture/dependency-injection.md).

**Developer experience:** from a clean consumer project, install only the necessary starter, register a handler, supply config, and run the application. Add a second capability without a framework-global business enum; a finite application-owned sum is permitted only when its single ingress genuinely combines different payload types. No JSON or runtime downcast substitutes for the typed contract. Inspect the dependency graph to prove optional code remains optional.

No source-repository test result is automatically a pass for the assembled framework. Preserve exact commands, environments, failures, and not-run checks when implementation begins.

Before executing any existing aggregate verifier, inspect it and its called helpers. A command named “verify” may restore tracked files, pull changes, install tools, regenerate sibling repositories, or commit/push. Select explicit non-mutating check commands for the first baseline; isolate intentional generation in a disposable workspace with reviewed output boundaries. Do not run a broad script merely because it appears in a README. These are concrete concerns in the [codegen review](../reviews/forge/forge-codegen.md), not hypothetical permission to alter source.

## Portfolio organization

Keep the public portfolio legible:

```text
docs/reviews/forge/       # one sanitized report per reviewed repository
docs/architecture/       # synthesis, decisions, module contract, editable diagrams
docs/roadmap/            # sequenced evidence-based implementation plan
src/rust/                # actual Rust implementations only when built
src/python/              # actual Python projects, not framework kernel dependencies
src/bash/                # operational scripts with documented scope
.agents/skills/          # reusable agent skills, separate from product code
```

Do not create empty implementation trees to imply progress. Keep private raw evidence, repository access details, credentials, deployment addresses, and source snapshots out of the public portfolio. Link architecture conclusions to the sanitized reports; keep deeper evidence privately traceable.

## Control growth before it starts

Every proposed addition must name a real consumer, the responsibility it owns, the dependency cost it imposes, the smallest public contract, and the tests proving it can be removed or replaced. Prefer one coherent capability over a new catch-all package. Retire duplicate prototypes only after their useful behavior is captured and the replacement is demonstrated; review does not itself authorize deletion or migration.
