---
name: design-package-boundaries
description: Design or review package and module boundaries for a feature, refactor, or architecture change. Use capability-first hexagonal boundaries with thin ingress adapters, a shared header-routed bridge, and cohesive policy packages without unnecessary layers. Not a mandate to restructure an entire repository.
---

# Design Package Boundaries

Make the next meaningful change local: cohesive capability packages, narrow public contracts, and dependencies that keep external mechanisms out of application policy. Prefer practical ports and adapters, not maximal abstraction or a directory tree that merely looks hexagonal.

This combines a package-oriented, hexagonal and shared-ingress convention with *Clean Code* study principles. Formal hexagonal architecture is a separate supplement, not a claim about what the book prescribes. See [sources and rationale](references/sources.md) when explaining provenance; the skill works without retrieving those sources.

## Required ingress shape: shared header-routed bridge

Keep any selected ingress **adapters** as thin as practical: they map to the **same transport-neutral, typed request envelope and single logical bridge ingress path**, never independently invoke business handlers. No particular transport is required: REST, CLI, queues, and other inputs are examples, not mandatory components. Start in-process when that is sufficient; add an adapter only for an actual consumer. A port is the contract; an adapter is its transport implementation. Delegate work inward, then allow the admitted use case to fan out to the capabilities and driven ports it needs.

The internal envelope has a canonical header plus a **capability-owned Rust payload enum**, with the shared envelope/bridge generic over that payload (`Envelope<P>` / `Bridge<P>` conceptually). For non-Rust work, preserve the analogous explicit typed sum rather than a generic JSON/value tree. In-process callers construct typed values directly. JSON is only an external serialization format if a selected adapter needs it; do not use `serde_json::Value`, an untyped map, `Any`/downcasts, or an unknown-JSON catch-all as an internal payload fallback. The header alone supplies enough explicit destination metadata to select one registered route deterministically; adapters must not inspect business-body contents to decide routing. At minimum define a stable logical destination/operation identifier (one combined key or separate concepts) and supported contract/version identity, including envelope version where independently versioned. Use domain-appropriate names, not mandated transport field names. Document any additional routing discriminator required by this system; do not guess destinations from missing metadata.

Configured adapter mappings may populate canonical header values, with explicit precedence and conflict rejection. Preserve correlation, deadline/cancellation, and idempotency metadata where the contract defines them; never reset an expired deadline or generate a fresh idempotency key merely because a delivery is retried. Trusted authentication context must remain distinguishable from untrusted header assertions.

The bridge resolves an explicit allowlisted registry entry from the header, validates supported versions and route authorization, then checks that the typed payload enum variant matches the selected destination/contract before any handler effects. Reject mismatches even for directly constructed typed envelopes; never infer the route from the payload or rewrite the header to conceal disagreement. Reject missing, ambiguous, conflicting, unknown, or unsupported routing metadata with controlled outcomes before business effects. A route identifies a destination, **not** permission to call it. Never interpret a supplied header as an executable name, arbitrary class path, reflective method lookup, or dynamic code-loading instruction.

Keep the payload enum with its capability, not in a framework-wide catalog of every domain. All entries in one generic `Bridge<P>`/`Registry<P>` share `P`. A single-capability host can use its enum directly. Only a genuinely heterogeneous single ingress needs a finite **application-composition-owned** sum of its selected capability enums; adding a capability may extend that local sum but must not edit a framework/bridge-owned business enum. Bindings extract only their registered variants. Do not pretend unrelated payload types fit one homogeneous registry without this explicit mechanism.

If a wire format is selected, an edge codec converts it to the enum using the registered header/version mapping; codecs are bounded and side-effect-free. Inject the capability-owned codec/binding rather than letting a generic transport adapter import concrete handlers or guess routes from payload contents. Serialization and wire DTOs stay at that optional boundary. The internal bridge still validates header/variant agreement after conversion.

