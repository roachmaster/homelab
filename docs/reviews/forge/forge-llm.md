# forge-llm — a demanding framework consumer, not the framework core

## Decision

**Keep this as an optional inference application and use its readiness, validated-configuration and ownership lessons as reference designs.** Its strongest contribution to a Spring-Boot-like Rust framework is showing what a demanding domain needs from lifecycle management. Its token generation, mathematical representations, placement controls and fixed worker machinery should not determine the generic application API.

This review covers the default `main` snapshot observed on 2026-10-30. The private inventory retains the exact revision and authenticated source evidence. Public locators identify the inspected files; no private source listing or operational details are reproduced here. This was a read-only review: no build, test, inference, benchmark or repository mutation was performed.

## Purpose and package reality

The root is a Rust 2024 application/library package with an explicit Rust-version declaration and publication disabled. It depends on a **local single-crate `forge-hpvs`** and pinned `final-bitwave`/`bitwave-spectrum` dependencies. Other dependencies support mapped model access, serialization, tokenizer/chat-template handling, hashing and deterministic sampling. Numerical experiment support is separately declared as a development dependency. (L1)

The local HPVS crate exposes machine, math, binding and lifecycle modules, including Linux-specific machine support. It is not the same package architecture as the separate `forge-hpvs` repository's 42-crate workspace. Combining them by name would conceal two distinct generations of contracts. The review inspected both manifests, the application module surface, the concrete generation entry point, model readiness, request session, worker ownership, representative tests and architecture material. (L1, L2)

The root library publicly exposes many domain stages: projections, attention, normalization, rotary position processing, residual joins, feed-forward stages, tokenization, sampling, prepared model caches and worker placement. This is an inference pipeline with a broad laboratory/application surface, not an HTTP/Kafka service platform. (L2)

## Actual request path

The representative `generate_cached` executable performs explicit orchestration rather than using an application container:

1. Parse arguments, resolve presets and explicit configuration overrides, and validate basic settings.
2. Load a prepared model through `ModelLoadingStage`, including model identity and representation preparation.
3. Prepare tokenization/chat input, validate token IDs and check the prompt/output allowance against session capacity.
4. Validate configuration against the admitted model and run prompt preflight before request execution.
5. Construct the worker resources and an `InferenceSession` borrowing the model and workers.
6. Prefill the prompt, then repeatedly select a vocabulary token and consume it through the model layers until a configured stopping condition.
7. Produce generated output and timing information; worker destruction closes queues and joins started threads. (L3–L7)

The session owns mutable per-request layer/KV state and its position while borrowing reusable model and worker resources. This suggests three useful lifecycle categories for a general framework: **application-owned immutable resources**, **owned execution resources**, and **request-scoped mutable state**. It does not require the framework to understand attention heads or token positions.

There is no canonical REST/Kafka header bridge on this inspected application path. Dispatch inside the worker pool routes domain tasks, and the local HPVS binding prepares mathematical kernels against machine execution capabilities. Neither is a replacement for an external operation registry. A future ingress handler should translate a validated domain request into this application service, not expose receiver variants, physical placement or internal stage commands as universal framework headers. (L2, L7)

## Invariants worth carrying forward

**Readiness is a real boundary.** `ModelLoadingStage` returns an owned, validated and touched model; it does not publish model files or start workers as a side effect. Failed loading clears stale readiness timings. The source-mapping contract is explicit, including the immutability assumption on a mapped source file. This is a good pattern for adapters that must finish preparation before the server advertises readiness. It is not a generic requirement that all application resources be memory-mapped. (L4)

**Configuration becomes a checked snapshot.** The application distinguishes mutable settings from `ValidatedConfiguration`; unknown fields are rejected, schema version is explicit, file size is bounded and precedence is documented as preset, supplied fields, then caller-applied overrides. Model-dependent validation is separate from settings-only validation. The framework should adopt this staged approach while leaving Qwen presets, tokenizer limits and generation rules in the inference module. (L5)

**Partially failed request state is not reused.** A session marks itself failed before entering token computation and clears that state only after all layers succeed. A computation panic is converted into an invalidated-session error; subsequent consumption rejects the poisoned session. Preflight rejects invalid requests before mutation. This is an important lifecycle lesson: retry policy must know whether a handler has partially mutated state, rather than blindly replaying a request against the same instance. (L6)

**Resource ownership can be tested independently of mathematics.** The local lifecycle ledger records construction, shutdown, worker exit, join and destruction. The fixed worker pool closes every sender before joining workers. Its tests cover failures and panics while waiting for submitted work. A framework test kit can borrow the principle—verify owners release resources and shutdown completes—without importing this domain-specific ledger unchanged. (L7, L8)

**Hardware admission and mathematical execution are separated.** The binding layer accepts declared execution requirements; the application’s prepared execution implementation verifies worker placement/readiness, then exposes a mathematical projection port. This is an optional compute-adapter boundary, not a reason to put hardware discovery in the ordinary command dispatcher. (L9)

## Risks and gaps for framework assembly

