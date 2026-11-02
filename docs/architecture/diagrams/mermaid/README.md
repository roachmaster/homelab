# Mermaid UML-like views

Editable Mermaid sources and committed SVGs for the current Forge framework. **M01–M03 describe implemented synchronous code. M04 distinguishes the implemented local checkpoint from proposed later phases. M05/M06/M08 detail proposed future interfaces; M07 shows the implemented local counter lifecycle. M09/M10 describe the implemented optional template library and its development examples.** M11 shows implemented Forge IDE inspection. M12 shows the implemented HTTPS gateway; M13–M17 show the proposed two-machine IDE milestone. These complement, rather than replace, the existing [DOT/C4 architecture views](../README.md). M04 is a milestone view; DOT07 remains the hosted-kernel component planning view.

| View | Status and purpose | Source | Image |
| --- | --- | --- | --- |
| M01 — Contracts | Independent execution/description traits, ownership and native examples | [Mermaid](m01-contracts.mmd) | [SVG](images/m01-contracts.svg) |
| M02 — Bootstrap | Implemented disk singleton assembly, successful path | [Mermaid](m02-bootstrap.mmd) | [SVG](images/m02-bootstrap.svg) |
| M03 — Request | Implemented checked list-current path, with rejection gates | [Mermaid](m03-disk-request.mmd) | [SVG](images/m03-disk-request.svg) |
| M04 — Kernel phases | Implemented local system/jobs versus proposed remote phases | [Mermaid](m04-kernel-phases.mmd) | [SVG](images/m04-kernel-phases.svg) |
| M09 — Template architecture | Implemented public integration and application-owned wiring | [Mermaid](m09-template-architecture.mmd) | [SVG](images/m09-template-architecture.svg) |
| M10 — Template rendering | Implemented validation, method chains and escaped output | [Mermaid](m10-template-render.mmd) | [SVG](images/m10-template-render.svg) |

## M01 — Implemented contracts

![Implemented Forge contracts and native disk examples](images/m01-contracts.svg)

UML-like notation abbreviates Rust, not a generated API reference. Dashed hollow-triangle arrows mean trait implementation; diamonds mean ownership; other labeled arrows show construction or use. Type parameters are omitted from class names for readability: the actual contracts include `Factory<T>` and `InstanceProvider<T>`, and the concrete containers are generic. `InstanceProvider::Lease<'a>` is a borrow-tied **GAT** promising `Deref`, not a dyn-compatible universal provider. `Singleton` owns one prebuilt value per provider and returns a borrow; `Transient` retains its factory and constructs an `OwnedLease` on each acquisition. Associated errors are preserved in `Result`.

`Task` and `IoClient` have independent normal dependency graphs. A pure task requires no client. `ListCurrentTask<C>` demonstrates constructor-injected client use, not a dependency of the Task contract on disk. The client accepts the task's prepared typed request. See the [actual Rust API](../../../../src/rust/README.md), [provider contracts](../../../../src/rust/forge-provider/src/lib.rs) and [disk consumer](../../../../src/rust/examples/forge-bootstrap-demo/src/disk.rs).

`DescribeCommand` is independent of `Task`: `NodeTask<C>` implements both,
while discovery implements description without Task and the older list-current
Task need not expose metadata. Static per-operation descriptors own names/help
and semantic argument types/presence; they are not reflection over Rust input
fields. The terminal owns its help-option syntax and renders only the admitted
catalog. See [command descriptions](../../../../src/rust/forge-command/README.md).

## M02 — Implemented bootstrap

![Implemented disk registrar, freeze and explicit singleton assembly](images/m02-bootstrap.svg)

This is the successful `DiskApplication` assembly path. The disk registrar is an ordinary package function returning a typed cohesive factory group, not a universal registrar trait. Registration validates configuration syntax without filesystem I/O. Consuming registry operations own metadata and typed tuples together; a conflict returns the unchanged original and incoming group. Freeze validates declared dependency availability/order; it is not an automatic graph executor or route-descriptor coverage checker. Factory creation is explicit after freeze; errors return no assembled application. The application itself wraps the singleton in `Rc`; the handle arrow is ownership shorthand, not a provider method returning `Rc`. The shown singleton path does not eagerly create transient instances. See [bootstrap contracts and limits](../../bootstrap-registrars.md).

