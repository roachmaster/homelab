# forge-hpvs — extract the composition discipline, not the whole machine

## Decision

**Adapt the bootstrap, immutable registration, provider-lifetime and architectural-testing patterns. Keep HPVS itself an optional compute subsystem.** This repository contains the strongest separation of construction from execution among these three specialized systems. It is not already a general web-application framework: its execution vocabulary, memory model and many providers serve mathematical workloads.

This is a read-only architectural review of the default `main` snapshot observed on 2026-10-30. The exact revision and authenticated evidence are retained in the private review inventory. Public source locators below identify inspected files without reproducing private source or exposing private links. No build, test, benchmark, training run or repository mutation was performed.

## What is actually here

The root workspace lists **42 Rust crates**, with separate contracts, portable/default implementations, specialized implementations, compositions and test/tooling packages. There is also substantial C, DSP, neural and experimental material outside that simple crate graph. The review inspected every workspace manifest and crate root, then followed representative bootstrap, dispatch, registry, execution and failure-cleanup paths; it is not a line-by-line qualification of every numerical subsystem. (H1, H8)

Important groups are:

- **Construction and identity:** bootstrap, provider factories, registrar, registry and dispatch.
- **Ownership and execution:** lifecycle, control, memory, storage, executor and observer.
- **Substitutable implementations:** portable control/memory, scalar math, fixed storage, bitmap workers, static registries and unrolled execution.
- **Applications/compositions:** in-memory machine, fixed/work pipelines, DSP and neural workloads.
- **Testing and policy:** invariant doubles, shared provider-contract assertions and crate-layer checks.

Names alone are misleading. `forge-hpvs-invariant` is test support, not the minimal production contract library. Conversely, several classified invariant crates include real logic, such as layout planning, a default executor and an observer guard. The architecture document explicitly acknowledges this; “contract layer” does not currently mean “traits only.” (H8)

Most core package dependencies are local contracts. DSP/neural compositions additionally depend on pinned external mathematical code and selected serialization/hash libraries. Those dependencies should never become a transitive requirement of a simple REST service. No project-wide license declaration was found at the root or in the inspected workspace manifests; third-party notices elsewhere do not settle the status of the whole project. Publication of extracted source needs a separately established licensing/provenance decision.

## Observed architecture and flow

The in-memory composition is a useful, concrete end-to-end slice:

1. A bootstrap constructs a registrar containing operation entries.
2. Entries are registered into a mutable builder; duplicate keys and exhausted capacity produce errors.
3. The builder is consumed into an immutable registry. The system retains that registry, control provider, memory controller and executor.
4. A run plans storage, reserves CPU identities and obtains prepared memory.
5. The program preparer decodes all dispatch words, resolves their registered providers and produces concrete operations before execution.
6. The executor performs the prepared operations against owned state. The sample operations compute a dot product or halt.
7. CPU ownership is released; successful returned state retains its backing storage. Ordinary errors release owned resources without emitting successful-completion milestones. (H2–H6)

The in-memory sample reserves logical CPU identities but runs its default executor synchronously; it is not evidence of physical placement or parallel execution.

This is **local operation dispatch**, not network ingress. The dispatch coordinates identify a one-hot domain plus operation, implementation and slot; a default codec packs these into a word. That word has neither a request identity nor correlation, authentication context, schema version, deadline or transport acknowledgment semantics. It must not be promoted unchanged into the public canonical request header. (H3)

The architectural correspondence is valuable nonetheless: normalize outside the core, resolve a known operation, prepare its dependencies, then execute through a stable contract. A generic application bridge can adopt that sequence while using meaningful typed operation identifiers and a richer transport-neutral envelope.

## Invariants and lifecycle worth preserving

**Registration closes before serving.** The registry builder has a consuming freeze boundary; the default implementation rejects duplicate identities and verifies both address index and key during resolution. The preparer resolves the entire input program rather than discovering unknown operations halfway through execution. (H2, H5)

**Provider lifetime is explicit.** The lifecycle API uses factories and borrowed leases. Implementations demonstrate singleton references, newly created transient values and pooled instances returned through RAII. This is much closer to a Rust-friendly dependency container than reflection-heavy injection. However, the default pool uses `RefCell`; it is not a ready-made shared asynchronous service pool. (H4)

**Cleanup is part of the protocol.** Bootstrap reports registry readiness only after success. Run observation distinguishes acquisition, preparation, execution and release. The release guard observes cleanup rather than owning resources, and avoids callbacks while unwinding. Tests explicitly exercise observer panics and ordinary failure recovery. This is a stronger template than “log start/end and hope drops happen.” (H6, H7)

**Real deterministic work stays real in tests.** The test-double manager configures provider failures at effect boundaries. Shared provider-contract tests exercise registry resolution and resource reservation behavior across implementations. The generic lesson is a contract test kit plus injected I/O, not an all-purpose mocking runtime. (H9)

## Risks when extracting a framework

