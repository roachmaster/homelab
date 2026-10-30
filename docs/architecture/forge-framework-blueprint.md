# Forge framework: small kernel, explicit starters, cohesive capabilities

**Status:** Proposed target architecture, based on the complete 15-repository review set dated 30 October 2026. This is a design, not an implemented or production-validated framework.

## The product decision

Build **a Rust application-assembly framework**, not a new all-purpose application. Its promise is: add a supported starter, provide typed configuration and a capability handler, and receive predictable admission, lifecycle, diagnostics, and test seams. Keep business workflows out of the framework. Keep developer tools, model execution, client SDKs, and domain-specific engines outside its mandatory dependency graph.

The framework is linked into a **reference application** that proves the developer experience. That application owns a jobs capability and demonstrates REST and Kafka reaching the same logical ingress. A separate deployment for every crate is neither required nor desirable.

“Spring Boot-like” means convenient starters, conditional defaults, explicit overrides, consistent configuration, and dependable application startup—not copying Java reflection or building a universal service locator. Spring Boot's own guidance describes packaged auto-configuration, conditional defaults that can yield to user configuration, and focused tests for those combinations. The Rust mechanism proposed here is explicit and statically linked. [Spring Boot auto-configuration](https://docs.spring.io/spring-boot/reference/features/developing-auto-configuration.html)

The selection is evidence-led: retain the small framework command seams, Mission Control’s shared command-port/lifecycle patterns, and HPVS’s registration/ownership discipline rather than combining whole workspaces. See the [repository reuse map](reuse-map.md), [module authoring contract](module-authoring.md), [decisions](decisions.md), and [delivery roadmap](../roadmap/forge-framework.md).

## 1. One logical ingress, multiple transports

```text
REST adapter ----\
                  -> canonical envelope -> shared bridge -> registered handler
Kafka adapter ---/                         admission          -> feature use case
                                                               -> owned ports / fan-out
```

The canonical header carries enough explicit information to resolve exactly one allowed operation and contract version. A single stable `command` identifier can be the destination; separate destination/operation fields are optional, not a requirement. Envelope schema identity and operation-contract identity are distinct when independently versioned.

Transport path/topic mappings may supply routing values, with documented precedence and conflict rejection. The body is not inspected to guess the route. Adapters preserve correlation, original deadlines, and stable idempotency metadata. Verified identity/context is separate from claims supplied inside an untrusted envelope.

For Kafka, the consumer's broker credentials authenticate the consumer connection, not the original producer or the tenant named in a message. The adapter needs a documented trusted producer/gateway mapping or independently verified producer claims before constructing caller context. Unverified header assertions cannot become an authorized principal.

The bridge owns envelope/header consistency, supported versions, deadline admission, allowlisted route lookup, route authorization, and dispatch. It does **not** own domain validation, resource-level authorization, SQL, HTTP response generation, Kafka commits, or business orchestration. A handler decodes its versioned body and calls its capability's application API. Fan-out begins only after admission and is governed by that use case.

The top-level catalog selects a capability-owned binding. Typed request enums, context, and secondary command registries can stay local to that capability; adding a domain must not enlarge a universal request enum or application context in the bridge. The registration object should provide both inspection metadata and its executable binding, so a capability cannot appear installed in one catalog but be absent from a separate dispatch map.

There must be no alternate public dispatch API that avoids admission. A route names a registered operation; it never names executable code, a class path, a shell command, or a library to load. “One path” is a logical contract, not one thread, one server, or one bottleneck process. Replicas and concurrent workers may implement it.

## 2. The diagrams answer different questions

