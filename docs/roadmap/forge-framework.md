# Forge framework delivery roadmap

**Reviewed 26 November 2026:** the framework foundation now supports a concrete
software-delivery application. The near-term work is integration, truthful
operation and source-bound acceptance of that application. Broader framework
and hosted-system milestones below remain useful context, not automatic next
work. [Current operating/source picture](../design/implementation-status.md).

## Immediate delivery sequence

| Step | Outcome | Exit evidence |
| --- | --- | --- |
| 1. Reconcile the integrated candidate | Review PR29 (`backup/main` → `main`), preserving the independent author/reviewer model and all legacy receipts | Reconcile the saved resubmission queue with implemented/remaining work; close historical source-fingerprint mismatches through explicit review of changed boundaries; validate branch-scope CI against the exact candidate. Passing local subsets alone is insufficient. |
| 2. Qualify the integrated main | Preserve the branch/main split and attach coverage to the actual source | After a trusted merge, run the selected main-scoped database/browser plan on the observed main commit; retain real WASM, desktop/narrow screenshot review, cleanup and exact-source receipts. Missing prerequisites remain not-run coverage. |
| 3. Prepare a coordinated rollout | Install compatible ABI16 host/assets/WASM and schema41 services/grants | Verify old schema/receipt compatibility, branch-owner recovery, open-PR fencing and rollback. Schema40 is the current observed database; consolidation did not activate schema41. |
| 4. Resume deliberately and prove the loop | Restore the chosen API/event path only when requested; verify an end-to-end delivery | Record Plan, authored candidate, independent review, branch checks, merge, main checks and protected release evidence. Worker process liveness is not completion proof. |
| 5. Expand from demonstrated demand | Reuse the small framework for Workbench, Studio, games or additional providers | Require a named consumer, bounded contract and acceptance case. Keep provider selection explicit and retain truthful unsupported/unknown states. |

Steps describe direction and acceptance, not deployment or service-start authority.
The API/event runner is currently stopped/boot-disabled; coding and delivery
workers remain active. Do not interpret the stopped front door as a full-system
pause. Main promotion, main qualification and runtime activation are distinct.
No fleet scheduler, broker, container runtime or Kubernetes rollout is selected
by this sequence.

The [preserved task inventory](../../resubmission-queue/README.md) records further
requests for defect/backlog flows, proxy boundaries and a verified-main Drive
snapshot stage. Re-triage them against the consolidated source and exact saved
acceptance criteria; their collection-time flags are not current execution orders.

## Delivery status now

| Slice | Current repository state | Remaining scope |
| --- | --- | --- |
| Typed core and assembly | Implemented headers, admission/variant gates, frozen routes, typed registrar groups, declared dependency-order checks and singleton/transient providers | Automatic graph execution, generic pooled scopes and a general managed lifecycle engine |
| Local terminal | Implemented node observations, admitted discovery, optional native disk sessions and a volatile manually stepped counter | Whole-machine health, a filesystem sandbox and arbitrary process execution |
| Access and durable jobs | Implemented verified subject/deadline contexts, exact-route grants, a private locked file store, committed sum-job receipts/results, leased attempts and owned worker shutdown | Generic programs, automatic receipt retention, remote job delivery and broad identity/delegation |
| Agent and managed editing | Implemented configured HTTPS/mTLS node/project/source calls and explicit managed-write grants with revision/journal guards | Enabling writes requires a provisioned private Unix workspace; arbitrary filesystem paths and shell execution remain outside the contract |
| Generated portal | Agents, Workbench, Home and Design; inactive Chat/Mail histories retained. Consolidated ABI16 adds triage, per-agent branches and CI stage/log inspection | Main/browser/source-fingerprint qualification and coordinated schema41/asset rollout remain open; general editor-widget integration remains separate |
| Coding delivery application | Python/PostgreSQL native and external author workflows, independent review, per-event retries, content identities and trusted Git/Jenkins delivery | This application owns workflow policy; it does not make the Rust core a scheduler. Verify installed configuration separately from source capability |
| Reuse and release | Concrete optional capability consumers and public registrar fixtures exist | Workspace crates remain `publish = false`; source availability, local tests, deployed configuration and a supported public SDK are different claims |

