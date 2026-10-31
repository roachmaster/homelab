# Forge hosted kernel research and implementation plan

**Recommendation:** keep Forge as a small, hosted application kernel on top of the
existing OS. Add **local node information and truthful status first**, then a
bounded job lifecycle, then one explicitly selected remote boundary. Reuse the
six core crates and optional `forge-kernel` clients; do not start a new kernel,
distributed scheduler or shell platform.

**Research baseline:** checked against homelab
`1267205e1ee0af849bdefd347167836dab82bd28` on 2026-10-31. The local node/disk/terminal checkpoint described below is now implemented. Job and remote command names remain illustrative contracts, not exported APIs. The research plan still selects no remote transport, async runtime, deployment target or device.

[Source ledger](forge-kernel-sources.md) · [Plan diagram](diagrams/07-hosted-kernel-plan.svg) ·
[Existing bootstrap](bootstrap-registrars.md) · [Current disk client](../../src/rust/forge-kernel/README.md)

The [Pi Cluster Control review](../reviews/forge/pi-cluster-control.md) supplies historical node/coordinator use cases for this sequence. Its cluster behavior remains specification, not an implemented control plane; it adds no mandatory crate, transport or provisioning dependency.

## What kernel means here

An OS kernel manages execution, protected memory, I/O resources and the system-call
boundary. A shell is a program using that boundary, not the kernel itself.
That is the useful distinction in MIT's teaching OS: Forge can offer a consistent
command interface without implementing those privileged mechanisms or wrapping
a shell. [MIT xv6 operating system interfaces](https://mit-pdos.github.io/xv6-riscv-book/unix.html)

| Basic responsibility | Host OS remains responsible for | Forge's proposed hosted counterpart |
| --- | --- | --- |
| Execution | Processes, threads and CPU scheduling | Registered tasks and inspectable application jobs |
| Memory and resource isolation | Address spaces and enforced resource controls | Bounded queues/results and honest resource observations |
| Files and devices | Filesystems, drivers and hardware access | Optional native typed clients; disk already exists |
| Communication | Local IPC and network mechanisms | Optional adapters and explicit remote outcomes |
| Protection | OS identities and access enforcement | Authenticated callers, per-operation/resource authorization |

This mapping is **our design analogy**, not an assertion that an application job
is a process or that a crate provides memory isolation. Linux cgroups, for
example, organize processes and distribute resources; an in-process job counter
does not replace those controls. [Linux cgroup v2 documentation](https://docs.kernel.org/admin-guide/cgroup-v2.html)

## Existing foundation and concrete gaps

| Existing code | Reuse | Missing capability, not an implicit promise |
| --- | --- | --- |
| [Ingress contract](../../src/rust/forge-ingress-contract/src/lib.rs) | Typed `Envelope<P>`/response; route/version, request ID and correlation | No node identity, deadline, cancellation or idempotency enforcement |
| [Bridge](../../src/rust/forge-bridge/src/lib.rs) | Header-selected registration → admission → variant check → handler | Synchronous closures lack `Send`/`Sync`; handlers receive payload/context, not the header |
| [Bootstrap](../../src/rust/forge-bootstrap/src/lib.rs) | Atomic typed groups, dependency-order validation, consuming freeze | No automatic graph execution, route catalog or managed host lifecycle |
| [Provider](../../src/rust/forge-provider/src/lib.rs) | Explicit factories, singleton/transient ownership and leases | No pools, request scopes, async construction or service locator |
| [Task](../../src/rust/forge-task/src/lib.rs) and [IoClient](../../src/rust/forge-io-client/src/lib.rs) | Task prepares/computes/orchestrates; client executes typed prepared requests | `run`/`execute` may block; neither supplies a scheduler or cancellation |
| [Kernel disk module](../../src/rust/forge-kernel/src/disk.rs) | Native list/pwd/cd/permissions; config/factory/registrar | No sandbox, file mutation, bounded listing, remote codec or per-user session manager |

Current executed checks, including the new local application/terminal tests, are recorded in the [Rust workspace guide](../../src/rust/README.md). The earlier research-only pass did not execute source tests.
The demo's caller-controlled admission flag is **not authentication**.

## Minimal command surface

| When | Operations | Meaning |
| --- | --- | --- |
| Already implemented | `DiskRequest::{Pwd,List,ChangeDirectory,Permissions}` | Native disk client; current demo routes use `demo.disk.*` |
| Implemented local slice | `node.info`, `node.status`, `system.commands` | Configured node label/target, client-instance uptime/responding, actual registered commands |
| After lifecycle design | `jobs.submit`, `jobs.inspect`, `jobs.cancel` | Manage one allowlisted application job kind; not arbitrary program execution |
| After a remote boundary is chosen | `nodes.list` plus remote node reads | Configured peers with last observation and explicit reachability state |
| Later if a consumer needs them | Bounded `jobs.list`, `resources.inspect`, structured recent events | Query limits and retention first; no unbounded monitoring platform |

The implemented terminal help is a presentation of the admitted registered catalog. Keep any richer generic home/menu as a future presentation over these operations. It must
not become a requirement, a second router or a source of permissions. Avoid
`exec`, kill-arbitrary-PID, mount, reboot, package installation, file deletion,
network reconfiguration and device management in these initial phases.

## Phase 1 Local node information and status

**Implemented narrow checkpoint:** [`forge-kernel::node`](../../src/rust/forge-kernel/README.md), [`forge-local`](../../src/rust/forge-local/README.md) and [`forge-terminal`](../../src/rust/forge-terminal/README.md) now provide the local runnable system. The terminal selects typed startup config, obtains admitted command discovery and invokes node/disk requests through the shared bridge. Node status means the observation client responded and reports its own uptime—not whole-host readiness. Actual selected route/version descriptors are returned by `system.commands`. Disk remains optional at Cargo feature and startup-selection boundaries. Generic readiness, resource telemetry and a managed lifecycle remain deferred.

**Original phase direction (broader than this checkpoint):** add one coherent node module to the consumer,
with two typed request variants and two bridge bindings. No new core crate is
needed just for two operations. The application selects its packages and supplies
typed `NodeConfig`; bootstrap calls registrars, freezes, constructs providers and
publishes the assembled node. Invalid config or construction returns no ready host.

`node.info` should return a configured logical node ID, build/contract version,
compile-target OS/architecture, application uptime and the selected capability
versions. Application uptime is not machine boot uptime; a compile target is not
a hardware probe. An ID is a label, not authenticated identity. Capabilities come
from explicit composition-owned descriptors, with tests matching actual bindings;
the existing bootstrap registry cannot discover a route catalog automatically.

`node.status` should report accepting/not-accepting state and actual selected
provider observations where available. Separate liveness from readiness and mark
unavailable/stale observations rather than substituting zero or unconditional
healthy. Do not run a slow filesystem probe on every status request or advertise
CPU/memory measurements that no collector implements.

**Exit gate:** success, denied admission, wrong variant and unknown version;
invalid/duplicate config; a failed injected provider is visible; node state is
isolated across assemblies; a fake observation provider substitutes cleanly; the
pure consumer still excludes optional clients. Keep calls local and read-only.

## Phase 2 One bounded application job lifecycle

A **task** is code; a **job** is one execution record with an owner, input,
state and outcome. Neither is an OS process. Start with one short, pure,
checkpointable computation—not arbitrary executable paths or blocking disk work.
Use an application/capability-owned job module; extract a separate reusable
`forge-jobs` package only when the boundary has a real second consumer. Do not put
workflow policy into `forge-kernel`, the bridge or a global business enum.

Proposed state model: accepted/queued → running → succeeded or failed. A cancel
request records intent; it becomes **cancelled only after execution has stopped**.
Completion may win a cancellation race. Define repeated cancel, already-terminal,
unknown ID, expired history and wrong-owner outcomes. Admission failure creates no
job; a receipt means only the explicitly stated acceptance level.

For a small first executor, prefer bounded cooperative steps on one host-owned
loop. Process control requests between steps; each step returns typed progress/next-state
or a terminal result rather than holding `run` open for the whole job. A task can receive a narrow
execution-control handle through its typed input or constructor; existing
`Task::run` does not magically become cancellable. A monolithic blocking call can
prevent that loop servicing inspect/cancel, so exclude it from this first proof.
If dedicated workers or an async runtime become necessary, make a separate
ownership/`Send`/`Sync` decision; do not move/share today's erased bridge across
threads unchanged or hold its admission path while waiting for long work.

RPC cancellation itself illustrates the distinction: gRPC documents that handlers
must cooperate in stopping computation; a notification is not arbitrary code
interruption. This supports the cancellation model, **not a gRPC selection**.
[gRPC cancellation guide](https://grpc.io/docs/guides/cancellation/)

Even local submission needs an explicit owner-scoped key plus typed intent,
bounded record retention and changed-intent rejection to suppress duplicates
within that declared window. Existing header request IDs do not enforce this.

Start with volatile, bounded records and say so: restart loses them. Include an
application incarnation in job ownership/IDs so stale references cannot silently
name a different run. Persistent recovery is a separate gate before promising
durable acceptance. Progress checkpoints are not durable restart checkpoints
unless their state/version is explicitly stored and tested.

**Exit gate:** fixed queue/concurrency/history/result limits; full queue rejects;
inspect stays responsive; cancellation before start and at a checkpoint; completion
race; task failure; no second execution from a duplicate accepted submission;
wrong-owner rejection; restart/expired-record semantics; shutdown stops admission
and follows a documented finish-or-cancel policy. If work does not cooperate within
a grace interval, report incomplete shutdown—never claim a bounded successful
join or release resources still in use. Forced process termination is a separate,
privileged process-isolation design, not a `Task` method.

## Phase 3 One explicit remote boundary

Begin with two explicitly configured peers and **node reads only**. No broker,
automatic LAN discovery, leader election or cluster-wide scheduler is required.
Choose a transport only after defining caller identity, deployment and streaming
needs. A shared interface should preserve useful operation vocabulary, not hide
latency or partial failure. That distinction is central to Waldo and colleagues'
distributed-computing argument. [Author-hosted research paper](https://waldo.scholars.harvard.edu/publications/note-distributed-computing)

A selected remote client takes a prepared typed operation; its edge codec encodes
it. The receiving thin adapter verifies the channel/caller, bounds/decodes input
into the capability enum, then calls the **same local bridge**. Header route/version
still chooses the registered handler; payload mismatch is rejected before effects.
No JSON/`Any` internal fallback, framework-wide command enum or direct native-client
bypass is introduced. A heterogeneous host may own a finite sum of its selected
capabilities, including explicit result unification.

Node selection belongs to the trusted configured peer resolver, distinct from
operation routing and DI selection. An arbitrary header must not become a URL,
provider name or executable. The current Header lacks a node field; define any
cross-node addressing envelope and compatibility rules before implementation,
without pretending it exists today. A peer's claimed node ID is not proof of its
identity. Verify the intended destination and authorize the original caller or
an explicitly delegated principal; transport-peer authentication alone does not
establish that caller's rights.

**Remote execution gates, after read-only connectivity works:**

- **Uncertainty:** timeout/disconnection can leave an operation's outcome unknown.
  Inspect the original node/job before resubmitting. A failed reachability probe
  means unreachable/stale, not proven dead. Never fail over an uncertain effectful
  job automatically or report exactly-once execution.
- **Retries:** reuse a caller submission key scoped by authenticated owner,
  operation/version and node/incarnation. Store/compare the corresponding typed
  intent, reject key reuse with changed input, bound retention, and define late
  retries/expiry. A request ID alone provides none of this. Durable deduplication
  and a receipt must commit consistently before a durable promise; downstream
  effects need their own idempotency/recovery contract. This is our proposed
  adaptation of AWS's token/intent approach, not a guarantee of universal
  exactly-once effects. [AWS idempotent API guidance](https://aws.amazon.com/builders-library/making-retries-safe-with-idempotent-APIs/)
- **Time:** distinguish caller wait budget, operation deadline and retention.
  Propagate remaining budget without resetting it on each hop; handle clock skew
  explicitly. Timeout may stop waiting without stopping the work. The current
  header and Task APIs supply no deadline propagation. [gRPC deadline guidance](https://grpc.io/docs/guides/deadlines/)
- **Authority:** default-deny operations and check job/session/resource ownership
  on each request, including inspect/cancel. Revalidate at execution where policy
  can change. Use a reviewed authenticated and encrypted remote channel, not homemade identity or
  headers asserting trust. [OWASP authorization guidance](https://cheatsheetseries.owasp.org/cheatsheets/Authorization_Cheat_Sheet.html)
- **Bounds:** cap decoded bytes, queue/in-flight requests, fan-out, response sizes,
  retries and retained events. Reject excess explicitly. A list response cap after
  an unbounded collection is not a memory bound.

The bridge currently passes only payload/context to handlers. Submission keys and
execution budgets therefore need an explicit typed capability input or a reviewed
immutable execution-context contract; do not assume handlers can read Header or
mutate shared context to smuggle metadata. Preserve the header/variant admission
path while testing that metadata mapping separately.

**Read-only exit gate:** real two-peer loopback tests for verified peer/caller
identity, authorized node reads, version/variant mismatch, oversized messages,
stale peer observations, request/reply loss and timeout. Stop here for the first
remote proof; do not quietly add job execution or disk exposure.

**Later remote-execution exit gate:** only after the additional gates above, test
wrong-owner job access, duplicate/changed-intent submission, lost receipts,
restart/expiry and cancel/complete races. Transport is still undecided; these are
proposed acceptance criteria, not implemented tests or an instruction to install
anything now.

## Keep native access safe and optional

`forge-kernel` may gain a cohesive `system` module when an actual native collector
is needed. Keep its platform-specific code/config/factory/registrar beside `disk`;
do not add empty modules now. Resource readings should identify units, sample age
and unsupported fields. Queue budgets are application controls; hard process
CPU/memory isolation remains host-OS work, not a new Forge allocator or scheduler.

Disk currently allows absolute paths, parents and symlink traversal. Its client
cwd is not a security root. Do not expose its current unbounded listing or shared
singleton session to untrusted remote callers. Decide per-principal session
ownership/expiry and a least-privilege filesystem policy first. A session ID or
path prefix is not authority; canonicalization is not a race-free sandbox.
`readonly`/Unix mode are metadata, not effective-access checks.
[Rust permission semantics](https://doc.rust-lang.org/std/fs/struct.Permissions.html)

A remote path codec must preserve native names or explicitly reject unsupported
representations/platform combinations; it must not silently replace non-UTF8
bytes. The existing client owns `PathBuf`/`OsString`, not a portable wire format.
Future create/delete operations require a separate scope, authorization,
partial-effect and retry design. None are part of this plan's first three gates.

## Package assembly stays familiar

```text
application chooses optional packages + typed startup configuration
  -> bootstrap calls cohesive registrars, without constructing clients
  -> validate/freeze owned typed factory groups
  -> factories construct; providers own session/acquisition lifetimes
  -> inject ready ports into node/job tasks and bind typed operations
  -> publish host; later, attach one selected input/remote adapter
```

Core remains dependency-inward. `forge-kernel` owns native mechanisms; node/job
policy belongs to capability modules, host lifecycle to composition, and remote
encoding to its optional adapter. No mandatory new crate per command or lifecycle
trait, dynamic plugin ABI, universal DI container or global service locator is
needed. Singleton disk scope shares navigation; transient construction resets it.
Freeze validates declared group ordering, not future worker health or route
catalog coverage.

## Decisions to make before each expansion

| Decision | Recommendation now | Revisit before |
| --- | --- | --- |
| Next design/implementation | One bounded pure job lifecycle; preserve current local APIs | Separate Phase 2 authorization and useful job selection |
| First job | Small pure computation with bounded checkpoints, volatile history | Phase 2; user chooses useful job later |
| Executor | Single owner with cooperative steps first | Blocking work or parallelism |
| Discovery | Explicit peer config; show observed freshness | More than a small managed peer set |
| Transport and identity | No product chosen; require authenticated, bounded calls | Any listener or remote operation |
| Disk exposure | Local/trusted only, no file mutations | Remote filesystem access or multi-user use |
| Durability | Label volatile acceptance honestly | Restart-safe promises or effectful remote retries |
| Raspberry Pi | Run the hosted application on a supported OS first | Actual hardware/target selection and tests |

A future Pi build is not obtained by renaming this crate. Raspberry Pi's official
kernel guidance covers model/architecture-specific configurations and a distinct
kernel-image boot/install process. A hosted Rust application, a custom Linux image
and a bare-metal kernel are different projects. Target compatibility must be
built and tested on selected hardware; no cross-compilation, flashing or kernel
replacement is authorized here. [Raspberry Pi kernel documentation](https://www.raspberrypi.com/documentation/computers/linux_kernel.html)

**Current checkpoint:** the narrow local node/discovery/terminal system is implemented with focused tests. Broader readiness and telemetry remain future work. **Next implementation, when separately requested:** design one bounded pure job lifecycle; do not infer authorization for remote services or all policies in this research plan.