## M03 — Implemented request

![Implemented list-current request, header admission gates, task and native client](images/m03-disk-request.svg)

The main arrows show **success**, with rejection points annotated rather than alternate executions drawn over the same path. Exact header destination/version lookup precedes admission, which precedes registered payload-kind consistency. Unknown route, denied policy or mismatched variant returns a typed error before calling the task/client. The task prepares `DiskRequest::List`; the injected handle acquires the singleton client; native filesystem inspection preserves paths and sorts entries. I/O errors propagate, not hidden retries. Header routing does not guess from payloads. The demo admission flag is not production authentication, and the current bridge is synchronous and not `Send`/`Sync`-shareable unchanged. A separate change-directory binding updates only client-local state after successful validation. See [typed ingress](../../framework-dsl.md) and [disk semantics](../../../../src/rust/forge-kernel/README.md).

## M04 — Local checkpoint and proposed kernel phases

![Existing core and disk foundation followed by proposed hosted kernel phases](images/m04-kernel-phases.svg)

The foundation, local node/discovery/terminal and explicitly stepped counter jobs are implemented. Teal marks existing code; amber marks proposed phases; gray marks host responsibilities or later gates. Later phase arrows show recommended delivery order, not runtime calls. The narrow M12 node-info proof selects optional mutual-TLS HTTPS on loopback; reachable remote deployment remains future. Jobs are not OS processes, and cancellation is cooperative. The host OS still supplies protected memory, processes and drivers. This is not a bootable kernel or a tested Raspberry Pi deployment. Follow the [researched kernel plan](../../forge-kernel-plan.md) for acceptance gates and open decisions.

## Proposed foundation interfaces

The [module design](../../foundation-interfaces.md) and [Rust contract sketches](../../foundation-contracts.md)
are the source of semantics for these four views. Broader module/port APIs remain **proposed**; the narrow M07 counter lifecycle is
now [implemented](../../../../src/rust/forge-local/JOBS.md). Proposed names are not
exported APIs and diagrams do not establish stability. Existing Task, description, I/O, provider and bridge contracts remain
independent and unchanged.

### M05 — Module and dependency boundaries

![Proposed foundation module boundaries](images/m05-foundation-modules.svg)

[Mermaid source](m05-foundation-modules.mmd). Amber nodes are logical units that may
start as private application modules; teal is the existing contract family.
Arrows are allowed **imports**, not calls, deployment units or registration order.
Access does not import jobs; concrete adapters depend inward on contracts. Crate
extraction requires an actual consumer boundary, not one package per class.

### M06 — Typed interface contracts

![Proposed typed foundation interfaces](images/m06-foundation-interfaces.svg)

[Mermaid source](m06-foundation-interfaces.mmd). “Proposed trait” marks new seams;
IoClient is existing. Rust associated types/lifetimes are abbreviated: each dyn
use fixes them; there are no generic dyn methods. The access-unit boundary owns
private principal issuance. JobService returns an owned bounded projection, not a
borrow escaping a RefCell guard. The owner pump releases engine borrows before
calling injected program code. Neither this diagram nor Arc makes today's bridge
concurrent; the provider GAT remains a separate non-universal-dyn contract.

### M07 — Job lifecycle and cancellation

![Implemented local counter lifecycle](images/m07-job-lifecycle.svg)