- **Duplicate HPVS evolution:** the local crate and standalone repository will drift unless a migration explicitly maps contracts and tests. Do not create a generic framework that depends on both and calls that consolidation.
- **Broad implementation exposure:** many internal inference stages are public modules. A stable inference facade should be smaller than this research-oriented library surface, with domain request/result types and a deliberate streaming/cancellation contract.
- **Concrete dependencies dominate the application:** sessions directly borrow `ModelCache` and `FixedWorkerPool`. The lifecycle is explicit, but these are not yet generic service ports. Wrap the application first; only generalize a seam when a second real implementation needs it.
- **Shutdown is synchronous and potentially lengthy:** `Drop` joins threads. An asynchronous server should drain admissions and coordinate shutdown explicitly; blocking destruction must not unexpectedly run on an event-loop thread. The review did not establish a hard shutdown deadline. (L7)
- **Generation budgets are not a universal request deadline:** the CLI describes generation time separately from loading and prompt work. A service needs an end-to-end admission/deadline policy as well as any domain generation budget. (L3)
- **Private pinned dependencies and model requirements affect portability:** a clean consumer needs documented dependency access and representative fixtures. Hardware-target build options belong to an optional inference/provider profile, never the default generic starter.
- **Maturity varies by layer:** numerical unit tests, model-backed integration checks and hardware measurements are different evidence. A passing mathematical fixture cannot prove model quality, deployment readiness or target-machine performance.

No repository-wide license file or package license field was found in the fully enumerated tree/manifests. This report does not infer permission to redistribute the application, its dependencies, models or supporting reference material.

## Reuse disposition and integration shape

| Candidate | Decision | Suggested use |
|---|---|---|
| Validated configuration and explicit precedence | Adapt | Framework configuration lifecycle pattern |
| Load/prepare/ready boundary | Adapt | Resource initialization and health/readiness hooks |
| Poisoned request-state behavior | Reference/adapt | Request-scoped service contract |
| Lifecycle ledger and failure tests | Reference | Generic shutdown/resource test scenarios |
| Local HPVS machine/math/binding design | Compare before migration | Optional execution-provider adapter |
| Model cache, tokenizer, generation and KV state | Keep domain-owned | Inference application package |
| Worker placement, CPU-specific receiver selection, numerical kernels | Exclude from generic core | Optional optimized inference backend |

**Proposed, not implemented:** thin transport adapters create the same canonical envelope, the shared bridge validates and resolves `inference.generate`, and its registered handler calls a small inference service port. That service owns model admission, request sessions, streaming and domain stopping rules. An inference provider owns the current compute implementation. The application composition root chooses it; the bridge never imports model arithmetic or CPU-placement types.

The integration acceptance test should establish the domain handler and header context through an in-process ingress driver, then check any input adapter actually selected against that contract. No network API or broker is a prerequisite. Separate tests should verify failed initialization prevents readiness, poisoned sessions are never returned to a pool, shutdown drains resources, and a disabled inference feature leaves the base service build free of model dependencies.

## Verification and scope

Observed tests include fixed-worker ordered coverage and panic/error handling, prepared-model ownership, dataset corruption/truncation rejection, configuration and generation checks, and lifecycle evidence. A real-model test is explicitly ignored unless its immutable model fixture is supplied. These are inspected test definitions, **not results from this review**. The Makefile declares locked tests and separate optimized/hardware-target runs; no Actions workflow directory was found and the pinned commit returned zero Actions runs/check runs. (L8, L10)

Only `main` appeared in the branch inventory. The complete file tree was enumerated, but broad experimental documentation and numerical kernels were sampled rather than exhaustively audited. Existing documentation and archived measurements were treated as historical claims, not fresh performance evidence.

## Evidence locators

| ID | Source path and lines | Supports |
|---|---|---|
| L1 | `Cargo.toml:1–25`; `crates/forge-hpvs/Cargo.toml:1–8` | Package/dependency identity |
| L2 | `src/lib.rs:1–55`; `crates/forge-hpvs/src/lib.rs:1–22` | Public module architecture |
| L3 | `src/bin/generate_cached.rs:319–621`, particularly `494–565` | Executable orchestration and admission |
| L4 | `src/model_loading.rs:1–105`; `src/model_cache.rs:532–540` | Readiness, ownership and identity |
| L5 | `src/configuration.rs:15–107`, `139–149` and following validation | Configuration stages |
| L6 | `src/inference_session.rs:15–50`, `112–165`, `211–272`; `src/generation.rs:240–305` | Request state and preflight |
| L7 | `src/fixed_worker_pool.rs:2301–2324`; `crates/forge-hpvs/src/lifecycle.rs:69–115` | Shutdown and release evidence |
| L8 | `tests/fixed_workers.rs:14–109`; `tests/gguf_bdd.rs:433–541` | Tests and explicit model prerequisite |
| L9 | `crates/forge-hpvs/src/binding/runtime.rs:8–28`; `src/execution_context.rs:130–167` | Mathematical execution port |
| L10 | `Makefile:31–61`; authenticated branch/check/run metadata | Declared checks and observed CI status |