Keep the bridge narrow: admission and registered dispatch, not a giant business god-class. Handlers own domain validation, resource-level authorization and use-case orchestration; transport parsing and any protocol-specific responses, acknowledgments, retries or rejection lifecycle remain in the selected adapters. Specify business retry safety and idempotency in the core contract; input retries alone do not guarantee them.

“Single logical path” permits multiple replicas, concurrent workers and internal fan-out after admission. It does not require a single thread/process, one deployment, or routing every outbound port through ingress. This shared-ingress convention does not imply vendor hardware or a mainframe technology requirement. It is more specific than generic hexagonal architecture; it supersedes examples that let any selected input adapter bypass the bridge. For a concrete acyclic graph, read [worked package graph](references/worked-example.md).

## Keep the assignment bounded

- Match the requested mode: **design** proposes a structure; **review** reports evidence and tradeoffs; **implementation** changes only the authorized slice. Do not turn a review into a rewrite.
- Follow repository instructions, compatibility constraints, and existing idioms. This skill does not authorize unrelated refactors, installation, deployment, publication, commits, or pushes.
- With no repository or change brief, explain a conditional design using stated assumptions rather than claiming observed defects. Ask only for decisions that materially block useful work.

## 1. Map the real boundaries

Inspect relevant manifests, directories, visibility rules, imports, public APIs, entry points, construction, and tests before prescribing packages. If useful and available, inspect change history to distinguish demonstrated change pressure from forecasts.

Build a compact inventory:

| Existing unit | Responsibility / invariant | Consumers | Reasons to change | External knowledge | Public surface |
| --- | --- | --- | --- | --- | --- |

Trace one representative use case, including failure and shutdown. Mark domain decisions, use-case sequencing, transport parsing, persistence, framework types, resource ownership, and startup wiring. Draw **compile-time dependency edges** separately from runtime calls: calling an injected adapter does not mean the core imports its implementation.

Determine what “package” means here: namespace, enforceable module, build artifact, or published dependency. A package boundary is **not** automatically a process, service, repository, or deployment boundary. Do not propose microservices merely to separate code.

## 2. Choose cohesive capability packages

Name top-level application units after domain capabilities or business responsibilities, not a global pile of `controllers`, `services`, and `repositories`. Organize internals by role when that makes a capability understandable.

**Split when there is evidence of** independent change ownership, a stable consumer contract, volatile external technology, distinct lifecycle/security constraints, reusable capability with actual consumers, or an enforceable dependency boundary. Explain the concrete change that becomes more local.

**Keep together when** parts maintain one invariant, normally change together, require chatty internal access, or the split would create pass-through types, cyclic dependencies, coordinated versioning, and little isolation. Use private/internal modules for smaller stable internals. Neither file length nor class count alone justifies a package.

- Domain rules and application use cases may share a coherent feature package; separate them only when their responsibilities or dependency constraints earn the cost.
- Keep meaningful external adapters and consumers outside that feature's policy boundary. Use a separate package/build unit when it buys enforcement or independent change; a private boundary module can suffice in a small application. Do not leave unrelated features and infrastructure in one universal package by default.
- Avoid one package per class, one port per method, interfaces for every helper, and unnecessary generics throughout the codebase or mandatory DI frameworks. The generic typed envelope/bridge contract above is intentional, not a mandate to genericize every helper. Functions, concrete types, enums, interfaces, traits, or callbacks are tools, not architectural goals.
- Do not create `common`, `shared`, `core`, or `ports` as an unowned catch-all. A shared kernel or contract package needs a specific concept, actual consumers, clear ownership, and an explicit compatibility cost. Similar syntax is not evidence of shared policy.

## 3. Point dependencies toward policy

For each proposed package, state what it owns, exposes, may import, and must not import.