[Mermaid source](m07-job-lifecycle.mmd). Arrows show allowed state transitions; the [state table](../../foundation-interfaces.md#state-and-cancellation-rules) defines their triggers. Local owner labels are trusted input, not production authentication. This is a state-machine-style flowchart,
chosen from the renderer's supported Mermaid subset. “Running, cancel requested”
is a flag on Running, not confirmation of stopping. Queued expiry means never
started; a runtime overrun requests cooperative cancellation. Terminal states
never restart. Completion/failure can win the cancellation race. Retention and
uncertain remote outcomes are deliberately not execution states.

### M08 — Authenticated remote read sequence

![Proposed authenticated remote read sequence](images/m08-remote-admission.svg)

[Mermaid source](m08-remote-admission.mmd). The successful path shows verification,
bounded typed decoding, unchanged bridge checks and resource policy. Denials stop
before business effects; possible delivery followed by a lost/malformed reply is
OutcomeUnknown. Destination identity and original/delegated caller authority are
not interchangeable. This broader proposal does not mandate a protocol; M12 now implements one narrow
HTTPS node-read proof, not disk navigation or job submission. No exactly-once claim follows.

## Implemented template renderer

These views describe an **optional library and runnable development examples**, not
another deployed application or a new Forge terminal command. The library exposes reusable config/factory/registrar integration and has exactly
two unconditional dependencies: bootstrap and provider contracts. The opt-in
[single-shot CLI](../../../../src/rust/forge-template/CLI.md) adds only its selected
contracts and is not pictured in this example-focused view. Its rendering engine
modules do not import those contracts. Application Task, description and bridge
imports remain dev-dependencies of the example and optional dependencies of the CLI.

### M09 — Architecture and Forge composition

![Implemented template library and development-only Forge composition](images/m09-template-architecture.svg)

[Mermaid source](m09-template-architecture.mmd). Arrows show construction, use or
module collaboration, not deployment edges. The upper panel is application-owned
example code: the host supplies explicit config and method registry to the public
registrar; consuming freeze precedes factory creation and template parsing. The
host wraps the configured renderer in its own task and singleton; the binding
acquires its lease only after header/admission/payload-kind checks. Help uses
admitted static metadata without acquisition or task execution. `RenderTask`
prepares the capability-local value tree from typed input; no I/O client or global
business enum is introduced.

The lower panel is the reusable library. `assembly` exports `RenderConfig`,
`register_renderer`, `RenderFactory` and `Renderer`; consumers import them without
copying an example. Factories share immutable schema/method membership and create
fresh parsed templates. Provider choice stays with the host; transient acquisition
does not reset shared callback state. The engine separates parsing from per-render
schema/data validation, expression preflight and execution. Custom callbacks are
trusted native code, not sandboxed or preempted by limits. See the [public API and
standalone consumer proof](../../../../src/rust/forge-template/README.md#public-forge-integration),
[all38 builtin semantics](../../../../src/rust/forge-template/METHODS.md) and
[composition example](../../../../src/rust/forge-template/examples/registered_render.rs).

### M10 — Parse, validate, chain methods, render

![Implemented method-enabled template rendering sequence](images/m10-template-render.svg)

[Mermaid source](m10-template-render.mmd). This is a successful render with failure
stops described in the notes, not a claim that execution continues after an error.
The parser constructs a reusable AST without executing methods. Each render first
validates its explicit schema and context, then preflights expressions in **all**
branches, including skipped ones. Known builtin signatures are checked statically;
custom/dynamic results are checked when available. Method arguments are literals or
paths resolved in the context stack, not nested calls or prior method-result fields.

Each chained method receives the preceding result. An absent optional receiver
skips the entire chain; this differs from an existing null value passed to `default`.
A method result may select/iterate a section or become an interpolation. Escaped
interpolation receives one HTML-escaping pass **after** transformation; raw tags
bypass it, and literal template text is unchanged. This is not whole-document or
context-specific HTML safety. The private bounded buffer is returned only on
success; errors do not return a partial document, but cannot undo trusted callback
effects. Traversal/output budgets do not preempt callback allocation, blocking,
panics or destruction. See [limits and compatibility](../../../../src/rust/forge-template/METHODS.md).

## Remote IDE proposal

[Proposal and acceptance gates](../../proposals/remote-ide.md) ·
[Six-page visual PDF](../../proposals/remote-ide-proposal.pdf).
These five views describe **new assembly and behavior**, not implemented remote
inspection. M11/M12 above remain the current local-IDE/loopback-network baseline.

| View | Focus | Source | Image |
| --- | --- | --- | --- |
| M13 | Two-machine C4-style context/container boundary | [Mermaid](m13-remote-ide-context.mmd) | [SVG](images/m13-remote-ide-context.svg) |
| M14 | Application and capability components | [Mermaid](m14-remote-ide-components.mmd) | [SVG](images/m14-remote-ide-components.svg) |
| M15 | Existing traits and proposed typed interface conversation | [Mermaid](m15-remote-ide-contracts.mmd) | [SVG](images/m15-remote-ide-contracts.svg) |
| M16 | Authentication, admission and project authorization | [Mermaid](m16-remote-ide-request.mmd) | [SVG](images/m16-remote-ide-request.svg) |
| M17 | Registrar staging, construction, readiness and shutdown | [Mermaid](m17-remote-ide-bootstrap.mmd) | [SVG](images/m17-remote-ide-bootstrap.svg) |

### M13 — Two-machine context

![Proposed two-machine Forge IDE context](images/m13-remote-ide-context.svg)

Process boxes are deployment boundaries, not crates. Amber marks proposed
behavior; gray is the human actor. This is a C4-style view, not strict C4 notation.
The proposed application owns both agent serving and its thin remote-client entry
point; today's `forge-ide` CLI remains local. Named nodes select explicit endpoint
configuration; project aliases select server-authorized rooted tasks, never paths
provided by the caller. No two-machine deployment is claimed yet.

### M14 — Components and module ownership

![Proposed agent and client components with reused IDE modules](images/m14-remote-ide-components.svg)

Arrows are **runtime calls**, not Cargo imports. The client application includes
its local admitted bridge binding before RemoteInspectTask; the remote agent repeats
its own authenticated destination checks. Compile-time direction is
`forge-agent → forge-remote-https → forge-ide::remote`, with agent also importing
IDE and core contracts. The proposed HTTPS IDE feature depends inward on the
optional typed conversation; IDE never imports HTTPS. This avoids a package cycle.
The agent supplies its finite payload/result sums, typed codec bindings and project
policy; the adapter must not import the concrete agent package. Bridge mechanism
is existing, but this combined host/binding is proposed.

### M15 — Typed interface shape

![Existing Task and IoClient with proposed remote inspection port](images/m15-remote-ide-contracts.svg)

Stereotypes distinguish actual traits from proposed names. This is a contract
sketch, not compilable Rust or a new core API. `RemoteIdeClient` fixes IoClient's
request/response/error types; `InspectReply` wraps the existing `ProjectReport`.
The existing node-specific client and destination remain supported. A new typed
agent dispatch/codec seam is required; today's HTTPS server is not already generic.
Provider leases retain their existing GAT constraints, separate from this diagram.

### M16 — Admitted inspection

![Proposed shared-entry inspection sequence](images/m16-remote-ide-request.svg)

The success path is conditional on every preceding gate. Verified TLS identity
is separate from operation and project authorization; project permission is checked
before inspection reads. Startup may already have opened explicitly configured
roots. The report is bounded and checked before client YAML output. A timeout does
not preempt native I/O or the synchronous parser, and metadata/type spellings may
expose sensitive information. Trusted configured projects remain the initial scope.

### M17 — Bootstrap and lifecycle

![Proposed configured agent bootstrap and lifecycle](images/m17-remote-ide-bootstrap.svg)

Registrars stage cohesive factories without resource creation, then consuming
freeze checks declared registration metadata. Explicit construction and policy/route
wiring precede readiness; freeze is not automatic graph execution. The depicted
host-scoped/singleton choice opens roots during assembly. Transients remain lazy
when selected. Shutdown stops admission and closes owned resources; grace exhaustion
must be reported rather than claiming synchronous callbacks can be forcibly canceled.

## Rebuild locally

The canonical renderer for these SVGs is **`mermaid-rs-renderer` 0.3.1 (`mmdr`)**, a native Mermaid parser/layout renderer supporting class, sequence and flowchart diagrams. It is not the official browser renderer and does not promise pixel parity with it. We use a deliberately small syntax subset and separately validate it with official **Mermaid 12.2.0**. No repository contents are sent to a rendering service. The official CLI was not usable in the restricted authoring host; no browser isolation was disabled. [Renderer documentation](https://docs.rs/mermaid-rs-renderer/0.3.1/mermaid_rs_renderer/), [Mermaid class syntax](https://mermaid.js.org/syntax/classDiagram.html), [sequence syntax](https://mermaid.js.org/syntax/sequenceDiagram.html).

From the repository root, with Rust/Cargo, Python3, Node24 (24.15 or newer) and DejaVu Sans available:

```bash
D=docs/architecture/diagrams/mermaid
# Documentation tools only; neither modifies the application Cargo workspace.
cargo install mermaid-rs-renderer --version 0.3.1 --locked --root "$D/.tools"
npm --prefix "$D" ci --ignore-scripts --no-audit --no-fund
python3 "$D/render.py" --renderer "$D/.tools/bin/mmdr" --png-dir "$D/previews"
python3 "$D/render.py" --renderer "$D/.tools/bin/mmdr" --check
```

`render.py` first runs the official grammar validator, then generates all ten SVGs in a temporary directory. It rejects active/external SVG references before writing the images and [hash manifest](render-manifest.json). `--check` independently rerenders and compares exact bytes and input hashes without changing tracked files. Fonts/platform changes can change layout: review the difference, do not silently bless it. PNGs are local previews; scalable SVGs are committed. Keep sources, configuration, manifest and images in the same change. **Inspect every render** for lost labels, clipping, ordering and semantics; parser success and matching hashes alone cannot prove visual correctness. Do not hand-edit generated SVGs.

## Implemented Forge IDE

![Implemented rooted project inspection and typed syntax report](images/m11-ide-inspection.svg)

[Mermaid source](m11-ide-inspection.mmd). Startup validates config, stages/freezes
the public factory, then explicitly opens the authorized root and constructs the
task/provider. Runtime header routing, local admission and variant checks precede
walking/reading. Help is admitted but selects no root/task. Task prepares typed
filesystem requests, the native client executes them, and the pure syn parser
exports declaration/member hierarchy, not expression bodies or semantic types.
The typed report becomes YAML only at the optional output boundary. Dashed edges
are ownership/result annotations, not additional entry points; no callbacks bypass
the bridge. The local policy flag is not authentication. Filesystem handles prevent
symlink traversal, not all snapshot/mount races, and parser budgets are not a
hostile-source execution sandbox. See [actual contracts and limits](../../../../src/rust/forge-ide/README.md).

## M12 — Implemented authenticated node-info boundary

![Implemented typed node-info over mutual-TLS HTTPS](images/m12-remote-node-info.svg)

[Mermaid source](m12-remote-node-info.mmd). The upper panel is explicit host
assembly through public registrars, consuming freeze, factories and chosen providers.
The lower panel is one successful typed call through `/forge/v1/dispatch`, not
per-task endpoints or alternate bypass paths. Bounded uniquely occurring HTTP
headers project routing/target/version/deadline metadata; the JSON body contains
only the typed payload and cannot override it. Only POST is currently supported;
other verbs fail with 405/Allow before task execution. Standard
TLS verifies certificate identity; the exact configured peer mapping is distinct
from the destination's operation policy. The same existing bridge performs header
lookup/admission/variant checks before injected observation. JSON and the private
runtime remain in the optional HTTPS adapter. One server owner services one
connection/request at a time; trusted synchronous callbacks are not preempted.
The client validates original metadata and target/incarnation. Unknown outcomes
are not retries, cancellation or durable acceptance. The demo is loopback-only
with ephemeral in-memory credentials, not a deployed daemon or an implementation
of all M08 delegation/resource-policy sketches. [Actual API and limits](../../remote-node-info.md).
