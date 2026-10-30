# From the Forge repositories to one framework

**Status:** Architecture synthesis from all 15 finalized repository reports, dated 30 October 2026. This is an architectural selection, not a claim that extracted code builds or is ready for public distribution.

## Recommendation

**Do not merge the repositories. Converge their best boundaries.** Use the small envelope/command seams in `forge-framework` as the primary contract candidate; use Mission Control as the strongest assembled example of one command port behind several transports; take consuming registration and resource-lifetime discipline from HPVS. Compare `forge`'s typed factories and versioned wire behavior when selecting the final handler model. Keep one canonical vocabulary, not four overlapping notions of task, command, event, registry, and runtime. [Framework review](../reviews/forge/forge-framework.md), [Mission Control review](../reviews/forge/mission-control.md), [HPVS review](../reviews/forge/forge-hpvs.md), [Forge runtime review](../reviews/forge/forge.md)

“Reuse” below means retain a bounded behavior or implementation candidate after validation. “Adapt” requires changing its contract or integration. “Reference” preserves a lesson or test scenario without importing the code. “Exclude” means exclude from the **mandatory framework core**, not delete or abandon the project. All source extraction still needs dependency, licensing/provenance, compatibility, and executed-test checks.

## Every repository has a place

| Repository / report | Disposition | Keep or adapt | Boundary / prerequisite |
| --- | --- | --- | --- |
| [forge-framework](../reviews/forge/forge-framework.md) | **Primary contract/dispatch candidate** | Dependency-light values/envelopes, input/output bridges, typed handler dispatch, explicit composition | Fix non-atomic duplicate registration; make external admission unavoidable; preserve correlation; avoid one universal typed request/context; keep template/build/AST tooling optional |
| [mission-control](../reviews/forge/mission-control.md) | **Selective runtime extraction + reference application** | Same command-port instance across HTTP/event/CLI/scheduled runners; supervisor, leases, retry/dead-letter and resumable result publication; adapter conformance tests | Separate generic lifecycle from agent/workspace/compute policy; remove bypass paths; authorize/version routes; health must inspect injected state; fence stale leases and own every task |
| [forge](../reviews/forge/forge.md) | **Adapt execution contracts** | Versioned task identity, typed factory registration, duplicate rejection, explicit bootstrap/I/O and wire tests | Converge duplicated admission paths; enforce deadlines/cancellation; preserve machine-readable failure; keep shell automation optional and never call its policy a sandbox |
| [forge-agent-runtime](../reviews/forge/forge-agent-runtime.md) | **Behavior/test reference** | Deterministic registration, dependency diagnostics, correlated context, policy-failure scenarios | Python implementation is not a Rust substrate; unify metadata and executable registration; dependency checks must run in assembly; do not copy dynamic discovery or unsafe multi-worker file-queue assumptions |
| [forge_rust](../reviews/forge/forge_rust.md) | **Historical reference; exclude runtime** | Typed provider-access / lifecycle-factory distinction, small bridge/observer ideas | Only `bitforge` is an active workspace member; do not copy global caches, zero-argument constructor assumptions or OS-thread locality as request scope; historical runtime remains unsupported |
| [forge-hpvs](../reviews/forge/forge-hpvs.md) | **Adapt composition discipline; optional compute** | Consume-to-freeze registry, prepare-before-execute, factory/lease scopes, cleanup contracts, architecture checks | Do not expose numeric dispatch words as ingress IDs; synchronous pools are not async-safe by default; keep numerical/C/DSP dependencies outside the kernel |
| [forge-bitwave-system](../reviews/forge/forge-bitwave-system.md) | **Adapt the envelope idea; replace infrastructure** | Header/payload separation, header-selected validation-before-command flow, domain task translation | Replace global duplicate-overwriting registry and permissive validator; unify schema ownership; actual workload bypasses this gateway; not an assembled external-input host |
| [forge-codegen](../reviews/forge/forge-codegen.md) | **Preferred optional generation candidate** | Explicit plan/materialize/check/prune, inspectable generated source, provenance and drift concepts | Protect handwritten edits; strengthen rollback/recovery; inspect scripts before use and separate check-only verification from restore/pull/install/materialize/commit/push; generated consumers must work without codegen installed |
| [forge-fsm](../reviews/forge/forge-fsm.md) | **Optional capability library** | Small domain-owned state definitions, origin guards, transition outputs and trace tests | No durable scheduler, timers, concurrency, or automatic child orchestration; terminal/restore and persistence policies belong to its consumer |
| [forge-ui-framework](../reviews/forge/forge-ui-framework.md) | **Optional presentation family** | Runtime-free presentation contracts, explicit action-to-header conversion, web/native boundary and QA contracts | Real host/browser/native acceptance still needed; preserve context; reject duplicate routes without mutation; a recorded screenshot action is not captured pixels |
| [forge-game-framework](../reviews/forge/forge-game-framework.md) | **Optional domain example; adapt semantics** | Revisioned state, optimistic concurrency, replay/conflict receipts, explicit transition effects | Replay must not repeat effects; bound/persist receipts when needed; slow I/O must not run under the reducer mutex; no existing durable delivery guarantee |
| [forge-ios-sdk](../reviews/forge/forge-ios-sdk.md) | **Optional external client** | Injected protocol adapters, HTTP fixtures, renderer strategies, independent consumer contract | “Publish” is request/reply, not a broadcast bus; fix visible projection refresh/error handling and overlapping-request ordering before an end-to-end showcase |
| [forge-llm](../reviews/forge/forge-llm.md) | **Optional demanding application** | Checked configuration snapshots, load/prepare/ready boundary, request-state poisoning, ownership/shutdown tests | Local single-crate HPVS and standalone HPVS are different generations; keep models, hardware placement, numerical types and blocking worker teardown behind a domain adapter |
| [forge-os](../reviews/forge/forge-os.md) | **Optional numerical experiment; exclude kernel** | Small pure numerical functions and one-way library dependencies | It is not an operating/application runtime; validate dimensions/input bounds and scientific expectations before exposing a numerical handler |
| [forge-oss](../reviews/forge/forge-oss.md) | **Historical tooling reference; exclude router** | Inspectable normalization/rendering pipeline and generation ergonomics | IDE dispatch helpers are unfinished and CLI does not wire them; generation edge cases and status semantics need correction; not a working ingress bus |

