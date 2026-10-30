# forge_rust — historical experiments, not the runtime baseline

## Decision

**Reference selected composition ideas; exclude this repository from the new framework's runtime dependency graph.** It contains useful evidence of the design journey—traits, state/event bridges, handler registries, factories, and generated stereotypes—but those ideas do not form one active application framework. Treating every Rust directory as a maintained workspace member would materially overstate what is assembled.

**Review snapshot:** 2026-10-30, default branch `master`. The exact commit and private evidence are retained separately. The complete tree and manifests were inventoried; active package entrypoints, representative bit-level implementations, traits, composition bridges, executor/message bus, historical routing, generators, and representative tests were inspected. No build scripts, macros, repository code, or tests were executed. Only the default branch was returned by branch inventory. No `AGENTS.md`, README, repository license, or CI workflow was present in the tree.

## Start with the manifest, not the directory names

The root `Cargo.toml` lists **only `bitforge`** as a workspace member. The nearby `forge_0_proc_macros`, `forge_1_traits`, and `forge_2_std` directories are not included. Neither is the large `history/` subtree. Consequently, a root workspace test run would not prove those other framework experiments work. This is the most important scope distinction in the repository. [Evidence: `Cargo.toml:1–3`.]

The active `bitforge` package has no dependencies declared and uses Rust edition 2024. It implements bit gates, arithmetic building blocks, bit-wave/pulse concepts, and logging helpers. Its library declares those modules, while attempted `no_std`/allocator settings are commented out. The bit-wave parser is a placeholder comment; the custom-memory module is entirely commented-out allocator experimentation. This is a low-level research package, not a REST/Kafka application host. [Evidence: `bitforge/Cargo.toml:1–4`; `bitforge/src/lib.rs:1–19`; `bitforge/src/bitwave_parser/default.rs:1`; `bitforge/src/memory/default.rs`.]

The non-member experiments form a separate intended chain: `forge_std` depends on `forge_traits`, plus `paste` and `futures`; the trait package depends on `paste`; the procedural-macro package depends on parsing/template/YAML tools. There is no manifest dependency from `bitforge` to those packages. Static parsing of the eleven manifests found no path-dependency cycle, but found five dependency edges targeting absent manifests. Historical backend/tests reference older standard-library and generated-target packages. Thus, there is no complete supported whole-repository build graph to reuse unchanged. [Evidence: `forge_1_traits/Cargo.toml`; `forge_2_std/Cargo.toml:6–9`; `history/forge_backend/Cargo.toml:7–10`; `history/forge_tests/Cargo.toml:15–20`.]

## What is implemented and worth learning from

The trait layer separates task-state tracking, notification, metrics, queues, scheduling interfaces, storage interfaces, and identity/lifecycle tags. Small bridge functions connect these contracts without owning their implementations. For example, the state/event bridge changes a task's state and enqueues an event; another bridge changes state, notifies, and increments a metric. This illustrates the desired principle that a shared logical bridge should orchestrate independent contracts rather than absorb all implementations. [Evidence: `forge_1_traits/src/concurrent/task_state_event_bridge.rs:14–27`; `transition_notify_and_count_bridge.rs:23–37`.]

However, these operations are sequential calls with no transactional result or failure protocol. A state change followed by failed event persistence is not modeled. They should inspire a narrow orchestration seam, not be presented as a durable event-processing guarantee. The state machine accepts transitions without a transition-validity contract; scheduler is an interface, not an installed scheduler implementation. [Evidence: `forge_1_traits/src/concurrent/task_state.rs:11–31`; `scheduler.rs:6–17`.]

`GenericMessageBus` holds callbacks through an abstract array and invokes subscribers synchronously. Its tests check subscribe/publish/clear behavior. It is an in-process observer list, not Kafka, a network broker, a durable queue, or a destination-routing bridge. There are no acknowledgments, retry policy, replay, subscriber failure values, or concurrency bounds. [Evidence: `forge_2_std/src/message_bus/generic_bus.rs:3–44`; `forge_2_std/tests/message_bus.rs:5–43`.]

The executor starts a fresh operating-system thread, blocks that thread on a future, and returns a one-shot response future. A closed channel/panicked thread becomes an `expect` panic. This is a useful learning implementation, but not a bounded application executor with shutdown/cancellation. The pool is an `Arc<Mutex<Vec<T>>>` with manual acquire/release, not a resource lifecycle manager with RAII return or capacity/backpressure. [Evidence: `forge_2_std/src/executor/std_executor_ops.rs:6–20`; `forge_2_std/src/pool.rs:4–47`.]

