# forge-agent-runtime — reference behavior for capabilities and orchestration

## Decision

**Use this as a behavioral reference and source of tests, not as the Rust framework's implementation base.** Its most valuable ideas are small capability metadata, explicit bootstrap roles, deterministic registration, dependency validation, correlated events, and inspectable orchestration. Its biggest architectural warning is that metadata discovery and executable dispatch are separate systems. Porting the abstractions literally would carry that split into Rust.

**Review snapshot:** 2026-10-30, default branch `master`. Exact commit and private source evidence are retained separately. The full tree was inventoried and the kernel, SDK, CLI dispatch, execution/policy/storage paths, representative tests, and workflow were read. No repository code, bootstrap script, or tests were executed. No `AGENTS.md` was present. Two additional documentation/governance branches were inventoried, not exhaustively reviewed.

## Actual shape and boundaries

This is Python, not Rust. `pyproject.toml` requires Python 3.10 or newer, names the project as a controlled vanilla-Python workspace, and declares a module entrypoint but no third-party dependencies. The source tree has four packages:

- `forge_sdk`: frozen dataclass contracts for tasks, pipelines, agents, dependencies, policies, artifacts, events, and runtime context.
- `forge_kernel`: module metadata, discovery, registration, resolution/provider roles, bootstrap, and dependency-graph helpers.
- `forge_agent_runtime`: CLI, command handlers, explicit executable-module registry, context, paths, display, and event logging.
- `forge_agent_modules`: concrete reporting, queue, scheduler, pipeline, policy, execution, and daemon capabilities.

The README accurately calls the repository an execution/control-plane prototype and a reference implementation from which contracts may later be extracted. The separate SDK/scaffold/template repositories named there are a platform direction, not proof that this checkout already implements a complete SDK distribution or project generator. [Evidence: `README.txt:3–29,50–65`; `pyproject.toml:1–8`.]

The kernel has more roles than the simplest Rust application needs. A data source owns metadata storage; a registry writes; a registrar handles sorted bulk registration and duplicate checks; a resolver reads; a provider offers known-key conveniences; a discoverer finds marked classes. These boundaries are implemented and inspectable, but they are largely wrappers around one in-memory dictionary, not a dependency-injection runtime. The provider returns `ServiceModuleContract` metadata rather than live managed service instances. [Evidence: `src/forge_kernel/bootstrap.py:18–65`; `datasource.py:23–37`; `provider.py:35–73`; `module.py:10–29`.]

## Two flows, not one plug-and-play runtime

**Executable path:** argparse selects a handler; the module handler uses the explicit `MODULE_REGISTRY`; `invoke_module` checks a logical name against that map, imports the corresponding Python module, calls `run(context, args)`, emits lifecycle events, and returns an integer exit code. This registry contains the concrete agent/control-plane capabilities. Unknown names are rejected before import. [Evidence: `src/forge_agent_runtime/cli.py:22–24`; `parser.py:21–28`; `registry.py:6–49,72–120`.]

**Kernel discovery path:** bootstrap imports configured packages and their submodules, discovers concrete `ForgeKernelModule` subclasses or instances, constructs no-argument classes, reads provider metadata, converts it to a service descriptor, then registers descriptors. This supports inspection and lookup. It does not replace the executable map or create an execution bridge from those descriptors. Argument metadata describes required/optional fields, but the registry dispatcher still passes raw argument strings. [Evidence: `src/forge_kernel/discovery.py:59–158`; `module.py:60–150`; `bootstrap.py:41–56`.]

That distinction is essential. Adding a discoverable class is not equivalent to adding a runnable capability to the existing dispatcher. The boundary tests explicitly preserve the old dispatch objects while testing the newer metadata system. For Rust, prefer one capability registration operation that contributes both a validated descriptor and an executable handler/factory. Derive inspection output from that same registered object.

The dependency helpers normalize metadata, reject unknown required dependencies, and detect cycles. They return normalized entries, however, not a topologically ordered service-start plan, and bootstrap does not call these helpers. Dependency validation exists as a utility, not as automatic lifecycle enforcement. Optional dependencies are metadata rather than required graph edges. [Evidence: `src/forge_kernel/dependencies.py:25–87`; `bootstrap.py:30–65`.]

## Orchestration, configuration, and lifecycle

The strongest end-to-end behavior is a filesystem control plane: the scheduler writes task manifests; the queue executor loads pending tasks; an agent executor invokes a named pipeline; the pipeline runner checks policy and invokes registered modules sequentially; execution creates artifacts and events; task files move into completed or failed directories. The daemon repeatedly runs scheduler and queue processing, sleeping between ticks and stopping on keyboard interruption. This is real local orchestration, not a distributed scheduler. [Evidence: `src/forge_agent_modules/runtime_daemon.py:44–76,111–144`; `queue_executor.py:213–289`; `pipeline_runner.py:131–184,263–293`.]

