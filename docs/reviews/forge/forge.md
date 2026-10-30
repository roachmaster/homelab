# forge — a useful execution substrate, not yet the application framework

## Decision

**Adapt the task contracts, duplicate-rejecting registry, typed factories, and boundary tests. Do not adopt this workspace wholesale as the new framework.** Of these three runtime-oriented repositories, this has the clearest implemented Rust request-to-handler path. Its next useful step is not more infrastructure: it is converging the existing entry paths on one shared bridge, then proving the canonical ingress contract with an in-process driver and checking any selected adapter against it.

**Review snapshot:** 2026-10-30, default branch `main`. This is a read-only source review; the exact commit and private source evidence are retained separately. Source paths and line ranges below refer to that snapshot. No repository code, build scripts, or tests were executed. The tree has no `AGENTS.md` or checked-in CI workflow. Two non-default branches exist, concerning code review and source export; they were inventoried, not exhaustively reviewed.

## What actually exists

The root workspace includes eleven crates under `crates/*`, all at version 0.1.0 and Rust edition 2024. The runtime-bearing chain is:

- `forge_node`: executable and composition root, with a single `run` CLI command.
- `forge_runtime`: bootstrap/application contracts and the synchronous syscall harness.
- `forge_wire`: versioned JSON request/response models and an I/O port, with stdio and test implementations.
- `forge_core`: task identity, envelopes, registry, factories, executable/worker traits, gateway, and local worker.
- `forge_tasks`: concrete system/Rust tasks and their execution policy.
- `forge_macros`: procedural bootstrap/registration/application wiring.
- `forge_std`: environment and filesystem helpers.

`forge_ident`, `forge_notation`, and `forge_diff` form a separate value-inspection/diff branch. `forge_test_support` supplies process and assertion helpers. These are distinct capabilities, not reasons to enlarge a shared kernel. The README's older six-crate directory sketch omits several current crates. [Evidence: root `Cargo.toml:1–5`; crate manifests; `README.md:81–92`.]

The dependency graph is acyclic in the inspected manifests: node depends on runtime/wire/core/tasks/macros; runtime depends on wire/core; wire depends on core; tasks depend on core/std/macros; core depends on std. Diff depends on notation and ident; notation depends on ident. However, bootstrap macro output directly references `forge_tasks::register_all`, producing semantic coupling to a concrete capability collection even though that dependency is not declared by the macro crate. This is an important distinction between Cargo dependency direction and generated-code architecture. [Evidence: `crates/forge_macros/src/bootstrap_worker_impl.rs:26–40`.]

## Implemented request flow

The actual executable path is:

1. CLI dispatch selects the syscall command.
2. Bootstrap creates the task registry, registers all first-party tasks, constructs a `LocalWorker`, and injects a stdio I/O factory.
3. `ForgeRuntime` reads a `WireRequestV1`, checks schema/admission, searches the worker's supported task keys, and constructs `TaskRequest`.
4. `LocalWorker` asks `TaskRegistry` to build an executable through its factory.
5. `GenericFactory<T>` deserializes typed arguments and validates the task. Task macros also validate during construction and execution.
6. Execution produces a correlated task response, converted to the versioned wire response and emitted as JSON.

[Evidence: `crates/forge_node/src/cli.rs:17–40`; `crates/forge_runtime/src/runtime.rs:34–103`; `crates/forge_core/src/local_worker.rs:46–65`; `crates/forge_core/src/typed_factory.rs:38–44`.]

This is real implementation, not merely a README diagram. Duplicate factory registration fails; unsupported capabilities produce explicit failures; typed parsing occurs before execution. `TaskType` supplies a namespace/name/version identity. The core request includes task ID, task type, JSON arguments, optional trace ID, and optional timeout. [Evidence: `crates/forge_core/src/registry.rs:35–48,64–99`; `crates/forge_core/src/envelope.rs:27–45`.]

**But there are two logical admission paths.** `Gateway::admit/submit` already normalizes a string capability key and calls a worker. `ForgeRuntime` does not call it: the runtime duplicates admission, key resolution, and envelope construction. The wire model even exposes `into_gateway_input`, which this runtime path does not use. Importing both into the new framework unchanged would preserve precisely the divergence that the shared-bridge philosophy is meant to prevent. [Evidence: `crates/forge_core/src/gateway.rs:51–76`; `crates/forge_runtime/src/gate.rs:19–57`; `crates/forge_runtime/src/runtime.rs:57–102`; `crates/forge_wire/src/wire.rs:43–51`.]

## Fit to the intended framework

