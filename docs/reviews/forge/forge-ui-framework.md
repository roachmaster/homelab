# forge-ui-framework — optional presentation contracts and platform adapters

## Verdict

**Reuse selected presentation contracts and adapters; keep the entire UI stack optional; add real platform acceptance evidence before claiming a complete frontend framework.** This is substantially more than a set of empty traits: it contains validated UI models, HTML/CSS generation, static artifact response logic, web/iOS shell packaging, canonical command-envelope conversion, and a reusable in-memory QA harness. It is nevertheless not a running web application, browser automation implementation, native host, or backend service framework by itself.

## Review scope and evidence

Reviewed the current `main` snapshot on 2026-10-30: full recursive inventory, all eight crate manifests and export surfaces, selected core ports/navigation/screen contracts, the complete command adapter, web computation/static-response/artifact/shell/application-shell code, selected rendering and iOS packet/FFI code, QA driver/harness, and representative integration-test sources. This is focused architecture review, not exhaustive review of every style, visualization, template or platform implementation. Only `main` was accessible.

No builds, build scripts, tests, browsers, WASM applications or native tools were executed. No GitHub Actions workflow appears in the tree, and the exact reviewed commit returned zero GitHub check runs. **Source tests were inspected; no fresh test pass, visual result, accessibility audit or end-to-end browser/native behavior was established.** Private pins and source permalinks are retained outside this public report.

## Actual packages and dependencies

The eight-package workspace separates `forge-ui-core`, `forge-ui-command`, `forge-ui-template`, `forge-ui-web`, `forge-ui-ios`, `forge-ui-qa`, `forge-ui-web-qa`, and an integration-test package (`Cargo.toml:1–26`). Framework dependency pins are aligned within the workspace. The core UI crate depends only on `forge-core`, keeping serde, HTTP libraries and async runtimes out of portable UI contracts.

Command integration is an opt-in adapter rather than a core dependency. The template adapter uses framework serde/template capabilities. Web and iOS depend on core/serde/UI core/UI template; their build dependencies use `forge-build` for static resources. QA contains driver contracts and a concrete in-memory implementation; web-QA derives frontend-tool-neutral plans from web contracts. Consumers can therefore select a backend-only framework without taking any of these crates.

The workspace declares Rust 1.75, edition 2021, and MIT OR Apache-2.0; packages are not configured for registry publication. No license text file appears in the reviewed tree. These manifest declarations have not been validated against all resolved dependencies or target platforms.

## What the implementation actually does

Core defines UI surfaces, components, layouts, style tokens, events/intents, navigation, screen layers, accessibility/content/visualization models and narrow ports. The inspected ports separate layout, behavior, session/runtime state, frame rendering, template rendering, artifact access and command submission (`crates/forge-ui-core/src/ports.rs:27–170`). A validated screen stack enforces exactly one base layer, unique layer IDs and nondismissible base content (`screen.rs:67–153`). These are reusable invariants, not application navigation policy.

The command adapter gives an important concrete example of the desired transport-independent pattern. A UI intent becomes a typed command request; conversion writes its action into the canonical envelope's **header command** and its event identity into the request ID. Component identity and data remain payload fields. The reverse bridge validates the envelope and reconstructs the typed UI request (`crates/forge-ui-command/src/lib.rs:46–100`). Header-deterministic routing is already feasible without a separately named destination field.

Web code renders portable surfaces into HTML/CSS, assembles artifact sets, and answers transport-neutral GET/HEAD requests. `UiWebArtifactService` does not bind a socket or start an async runtime; a host must map real HTTP traffic to its request/response types (`crates/forge-ui-web/src/http.rs:105–202`). Its health response checks for a nonempty index artifact, not the health of backend dependencies or the application as a whole.

`ForgeWasmWebPage` packages generated HTML/CSS/JavaScript around **application-supplied WASM bytes**. It documents required exports and an ABI version, but packaging verifies only that the supplied bytes are nonempty rather than compiling or proving the application ABI (`shell.rs:9–45,107–174`). `UiWebRustComputationPort` asks a consumer-owned implementation to render and dispatch intents. Expected revision is present in request data; the trait does not enforce revision consistency (`computation.rs:27–78`).