## Registration, routing, DI, and lifecycle: distinguish names from behavior

The forgeable system creates metadata/tag combinations for environment, lifecycle, and kind: logic/server/browser; singleton/transient; task/service/adapter and other roles. The source expands many combinations before there is a demonstrated application requiring them. These tags communicate intended stereotypes but do not, by themselves, implement managed singleton construction, dependency injection, scope disposal, or runtime lifecycle. A framework should earn each abstraction through a concrete capability, not inherit the full Cartesian product. [Evidence: `forge_1_traits/src/forgeable/composite.rs:3–49`; `lifecycle.rs:7–14`; `mod.rs:9–32`.]

The historical backend has a small task message containing task name and JSON properties and a string-keyed handler registry. The registry overwrites duplicate names rather than rejecting them. This is an ancestor of the newer `forge` registry idea, but lacks its versioned task identity and stricter duplicate policy. There are no header, correlation, deadline, authenticated identity, or structured response fields in this old message. [Evidence: `history/forge_backend/src/task/task_message.rs:4–8`; `task_registry.rs:5–35`.]

Historical `TaskRoute` queues inputs and invokes a handler, discarding execution results. `TaskRouteRegistry` has logic/browser queues, but forwarding only logs a message: its own comments label the real forwarding as TODO and the execution as a stub. Do not draw an implemented browser/server dispatch network based on those types. [Evidence: `history/forge_backend/src/task/task_route.rs:27–46`; `task_route_registry.rs:23–48`.]

The older backend execution trait defaults to invoking a shell command derived from properties. Errors are strings; process output feeds later lifecycle conversion methods. There is no allowlisted network destination or common REST/Kafka admission path. This historical default belongs outside a trusted framework core and should not be exposed as a generic remote handler. [Evidence: `history/forge_backend/src/task/backend_task.rs:32–62`.]

Code generation is also partial. The non-member procedural macro reads YAML, prints intermediate data, and ultimately emits a dummy function. Its selected identifier is parsed but does not drive an implemented renderer. Historical generators are more extensive, but their mere presence does not make the current macro a working dependency-injection system. [Evidence: `forge_0_proc_macros/src/generate_code_impl.rs:8–27`.]

## Risks and tests

The active `BitwavePulse::pulse` method performs an unsafe volatile read from a fixed hardware address without a target guard or caller safety contract. Calling it in an ordinary host process is not a safe application-runtime operation. The accompanying test exercises accumulation arithmetic, not the pulse's memory access. Keep hardware/embedded exploration separate from homelab server composition. [Evidence: `bitforge/src/bitwave_pulse/default.rs:33–40,47–68`.]

The tree includes 23 integration-test files under `forge_2_std/tests`, one under `forge_1_traits/tests`, historical tests, and inline bitforge tests. Representative tests cover message subscription, executor results, and state/event ordering. Some active arithmetic tests compare a function result with another call to the same function, which cannot establish arithmetic correctness against an independent oracle. Stronger tests should use truth tables and explicit expected values. [Evidence: `bitforge/src/bit_compute/default.rs:127–166`; `forge_2_std/tests/task_state_event_bridge.rs`; `forge_2_std/tests/executor.rs`.]

These are observed source tests only. None was executed here, and non-member tests are not covered by root workspace membership. The Actions API returned zero runs. No conclusion that this tree builds or its suites pass is warranted.

## How it should influence the new framework

**Reference:** tiny orchestration bridges, separate state/notification contracts, explicit registration, and capability-oriented thinking. **Adapt only after a real use case:** the local observer concept, with bounded execution and explicit failure semantics. **Exclude from runtime assembly:** bit-level hardware experiments, historical backend shell defaults, logging-only route stubs, dummy code generation, and broad standard-collection wrappers.

The newer `forge` repository already has the stronger task envelope/factory/registry starting point; `forge-agent-runtime` supplies more concrete operator and dependency-validation behavior. Importing their older counterparts from this repository would create competing task, event, lifecycle, and registry vocabularies. Keep a single contract vocabulary in the new framework and preserve this repository as design history.

For the professional portfolio, show the progression: experimental abstraction → observed limits → a smaller capability package and shared bridge proven by adapter-conformance tests. The target remains thin REST/Kafka adapters, canonical request context, allowlisted destination dispatch, and ordinary package composition—not a generated god-crate, custom collection ecosystem, or dynamic plugin ABI.

No package-level license declaration was found in the inspected manifests. Resolve licensing/provenance before copying source. Public documentation should explain the architectural lessons without presenting inactive code, generated placeholders, or unsafe hardware demonstrations as production framework features.
