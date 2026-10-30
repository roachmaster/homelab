# forge-framework — reusable command foundation, not an application runtime

## Verdict

**Reuse the small contracts and dispatcher; adapt the ingress and registration guarantees; keep developer tooling optional.** This is the strongest direct foundation in the reviewed group for a package-oriented Rust framework. It already separates serialization, transport-neutral values, dispatch, templating, build resources, source introspection, and test doubles. It is not a Spring Boot-equivalent runtime, an HTTP server, or a broker integration today. Building on its narrow seams is preferable to expanding it into a universal application crate.

## Review scope and evidence

Reviewed the current `main` snapshot on 2026-10-30: complete recursive inventory, all crate manifests, the core and command implementations, selected template/schema/build/AST/test-tool implementations, examples, and test sources. The only accessible branch was `main`; no alternate branch divergence was exposed. Source references below identify repository-relative files and line ranges. Exact source pins and private links are retained separately, not published here.

No code, build script, test, or repository script was executed. The inspected tree contains no GitHub Actions workflow, and the exact reviewed commit returned zero GitHub check runs. That means **no current CI pass was established**, not that tests fail. Source contains 142 `#[test]` attributes across crate Rust files; this is a static inventory count, not an executed test result or coverage percentage.

## Actual packages and dependency direction

The workspace has seven reusable crates, an integration-test crate, and five examples (`Cargo.toml:1–34`). `forge-core` has no external dependencies. It supplies the value tree, metadata, errors, command/response envelopes, argument descriptors, risk/access classifications, and a generic backend-administration seam. `forge-command` depends only on core and uses ordinary Rust futures and trait objects, not Tokio or procedural macros. This makes both suitable for use below transport and runtime adapters.

`forge-serde` adds JSON/YAML/TOML conversion and a small schema-shaped validator. `forge-template` adds validated rendering and an explicitly registered method table. `forge-build` discovers template/overlay/schema resources and produces Cargo build outputs. `forge-ast` uses Rust syntax parsing to extract structured source documentation. `forge-test-tools` provides an in-memory mock backend. These are distinct capabilities, not requirements every running application should link.

The manifest declares version 0.1.0, Rust 1.75, and MIT OR Apache-2.0; reusable packages are not configured for registry publication. No license text file appears in the reviewed tree. These are manifest declarations, not a release-readiness or minimum-toolchain verification. Dependency freshness, vulnerabilities, and actual MSRV were not audited.

## Implemented request flow

The concrete envelope path is:

**application-created envelope → envelope validator → input bridge → typed request → registry lookup → handler validation → handler execution → response envelope**.

The header contains a command string, request ID, optional correlation ID, and metadata; the body is structured fields or bytes (`crates/forge-core/src/envelope.rs:5–102`). The command header can serve as a logical destination if the application establishes a closed, stable mapping. A separately named destination field is not required to satisfy header-deterministic routing. What is absent is a shared, enforced destination vocabulary and a reusable REST/Kafka normalization contract.

`CommandApi::receive_envelope` validates before invoking the bridge (`crates/forge-command/src/api.rs:96–107`). The dispatcher resolves the request key and calls one handler (`dispatcher.rs:27–34`). The handler's default lifecycle verifies key agreement before executing an async future (`handler.rs:64–102`). Registration can combine different concrete handlers through explicit `Arc` trait objects, and exact-key coverage can be checked (`api.rs:38–51`; `registry.rs:74–85`). This is ordinary in-process composition, not dynamic library discovery.

The registry shares one key, context, and request type across its handlers. That is a useful bounded command surface, but not yet a package-independent starter system: naïvely extending it across every capability would encourage one enormous request enum or context. Package-owned ingress facades should keep typed requests local. A small top-level route table can select those facades from header keys without owning each package's DTO parsing or business workflow.

## What is real, and what remains application work

Implemented foundations include deterministic value maps, recursive value validation, explicit missing-command errors, typed handler lifecycle, envelope validation, edge serialization, template/schema validation, source introspection, and mockable ports. The test suite contains assertions for these boundaries, including malformed envelopes rejected before bridge dispatch and exact registry coverage.

Not supplied here are socket listeners, Kafka consumers, authentication, authorization enforcement, deadlines, retry policy, idempotency, durable state, fanout execution, graceful shutdown, dependency-ordered module lifecycle, configuration binding, or production health aggregation. Access/risk enums describe categories; they do not authorize a request. The minimal-service example constructs an API and prints readiness text but does not start or exercise a service (`examples/minimal-service/src/main.rs:35–39`). Present it as a composition example, not deployed-service evidence.

## Important gaps before reuse

1. **Registration errors are not atomic.** Duplicate detection inserts the new handler before returning an error (`registry.rs:36–50`). A caller that retains the registry after that error now has the replacement binding. The observed duplicate test checks the error category, not the original binding's preservation. Validate before mutation or fail startup without exposing the partial registry.
2. **There are multiple validation entry paths.** The generic `handle` method and direct `dispatch` do not invoke envelope validation (`api.rs:88–123`). This is legitimate for trusted typed callers, but external adapters must all target the same validated ingress contract rather than choosing their own shortcut. The publicly constructible validated-request wrapper is a convention, not an unforgeable proof of domain validation (`handler.rs:17–39`).
3. **Response identity is not automatically propagated.** Convenience responses start without request/correlation IDs, and the reviewed dispatcher does not attach them. Cross-transport tracing needs an explicit response policy.
4. **Schema support is deliberately smaller than full JSON Schema.** The parser implements type, object properties/required/additional-properties and array items; unsupported keywords are not a general validation engine (`crates/forge-serde/src/schema.rs:164–224`). Do not claim that arbitrary JSON Schema constraints are enforced.
5. **General tooling already contains specialized templates.** The built-in registry includes game contracts and signal-processing artifacts (`crates/forge-template/src/builtins.rs:13–32`). Keep the rendering engine reusable, but consider capability-owned template packs instead of growing domain knowledge in the foundation.

## Recommended place in the assembled framework

Use core and command as the smallest runtime foundation. Add a proposed application/starter layer above them that accepts explicitly selected capability registrations, detects missing dependencies and duplicate routes before serving, and wires ports to implementations. Keep that layer responsible for wiring—not storage, feature behavior, or application use cases.

Any selected input adapters should normalize to the same envelope and invoke one logical ingress path. No particular transport or second adapter is required. That ingress validates common metadata and resolves only allowlisted header destinations; package-owned handlers perform domain validation and downstream fanout. The logical bridge can be implemented as small cooperating components. It need not become a giant crate.

Keep serde in adapters, code generation in development tooling, and domain-specific state in capability packages. The first acceptance proof should use an in-process ingress driver to reach one registered capability handler, reject unknown destinations, preserve correlation identity, and show that duplicate startup registration cannot alter an already valid configuration. Each later input adapter must conform to the same contract. Those are proposed integration checks, not results of this review.
