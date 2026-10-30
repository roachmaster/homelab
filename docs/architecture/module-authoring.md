# Writing a Forge module or starter

**Status:** Proposed extension contract; no stable SDK or executable API is claimed yet.

## Plug-and-play should mean three explicit steps

1. Add a supported dependency and opt in to its module in the application composition root.
2. Supply typed namespaced configuration and any required capability implementation.
3. Start the host, or receive an actionable validation error before the application becomes ready.

Do not require copying a large example application, editing bridge internals, reflecting over type names, or adding a new domain to a central all-knowing switch. Static linking is the default. A runtime trait object is acceptable; a stable cross-library ABI is a different, deferred requirement.

## Choose the right extension

| Extension | Owns | Does not own |
| --- | --- | --- |
| Ingress starter | HTTP/Kafka parsing, mapping, transport lifecycle, outcome translation | Domain handlers or business policy |
| Capability module | Cohesive application/domain behavior and owned outbound ports | Transport DTOs, bridge implementation, host internals |
| Handler binding | Versioned body decoding and mapping into a capability API | Independent routing policy that bypasses admission |
| Driven adapter | One capability's external conversation and real contract tests | All repositories' data models or a generic universal repository |
| Operational module | Focused configuration/lifecycle/diagnostic integration | Unrelated domain logic hidden behind hooks |

A module need not be a separately published crate. A starter earns its dependency cost by making a real infrastructure integration easier to install and verify. Optional dependencies stay optional when the starter is absent.

## A module declaration should be inspectable

Define the following information, using ordinary Rust constructs initially:

- Stable module identity and compatible framework contract version.
- Required capabilities and provided implementations, with ownership of acquired resources.
- Typed configuration namespace, defaults, validation, and a redacted effective-configuration view.
- Explicit route/version registrations or transport mappings, if any.
- Dependencies that determine construction and lifecycle order.
- Startup, readiness, cancellation/drain, and shutdown behavior; partial-start cleanup.
- Public override points and exactly which default is suppressed by an override.
- Meaningful errors, health semantics, and the verification commands/scenarios.

Inspection metadata and the executable binding come from the same registration object. Health probes inspect the actual injected resources; liveness, readiness, and optional capability degradation are not interchangeable constant-success responses.

This is a design checklist, not a request to invent one giant trait with every hook. Use smaller interfaces and ordinary constructor parameters when different modules need different facilities. Avoid a global `Any` container that makes dependencies discoverable only at runtime.

## Registration and override rules

A registry is built locally by composition, validated transactionally, then frozen. Reject duplicate destination/version pairs, duplicate aliases, unsatisfied requirements, incompatible versions, and cycles. Failure must not insert or replace a binding before returning an error.

Choose explicit provider overrides before defaults are installed. A default may back off for one documented matching provider; two explicit providers remain an error. A string in configuration may select among compiled, allowlisted providers, but cannot load arbitrary code. Effective configuration should explain what was selected and why without logging credentials.

Route authorization is checked against trusted context before dispatch. Resource-level permission and domain validation belong to the capability. A binding does not make a route public merely by registering it.

## Example: add a `notifications.send` capability

1. Define a small notification command/result and the feature-owned `Delivery` conversation. Keep vendor DTOs out of these types.
2. Implement policy and a registered binding for one supported command version. Header routing chooses that binding; the body is decoded only after admission.
3. Add one delivery adapter or controlled fake, according to the environment. State whether the result means delivered, durably queued, or merely attempted.
4. Compose the binding and its dependencies explicitly. Configure a REST mapping and, only if needed, a Kafka mapping to the same logical operation/version.
5. Add tests for rejected admission without effects, resource authorization, delivery failure, cancellation, duplicate request behavior, and actual adapter semantics.
6. Verify that enabling notifications changes no bridge dispatch code and does not force the jobs capability or an unrelated model runtime into every application.

These are illustrative names, not an existing implementation or a mandatory public API. If durable acceptance is promised, install and test a real durable handoff before returning it.

## Minimum extension test matrix

| Scenario | Expected invariant |
| --- | --- |
| Starter absent | No listener, worker, or mandatory transport dependency |
| Starter enabled with valid config | One intended provider; readiness after required resources start |
| Explicit custom provider | The documented default backs off; unrelated defaults remain |
| Duplicate explicit provider / route | Assembly fails; previous registry unchanged |
| Missing requirement / incompatible version | Clear error before admission opens |
| Partial startup failure | Previously acquired resources close in reverse dependency order |
| Shutdown during in-flight work | Stop admission, bounded drain, cooperative cancel/join; report grace exhaustion and apply explicit host-failure policy for uncooperative work |
| Repeated or conflicting request key | Stable receipt or controlled conflict; no duplicate business effect |
| Real external dependency unavailable | Classified failure, not false readiness or successful-empty data |

The host is complete when the module's lifecycle remains understandable without reverse-engineering hidden callbacks. Boilerplate reduction alone is not success.