- **Feature core:** domain invariants, use-case policy, and the application-facing (driving/inbound) API. It does not import its HTTP, SQL, vendor SDK, UI, or framework implementation packages. Avoid leaking their types into its contracts.
- **Driving adapter:** parses and validates transport shape, handles protocol concerns, maps input through any needed edge codec to the canonical typed envelope, submits only through the shared bridge ingress port, and maps outcomes back. It does not select or call business handlers directly; protocol authentication mechanics may live outside the core.
- **Ingress bridge:** owns generic typed-envelope admission, header/variant validation and authorized registered dispatch. Its narrow contracts have explicit ownership; it depends on handler contracts, not concrete capability implementations. Domain rules stay with feature handlers.
- **Driven port:** the consuming core owns the smallest contract that expresses its external need. Keep the contract next to its use case/capability, not in a repository-wide port mega-package. A port describes a purposeful conversation and its semantics, not a mirror of the vendor SDK.
- **Driven adapter:** depends on that contract and translates it into the external mechanism. Multiple adapters are useful when real needs justify them; one production adapter plus a test double does not require a plugin framework. With a task/client pattern, the task prepares typed business requests; the client executes I/O and necessary boundary encoding, not business payload assembly. Inject client abstractions and keep pure computation free of mandatory I/O dependencies.
- **Composition root:** selects concrete implementations, reads deployment configuration, wires the allowlisted route-to-handler registry and other dependencies, and initiates the declared lifecycle. No business decisions or use-case workflows belong here. Runtime creation timing can stay in the use case through a narrowly scoped factory when genuinely needed.

**Provider/factory DI:** separate typed construction (factory), scoped acquisition/reuse (provider), and bootstrap ownership. Where a plug-in package exposes registration, let application bootstrap supply typed startup config and invoke a registrar that stages a cohesive factory group without constructing resources. Concrete clients and implementation dependencies remain optional; startup config is not a runtime business payload. Prefer constructor-injected capability ports; inject a narrow provider only for genuine runtime acquisition. Validate dependency/override/lifetime constraints before construction and admission; do not turn the operation registry into a service locator. For scope, lease, async cleanup and behavioral checks, read [provider/factory boundaries](references/provider-factory.md).

The intended package dependency graph is a DAG pointing inward. For a cycle, identify the actual edges and misplaced responsibility. Break it by moving policy to its owner, narrowing an API, introducing a consumer-owned port, or moving coordination to the appropriate higher-level use case. Do not hide cycles behind dynamic lookup, a service locator, or a new shared dumping ground.

Across capabilities, depend on the provider's intentional public API, not its internals. A consumer-facing port plus translation adapter is useful when vocabulary, versioning, or availability differs; it is not mandatory for every ordinary call. Do not permit mutual core imports or cyclic capability dependencies. A genuinely separate coordinating capability may own the workflow.

For a concrete example and split/keep decisions, read [worked package graph](references/worked-example.md). For a Rust, Java, or C implementation, consult only the relevant section in [language mappings](references/language-mappings.md).

## 4. Write contracts before moving files

Specify the externally meaningful properties of each changed boundary:

- **API:** intended consumers, minimal public operations, invariant protection, version/compatibility expectations, and forbidden internal access. Do not broaden visibility solely to make tests easier.
- **Data:** keep internal capability payload enums and typed results explicit; distinguish domain values and application request/result contracts from HTTP/message DTOs, persistence rows, and vendor objects. Translate at the boundary when representations carry different meaning. Do not duplicate identical types through every layer without a semantic reason.
- **Outcomes:** distinguish successful empty/absent results from failure; define caller-actionable error categories, contextual causes, and where translation and recovery belong. Do not swallow failures into empty collections, universal errors, or success-shaped defaults.
- **Effects:** state mutation, ordering, idempotency, retry safety, cancellation, and any consistency or atomicity guarantee callers rely on. These concerns are not supplied automatically by an interface.
- **Ownership:** who acquires, owns, shares, and releases connections, buffers, handles, workers, and subscriptions; startup failure cleanup and shutdown order; any thread-safety and callback lifetime constraints. Put the invariant with its owner rather than making each caller reconstruct a locking or cleanup protocol.
- **Multi-port workflows:** identify what happens when an early effect succeeds and a later effect fails. Choose an actual supported transaction boundary or an explicit recovery design; consider compensation, idempotency, or an outbox only when requirements justify them. Never imply that sequential calls, a `try` block, or a unit-of-work interface creates atomicity across independent systems.

