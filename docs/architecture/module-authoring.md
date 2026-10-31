# Writing a Forge module or starter

**Status:** Proposed extension contract; no stable module/registrar SDK is claimed yet. The five core crates already exist. Start with the [bounded bootstrap/registrar slice](bootstrap-registrars.md), not every future lifecycle hook below.

## Plug-and-play should mean three explicit steps

1. Add a supported dependency and opt in to its module in the application composition root.
2. Supply package-owned typed configuration and let application bootstrap call the package's registrar to stage its cohesive factory group.
3. Start the host, or receive an actionable validation error before the application becomes ready.

Do not require copying a large example application, editing bridge internals, reflecting over type names, or adding a new domain to a central all-knowing switch. No transport is mandatory; the first proof can use an in-process driver. Static linking is the default. A runtime trait object is acceptable; a stable cross-library ABI is a different, deferred requirement.

## Choose the right extension

| Extension | Owns | Does not own |
| --- | --- | --- |
| Ingress starter | Chosen input parsing/mapping, actual transport lifecycle, outcome translation | Domain handlers or business policy |
| Capability module | Capability-owned payload enum, domain/application behavior and owned outbound ports | Transport DTOs, bridge implementation, host internals |
| Handler binding | Header/typed-variant consistency and mapping into a capability API | Independent routing policy, JSON fallback, or admission bypass |
| Optional wire codec | Decode/encode an external format to/from the capability enum using the registered header/version mapping | Domain dispatch, payload-derived routing, or a generic JSON internal representation |
| Driven adapter | One capability's external conversation and real contract tests | All repositories' data models or a generic universal repository |
| Implementation crate + registrar | Concrete client, its dependencies, typed config enum/struct, configured factory group and reusable registration function | Mandatory imports in core, eager client construction during registration, or scattered environment reads |
| Factory/provider binding | Typed creation and scoped acquisition for a narrow dependency; explicit lifecycle owner | Arbitrary service lookup, ingress routing or domain policy |
| Operational module | Focused configuration/lifecycle/diagnostic integration | Unrelated domain logic hidden behind hooks |

Contract-only crates need not expose a registrar. An optional implementation crate should expose a reusable registrar when it participates in application assembly; it can own several cohesive factories rather than one crate per factory. A module need not be a separately published crate. A starter earns its dependency cost by making a real infrastructure integration easier to install and verify. Optional dependencies stay optional when the starter is absent.

## A module declaration should be inspectable

Define the following information, using ordinary Rust constructs initially:

- Stable module identity and compatible framework contract version.
- Required capabilities and provided implementations, with ownership of acquired resources.
- Typed configuration namespace, defaults, validation, and a redacted effective-configuration view.
- Explicit route/version registrations or transport mappings, if any.
- Dependencies that determine construction and lifecycle order; typed factory inputs, provider scope, lease ownership and any concurrency restrictions.
- Startup, readiness, cancellation/drain, and shutdown behavior; partial-start cleanup.
- Public override points and exactly which default is suppressed by an override.
- Meaningful errors, health semantics, and the verification commands/scenarios.

Inspection metadata and the executable binding come from the same registration object. Health probes inspect the actual injected resources; liveness, readiness, and optional capability degradation are not interchangeable constant-success responses.

This is a design checklist, not a request to invent one giant trait with every hook. Use smaller interfaces and ordinary constructor parameters when different modules need different facilities. Avoid a global `Any` container that makes dependencies discoverable only at runtime.

## Registration and override rules

Bootstrap invokes registrars; registrars stage complete factory groups into narrow typed application slots. Metadata and typed recipes travel together, and a failed group leaves earlier groups unchanged. The application resolves documented default/replace/extend choices before construction. Startup config is not a command envelope; environment loading belongs to the composition boundary.

Provider selection and operation registration are distinct parts of one local composition plan. Validate before factory effects; consume/freeze, construct dependencies, establish provider ownership, bind task dependencies and finalize frozen operation bindings before exposing the application. Neither catalog is a service locator. Heterogeneous groups stay in application-owned typed fields/tuples; the immediate design validates an explicit order rather than inventing auto-wiring. See [bootstrap registrars](bootstrap-registrars.md) for the single illustrative assembly sketch and [provider/factory DI](dependency-injection.md) for ownership.