The desired transport-independent canonical envelope → shared bridge → allowlisted destination → handler flow is **not fully implemented here**; the existing admission paths remain separate. There is no HTTP server, Kafka consumer, header map, authenticated request identity, destination-header policy, internal fan-out contract, or broker acknowledgment model in the inspected workspace. The existing stdio port is a good adapter example, not evidence that network adapters already exist.

The registry is a useful destination allowlist substrate: external text resolves only to registered task identities. Extend that idea into a validated destination type. Do not dispatch arbitrary class names, filesystem paths, import paths, or executable names from headers. Adapters should normalize permitted header values and submit the same envelope to the same bridge; transport-specific response and acknowledgment mapping should remain outside handlers.

Dependency injection is explicit construction, not a service container. The runtime receives a worker and an I/O factory. This is a good minimal foundation. However, the task macro constructs `Policy::default()` rather than capturing host-provided capability dependencies. Default policy reads an environment variable or current directory, contrary to the README's stronger claim that policy is never inferred. Adapt factories to receive explicit capability configuration during composition. [Evidence: `crates/forge_runtime/src/bootstrap.rs:15–28`; `crates/forge_tasks/src/policy.rs:18–35`; `crates/forge_tasks/src/system/shell_exec.rs:170–173`; `README.md:128–136`.]

## Gaps that matter before reuse

1. **Timeout is metadata, not enforcement.** It is propagated into the request but ignored by `LocalWorker`. The synchronous executable API provides no cancellation token or deadline. Lifecycle enums include accepted/running/cancelled, but the local path returns only succeeded/failed. Do not describe this as a bounded execution engine yet.
2. **Command policy is not a sandbox.** The shell task directly executes an allowlisted executable with unrestricted argument strings and inherited process environment. Working-directory confinement does not constrain what a command can access. Output collection and runtime are not bounded. Canonicalization failures fall back to lexical paths. These are application-policy checks, not operating-system isolation. [Evidence: `crates/forge_tasks/src/system/shell_exec.rs:69–129`; `crates/forge_std/src/path.rs:26–35`.]
3. **Failure semantics need one authoritative status.** A nonzero subprocess status becomes an output field saying failed, but the task still returns a successfully serialized value; the worker then marks the outer response succeeded. Consumers must not need to inspect two competing success indicators. [Evidence: `crates/forge_tasks/src/system/shell_exec.rs:139–159`; `crates/forge_core/src/local_worker.rs:57–64`.]
4. **Typed errors become strings at the boundary.** Validation/execution/internal are explicit internally, but responses expose optional error text rather than stable machine-readable codes. Preserve classification and retryability through the envelope.
5. **No service lifecycle supervision.** Bootstrap exists; resource startup ordering, reverse-order shutdown, graceful draining, health/readiness, bounded concurrency, and retry/dead-letter behavior do not. Add these only where the first real adapters require them.

Repeated validation across factory/build/execute is visible and may be intentional defensive checking, but it should be specified as pure and cheap rather than becoming repeated I/O. The README's deterministic/bounded claims should be narrowed to stable contract and output shape: subprocess behavior, elapsed durations, environment, and filesystem state are not deterministic.

## Tests and confidence

The materialized Rust sources contain 112 `#[test]` attributes and 20 integration-test files; these are source counts, not executed/pass counts. Coverage includes duplicate registration, typed factories, local worker success/failure, wire round-tripping, malformed/schema-invalid inputs, process exit semantics, stdout purity, command validation, and diff/identifier behavior. Particularly reusable acceptance tests are under `crates/forge_node/tests/f7_*`, `crates/forge_core/tests/f2_03_task_registry.rs`, and `crates/forge_wire/tests/f7_05_wire_schema_roundtrip.rs`.

No checked-in CI workflow exists. The Actions API returned three historical dynamic runs, none for the reviewed default-branch commit; these do not establish a current test pass. A full build/test pass remains an explicit later gate, not an implied result of this review.

## Extraction plan and portfolio placement

**Adapt:** envelope/response identity, factory registration, explicit bootstrap, injected I/O, and contract-test design. **Keep optional:** system automation tasks; enable only in applications that intentionally need them. **Separate:** notation/diff utilities. **Reference rather than preserve:** the macro-heavy application harness; handwritten composition is easier to audit until stable repetition justifies generation.

A first vertical slice should register one harmless capability and prove bridge decisions and handler results with an in-process driver. The existing stdio adapter can be checked against that contract if retained; add no new transport solely for this proof. Tests should cover unregistered destinations, duplicate registration, forbidden headers, deadline expiration, and nested fan-out context propagation.

No repository-wide license file was found. Only the test-support manifest declares a license, which does not establish licensing for the other crates. Before publishing source, resolve ownership and licensing, package metadata, and dependency/version policy. This public review intentionally contains original technical analysis, not copied private source or private source links.