Keep vendor-specific diagnostics available for investigation without leaking secrets or forcing callers to understand vendor recovery policy. Use the language's normal failure model rather than copying Java exceptions into Rust or C.

## 5. Verify the boundary, not just the diagram

Select checks that establish the changed contracts:

- **Ingress adapter units:** prove the canonical ingress with an in-process driver. Each selected adapter maps equivalent logical requests to that same contract; compare multiple adapters only when they exist. Verify mapping conflicts, metadata preservation, and absence of direct handler dispatch. Do not add a second transport merely to satisfy a test.
- **Bridge units:** test deterministic registered routing, unsupported versions, missing/ambiguous/unknown routes, spoofed authorization context, denied routes, header/payload-variant mismatches (including direct typed construction), and no handler effects on rejected admission. Test domain authorization and retry/idempotency semantics with the owning handlers.
- **Real ingress lifecycle tests:** for each installed adapter, verify its actual parsing, outcome mapping, capacity and shutdown behavior. Test acknowledgment, retry, ordering or dead-letter behavior only if its protocol has those mechanisms. An envelope-mapping unit test does not prove real transport lifecycle or end-to-end deduplication.

1. **Core tests:** exercise rules and use cases without UI/database/framework setup; use controlled collaborators for time, randomness, I/O, and failure when they affect behavior. Assert outcomes and meaningful effects, not incidental call choreography.
2. **Real adapter contract tests:** run the real adapter against the actual dependency or a representative isolated local instance. Check meaningful success, absence, error translation, serialization, and relevant lifetime/transaction assumptions. A fake passing its own tests is not evidence that a real adapter honors the port.
3. **Assembly/integration tests:** verify actual configuration and wiring, a representative end-to-end path, initialization failure, and relevant cleanup/shutdown. Unit isolation does not prove correct composition.
4. **Dependency enforcement:** use existing build/module visibility or architecture checks to prevent forbidden imports, adapter-to-handler bypasses, internal JSON/value-tree fallback payloads and cycles. Inspect public re-exports and test-only shortcuts, not just directory names. Report observed edges and any remaining unenforced constraint.

Use repository-native format, lint, build, and test commands; discover them rather than inventing a stack. Record exact commands and results. If a check needs unavailable credentials, infrastructure, or authorization, report it as **not run / blocked**, not passed. Do not weaken safeguards to make a refactor green.

## 6. Migrate in behavior-preserving slices

When implementation is authorized, establish baseline behavior and characterization tests where coverage is weak. Select one seam; define its contract, move or wrap one implementation, switch one caller, and verify. Preserve public behavior, error meaning, state transitions, ordering, ownership, and compatibility unless the user authorized a change to them.

Keep mechanical moves separate from behavior changes where practical. Retain a temporary forwarding API when needed for compatibility, with an explicit removal condition. Recheck the dependency graph and assembled system after each meaningful slice. Prefer reversible steps over a wholesale rewrite; stop and surface newly discovered requirements that exceed the agreed scope.

## Deliver a decision, not a folder tree

Use this compact review/design output; omit sections with no relevant content:

```text
Scope and mode: requested slice; inspected artifacts; assumptions / missing evidence
Current shape: responsibilities, public surfaces, observed dependency edges
Findings: severity; file/symbol evidence; consequence; minimal correction; tradeoff
Target: package -> responsibility / public API / allowed imports / forbidden imports
Dependency graph: compile-time arrows, separately noting runtime flow if needed
Split / keep decisions: change driver; expected benefit; cost; rejected alternatives
Contracts: errors, effects, data mapping, resource ownership, multi-port consistency
Migration: smallest ordered slices; preserved behavior; compatibility / rollback
Verification: commands and outcomes, or proposed checks; unrun checks and blockers
Open decisions: only unresolved choices that materially affect the design
```

For a small request, a short package graph and a few evidence-backed decisions are enough. Do not reward architecture by package count. Success means a maintainer can change one policy or adapter without learning or editing unrelated internals, and the tests and dependency rules support that claim.