## Typed payload lineage

Source reinspection confirms an important distinction. In the reviewed `forge-framework`, `crates/forge-core/src/envelope.rs` defines a `CommandPayload` enum with structured fields of custom `ForgeValue` or raw bytes; `crates/forge-command/src/api.rs` is generic over typed `Request`. Mission Control's `services/forge-core/src/envelope/mod.rs` has the same fields-or-bytes shape, while its command framework is generic over typed requests; the inspected database facade converts that envelope into its capability request. These are custom enum/value contracts, **not `serde_json::Value` as the universal envelope**, but neither inspected shared envelope is already `Envelope<P>` over a capability-owned business enum. [Framework report](../reviews/forge/forge-framework.md), [Mission Control report](../reviews/forge/mission-control.md)

The target retains the typed-command intent and strengthens it: capability-owned Rust payload enums, a generic envelope/bridge, and explicit header/variant agreement before effects. A selected external codec may translate JSON into those enums; an in-process caller constructs them directly. Do not reproduce a generic fields map internally under another name. If one application combines several capability payload types, a finite sum may belong to its composition package—not to a central framework enum. This is a proposed adaptation, not a claim of drop-in compatibility with either snapshot.

## Provider/factory lineage

A focused reinspection separates useful mechanisms from a container that does not yet exist. Older `forge_rust` provider accessors delegate to shared/fresh/thread-local factory experiments; `forge` has typed task factories and caller-supplied I/O construction; `forge-framework` has injected factory examples and a fallible session-factory template. HPVS distinguishes typed construction, provider acquisition and borrowed leases. Its singleton is per provider, transient is per acquisition, and pooled instances retain state.

The target adapts these into explicit constructor injection and narrow provider lifetime policies. It does **not** copy global caches, assume async-safe pools, conflate thread/request scope, or expose dependency lookup through the ingress header. HPVS’s frozen operation registry neither resolves a heterogeneous service graph nor validates missing dependencies/cycles; receiver creation can occur during registration. Transactional pre-construction graph validation and managed async shutdown are proposed additions, not existing HPVS guarantees. Read the [DI design and evidence distinctions](dependency-injection.md) before extracting any of these implementations. Historical reviews remain unchanged; no source tests were run for this focused reinspection.

## Why Mission Control grew difficult—and what it got right