A registry is built locally by composition, validated transactionally, then frozen. Reject duplicate destination/version pairs and unsatisfied selected requirements without inserting or replacing a binding on error. The next slice checks declared dependency order and typed constructor compatibility; generalized alias/version negotiation, graph diagnostics and lifecycle orchestration remain full-target extensions, not prerequisites for a useful registrar.

Choose explicit provider overrides before defaults are installed. A default may back off for one documented matching provider; two explicit providers remain an error. A string in configuration may select among compiled, allowlisted providers, but cannot load arbitrary code. Effective configuration should explain what was selected and why without logging credentials.

Route authorization is checked against trusted context before dispatch. Resource-level permission and domain validation belong to the capability. A binding does not make a route public merely by registering it.

For a later home/menu capability, replacement chooses one home provider explicitly; extension adds identified entries to a documented extension surface. It does not register multiple winners for the home command. Navigation is not authorization or a DI lookup, and no CLI/HTTP/IPC front end is required for core registration.

## Example: add a `notifications.send` capability

1. Define a capability-owned `NotificationsPayload` enum for the supported typed requests. Separately define the typed result and feature-owned `Delivery` conversation. Keep JSON/value trees and vendor DTOs out of these contracts.
2. Implement policy and a registered binding for one supported command version. Header routing chooses the destination; validate that the typed enum variant matches it before handler effects. A mismatch is an error, not a reason to infer or repair the route from the payload.
3. Add one delivery adapter or controlled fake, according to the environment. Supply its typed factory/configuration at composition, select a host-owned provider initially, and inject the ready `Delivery` port. Only inject an acquisition provider if delivery genuinely needs per-operation sessions. State whether the result means delivered, durably queued, or merely attempted.
4. Compose the binding and its dependencies explicitly. Use `Bridge<NotificationsPayload>` for a single-capability host; only a genuinely heterogeneous ingress needs a finite application-owned payload sum and bindings that extract its variants. Exercise it through the in-process ingress contract. Add an input adapter/mapping only when a real caller requires one; all selected adapters use the same logical operation/version.
5. Add tests for rejected admission without effects, resource authorization, delivery failure, cancellation, duplicate request behavior, and actual adapter semantics.
6. Verify that enabling notifications changes no bridge dispatch code and does not force the jobs capability or an unrelated model runtime into every application.

If an external format is selected, add a boundary codec mapping that format to the enum using the registered header/version contract. In-process calls construct enum values directly. All internal registry entries share the bridge's payload type; no `serde_json::Value` or `Any` escape is allowed. These are illustrative names, not an existing implementation or a mandatory public API. If durable acceptance is promised, install and test a real durable handoff before returning it.

## Full-target extension test matrix

Apply only rows whose mechanism exists; the [next-core gates](../roadmap/forge-framework.md#next-core-slice-bootstrap-and-registrars) are narrower.

| Scenario | Expected invariant |
| --- | --- |
| Starter absent | No listener, worker, or mandatory transport dependency |
| Starter enabled with valid config | One intended provider; readiness after required resources start |
| Explicit custom provider | The documented default backs off; unrelated defaults remain |
| Duplicate explicit provider / route | Assembly fails; previous registry unchanged |
| Header and typed payload variant disagree | Controlled rejection before handler effects, including direct in-process envelopes |
| Missing dependency / incompatible version / cycle / invalid scope capture | Clear error before factory side effects or plan freeze |
| Factory fails during construction | No published partial host; previously acquired resources cleaned up; no silent fallback |
| Selected provider lifetime | Host identity and isolation, or fresh transient / exclusive pooled acquisition, match the declared scope |
| Lease error/cancellation and provider close, if relevant | Explicit return/reset/quarantine policy; no loss of capacity or resource destruction under live users |
| Partial startup failure | Previously acquired resources close in reverse dependency order |
| Shutdown during in-flight work | Stop admission, bounded drain, cooperative cancel/join; report grace exhaustion and apply explicit host-failure policy for uncooperative work |
| Repeated or conflicting request key | Stable receipt or controlled conflict; no duplicate business effect |
| Real external dependency unavailable | Classified failure, not false readiness or successful-empty data |

The host is complete when the module's lifecycle remains understandable without reverse-engineering hidden callbacks. Boilerplate reduction alone is not success.