C4 offers system context, container, component, and code views at different levels of detail. Here the first three describe the **reference application**. A C4 container is an application or data store, not a Rust crate; the separate dependency diagram describes packages. The component view zooms into one application host. These distinctions follow the official [C4 diagram overview](https://c4model.com/diagrams), [container guidance](https://c4model.com/diagrams/container), and [component guidance](https://c4model.com/diagrams/component).

### System context: who uses the assembled application?

![C4 system context of the proposed reference application](diagrams/01-context.svg)

The downstream action is optional. Neither the framework nor its crates appear as independently deployed systems.

### Containers: what runs and stores state?

![C4 containers of the proposed reference application](diagrams/02-containers.svg)

One Rust host contains ingress, dispatch, capability handlers, and supervised workers. PostgreSQL is a proposed reference-adapter choice, not a framework requirement. Kafka is an external delivery platform. Replicas, ingress gateways, and deployment topology are intentionally outside this container view.

### Components: where does responsibility live?

![C4 components inside the application host](diagrams/03-components.svg)

Dashed arrows show construction/lifecycle control; solid arrows show runtime interactions. Registry and bridge are conceptual components and may be private modules within one crate. Workers are a later milestone; the first acceptance slice need not pretend to execute work.

This component view focuses on admission and durable job state. The optional downstream effect shown in the context/container views is deliberately not expanded here; when introduced, its worker invokes a capability-owned effect port through a separate adapter, with the recovery guarantees below.

### Packages: what may import what?

![Proposed compile-time package dependency DAG](diagrams/04-package-dependencies.svg)

The application is the composition boundary. Handler bindings and small feature-specific adapters can start as its boundary modules, depending on both ingress contracts and the `jobs` capability. Extract a reusable adapter crate when another real consumer or an enforceable dependency need justifies it. Never move feature-owned ports into a universal framework `ports` crate.

## 3. A deliberately small package surface

Names below describe proposed roles; they are not claims that these packages already exist or final public API names.

| Unit | Owns | May depend on | Must not depend on |
| --- | --- | --- | --- |
| `forge-ingress-contract` | Canonical envelope/header, ingress/handler/admission contracts, controlled outcomes | Small transport-neutral value dependencies | HTTP/Kafka, concrete handlers, domain models, storage drivers |
| `forge-bridge` | Validated immutable registry and admitted dispatch | Ingress contract | Concrete capabilities or transport implementations |
| `forge-runtime` | Module plan, typed config assembly, startup/shutdown supervision | Focused runtime facilities | Domain policy or transport-specific mandatory dependencies |
| `forge-starter-http` | HTTP adapter and optional installation defaults | Ingress contract, runtime extension surface, HTTP library | Concrete business handlers |
| `forge-starter-kafka` | Kafka adapter, bounded consumer lifecycle, optional defaults | Ingress contract, runtime extension surface, Kafka library | Concrete business handlers |
| `jobs` | Job invariants, submission/status use cases, owned persistence/effect ports | Domain values and stable policy dependencies | Framework bridge, transport DTOs, SQL client |
| `reference-app` | Explicit composition, handler bindings, selected adapters | Selected framework packages and capability | Unrelated tools or domains merely because they exist |

The ingress contract is justified by actual shared consumers, not a license for `common-core` growth. Keep domain and application logic together inside a cohesive capability until a real dependency or change driver warrants another boundary. The framework must build without optional starters. Adding a starter must not silently enable every available transport.

## 4. Assembly without a second monolith

Use a two-phase host plan: **describe and validate**, then **construct and start**.

1. Load typed, namespaced configuration. Proposed precedence is framework defaults → explicit file → environment overrides → explicit programmatic overrides. Record the winning source without exposing secret values. Reject unknown critical keys, invalid values, and incompatible enabled modules.
2. Explicitly opt in to linked modules/starters. Each module declares its identity, required/provided capabilities, default registrations, and lifecycle resources. Nothing scans arbitrary packages or executes names from a configuration file.
3. Apply explicit user-provided implementations before conditional defaults. A default backs off only for a documented matching provider. Two explicit providers or duplicate route/version registrations fail with a diagnostic; never let insertion order choose silently.
4. Validate missing requirements, version compatibility, alias collisions, and dependency cycles **before changing the live registry or opening listeners**. Registration failure leaves the existing plan unchanged. Freeze the registry for normal operation.
5. Construct dependencies and start resources in dependency order. Startup failure closes already-acquired resources in reverse order. Readiness becomes true only after required dependencies are ready.
6. On shutdown: stop admission, pause new broker delivery, drain bounded in-flight work, cancel remaining cooperative tasks, join them, then close resources in reverse dependency order. Report unfinished or ambiguous work; do not label cancellation as rollback. If a task ignores cancellation or a blocking operation exceeds the grace period, record the unfinished work and apply an explicit host-failure/termination policy. A cancellation signal cannot guarantee a bounded join or undo committed effects; do not destroy resources still owned by a running task while pretending shutdown succeeded.

Keep construction and lifecycle management distinct from business workflows. Extension registration can use ordinary Rust functions, traits, and explicit `Arc` ownership. An in-process trait object is not a dynamic library/plugin ABI. Runtime library loading, reflection, distributed discovery, hot reload, and a large DI framework are out of scope until a real requirement proves their value.

Resource lifetime is explicit: host-owned singleton, request-owned value, or borrowed pool lease as needed. Returning a lease must be reliable on error and cancellation. Do not copy a synchronous single-threaded pool into an async shared host without establishing its concurrency contract.

**Health must report the injected implementation, not a separate memory double.** Liveness means the process can make progress; readiness means its validated configuration and required resources can serve the promised operations. Optional capability degradation is reported separately. Beginning shutdown removes readiness before draining. A database outage should not automatically turn a meaningful liveness probe into a restart loop; an uninitialized required store must never produce false readiness.

Define request/body limits, queue capacity, per-route concurrency, and retry budgets. When capacity is exhausted, apply documented backpressure or controlled rejection rather than accumulating unbounded futures or receipts. Health, queue depth, latency, and outcome diagnostics need stable semantics and redaction; headers are not permission to emit arbitrary high-cardinality or secret log fields.

See [module authoring](module-authoring.md) for the plug-and-play contract and override test matrix.

## 5. Reliability is a contract, not a transport feature

### Outcome semantics

| Outcome | Meaning | Adapter responsibility |
| --- | --- | --- |
| Rejected | No handler invocation after failed admission; later business rejection has its own effect contract | Controlled response or poison-message policy; no secret/internal detail |
| Completed | The operation's stated synchronous guarantee is satisfied | Map result; do not assume every side effect is transactional |
| Accepted | A named durable handoff/receipt has committed | Return accepted response or acknowledge delivery only at this point |
| Retryable failure | A transient condition exists; retry safety is separately defined | Respect retry budget, original deadline and idempotency identity |
| Failed / partial outcome | A contract-defined failure, possibly after earlier effects | Preserve actionable state and recovery information |
| Canceled / deadline exceeded | Work stopped or admission refused according to the contract | Do not imply prior committed effects disappeared |

An in-memory queue or successful header check is not durable acceptance. A handler returning quickly does not justify HTTP acceptance or a Kafka offset commit. Define route-specific completion points and teach each adapter to translate them.

### Idempotency and durable handoff

For the reference `jobs.submit` workflow, propose one atomic store operation covering the job record and its idempotency receipt. Scope the key to authenticated tenant/actor policy, route/version, and client key; compare a defined semantic request digest. The same key and same request replays the stable receipt. The same key with different content is a conflict. Concurrent first submissions must produce one winning job.

Replaying a receipt must **not** re-emit its original external effects. Define receipt retention and the supported retry window; expiration must not silently promise lifetime deduplication. A process-local map can support unit tests, not cross-restart correctness.

If a later milestone requires an external effect, a database transaction cannot atomically commit both local state and an unrelated remote service. Use an owned durable outbox/worker when required, plus destination idempotency or explicit reconciliation. Do not promise exactly-once external execution from at-least-once delivery.

### Kafka and cancellation

The Kafka adapter owns polling, partition ordering, acknowledgments/commits, retry timing, and dead-letter delivery. With concurrent handling, commit only the safe contiguous completed position; never skip an unfinished earlier record. Terminal rejection may be committed only under the configured disposition policy, including durable dead-letter publication when required. A failed dead-letter send is not successful handling.

A deadline is not reset on redelivery. Canceling an HTTP wait after durable acceptance does not erase the job. Canceling an accepted job is a separate authorized domain transition. An ambiguous database commit or lost response is recovered by the same idempotency key, not by issuing a new one.

### Submission sequence

![Submission sequence through the shared bridge to durable acceptance](diagrams/05-submit-sequence.svg)

This is the new-key success path. Admission rejection stops before handler effects. Store failure produces no accepted receipt unless durable commit is confirmed or recovered. Replay returns the existing receipt without another job/effect. Broker offset handling remains transport-owned.

## 6. Evidence needed before calling it a framework

- Core rules run without a server, broker, database, or framework container.
- Equivalent REST/Kafka inputs produce the same logical envelope and reach the same registered handler; mapping conflicts and spoofed identity are rejected.
- Every ingress entry point goes through header/version/admission checks; no convenience method bypasses them.
- Duplicate registration fails without replacing the original binding. Missing module requirements, incompatible versions, and override conflicts fail assembly deterministically.
- Real HTTP and Kafka tests establish parsing, response/disposition mapping, redelivery, offset ordering, backpressure, and shutdown—not just mocked method calls.
- Real storage tests establish concurrent idempotency claims, persistence across restart, failure after acquisition, and ambiguous-result recovery.
- Startup failure and shutdown leave no leaked tasks/listeners/connections; cancellation reaches the owner of the work.
- Package/dependency checks forbid transport-to-concrete-handler bypasses and bridge-to-feature imports.
- A second small capability can be added without changing bridge internals, adding a global registry lookup, or making unrelated starters mandatory.

Performance claims require a defined workload, environment, and measurement. This architecture is not a benchmark result. Existing passing tests in a source repository do not certify the assembled framework; the [roadmap](../roadmap/forge-framework.md) defines the necessary proof sequence.