The review found a working shared command-port seam and useful worker supervision, not a failed idea. Its growth problem was **responsibility leakage**: framework packages accumulated agent/research/deployment concepts, CLI composition knew many domain/backend combinations, older ingress paths remained, and facades did not consistently use the injected implementation they claimed to represent. The inspected manifest graph was acyclic; “a cycle caused everything” would be an unsupported explanation. [Mission Control review](../reviews/forge/mission-control.md)

The corrective design is therefore not “fewer crates at any cost” or “more ports everywhere.” It is:

- One admitted ingress contract, with compatibility adapters for old requests instead of parallel frameworks.
- Capability-owned typed facades behind a small header-key route catalog; no ever-growing global event/request enum.
- A composition root that chooses modules and concrete adapters, not domain workflows or CLI argument re-entry.
- Truthful readiness from actual injected resources, not constant success or separate memory doubles.
- Every task, lease, retry, and accepted request owned through failure and shutdown.
- Optional research, UI, inference, and operations packages that can be absent from a normal service build.

These points are reflected in the [blueprint](forge-framework-blueprint.md) and [decision record](decisions.md).

## Evidence-to-design ledger

| Observed cross-repository lesson | Target decision | Proof required |
| --- | --- | --- |
| Header command/type already selects operations in several generations | Preserve an explicit logical header key; no mandatory extra destination field | Canonical ingress tests plus normalization/conflict tests for each selected adapter |
| Some registries replace existing entries before returning a duplicate error | Validate transactionally, then freeze | Failed duplicate insertion preserves original behavior, not merely an error category |
| Metadata discovery and executable dispatch can drift apart | One registration object yields metadata and executable binding | Every advertised route is invocable under the same validated catalog |
| Generic typed registries and central event enums can expand into domain hubs | Keep payload enums/context capability-owned; shared bridge generic; optional finite composition sum | No framework-global enum or internal JSON fallback; header/variant mismatch rejected before effects |
| Memory implementations and profile names can overstate real behavior | Health and conformance tests target the actual injected adapter | Failures of actually selected stores/adapters change readiness/outcomes honestly |
| Effects can replay after cached receipt replay or crash-before-recording | Define dedupe scope, durable handoff, effect identity and recovery | Concurrent/restart/lost-response/crash-window tests |
| Factory creation, provider acquisition and operation registration are different mechanisms | Explicit typed bootstrap and scope-owned leases, separate from header dispatch | Constructor failure/partial cleanup, host isolation, selected-scope identity, lease return/state policy and actual concurrency tests |
| Lifecycle enums, metadata, or logs do not enforce deadlines and cleanup | Owned tasks, effective cancellation, bounded drain and reverse teardown | Forced failure at acquisition/execution/shutdown leaves truthful state |
| Source generation can be real while generated product assembly is incomplete | Optional developer tooling, ordinary inspectable Rust output | Clean consumer builds without generator; check mode is non-mutating |
| Same project/crate name appears in materially different generations | Select one canonical contract, use explicit compatibility boundaries | Dependency inventory and migration/conformance tests before consolidation |

The ledger synthesizes the detailed findings above; it is not a new static-code defect list. Refer to each linked report for its source scope and qualification.

## Scope and confidence

This synthesis covers **14 Forge repositories plus Mission Control**. The reports inspect actual manifests, representative executable paths, registration, lifecycle and tests; depth varies by repository and is stated in each report. Historical/non-default branches were inventoried where available, not all exhaustively reviewed. A name, README assertion, test count, generated file, or historical run log was not treated as implemented end-to-end behavior.

No source code, build scripts, tests, generators, inference runs, or deployment scripts were executed as part of these architectural reviews. Most reviewed snapshots lacked observed current test-check evidence, but not all CI was absent: the Bitwave-system snapshot has an observed failed CI run whose cause was not established. The Python agent runtime has a historical successful run whose job contents were not audited as suite proof. This is **not** a blanket passing-build assessment. [Bitwave-system verification](../reviews/forge/forge-bitwave-system.md), [agent-runtime verification](../reviews/forge/forge-agent-runtime.md)

The new framework remains a proposed synthesis. Historical transport implementations are evidence, not a requirement to install the same transports in the new framework; no broker or network API is selected. The smallest useful next step is the [durable admission vertical slice](../roadmap/forge-framework.md), with explicit failure tests and no wholesale repository merge.
