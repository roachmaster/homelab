# Foundation interfaces: jobs, access and remote calls

**Broader proposal; a narrow local jobs subset is implemented.** The
[bounded counter guide](../../src/rust/forge-local/JOBS.md) covers actual explicit
stepping, volatile history and trusted-local owner checks. Generic jobs, production
identity and remote interfaces below remain proposals.

**Original design baseline.** Reviewed against public
homelab `67b80a8e6f16f1d81995596dfb7865e0f986a399` on 2026-10-31. Names and Rust
signatures below are candidate contracts, not existing exports or a stability
promise. Preserve the invariants; earn API stability through implementation,
consumer tests and failure tests before freezing exact signatures.

This refines the [kernel plan](forge-kernel-plan.md), not a replacement for it.
[Interface sketches](foundation-contracts.md) · [UML-like gallery](diagrams/mermaid/README.md#proposed-foundation-interfaces)

## 1. What we keep, and what is actually missing

| Existing boundary | Keep | Do not assume |
| --- | --- | --- |
| `Task<Input, Output, Error>::run` | Pure computation or orchestration of injected clients | Scheduler, cancellation, `Send`, async or command metadata |
| `DescribeCommand` | Independent per-operation names/help/semantic arguments | Rust reflection, permission or automatic decoder |
| `IoClient::execute` | Executes a prepared typed request; adapter owns boundary encoding | Retry, durability or a nonblocking call |
| `Envelope<P>` / `Header` | Capability-owned typed payload; route/version and request/correlation IDs | Target node, identity, deadline or deduplication |
| `Bridge::dispatch` | Header lookup → admission → variant match → handler | Thread-safe erased handlers, header passed to handler, resource authorization |
| `Factory<T>` / `InstanceProvider<T>` | Construction versus scoped acquisition; explicit owner | Universal `dyn` provider, pool, async cleanup |
| Bootstrap / local application | Typed registrar groups, conflict preservation, freeze before creation | Managed lifecycle, automatic graph execution or production authentication |

These are observed [current APIs](../../src/rust/README.md), not proposed additions.
The current local policy permits this process's calls; a configured node name is
not authenticated identity. Existing disk paths are not sandboxed, and listing
materializes an unbounded directory. Do not expose disk remotely in the first
remote milestone.

## 2. Coherent internal modules, not a class-per-crate tree

Use these **logical units first**. Initial jobs/access orchestration may live in
private `forge-local` modules. Extract candidate `forge-jobs`, `forge-access` and
`forge-remote` packages when independent consumers or an external adapter make the
boundary useful. No empty crates are to be created by this design. Contract-only
packages need no ceremonial registrar.

| Unit and candidate home | Internal modules / visibility | Imports and forbidden knowledge |
| --- | --- | --- |
| Access, `forge-access` candidate | Public `identity` facts/context and `policy` queries; private context issuance; `audit` bounded event contract | May use ingress `RouteKey` for action identity. No jobs, remote transport, database, kernel or credential SDK import |
| Jobs, `forge-local::jobs` initially; `forge-jobs` candidate | Public `contract`, `program`, `service`; private `engine`, `admission`, `retention`, `intent`; `assembly` owns config/recipe/registrar if packaged | Access's narrow identity/policy types, optional command descriptions; bootstrap/provider in assembly. No terminal, network, concrete disk client, global business enum or mandatory Task dependency |
| Remote boundary, `forge-remote` candidate | `address`, `call`, `outcome`; small `peer` resolution contract | Existing ingress and I/O-client contracts. Target-node identity is distinct from operation routing. No jobs policy, concrete protocol or credential provider |
| Selected network/credential implementation, name chosen later | Private framing/codec/connection/verification modules; public typed config and cohesive registrar | Imports the contracts it implements plus its chosen libraries. Never imported by the existing core or by jobs |
| Application, existing `forge-local` or a separately justified host | `composition`, finite `commands`, private `verified_ingress`, `job_pump`, capability-owned codecs | Owns selected implementations, result sum and route bindings. Keeps dependency wiring separate from job/business policy |
| Native systems, existing `forge-kernel` | Related `disk`/`node` modules; later low-level clients only when needed | Remains hosted native I/O. Jobs, credential verification and transport do not become kernel catch-all modules |

The proposed normal-import direction is application → capabilities/contracts and
selected adapters → their consumed contracts. Jobs may depend on access; access
must not depend on jobs. Remote addresses contain identifiers, not an access
policy. The [module diagram](diagrams/mermaid/images/m05-foundation-modules.svg)
shows these **proposed** edges, separately from the actual package DAG.

### Type ownership

- **Jobs:** `JobId`, `IncarnationId`, `JobState`, `JobSnapshot`, `CancelAck`,
  `Submit<I>`, `IntentKey`, bounded acceptance/lookup errors and `JobLimits`.
- **Job capability/application:** finite input `I`, result `R`, failure `E`, their
  bounded representation and versioned semantic intent comparison. No framework
  enum of all possible jobs. First example: a bounded pure counter, not an OS process.
- **Access:** issuer-qualified `PrincipalId`, verified identity facts, policy
  query/decision and sanitized audit event. Jobs derive ownership from this
  trusted context, never from a submitted `owner` string.
- **Remote:** configured `NodeId`, expected target incarnation where meaningful,
  addressed typed call, bounded delivery outcomes. Credential evidence and wire
  DTOs belong to the selected adapter, not the business payload.
- **Application:** `HostContext` combines verified principal, immutable call
  metadata and ready capability handles; a finite request/result sum binds the
  selected capabilities. Context is per call, not mutable global “current user”.

## 3. Jobs: admission, stepping and bounded records

A job is an owned execution record around a program, not a `Task` replacement.
The first engine is **volatile, single-owner and cooperative**. A separate
`JobProgram` prepares a bounded `JobStepper`; it does not change Task's input or
force every task to accept cancellation tokens. A known-short pure Task may be
adapted to one step, but a blocking Task cannot be called a responsive job merely
by wrapping it.

### Internal responsibilities

- `contract`: typed submit/inspect/cancel requests and owner-safe views; operation
  descriptors stay with this capability.
- `admission`: authorize, validate kind/version/input/limits, compare intent and
  reserve capacity; no program execution here.
- `engine`: owns queue, active program states, revisions and transition checks.
  The host decides when to call its pump. No background progress is implicit.
- `program`: capability-owned preparation and bounded steps; injected ports only.
  First program performs no I/O. Preparation creates state but performs no work
  or external effects; failures release reservations without an acceptance receipt.
- `intent` and `retention`: one transaction boundary with admission, not unrelated
  cache writes. First implementation can use ordinary bounded in-memory maps.
  No persistence trait is necessary until an actual durable adapter is designed.

### Acceptance is one invariant

Within one owner-thread operation: authorize → validate → find prior intent →
reserve queue/record/intent budgets → prepare state → commit queue + record +
intent receipt together. Only then return `AcceptedVolatile`. A conflict or failed
preparation returns no receipt and releases all reserved capacity. A returned
receipt names the server node, application incarnation and job; it is **not** a
claim of durable acceptance or completed effects.

First limits must cover queued jobs, active jobs, per-owner outstanding jobs,
program input/state/result size, diagnostics, terminal history and intent entries.
Reject before overcommitting capacity. First program uses intrinsically bounded
values; arbitrary `Vec` results plus an after-the-fact length check cannot promise
a hard peak-memory bound. These are trusted-code application quotas, not OS
isolation or preemption. Input decoding and reply encoding also need size limits.

### State and cancellation rules

| Current state | Event | Next state / response |
| --- | --- | --- |
| Absent | Rejected submit | Absent; no job ID/acceptance |
| Absent | Accepted submit | Queued with revision 1 |
| Queued | Owner-authorized cancel | Cancelled; program never started |
| Queued | Queue deadline elapsed | Expired; program never started |
| Queued | Permission recheck denies, or policy is unavailable | Failed before any step, with bounded PermissionDenied or PolicyUnavailable reason; no automatic retry |
| Queued | Admitted start | Running; recheck current execution permission |
| Running | Cancel request / runtime budget elapsed | Running with `cancel_requested` and reason; acknowledgment is Requested, not Cancelled |
| Running | Step yields | Running; bounded progress/revision update |
| Running | Step completes / fails | Succeeded / Failed, even if a cancel request raced with completion |
| Running | Step confirms stopped | Cancelled; no active step remains |
| Any terminal state | Repeated cancel | AlreadyTerminal; never run again |

`Expired` is specifically **never-started queue expiry**. A running timeout asks
for cooperative stop; it cannot truthfully manufacture that terminal state.
Completion wins if already committed before cancellation; a completed step can
also win after cancellation was requested. No transitions out of a terminal
state. Revision exhaustion fails closed rather than wrapping. Eviction is a
retention event, not a new execution state.

A deadline is checked before starting work and at checkpoints, not a hard-real-time
interrupt. A step that overruns its budget can block progress; that is a contract
violation, not proof it was forcibly stopped. The job runtime budget is separate
from the caller's wait deadline: an accepted job does not disappear just because
the submitter stops waiting. gRPC's documentation likewise distinguishes a cancel
signal from the handler's responsibility to stop; it does not supply arbitrary
handler interruption. [Primary cancellation guidance](https://grpc.io/docs/guides/cancellation/)

### Bridge compatibility and the owner loop

The existing bridge stores `Fn(P, &C)` handlers; it cannot borrow an engine as an
unrestricted shared `&mut` service. A proposed local `JobsHandle` can hold
`Rc<RefCell<Engine>>`, using **short `try_borrow`/`try_borrow_mut` scopes** for queue
operations and returning `Busy` on conflicting access rather than panicking.
Do not hold a borrow while calling injected program/policy/client code.

The pump takes a work token/state out under a short borrow, drops the borrow,
executes one step, then commits its result under a new borrow with job/revision
validation. The in-flight token keeps state/resources owned until the step returns.
Preparation similarly reserves, drops the borrow, prepares, then commits or
releases. Reentrant dispatch from a program is unsupported; no callbacks into the
same application while stepping. This is a concrete first-host design, **not** a
promise that `Rc<RefCell<_>>` is thread-safe or suitable for async jobs.

The current terminal blocks on `ReadLine`. Merely adding an engine will not make
jobs progress while that read blocks. First prove a scripted in-process pump;
interactive progress needs a separately specified input/event pump. A later
bounded reader-to-owner channel may keep the bridge on one thread, but payload
`Send` bounds, reader shutdown and console ownership must be tested before adding
threads. No bridge sharing, `Arc` retrofit or async runtime is selected here.

### Intent and history

Use `(verified issuer/owner, server incarnation, operation + version, IntentKey)`
as the submission scope. Store the bounded **semantic intent** with the receipt:
job input and job-execution limits, excluding attempt request/correlation IDs and
caller wait timeout. Capability-owned equality is sufficient for the first small
input; don't use Rust's ordinary `Hash` as a durable canonical fingerprint.
A later digest requires a versioned canonical representation and collision policy.

- Same scope/key + same intent → original receipt, no new preparation/execution.
- Same scope/key + different intent → `IntentConflict`, never replacement.
- Active-job entries do not expire while work remains active. Reserve their space.
- Retain terminal intent tombstones for a declared bounded window. If result
  history expires first, return the original receipt plus `ResultExpired`; do not
  recreate the job. Never evict unexpired intent entries merely to admit more work.
- Once both retained history/tombstone are gone, arbitrary lookup is `NotKnown`,
  not a confident “never existed” or `Expired`. A key reused after its advertised
  protection window may be a new submission. Clients must not assume otherwise.
- Restart changes incarnation and loses volatile records. Stale job references
  return incarnation mismatch, not a new job with a recycled ID. An uncertain
  remote submit is never automatically retried against a new incarnation.

This is our bounded adaptation of caller-token and changed-intent handling,
not universal exactly-once execution. Durable acceptance later requires an atomic
commit of intent, receipt and job state; external effects still need their own
idempotency/recovery contract. [AWS primary idempotency guidance](https://aws.amazon.com/builders-library/making-retries-safe-with-idempotent-APIs/)

## 4. Identity, authorization and visible metadata

**Authentication** establishes facts about an actor; **authorization** decides
whether those facts permit this operation on this resource. An ingress header,
configured node label or a `PrincipalId` constructor is not authentication.
The first production-capable policy is default-deny, checks every call/resource,
and separately controls discovery visibility. This follows the primary maintainer
recommendations, not a claim that current local admission implements them.
[OWASP authorization guidance](https://cheatsheetseries.owasp.org/cheatsheets/Authorization_Cheat_Sheet.html)

Proposed issuance path: selected verifier consumes protocol-specific evidence →
returns verified facts → an authenticated-boundary component **inside the access unit** creates the
principal/context through its private constructors. `VerifiedPrincipal` has no public “username means verified” or
wire-deserialization constructor. For an external verifier implementation, the
bootstrap-owned `AuthenticatedBoundary<V>` from that access unit converts successful verified facts; the verifier itself
is trusted injected code. Private constructors prevent accidental misuse, not a
sandbox against malicious code compiled into the host.

Do not pass a principal supplied by a remote request through unchanged. Verify
peer identity **and** any delegated original actor, including audience, scope and
expiry; a service peer is not automatically its claimed human user. First remote
read-only access can use one explicitly configured service principal per peer;
end-user delegation stays unsupported until its verification contract exists.
Local trusted-process mode must be explicit and labeled; never silently upgrade
it to remote authentication or treat `--node-id` as a caller identity.

Apply checks at two levels:

1. Bridge admission checks verified context, operation/version permission and call
   budget. A destination still authorizes calls even when the sender authorized
   them. The existing bridge's route-lookup-before-policy ordering remains intact.
2. Capability service checks actual resource ownership/scopes before effects or
   disclosure: submit owner comes from context; inspect/cancel/result use the
   stored owner, not a body claim. Recheck execution permission before starting a
   queued job and at relevant later resource-effect checkpoints. Revocation/cache
   freshness during long execution is an explicit policy requirement, not solved
   by a submission-time check.

`system.commands` remains an admitted operation, but a secured host must filter
its registered catalog for the caller's discoverable operations. Help cannot use
an unfiltered static list or imply permission to execute. Retain exact descriptor
identity; visible subsets are policy projections, not a second route registry.
Unknown/hidden job IDs can share `NotVisible` externally to reduce enumeration;
internal audit distinguishes causes without promising constant-time behavior.
The current local application's all-visible catalog remains a documented local
mode, not a backwards-compatible remote security policy.

Audit bounded IDs, action, decision, policy version and outcome category. Never
log bearer credentials, raw authentication evidence, secret payloads or arbitrary
exception strings. Keep diagnostics bounded and redacted. Authentication and
policy outages deny or return explicit unavailable errors, never fallback to
anonymous success. Audit sink backpressure needs an explicit policy: required
audit rejects new effectful admission if it cannot record; optional diagnostics
may drop with counters. Neither is a transaction with external business effects.

## 5. Remote calls without a new internal JSON bus

`AddressedCall<P>` wraps the existing `Envelope<P>` plus target and call options;
**do not silently add fields to Header or put the target in its operation name**.
A node resolver maps configured logical nodes to allowlisted endpoints and
expected peer identity. Callers cannot turn a header into an arbitrary URL,
filesystem root, provider key or executable. Target-node and expected-incarnation
checks happen at the receiver before dispatch; authenticate the intended peer at
the sender as well.

Keep the local payload/result typed. The application owns finite selected sums;
its versioned edge codec maps bounded frames to these enums. The chosen protocol
may use JSON, another encoding or no serialized hop locally; no `Value`, `Any` or
framework-global business enum enters the bridge. An outbound adapter can use
existing `IoClient` with all associated request/response/error types fixed; a
second universal client trait is unnecessary.

### Request identity and time

The caller supplies a request ID; it is preserved into the existing Header and
validated against the reply. Correlation identifies a logical trace. An optional
`IntentKey` is a separate, capability-supported idempotency contract, not another
name for request ID. Retries preserve intent and correlation, remain on the same
authorized target/incarnation, and must not silently reset their budget. Exact
per-attempt request-ID convention is negotiated by the future protocol; first
proof uses one ID and no automatic retries.

A local monotonic deadline governs the caller's waiting. The adapter sends the
**remaining duration**, after time already spent, and the receiver clamps it to
its configured maximum. Every local hop deducts elapsed time; a forwarded call
never receives a fresh original timeout. `Instant` is local timing state, not a
portable cross-node timestamp. Relative budgets avoid requiring identical clocks,
but do not by themselves measure unknown transit time or guarantee a synchronized
global cutoff; caller timeout still bounds its own wait. A hard cross-node absolute
expiry needs a separate clock-skew policy or protocol guarantee before being
promised. [Rust Instant](https://doc.rust-lang.org/std/time/struct.Instant.html),
[primary deadline propagation guidance](https://grpc.io/docs/guides/deadlines/)

### Destination flow and outcomes

Bound frame/header size → verify channel/evidence → verify target/audience →
select a registered versioned codec → decode typed payload without effects →
construct per-call trusted context → same bridge admission/variant gate →
resource authorization → handler. Pure decoding can precede final resource
checks, not I/O. Replies are bounded and matched to target/incarnation, request,
correlation and contract identity before the caller accepts them.

Separate these outcomes:

- **NotSent:** locally rejected/expired/unconfigured; only claim this when the
  adapter can prove no request was transmitted for execution.
- **Rejected:** an authenticated matching peer explicitly reports rejection before business acceptance/effects. A deadline
  error after work began is not this category.
- **Completed:** authenticated matching response with typed success or typed
  handler failure; failure may follow effects, not rollback.
- **OutcomeUnknown:** timeout, lost connection after possible delivery, malformed
  or mismatched reply after send, or remote restart with no authoritative receipt.
  It is not “failed without effects”. Never automatically fail over/resubmit work.

First remote proof exposes node reads only; a timeout yields unavailable/unknown
observation, not proof the node died. Later remote job submission needs server
idempotency/retention and receipt lookup **before** retries are enabled. A network
cancel signal stops waiting or requests cancellation; it is not a force-kill and
is not equivalent to owner-authorized `jobs.cancel`.

## 6. Assembly, lifecycle and failure ownership

Bootstrap selects optional capabilities and typed config; environment reading is
one boundary step. A proposed `JobsConfig::Volatile` carries validated quotas,
queue/runtime limits and retention policy. `AccessConfig` selects an explicit
local mode or configured verifier/policy; `RemoteConfig` selects configured peers,
limits and adapter settings. No mandatory HTTP, Kafka, discovery daemon or async
runtime follows from these enums.

Concrete packages expose cohesive registrars: configured factories are staged,
not run. Validate duplicate/default replacement decisions, required bindings and
explicit order before consuming freeze. Then construct verifier/policy → bounded
job program/engine → selected peer clients/codec → host context/bindings. Reject
incompatible quotas, unsupported trust modes or missing peer identity settings;
no partly published host and no fallback implementation after failure.

Keep factory creation distinct from provider scope: one host owns its engine,
policy and peer table; a transient factory remains lazy until acquisition. A
long-lived program must own a ready client or deliberately acquire a narrow lease
per step. It cannot retain a borrow from a temporary pooled lease. Do not erase
`InstanceProvider<T>::Lease<'a>` into a pretend universal dyn container. Async
resources later need explicit async close/reset contracts; ordinary Drop cannot
await them.

On shutdown: reject new submissions; continue authorized inspect/cancel while
controls drain; follow the selected finish-or-request-cancel policy; release
clients after active users stop. Stop new acquisitions only according to that
policy—otherwise already-admitted work may receive `Closed` mid-flight. A grace
expiry reports incomplete shutdown; no successful forced stop or bounded join is
claimed. Trusted step/program panics invalidate the first host and follow a fail-stop
process policy, not a fabricated Failed/Cancelled receipt. Same-host diagnostics
are best effort during unwind/abort; only an external observer or supervisor, if
configured, can reliably observe process termination. No supervisor exists merely
because this document names one. A later unwind boundary must specify unwind
safety and state recovery before continuing admission.

## 7. What is worth stabilizing, and what is not yet settled

| Preserve as an invariant | Keep provisional until evidence exists |
| --- | --- |
| Header selects route; typed variant gate before business effects | Address wrapper names, codec/wire schema and protocol choice |
| Task, command description, I/O and provisioning are independent | Exact trait methods, error fields and crate extraction dates |
| Verified context is separate from untrusted request metadata | Credential mechanism, identity delegation and revocation freshness |
| Every resource operation is authorized at its destination | Concrete policy language/store and audit delivery strategy |
| Cancel requested is not confirmed stopped | Worker count, input pump, async runtime and process isolation |
| Admission, intent receipt and job state commit consistently | Persistence format, storage adapter and restart recovery |
| Bounded admission/retention; unknown outcomes remain unknown | Actual quotas/retention durations; fairness/load-shedding parameters |
| Constructors/owners and dependencies are explicit | Pooling, async factory interfaces and lifecycle framework |

No SemVer stability pledge is made for unimplemented interfaces. First implement
small slices with private internals; promote only the ports a second consumer
actually needs. Test invariants rather than freezing accidental implementation
layout.

## 8. Ordered proofs before calling the foundation stable

1. **Local jobs:** one bounded pure program, scripted owner pump, volatile records;
   submit/inspect/cancel, queue-full, intent conflict/replay, no effects on denied
   calls, completion race, expiry/eviction/restart, over-budget/uncooperative step
   permission-revoked/policy-unavailable before-start failures and startup/shutdown
   failure tests. Existing Task consumers still compile.
2. **Access boundary:** independent fake verifiers/policies; forged body owner and
   header principal rejected; local/remote entry parity; hidden help/catalog
   filtering; owner and delegated-actor tests; policy/audit outage behavior.
   Production remote access remains disabled until real verifier conformance.
3. **Remote node reads:** one selected transport between explicitly configured
   peers, real identity verification, bounded codecs, mismatched target/version/
   reply rejection, budget consumption and uncertain outcomes. Keep disk/jobs
   remotely inaccessible. A fake channel test is not network/security proof.
4. **Later remote jobs/durability:** implement atomic persistence/recovery only if
   needed; prove duplicate submission after lost reply and across restart, retained
   receipt lookup, permission changes, late retries and downstream partial effects.
   No exactly-once or transparent-location claim.

The [contract sketches](foundation-contracts.md) include the explicit migration cost of extending today's public exhaustive application enums. Keeping core traits unchanged does not automatically make such an application change source-compatible. They make the proposed seams concrete;
they deliberately do not pretend a new implementation or finished security model.
