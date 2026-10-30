# forge-ios-sdk — a useful optional client, not the Rust runtime

## Decision

**Reuse as an optional Apple client after integration hardening; adapt its contract-testing and injected-adapter patterns.** This repository contains materially implemented Swift behavior, not just a UI architecture sketch. Its best contribution to the new framework is a concrete external consumer that can prove a stable contract works independently of transport and rendering choices. It should not determine the server's universal event model or pull UI concepts into the Rust kernel.

**Snapshot:** default branch `main`, privately pinned revision, inspected 2026-10-30. Source is private. This report contains original technical analysis; exact pinned private source links remain in the private evidence inventory. Branch inventory returned only `main`. No repository-specific `AGENTS.md` or repository skill was present.

## Actual package and dependency boundaries

`Package.swift` defines **one library target and one test target**, with Swift tools version 6.0 and minimum platforms iOS 16/macOS 13. There are no third-party package dependencies. Contracts, transport, providers, view models, runtime, styling, composition, and SwiftUI views are directories within the same compiled library—not separately enforced package boundaries. Foundation, Combine, and SwiftUI provide the underlying platform facilities. **Evidence I1.**

The useful conceptual layering is:

```text
Apple app composition root
  -> projection/event provider protocols
       -> mock, HTTP, no-op, or local-loop implementation
  -> projection runtime / view model
  -> SwiftUI projection view
       -> element / macro / layout registry
HTTP provider -> injectable HTTP protocol -> URLSession
```

The provider factory assembles a pair of protocol implementations rather than exposing a global container. The renderer registry is likewise explicit and injectable: built-in elements, macros, and layouts are selected by node-kind strings. Tests can replace those strategies with recording implementations. This is useful plug-and-play behavior without dynamic libraries or a microservice per strategy. **I2–I3.**

The package is not platform-neutral merely because its models are domain-neutral. SwiftUI and Combine are part of its single product. Keep it in a client layer; a later split of portable wire contracts from Apple rendering would be a separate, evidence-driven decision, not a requirement for the Rust framework's first version.

## Implemented flows and where the README is accurate

The conventional flow loads a projection through `ForgeProjectionProviding`, stores it in a main-actor view model, renders sections/entities/actions, turns an enabled action into an event, sends that event through `ForgeEventPublishing`, and replaces the displayed projection with the returned value. The HTTP strategy encodes an event as JSON and decodes a projection response; configured routes supply path, method, and headers. Mock and no-op alternatives exist. **I2, I4–I5.**

The composition flow is more flexible: a projection may contain a tree of element, macro, and layout nodes. The registry resolves kinds to renderer implementations; macros expand to subtrees; a shared state store supplies text-input bindings; an action dispatcher merges those bound values into an outgoing event. Styling is resolved through named tokens instead of hard-coded product behavior. **I3, I6.**

A local-loop adapter resolves an event to response actions, applies them to a projection runtime, and returns its current projection. Implemented response actions replace a projection by identifier, set a string state value, or clear render state. Unsupported actions and missing fields surface errors. **I7.**

Important distinction: **“publish” here means an async request/reply operation returning a UI projection. It is not a broadcast bus.** There is no Kafka adapter, backend handler registry, durable queue, subscription graph, acknowledgment policy, or internal fan-out facility. The local resolver is injectable, and the default implementation is a no-op; application-specific resolution is not supplied by this SDK. The README's limits on retries, caching, persistence, authentication refresh, and standalone example applications are consistent with the inspected implementation. **I2, I4, I7, I10.**

## Transferable mechanisms

1. **One protocol, multiple adapters.** The same caller can use memory, HTTP, or local-loop providers. Reproduce that principle on the server: any selected input adapters feed one normalized ingress boundary rather than duplicate application routing. The framework does not require a particular transport.
2. **Composition at application startup.** Route sets, provider pairs, and renderer registrations are app-owned choices. A Rust application builder can adopt that explicit assembly style without becoming a global service locator.
3. **Executable contract fixtures.** JSON examples, recording doubles, and intercepted HTTP requests are stronger evidence than documentation-only diagrams. Shared cross-language fixtures would be particularly valuable for the Rust/Swift boundary.
4. **Stable semantic identifiers.** Event types and render kinds let adapters remain generic. They still need versioning, validation, and ownership rules before becoming an externally supported contract.

The proposed server boundary should define its own transport-neutral command/event envelope, including version, route, request identity, correlation, and trusted caller context. The Swift client can map its interaction event into that contract. Its current event contains ID, event type, source, payload, and optional correlation ID, but no required schema version or explicit common ingress-header mapping. Static route headers are configurable; a complete server header contract is not implemented here. **I4–I6.**

## Integration risks visible in source

**Composition dispatch does not automatically refresh the displayed projection.** The built-in button renderer launches a task, suppresses its error, and discards the dispatcher's returned projection. Unlike the conventional view-model path, it does not assign that response to the view model. A local-loop publisher can update its separate runtime, but wiring that runtime into the visible view still belongs to the application. This is a concrete gap between successful dispatch tests and a complete user-visible action cycle. **I6, button renderer lines 27–30; I5, view-model lines 43–60.**

