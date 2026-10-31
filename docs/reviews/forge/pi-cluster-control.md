# Pi Cluster Control — a cluster use-case blueprint, not an implemented control plane

**Review date:** 31 October 2026. **Source branch:** `main`, inspected at a pinned snapshot. **Disposition:** preserve the cluster requirements and composition lessons; implement selected capabilities as optional consumers of the existing Rust framework. Do not import this repository wholesale or make cluster management part of the mandatory core.

The important distinction is between the repository's ambition and its current implementation. Its documentation describes a Raspberry Pi agent/controller/backend system, but the inspected tree contains primarily C++ construction contracts, a JavaScript factory/lifecycle prototype, Python scaffolding, and behavioral specifications. The README explicitly lists agent/master implementations and k3s deployment specifications as future work. This is useful design provenance, not evidence of a deployable fleet controller. [E1–E3]

## What actually exists

| Area | Observed implementation | What it does not establish |
| --- | --- | --- |
| C++ | CMake assembly of `forge-core` and `forge-test`; factory, resolver, registrar, cache and ownership-shaped contracts | A running node agent, coordinator, command service or complete dependency container |
| JavaScript | String-keyed factory registration/resolution, lifecycle caches, YAML configuration and a small outbound HTTP client | A verified cluster protocol, reliable resource ownership or managed application lifecycle |
| Python | Configuration loading, Mustache-based module generation, command resolution, CMake/test wrappers and BDD-to-metadata conversion | Safe runtime discovery, successful builds/tests or executable cluster acceptance tests |
| Cluster behavior | Agent, master and backend `.bdd` scenarios | Implemented heartbeats, enrollment, telemetry persistence, remote jobs or provisioning |

The inventory has no Rust workspace, root README, root license file, GitHub workflow directory, or implemented `agent/`, `master/` and `backend/` source trees matching the advertised structure. The active CMake build adds only the two Forge modules. Archived C++ material is separate from that assembly and was sampled, not treated as current production code. [E1–E3, E11–E12]

## Actual execution and construction paths

### Developer tooling, not a node runtime

The Python entry point parses configured subcommands, resolves a command factory, and invokes its executor. Scaffolding combines module/layout/template configuration, generates module and root files, and persists its resulting configuration. The BDD utilities convert scenario text into YAML and step metadata; they do not implement the described cluster effects. [E4, E11]

Command lookup also has a surprising side effect: an import or attribute error triggers source generation, writes a command module, refreshes an editable Python installation, and loads the generated code. An import failure inside an existing command can enter this path too. That behavior belongs, at most, in explicitly invoked development tooling. It must not become agent startup, request dispatch, or automatic runtime repair. [E4]

### Factory/provider experiment

JavaScript startup creates a map-backed manager, provider and registrar, registers a cache factory, and then registers lifecycle-cache factories. It separately installs configuration and configured outbound clients. Acquisition resolves a string identifier, invokes the corresponding factory, and validates a marker on the result. Several failures become warnings plus `undefined`, rather than a validated application assembly error. [E5]

This preserves a useful distinction between construction recipes and access to constructed objects. However, it is runtime service lookup, not Homelab's typed constructor injection. Registration overwrites an existing map entry; there is no consuming freeze boundary in this path. Singleton acquisition retains an instance, while the pool cache creates on demand and accepts manual returns without capacity, reset, lease ownership or duplicate-return enforcement. A caller-supplied thread identifier is not proof of async request isolation or worker-thread safety. The pool/thread-local abstract factories also import a provider module absent from the inspected tree and expect a different accessor shape. These are static findings, not executed failure results. [E5–E7]

The HTTP client concatenates a configured base URL with a path and calls `fetch`. Its JSON-returning methods do not explicitly reject non-success status codes; no deadline, response-size bound, peer-authentication contract or cluster request schema is implemented there. Startup prints the entire configuration, which would need redaction before using operational credentials. Neither the client nor a configuration key named for authentication proves an authenticated control plane. [E5, E8]

### C++ contracts without an assembled service

The active C++ tree contains declaration-heavy factory/registry/resolver interfaces, a map cache implementation, and a constrained unique-pointer alias. Its library translation unit is essentially a placeholder. The test executable prints beginning/end messages, leaves initialization, registration and execution as TODOs, and returns success. A build of those targets would therefore not, by itself, validate the advertised dependency model or cluster behavior. [E2–E3, E11]

## Cluster requirements worth preserving

The behavioral specifications separate three responsibilities reasonably well:

- **Node agent:** heartbeat, service observations and package inventory; later, local effects.
- **Coordinator:** maintain observations, compare inventory, select nodes and aggregate results.
- **External adapter:** expose authorized operations and telemetry to a UI or automation client.

These are useful responsibility boundaries, not a requirement for three deployed services. A first application can run locally with an in-process ingress driver. Network separation should follow a demonstrated need. [E9–E10]

Other scenarios require substantial redesign before implementation. Remote shell execution, service restarts, uploaded scripts and temperature-triggered reboots carry stronger authority than reading status. “Sanitize the command” is not an adequate effect contract. A future implementation should select explicit typed operations, authorize their targets, bound execution/output, record truthful outcomes and define cancellation and uncertain-result behavior. A missed heartbeat means an observation is stale or a peer is unreachable; it does not prove the machine is dead or make automatic recovery safe. These are proposed safeguards, not features found in this repository. [E9–E10]

## Where it belongs in Homelab

Use the [implemented Rust packages](../../../src/rust/README.md), [bootstrap design](../../architecture/bootstrap-registrars.md) and [hosted-kernel plan](../../architecture/forge-kernel-plan.md) as the destination architecture—not the historical repository's module names.

| Boundary | Current Homelab status | Pi Cluster Control's contribution |
| --- | --- | --- |
| Six core crates | Typed envelope/bridge, factory/provider primitives, task and I/O-client contracts, typed bootstrap registration/freeze exist | Requirements that exercise these contracts; no additional mandatory core responsibility |
| Optional hosted `forge-kernel` | Native disk inspection/navigation exists; not an OS kernel or security sandbox | A possible home for cohesive low-level node-observation clients, **not yet implemented** |
| Optional node-management application | Node/status, bounded jobs and remote boundaries are planned | Agent/coordinator scenarios and inventory comparison become application-owned tasks and typed payloads |
| Selected transport adapter | No transport is mandatory | A future remote peer client or ingress codec; the old HTTP wrapper is reference material only |
| Infrastructure provisioning | Outside these core/runtime contracts | Images, networking, k3s, package installation, service-manager configuration and credential provisioning remain separately managed infrastructure |

The proposed request path remains: selected thin adapter or in-process caller → canonical typed envelope plus separately trusted context → shared admission bridge → registered capability handler → task → injected I/O clients. Header routing must not resolve arbitrary dependencies, choose raw shell commands, or bypass admission. The current bridge is synchronous; it does not already supply remote authentication, job scheduling, durability or cancellation enforcement.

At bootstrap, each selected package owns typed configuration and a cohesive factory-group registrar. The application registers groups in declared dependency order, validates/freezes the plan, then explicitly constructs and injects the selected clients. Current bootstrap validates metadata; it does not execute a dependency graph or supervise services. **Factory registration, ingress-operation registration and node enrollment are three different operations.** Peer identities/endpoints belong in a future application-owned configuration boundary; a configured node label is not authenticated identity. [Homelab bootstrap](../../architecture/bootstrap-registrars.md), [DI design](../../architecture/dependency-injection.md)

Avoid a separate crate for every node operation. Start with cohesive modules in the optional package/application and split only when dependency or release boundaries justify it. The generic core should compile without fleet management, HTTP, k3s, package scanners, shell execution or infrastructure tooling.

## Reuse decisions and smallest useful sequence

- **Reference now:** the node/coordinator/adapter separation and BDD scenarios. Translate them into precise acceptance criteria, including stale/unknown observations and partial failure.
- **Adapt:** configuration-owned construction and factory/provider separation. Use existing typed registrars and explicit dependencies, not the string service locator or a second DI vocabulary.
- **Keep optional:** inventory comparison, telemetry presentation, coordination and any eventual node-management application. Domain fan-out and partial-result policy belong there.
- **Exclude from the runtime:** import-triggered generation/install behavior, unrestricted shell/script execution, infrastructure provisioning and historical lifecycle caches presented as production ownership machinery.

Follow the already documented kernel sequence rather than adding a parallel roadmap:

1. **Local, read-only node observations.** Add only the selected typed node-info/status clients and tasks, with explicit unavailable/stale values. Demonstrate them through the existing in-process bridge and configured factories. No cluster or provisioning dependency is necessary.
2. **Bounded application jobs.** Introduce job identity, capacity, observable state, result limits and cooperative cancellation before remote effects. A task invocation is not automatically a background job or OS process; the planned first job store is volatile, not durable acceptance.
3. **Explicit remote boundary.** Choose a transport deliberately, configure two peers and prove read-only node calls first. Add identity verification, authorization, deadlines and bounded fan-out. A lost response has an unknown outcome; do not blindly retry effects. Remote jobs need an explicit idempotency/recovery contract before release.