Pipeline configuration preserves explicit module order and fails on invalid module references. It is parsed by a handwritten, indentation-sensitive subset parser rather than a general YAML library. Policy and schedule parsing use similar local parsing machinery. The SDK's versioned serialized pipeline mapping includes a top-level contract version, while the runtime parser expects `pipelines:` as its first substantive line: the versioned SDK representation is not automatically the runtime's accepted configuration format. This is a concrete reason to prove serialization/consumer compatibility before extracting a shared SDK. [Evidence: `src/forge_sdk/contracts.py:100–108`; `src/forge_agent_modules/pipeline_manifest.py:73–95,126–150`.]

`RuntimeContext` carries workspace/log paths, session/correlation identifiers, capability flags, permission scopes, and an injected event emitter. JSONL events have stable fields and sorted serialization, with runtime timestamps. This is worth adapting into an explicit Rust execution context, while keeping persistence and logging adapters outside the request contract. [Evidence: `src/forge_agent_runtime/context.py:16–45`; `events.py:31–75`.]

## Where claims need qualification

- **Permission metadata is not authorization.** The dispatcher does not enforce context permission scopes. Policy configuration parses permission rules, but the inspected enforcement path evaluates restricted pipeline/module names and optional runtime checks, not a comprehensive principal-to-capability authorization decision. Put that decision in the shared Rust bridge, not in optional capability code.
- **Policy checks are path-dependent.** Pipeline, queue, and agent orchestration call policy enforcement; direct module invocation does not. Clean-working-tree and tests-passing requirements are evaluated only when `include_runtime_checks` is enabled, whose default is false. The ordinary orchestration calls shown do not enable it. [Evidence: `src/forge_agent_runtime/registry.py:72–97`; `src/forge_agent_modules/policy_guard.py:239–246,301–311`; `pipeline_runner.py:131–136`.]
- **The filesystem queue is not multi-worker safe.** It enumerates pending files and executes before moving them; there is no atomic pre-execution claim or lease in that flow. Concurrent workers can observe the same task, and a crash between execution and move permits replay. Destination collisions delete an existing completion file. Scheduler state and manifests use direct writes rather than a transaction. Treat this as a single-process prototype, not durable exactly-once execution. [Evidence: `queue_executor.py:102–108,153–154,213–269`; `task_scheduler.py:248–250,298–306,397–415`.]
- **Discovery executes code.** Package import and no-argument construction are trusted startup operations, not safe plugin inspection. Do not reproduce dynamic scanning in Rust merely to resemble Spring annotations.
- **Lifecycle labels are not resource management.** Module started/completed/failed events exist; dependency-aware resource startup, teardown, graceful draining, and supervised workers do not form one general lifecycle contract.

The package graph also crosses its intended kernel boundary: kernel registry/context compatibility imports come from the runtime, while runtime kernel handlers import the kernel. This is bidirectional package coupling, not a demonstrated Python import-time failure. Extracting a Rust kernel should reverse this: neutral contracts at the bottom, runtime/adapters above, capability packages depending inward. [Evidence: `src/forge_kernel/registry.py:5`; `src/forge_kernel/context.py`; `src/forge_agent_runtime/handlers/kernel.py:9–11`.]

## Tests and evidence quality

The tree contains 22 `tests/test_*.py` files plus shared cases and fixture packages. Reviewed tests cover deterministic ordering, duplicate registration, missing/cyclic dependencies, metadata lookup, discovery failures, SDK mappings, manifest failures, policy rejection, and scheduler behavior. These are useful behavioral specifications. They are **observed test definitions, not passed tests in this review**. [Evidence: `tests/test_kernel_boundary.py:76–239,927–1188`; `tests/test_sdk_contracts.py:21–174`; `tests/test_pipeline_runner.py`; `tests/test_policy_guard.py`; `tests/test_scheduler.py`.]

The only checked-in workflow reacts to opened issues on a self-hosted runner and uploads a small artifact; it is not a push/PR test gate. The Actions API showed a successful historical dynamic run at the reviewed commit, but its jobs were not audited here, so it is not counted as suite verification. No execution-quality conclusion should be inferred from its green status alone.

## Extraction and portfolio recommendation

**Adapt as specifications:** duplicate-rejecting registration, deterministic capability inspection, missing/cyclic dependency diagnostics, context/event correlation, pipeline short-circuit behavior, and policy-failure tests. **Reference:** queue/artifact/status concepts as optional operator capabilities. **Do not port wholesale:** four packages of compatibility exports, dynamic import discovery, raw-argument execution, repeated policy wrappers, or the filesystem queue's concurrency model.

For the new framework, one Rust capability package should own its configuration, typed handler, registration function, and tests. A small host composes selected packages. Scheduler and operator-control inputs, if selected, should enter through the same canonical bridge rather than calling handler modules directly. No network API or broker is required. Header-selected destinations remain allowlisted registration keys; they are not Python import paths or commands. No REST/Kafka implementation or canonical header-routing envelope was found here.

No license file or project license declaration was found. Resolve licensing and provenance before copying implementation into a public package. In homelab, preserve this as an architectural case study and test-design influence, not as a claim that the future Rust runtime is already implemented.