**Projection lookup by ID is weaker than the protocol name suggests.** The provider protocol's default `loadProjection(id:)` implementation ignores the identifier and returns the current projection. The HTTP projection provider implements only the current-projection operation and therefore inherits that fallback. Fixture tests proving identifier-based replacement do not prove equivalent HTTP behavior. An application needing remote projection lookup must implement it explicitly. **I2, protocol lines 16–20; I4, HTTP projection provider lines 20–28.**

**Multi-action response application is not atomic.** State-store writes and clears happen as actions are processed. If a later action fails, the catch path records an error and preserves the published projection, but earlier state mutations are not rolled back. Decide whether responses are transactional or intentionally best-effort, and test that contract. **I7, runtime lines 49–97.**

**Async ordering is not established by main-actor isolation alone.** View-model and runtime methods suspend while awaiting providers, with no request-generation check or cancellation ownership. An older request can therefore complete after a newer request and replace state. The local-loop order test awaits each event sequentially; it does not demonstrate ordering under overlapping calls. **I5; I7; I9, local-loop lines 156–168.**

**Transport trust and diagnostics need a clear policy.** URL construction accepts an absolute route URL as well as a relative path and then applies configured authorization headers. App-supplied routes must therefore be trusted, or origin/scheme restrictions must be added before routes become remotely configurable. Transport/encoding/decoding failures are deliberately reduced to coarse error categories. That is a useful public boundary but needs separate diagnostic detail and cancellation handling for production operations. **I4, client lines 69–121.**

Renderer registration silently replaces a duplicate kind, and recursive macro expansion has no visible cycle/depth budget. Both are acceptable only under a trusted, bounded registration/composition policy. These are further hardening considerations—not evidence of a tested exploit. **I3.**

## Tests, CI, and recommended next step

The source contains **94 Swift Testing annotations across 14 test-suite files**, plus JSON fixtures and recording/scripted helpers. Inspected tests assert contract round-trips, provider replacement, disabled action behavior, payload binding, renderer injection, style errors, HTTP methods/headers/bodies/status mapping, runtime replacement, response actions, and local-loop behavior. HTTP tests intercept `URLSession` through `URLProtocol`; they are not live backend integration tests. Renderer construction and recording assertions are not an interactive UI or visual-accessibility test. **I8–I9.**

The local validation script checks documentation/example presence and textual wiring, builds the Swift package, runs tests, and checks whitespace. Example checks do not compile a standalone example app, and the package manifest includes no example application target. No tracked GitHub Actions workflow exists; the exact-SHA Actions query returned zero runs. **No tests, builds, scripts, shipping helpers, or UI automation were executed in this read-only review.** **I1, I10.**

The highest-value next step is one end-to-end contract fixture shared by Rust and Swift: action -> normalized ingress command -> handler -> projection result -> visibly updated composition. Add overlapping-request and partial-response-failure tests before broadening the UI model. Keep internal backend fan-out separate from the client request/reply abstraction. This makes the SDK an optional showcase consumer of the framework rather than another source of kernel coupling.

## Source evidence index

All paths are pinned to the snapshot above; private permalinks are retained in the private inventory.

| ID | Inspected location |
|---|---|
| I1 | `Package.swift:1–28` |
| I2 | `Sources/ForgeIOSSDK/Providers/ForgeProviderProtocols.swift:1–21`; `Factories/ForgeProviderFactory.swift:3–77` |
| I3 | `Sources/ForgeIOSSDK/Composition/ForgeRendererRegistry.swift:3–105` |
| I4 | `Sources/ForgeIOSSDK/Transport/URLSessionForgeHTTPClient.swift:17–124`; `Providers/HTTPForgeEventPublisher.swift:20–38`; `HTTPForgeProjectionProvider.swift:20–29` |
| I5 | `Sources/ForgeIOSSDK/ViewModels/ForgeProjectionViewModel.swift:19–62`; `Views/ForgeProjectionView.swift:32–97` |
| I6 | `Sources/ForgeIOSSDK/Composition/ForgeActionDispatcher.swift:24–51`; `Elements/ForgeButtonElementRenderer.swift:10–39`; `Contracts/ForgeEvent.swift:3–25` |
| I7 | `Sources/ForgeIOSSDK/Services/ForgeProjectionRuntime.swift:39–125`; `ForgeLocalEventLoopPublisher.swift:15–30` |
| I8 | `Tests/ForgeIOSSDKTests/ForgeHTTPTransportTests.swift:31–208`; `ForgeAbstractionCoverageTests.swift` |
| I9 | `Tests/ForgeIOSSDKTests/ForgeEventResponseTests.swift`; `ForgeLocalEventLoopTests.swift:7–200`; remaining suite inventory |
| I10 | `scripts/cicd.sh:11–26`; `scripts/check-docs.sh:4–23`; `scripts/check-examples.sh:17–66`; `README.md:181–202`; `docs/testing.md:6–72` |