This gives the Pi project a concrete role: a demanding but optional reference application proving the framework's composition seams, rather than another platform that absorbs them.

## Verification, risks and licensing

No repository builds, tests, generators, installation commands or deployment actions were run. The inspected branch inventory contained only `main`; no alternate branch was available to establish a more complete implementation. The snapshot had no observed check runs or Actions runs. That is absence of verification evidence, not a passing result.

The JavaScript suite contains three shallow singleton-cache integrity checks, not end-to-end construction or cluster tests. The active C++ runner does not execute tests. Python build failures are printed without being propagated; the test wrapper also prints failures or a missing executable and returns. Its outer executor can subsequently announce success. Those status semantics would undermine CI and must be corrected before trusting automated validation. The documented bootstrap/hook/scaffolding shell commands are absent from the inspected tree. [E1, E4, E11]

Python requirements are unpinned, and the inspected tree contains no JavaScript lockfile. The JavaScript package declares ISC metadata, but no root license file was found; that declaration is not a repository-wide provenance assessment. Prefer original Rust implementations of the architectural ideas, and resolve extraction/licensing scope before copying source. [E12]

## Source evidence

These are source-relative locators, not files included in Homelab. Exact private source pins and authenticated evidence remain outside the public portfolio. Recommendations above are original synthesis; cited source findings are snapshot-specific.

| ID | Inspected evidence |
| --- | --- |
| E1 | Full recursive tree; `blogs/README.md:10–38, 44–59, 78–89` — intended structure, absent documented scripts, coming-soon services |
| E2 | `modules.yaml:1–30`; `forge/CMakeLists.txt:1–10`; `forge/forge-core/CMakeLists.txt:15–17`; `forge/forge-test/CMakeLists.txt:15–36` — active targets and dependencies |
| E3 | `forge/forge-core/include/cppforge/core/forge/forge_manager.hpp:28–56`, `forge.hpp:23–33`, `cppforge_ptr.hpp:14–21`; `forge/forge-core/src/forge_core_lib.cpp:4–8` — C++ construction boundary |
| E4 | `forge.py:3–10`; `py/cli/cli_command_factory.py:12–64`; `py/cli/forge_command_executor.py:5–17`; `py/scaffold/scaffold_module.py:23–36`; `py/build/build_module.py:19–36`; `py/test/test_module.py:13–35` — tooling and error behavior |
| E5 | `forge-js/startup.js:7–41`; `forge-js/core/core.js:21–41`; `forge-js/core/forgeable_provider.js:33–60`; `forge-js/core/cache_map.js:26–27` — bootstrap, resolution and replacement |
| E6 | `forge-js/core/abstract_singleton_forge.js:31–70`; `forge-js/core/lifecycle/pool_lifecycle_cache.js:49–79` — acquisition and return behavior |
| E7 | `forge-js/core/abstract_pool_forge.js:1–4, 25–44, 53–63`; `forge-js/core/abstract_thread_local_forge.js:1–4, 32–70`; full tree — inconsistent/missing provider dependency |
| E8 | `forge-js/clients/rest/forgeable_rest_api_client.js:10–46` — outbound client semantics |
| E9 | `features/agent/1_agent_heartbeat.bdd:1–6`, `2_monitor_local_services.bdd:1–11`, `3_agent_package_reporting.bdd:1–11`, `4_remote_command_execution.bdd:1–24` — agent requirements |
| E10 | `features/master/1_monitor_and_register_agents.bdd:1–17`, `3_package_inventory_and_comparison.bdd:1–11`, `4_execute_commands_on_agents.bdd:1–13`; `features/backend/2_backend_automation_and_alerts.bdd:1–16`, `3_backend_realtime_testing_uploads.bdd:1–16` — coordination and privileged-operation aspirations |
| E11 | `forge/forge-test/src/main.cpp:3–11`; `forge-js/test/integrity/lifecycle_integrity.test.js:6–54`; `py/bdd/bdd_to_yaml.py:12–48`; `py/bdd/convert_bdd_yaml_to_step_json.py:25–77` — observed verification and metadata conversion |
| E12 | `forge-js/package.json:1–16`; `requirements.txt:1–4`; full recursive tree — dependency/license scope |

[All repository reviews](README.md) · [Reuse map](../../architecture/reuse-map.md) · [Hosted-kernel plan](../../architecture/forge-kernel-plan.md)