Use the [source-backed blueprint inventory](../architecture/forge-framework-blueprint.md#current-implementation)
for implementation entry points. Test definitions describe intended coverage;
this documentation review does not certify a fresh test run or a new deployment.

## Earlier framework delivery checkpoints

The [initial implementation](../../src/rust/forge-ide-web/README.md) contains typed
Rust component declarations, YAML style tokens, generated semantic assets, a real
worker/WASM model and an explicitly admitted loopback host to the existing agent.
It includes file/source inspection, guarded editing for explicitly writable
managed projects and distinct partial/error/cancel/stale states. The host supports
an explicit HTTPS origin and trusted-proxy sessions; live enablement and access
controls are recorded separately in the service inventory.
Native checks, real WASM ABI tests and generated-worker fault tests are distinct
from real-browser end-to-end acceptance; never promote one into the other.

The [workbench follow-on](../../src/rust/forge-ide-web/WORKBENCH.md) adds the compact
header project/view sheets, a unified report-local entry/symbol query, Problems
counts/navigation, reusable pane geometry/narrow controls and protected
DOM teardown. Every local action carries accepted report context; stale integer
IDs cannot select data from a new report. That earlier bundle used local WASM
ABI5; consolidated source is ABI16. Older ABI diagrams describe their own checkpoints. Native HTTP protocol 1
remains separate. Mail remains inactive in the current portal. Manual Mail archive is a retained repository capability behind the
native `chat.mail_archive_enabled` opt-in, false by default. It requires matching
runner/schema and host support; the dated live ABI 4 checkpoint has not been
upgraded by this source change. See [archive compatibility and rollout](../../src/rust/forge-ide-web-host/README.md#workbench-asset-compatibility).
The original UI milestone required same-head Rust and sandboxed desktop/narrow
browser evidence plus visual review. Current branch/main acceptance follows the
selected scoped CI contract described above. Bounded source and managed-edit operations now have
separately admitted native paths. An external editor-widget lifecycle remains a
proposal.

## Authenticated node-info boundary — implemented narrow proof

[Typed remote conversation and optional HTTPS adapter](../architecture/remote-node-info.md)
now exercise one read-only node.info call over mutual TLS on loopback. Configured
certificate mapping and independent operation policy precede the existing bridge;
wire DTOs/runtime remain in the adapter. Public config/factory/registrar and a
separate consumer establish reusable integration and dependency omission. No
remote jobs, credential provisioning or Bitwave is included in that original
node-only proof. The configured agent subsequently added project/source operations
and explicit managed writes. Operational deployment is separately configured;
the initial loopback example is not physical-host validation.

## Forge IDE initial capability — implemented

The optional [IDE inspector](../../src/rust/forge-ide/README.md) assembles a rooted
native filesystem client and inspection task through public config/registrar/factory
exports. A feature-gated CLI uses typed shared ingress and emits YAML for the typed
file tree, text line counts and Rust declaration hierarchy. Real syntax parsing is
not macro expansion or semantic analysis. Symlinks are not followed; budgets and
incomplete inspection statuses are explicit. External editor-widget/LSP integration, watch mode, incremental
analysis, cross-file resolution and untrusted-source process isolation are **not
implemented** and are not automatically authorized next work.

## Current implementation checkpoint

The [Rust core](../../src/rust/README.md) establishes validated headers, typed
request/response values, payload-kind agreement, frozen duplicate-safe routes and
injected admission. Factory/provider/lease primitives support singleton/transient
injection; a sixth core crate stages typed factory groups and validates declared
construction order before freeze. Independent `IoClient` and `Task` contracts stay
synchronous. Deadline/identity wrappers and durable jobs now exist in selected
capability/adapter packages; they do not turn Task into a scheduler or the
bootstrap registry into an automatic graph executor.

## Local runnable system checkpoint — implemented

`forge-kernel::node` now provides explicit configured identity and local observation contracts. `forge-local` composes selected node/disk registrars, freezes before creation, owns providers, injects tasks and registers typed bridge bindings with their discovery metadata. `forge-terminal` is a separate std-only adapter with help, argv commands and bounded literal-line sessions. Help uses the admitted catalog; every business request uses the shared bridge.

Proof covers node-provider replacement without task edits, missing/conflicting group failures before creation, earlier-client cleanup after later startup failure, unknown/mismatched routing before effects, per-session native cwd and failed-cd preservation, actual terminal process execution, error codes and safe output. Compile-time node-only builds omit the disk module; the earlier pure consumer still omits the whole kernel package. No six-core-library API changed.

This is **not** production health, authentication, bounded directory listings, networking, remote execution or a bootable Pi OS. Node status reports only this client's response and instance uptime. The local jobs checkpoint below follows this; an authenticated remote boundary remains separately scoped. [Run instructions](../../src/rust/forge-terminal/README.md) · [Assembly contract](../../src/rust/forge-local/README.md).

## Independent command description checkpoint — implemented

`forge-command` now provides `DescribeCommand` separately from the unchanged
`Task` execution trait. Selected node/disk tasks and the non-Task discovery
handler share authored names/help and named argument types/presence/defaults.
Successful command bindings derive their catalog from validated descriptors;
terminal help uses admitted discovery without executing the business task/client.
The reusable help option stays in presentation metadata, not typed task inputs.
Explicit codecs and tests establish agreement; this is not reflection, a universal
schema/parser or automatic execution. Pure tasks remain metadata-free. See the
[description contract](../../src/rust/forge-core/README.md).

## Bounded local jobs checkpoint — implemented

The optional [forge-local jobs module](../../src/rust/forge-local/JOBS.md) provides
submit/inspect/list/step/cancel for a pure counter. Typed configuration and registrar
stage its factory before freeze; a singleton owns the ready service. Bounded
volatile records, owner isolation, intent replay/conflict/retention, time checkpoints
and cooperative cancellation are tested. Task/core traits remain unchanged.

Progress is manual, receipts disappear on exit, and local owner labels are trusted
input—not credentials. Enabling jobs adds documented public application-enum
variants, requiring downstream exhaustive-match migration. No remote/durable work
is implied by this proof.

## Optional template renderer — implemented subset

The [rendering capability](../../src/rust/forge-template/README.md) now supports
parse-once templates, explicit typed schema/context, escaped/raw scalar tags,
sections/inverted sections, parent lookup and finite resource budgets. It is
algorithmically independent of startup and routing. Its public `assembly` module
now provides typed config/factory/registrar exports, with bootstrap/provider as
the only unconditional normal dependencies. The registered-render example consumes those exports
and keeps its Task, descriptions and bridge bindings application-owned. A separate
Cargo fixture verifies importability, injected methods, failure phases and optional
dependency omission; an example alone was not sufficient public integration. Its opt-in [single-shot CLI](../../src/rust/forge-template/CLI.md) now renders
inline/file sources with typed scalar bindings; the separate Forge terminal is
unchanged. It does not install files or implement build/codegen policy. The 38 historical builtins and explicit custom-method chains are included. Bundled
source templates and serialized schema formats remain separate future slices.

## Broader foundation interfaces — still proposed

The [module/interface design](../architecture/foundation-interfaces.md) and
[contract sketches](../architecture/foundation-contracts.md) detail a more generic
bounded job engine, independent verified-identity/resource-policy boundary and
transport-neutral addressed call. The views distinguish proposed imports/ports/remote reads from the implemented
local counter lifecycle. No extra source crate or cancellable Task is implied.
The concrete `forge-access`, `forge-jobs`, `forge-remote` and HTTPS packages now
implement narrower contracts with their own exact exports. The generic sketches
remain proposals; do not import their illustrative names as if they were the
implemented APIs. Coordinated changes to exhaustive application enums remain a
source-compatibility concern.

## Implemented foundation milestone: bootstrap and registrars

**Implemented narrow checkpoint:** `src/rust/forge-bootstrap` now provides prepared groups, atomic consuming registration, declared-order validation/freeze and typed tuple extraction. The five earlier library contracts are unchanged. Three demo packages prove optional capability/implementation registration and explicit provider/task assembly; that bootstrap checkpoint added no CLI, HTTP, IPC, remote execution, home renderer or async runtime. Native disk access is the separate follow-on checkpoint below. The [bootstrap reference](../architecture/bootstrap-registrars.md) distinguishes the real APIs from deferred policies.

| Patch | Concrete implementation boundary | Acceptance evidence |
| --- | --- | --- |
| A — Registration groups (implemented) | Std-only `forge-bootstrap`: prepared groups, controlled duplicate/missing-selection errors and consuming freeze. One registry owns metadata plus its typed nested tuple; rejected registration returns unchanged state. | A registrar stages recipes without calling `create`/`acquire`; duplicate within a group, duplicate group identity or cross-group binding conflict leaves all prior registrations intact. Failed whole validation exposes no frozen plan. Freeze prevents further registration. |
| B — Typed bootstrap demonstration (implemented) | Application/example module calls two package-style registrars with typed config enums/structs; all environment mapping stays at the boundary. Three explicitly labeled demo packages provide real package-selection evidence, not production clients. Validate selected requirements and explicit order; no automatic graph executor. | Missing selection, conflicting explicit choices, invalid config and self/forward dependencies fail before factory effects. Declared order matches recorded constructor order. Disabling an optional fixture does not construct its factories or add a mandatory I/O requirement to pure tasks. |
| C — Factory/provider/task assembly (implemented) | Explicit application code consumes the frozen groups: singleton creation or lazy transient acquisition, dependent task recipe binds ready client handles, then task construction and the existing bridge binding. Preserve typed factory/provider errors. | Singleton reuse and host isolation; transient creation only on acquire; construction failure at each stage releases owned earlier values and publishes no application. Fake client substitution leaves task policy unchanged; denied/mismatched/unknown commands still cause no client effects. |
| D — Small consumer proof and safeguards (implemented) | Runnable in-process front door plus a second configured group/default substitution. Update dependency guard, API docs and skills to mark only the verified subset implemented. No new transport. | Clean consumer builds; no normal core → implementation dependencies; no `Any`/JSON erasure or framework-global business enum; factory counters prove an overridden default never runs; duplicate is not implicit replacement; fmt, debug/release tests, strict Clippy/rustdoc and dependency checks pass. |

The concrete implementation refines A's separate-slot sketch: consuming registration owns both metadata and typed groups, so no slot can drift from the index. Package registrars are ordinary typed functions returning prepared groups; application bootstrap chooses them and the construction order. Core tests exercise failed *multi-factory* groups, not only a single duplicate. Binding metadata is not a route-descriptor layer: the existing bridge rejects duplicates, the demo tests its known routes, and generic catalog/route coverage remains deferred.

Original bootstrap checkpoint: 75 all-feature integration tests plus five doctests, debug/release and pure-consumer checks, strict Clippy/rustdoc, four runnable modes and dependency-guard positive/negative checks. These are in-process/fake proofs, not production I/O, operational readiness or a general lifecycle certification.

**Demonstrated:** a small consumer can choose implementation packages, pass typed startup config, run bootstrap and obtain a working typed in-process application—or a specific error with no partial publication. A client-using task prepares requests; injected clients execute them. A pure task remains independent of clients. This establishes the plug-and-play core, not a complete operational host.

**Later, only when needed:** automatic graph ordering/diagnostics, runtime registration, pooled/request-scoped providers, async factories/cleanup and a general managed lifecycle engine. Concrete owned workers, remote/local clients and presentation adapters already exist; their lifecycle remains application/capability-specific. A generic home can later expose explicit default/replace/extend contracts; do not implement a UI to prove factory registration. That original core checkpoint selected no transport. The later optional HTTPS agent and generated UI host now provide concrete adapters without making them mandatory core dependencies.

## Native disk access checkpoint

**Implemented:** optional `forge-kernel::disk` using native hosted Rust APIs, typed request/result enums, a config-driven registrar and factory. Operations are list, pwd, client-local cd and permission inspection. There is no shell invocation, process cwd mutation or create/delete/chmod API. The `disk` consumer feature proves explicit assembly, injected-task substitution and the unchanged header/admission/variant gate. Pure and message-only consumers do not import the kernel package.

Native temporary-fixture tests cover deterministic listings, state isolation, failed cd, typed OS errors, registration before filesystem validation, provider scopes and permission metadata. Unix fixtures additionally cover symlinks and non-UTF8 names. See the [current executed check counts](../../src/rust/README.md) and [disk contract/limits](../../src/rust/forge-kernel/README.md). These checks are not proof of sandboxing, Windows behavior, effective access or Raspberry Pi/bare-metal readiness. Filesystem mutations and additional kernel-like clients require separate scoped work.

## Kernel planning sequence

The [researched hosted-kernel plan](../architecture/forge-kernel-plan.md) now records the narrow implemented **local node/status/discovery and terminal checkpoint**, reusing typed bootstrap/bridge/provider contracts. The bounded local counter, optional authenticated HTTPS node/IDE agent and generated UI prototype now exist. The HTTPS adapter privately owns its runtime; synchronous core traits are unchanged. The durable sum-job store/worker, configured web host and explicitly granted managed writes are implemented. Generic job hosting, automatic peer discovery and a bootable Pi kernel remain deferred. UI browser acceptance is tracked separately from native/WASM tests.

The small volatile job proof in that plan precedes—not satisfies—the durable acceptance gate below. The existing durable jobs example remains a separate later consumer, not a requirement to turn every local command into a stored job.

## Durable vertical slice — implemented bounded consumer

The [durable jobs capability](../../src/rust/forge-jobs/README.md) implements
typed submit, status and cancel over the shared bridge. It commits receipts
and results to a local store and runs one bounded sum job with leased attempts
and restart recovery. Framework-wide identity and service lifecycle work
remain separate milestones.

The original gate was the `jobs.submit` → `jobs.status` workflow, now implemented for the bounded sum consumer. The first route validates a bounded work request and returns a stable durable receipt; the second makes that acceptance observable. Use a trivial, explicitly documented job kind. Execution, network/broker integrations, scheduling, remote model calls, and deployment orchestration are not prerequisites for proving the first admission/storage seam.

Start with an in-process contract driver, the shared bridge, one capability, and a real local durable-store adapter. No input transport is selected or required. The optional Unix `forge-jobs` CLI now supplies one authenticated local consumer; remote delivery is a separate extension. A memory adapter is useful for unit tests but cannot satisfy the durability gate. This is intentionally more meaningful than a health endpoint and smaller than recreating Mission Control.

## Original milestones and exit criteria

Stages 1/1b, the bounded Stage 2 store and Stage 4 sum worker have concrete
implementations linked above. Stage 3 is implemented in specific host paths,
not as a universal lifecycle framework. Later extension/release goals remain
provisional; this table preserves their intended acceptance criteria.

| Stage | Scope | Exit evidence | Do not add yet |
| --- | --- | --- | --- |
| 0 — Select the salvage | Complete the repository reuse map; identify minimal contracts and licensing/ownership constraints before code copying | Every candidate has a source report, concrete extraction boundary, known limitations, and a validation plan | Wholesale repository merges or renamed application scaffolds |
| 1 — Admission kernel | Generic `Envelope<P>`/`Bridge<P>`, capability payload enum, immutable registry, header/version/variant checks, trusted-context authorization | Duplicate registration is non-mutating; every entry point validates; denied/unknown/conflicting routes and header/variant mismatches have zero handler effects; no internal JSON fallback | Transport libraries in the kernel; generic global service lookup |
| 1b — Bootstrap core (implemented narrow slice) | Selected typed registrars stage cohesive factory groups; validate/freeze; explicit construction/provider ownership and task binding | Patches A–D above; zero factory effects during registration, atomic group failure, no partial application, correct optional/default selection | Concrete transports, automatic heterogeneous graph execution, home UI |
| 2 — Smallest durable vertical slice | Typed-enum in-process submit/status, jobs policy, real store, typed factory plus one host-owned store provider, constructor-injected ports, stable receipts | Integration tests cover success, same-key replay, changed-body conflict, concurrent duplicate submissions, restart persistence, store failure, and lost-response recovery | Worker execution, remote side effects, extra domains |
| 3 — Prove lifecycle and substitution | Validate/freeze the provider dependency plan before construction; explicit overrides, supervised startup/shutdown and replaceable provider; add an input adapter only for a real consumer | Missing/cyclic/invalid-scope bindings fail before factory effects; host instances are isolated; default/override conflicts fail without mutation; factory failure cleans partial assembly; replacing the provider preserves ingress/domain contracts; actual selected adapters conform; shutdown reports grace exhaustion | Mandatory second transport, dynamic plugin loading or distributed host orchestration |
| 4 — Execute one job type | Supervised worker, durable claim/completion, cancellation policy, retry budget | Crash/reclaim tests; no lost accepted jobs; state transitions and concurrency controls explicit; status reflects reality | General workflow DSL or unbounded fan-out |
| 5 — Prove extension ergonomics | Add a second small capability and one custom provider override without modifying bridge internals | Small composition-only integration; no mandatory unrelated dependencies; absent/default/override/conflict test matrix passes | Broad starter catalog before the extension contract stabilizes |
| 6 — Package for reuse | Version public contracts, document compatibility/support, publish runnable examples and measured limits | Clean consumer build, contract tests, dependency checks, reproducible demo and release checks | Production claims unsupported by operational evidence |

A stage can be split into small reversible patches. Its exit gate—not a date or amount of generated code—determines readiness for the next stage.

## Verification strategy

**Fast:** typed factory inputs/errors, provider selection and scope compatibility, no side effects on invalid assembly, capability enum contracts, header/variant mismatch rejection including direct typed callers, optional edge-codec conversion, admission rejection without effects, config merge/validation, registry transactionality, override conflict detection, dependency-graph checks.

**Real boundaries:** storage concurrency, cleanup, restart persistence, and ambiguity recovery; parsing, outcome mapping and lifecycle of each input adapter only if selected. Test retry, ordering and acknowledgment semantics only when that adapter actually has them. Keep test doubles, local real-dependency tests, and deployed-system tests visibly distinct.

**Lifecycle:** one shared instance per host with isolation across hosts; factory failure and start failure after each acquisition step; stop during active requests and poll loops; bounded drain and cancellation; all workers joined before resource destruction. Canceling a caller must not falsely report that an already committed job vanished.

The next typed registration index is not a runtime provider lookup catalog. Do not implement a pool, request scope or async factory abstraction just to check a DI feature box. Begin with configured recipes, explicit typed constructors and existing singleton/transient behavior; the later durable slice adds its host-owned store provider. Add scoped acquisition only for a real consumer, with lease-return, retained/reset state, capacity and cancellation tests; do not substitute an OS-thread cache for request scope. See [provider/factory DI](../architecture/dependency-injection.md).

**Developer experience:** from a clean consumer project, install only the necessary starter, register a handler, supply config, and run the application. Add a second capability without a framework-global business enum; a finite application-owned sum is permitted only when its single ingress genuinely combines different payload types. No JSON or runtime downcast substitutes for the typed contract. Inspect the dependency graph to prove optional code remains optional.

No source-repository test result is automatically a pass for the assembled framework. Preserve exact commands, environments, failures, and not-run checks when implementation begins.

Before executing any existing aggregate verifier, inspect it and its called helpers. A command named “verify” may restore tracked files, pull changes, install tools, regenerate sibling repositories, or commit/push. Select explicit non-mutating check commands for the first baseline; isolate intentional generation in a disposable workspace with reviewed output boundaries. Do not run a broad script merely because it appears in a README. These are concrete concerns in the [codegen review](../reviews/forge/forge-codegen.md), not hypothetical permission to alter source.

## Portfolio organization

Keep the public portfolio legible:

```text
docs/reviews/forge/       # one sanitized report per reviewed repository
docs/architecture/       # synthesis, decisions, module contract, editable diagrams
docs/roadmap/            # sequenced evidence-based implementation plan
src/rust/                # actual Rust implementations only when built
src/python/              # actual Python projects, not framework kernel dependencies
src/bash/                # operational scripts with documented scope
.agents/skills/          # reusable agent skills, separate from product code
```

Do not create empty implementation trees to imply progress. Keep private raw evidence, repository access details, credentials, deployment addresses, and source snapshots out of the public portfolio. Link architecture conclusions to the sanitized reports; keep deeper evidence privately traceable.

## Control growth before it starts

Every proposed addition must name a real consumer, the responsibility it owns, the dependency cost it imposes, the smallest public contract, and the tests proving it can be removed or replaced. Prefer one coherent capability over a new catch-all package. Retire duplicate prototypes only after their useful behavior is captured and the replacement is demonstrated; review does not itself authorize deletion or migration.

## Configured remote IDE checkpoint

The reviewed two-machine proposal now has a runnable `forge-agent` implementation:
public batch project registrar, named-node client config, one mTLS POST gateway,
operation/project authorization, reused IDE task and typed bounded report to YAML.
Actual tests run two localhost processes, including denied identity/alias, timeouts,
limits and signal shutdown. [Current details](../../src/rust/forge-agent/README.md).
The original proposal PDF remains a dated design record. Current source also
supports bounded `ide.source` and explicitly granted `ide.managed_write`; read
permission alone cannot enable mutation. Physical-host validation and credential
lifecycle are operational evidence in the service inventory, not implied by a
localhost fixture. Generic remote jobs and arbitrary filesystem paths remain
outside this agent contract.