The iOS capability models render packets and host/computation boundaries and packages shell resources. Its FFI helper serializes errors; unsafe ABI implementation and memory ownership explicitly belong to the consuming application (`crates/forge-ui-ios/src/ffi.rs:6–16`). These platform ABIs are presentation boundaries, not a reason to introduce runtime-loaded Rust ABI plugins into the homelab backend.

## QA: valuable contracts, not a browser pass

The QA driver expresses semantic locators, actions, assertions, observations and explicit network stubs. The harness creates a driver through a factory, runs a scenario, preserves trace/failure information and can reject unstubbed network requests (`crates/forge-ui-qa/src/harness.rs:93–151`). That is a useful seam for a real browser or native adapter.

The included in-memory driver validates component state and records actions. Its screenshot operation records a screenshot action; it does not capture pixels. Clicking a component records a click; it does not automatically execute the real product's JavaScript or Rust application behavior (`driver.rs:698–770`). In-memory scenarios can verify the model and test harness but cannot establish layout, focus, actual rendering, browser history, WASM correctness or native behavior.

Observed integration tests cover HTML escaping, component projection, artifact path validation, duplicate artifacts, static response behavior, locator uniqueness, disabled actions, scenario shape, explicit network stubs and preserved failure context. The command test checks intent/envelope round-trip. These are good authored tests. Real Playwright/WebDriver/native adapters and application-level journeys remain the consuming application's work; they were not exercised here.

## Risks and integration gaps

1. **Duplicate route insertion mutates before reporting failure.** `UiWebRouteMap::insert` replaces an existing href and then reports a duplicate (`application_shell.rs:31–41`), the same pattern found in the command registry. Preserve the original binding or discard failed configuration completely.
2. **Command conversion loses context.** The UI request model does not carry correlation ID or arbitrary envelope metadata, and conversion creates fresh empty metadata. When UI operations enter the shared service ingress, preserve trace/authorization context in the common boundary rather than discarding it during projection. Never treat the UI's chosen action as authorization.
3. **Rendering validation is not an untrusted-content sanitizer.** Text/attribute escaping and syntactic attribute validation are implemented, but allowed attribute names are not a security policy (`document.rs:18–46`). Treat application-provided render structures as trusted or add a separate explicit policy before allowing untrusted content to define markup/attributes.
4. **Artifact export is not a safe transactional installer.** It validates relative lexical paths, then directly creates/writes files beneath the chosen directory (`artifact.rs:208–266`). Existing symlink containment, rollback and concurrent writers are not handled there. Use a controlled fresh staging directory and a separate deployment mechanism.
5. **HTTP edge semantics need adapter tests.** HEAD handling clears both the body and the original content-length (`http.rs:88–95`), and invalid-path responses take a separate path. Test response metadata and method handling against the real server adapter rather than assuming this model is a complete HTTP implementation.
6. **UI state ownership needs a clear convention.** A WASM computation seam coexists with a progressive-enhancement application shell that manages local navigation-chrome behavior. Distinguish local presentation behavior from domain state and backend routing; do not duplicate business workflows in shell JavaScript or a central UI service.

## Recommended place in homelab

Keep this as an optional presentation capability family, not the base application package. A web-shell starter may select core, web and command integration; a backend application can omit all of them. Real HTTP adapters belong beside the host transport, with static assets handled separately from domain commands.

Domain-affecting UI actions should normalize into the same transport-neutral canonical envelope used by the shared ingress, then enter the same logical validated ingress and allowlisted destination resolution. The bridge should route to capability-owned handlers, not become a UI-plus-backend controller that parses every payload and orchestrates every domain. Rendering/session ports can remain local where they are presentation-only; there is no benefit in forcing every layout operation through a remote command bus.

For the professional portfolio, show a thin host adapter, one registered capability interaction, a small browser/WASM or server-rendered shell, and real acceptance tests for repeated navigation, dismissal, error states and network isolation. Keep those results distinct from in-memory contract tests. The demonstrated asset today is the separation of portable models, command conversion and platform packaging; the complete plug-and-play application experience is still an integration deliverable.