1. **Numerical identity is not application identity.** One-hot domain selection limits that representation to sixteen hot-path choices and exposes implementation/slot details. Keep it behind an optional compute adapter; use a versioned application routing key at ingress.
2. **Validated addresses are not authority tokens.** Their public constructor means consumers can manufacture them; the default registry checks consistency, but the address carries no registry-instance identity. Do not treat a resolved address as authorization or a durable cross-process handle. (H2)
3. **Executor semantics need a narrower home.** The default executor follows next/jump/halt until an operation is absent. It provides no generic execution deadline or loop limit. These are acceptable documented local-machine semantics, not safe defaults for arbitrary externally submitted programs. (H5)
4. **Synchronous milestones are not an observability backend.** Observer callbacks can panic and run on the execution path. Preserve semantic event names, but design service tracing, metrics and failure isolation separately.
5. **Many crates do not automatically mean good product packaging.** Forty-two pieces can be manageable internally while overwhelming a new application author. Offer a small facade/starter and keep contract boundaries enforceable underneath it.
6. **Layer checking has a precise limit.** The freeze checker validates declared package edges, including target-specific/build/optional dependencies and local overrides; it is not a semantic proof that source code obeys every architectural intent. External unclassified packages are outside that local policy. (H8)

## Reuse disposition

| Candidate | Decision | Framework destination |
|---|---|---|
| Consuming registry builder, duplicate rejection, registrar | Adapt | Small explicit application registry |
| Bootstrap versus execution separation | Reuse as a design invariant | Application composition root |
| Factory/lease ownership model | Adapt | Optional singleton, request and resource scopes |
| Semantic lifecycle events and failure tests | Adapt | Lifecycle contract and test kit |
| Package-layer manifest checks | Adapt early | Framework architecture CI |
| Memory/controller seams and shared provider tests | Reference | Pattern for optional infrastructure adapters |
| Dispatch-word encoding and prepared-program executor | Keep optional | Compute subsystem implementation |
| DSP/neural models, fixed arithmetic, CPU tuning, C experiments | Exclude from generic core | Domain applications and specialized providers |

The first extraction should prove one ordinary command with two substitutable providers, not recreate the HPVS workspace under new names. Freeze registration, validate before admission, acquire scoped resources, invoke the handler and release deterministically. Then test the same command through REST and Kafka adapters. The HPVS handler may be one optional domain consumer of that shared bridge.

## Verification and branch scope

Observed source tests cover exhaustive one-hot dispatch, checked registry behavior, bootstrap protocol, failed preparation/execution, panic cleanup, resource recovery and provider equivalence. The validation script declares debug/release Rust tests, formatting, strict Clippy and Python architecture/BDD checks. **None were run for this review.** No Actions workflow directory was present at the inspected root, and the pinned commit returned zero Actions runs and zero check runs; that is absence of observed CI evidence, not a passing result. (H7, H10)

The branch inventory also contains an alternative mainline, a signal-continuity development line and a release-candidate line. Their heads were recorded privately, but only default `main` received this architectural review. Historical research and those branches must not silently be treated as current production implementation.

## Evidence locators

| ID | Source path and lines | Supports |
|---|---|---|
| H1 | `Cargo.toml:1–47`; workspace crate manifests | Workspace membership and dependency split |
| H2 | `crates/forge-hpvs-registry/src/lib.rs:1–58`; `crates/forge-hpvs-registry-default/src/lib.rs:17–67` | Identity, freeze and resolution contracts |
| H3 | `crates/forge-hpvs-dispatch/src/lib.rs:20–50`; `crates/forge-hpvs-dispatch-default/src/lib.rs:23–81` | Logical coordinates and codec |
| H4 | `crates/forge-hpvs-lifecycle/src/lib.rs:1–16`; `crates/forge-hpvs-lifecycle-default/src/lib.rs:13–145` | Provider leases and concrete scopes |
| H5 | `crates/forge-hpvs-executor/src/preparer.rs:17–35`; `crates/forge-hpvs-executor/src/lib.rs:25–41` | Prepare-before-run and loop semantics |
| H6 | `crates/forge-hpvs-in-memory/src/lib.rs:159–294`; `crates/forge-hpvs-observer/src/lib.rs:27–108` | Actual composition and cleanup |
| H7 | `crates/forge-hpvs-in-memory/tests/bootstrap_protocol.rs:9–77`; `crates/forge-hpvs-in-memory/tests/failure_release.rs:87–184` | Protocol/failure tests inspected |
| H8 | `architecture/infrastructure-freeze.md:3–37`; `architecture/crate-layers.tsv`; `scripts/check-hpvs-freeze.py:1–148` | Policy, classification and its limits |
| H9 | `crates/forge-hpvs-invariant/src/lib.rs:1–145`; `crates/forge-hpvs-provider-contracts/src/lib.rs:1–76` | Effect-boundary doubles and reusable assertions |
| H10 | `scripts/validate-hpvs.sh:4–30`; authenticated branch/check/run metadata | Declared checks and observed CI status |
