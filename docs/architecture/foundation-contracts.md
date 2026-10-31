# Proposed foundation contract sketches

**Not implemented APIs.** This companion makes the [module design and invariants](foundation-interfaces.md)
concrete. Supporting value types are summarized rather than implemented here;
these fragments are not a new compilable crate, a wire specification or a stability
promise. Exact signatures should change if the first consumer tests expose a
better boundary. No source packages, dependencies or runtime behavior are changed.

## A. Access: verified context, then resource decisions

```rust
// Proposed forge-access contract; E is fixed for a selected verifier.
trait IdentityVerifier {
    type Evidence;
    fn verify(
        &self,
        evidence: &Self::Evidence,
        expectation: &PeerExpectation,
    ) -> Result<VerifiedFacts, AuthenticationError>;
}

trait AccessPolicy {
    fn authorize(
        &self,
        principal: &VerifiedPrincipal,
        query: &AccessQuery<'_>,
    ) -> Result<Permit, Denied>;
}

struct AccessQuery<'a> {
    route: &'a RouteKey,                // existing operation + version type
    mode: AccessMode,                   // Discover or Invoke; not all business actions
    resource: ResourceFacts<'a>,        // trusted owner/ID facts from capability state
}
```

`VerifiedFacts` is the configured verifier's output, not a DTO accepted from an
untrusted caller. `AuthenticatedBoundary<V>` belongs to the same access unit as the private
`VerifiedPrincipal` / `AuthContext` constructors. Bootstrap supplies V and trusted
expectations; its public `authenticate(&Evidence) -> Result<AuthContext, AuthenticationError>`
method invokes V and creates context only on verification success. The application
combines that returned context with call metadata; it does not call a private
constructor across a crate boundary. A
third-party verifier is trusted injected Rust code, not an untrusted plug-in;
returning facts incorrectly is a security defect, not something trait typing can
prevent. `dyn IdentityVerifier<Evidence = SelectedEvidence>` and `dyn AccessPolicy`
are intended to be dyn-compatible: no generic methods or associated type families.

| Value | Proposed meaning and bounds |
| --- | --- |
| `PrincipalId` | Bounded issuer + subject identity, not a local display name |
| `VerifiedPrincipal` | Private owned verified facts: actor, effective subject, bounded granted scopes, audience/expiry where the mechanism requires them; no credentials |
| `PeerExpectation` | Configured target identity/audience, allowed mechanism and verification-time policy; not caller-chosen trust roots |
| `AuthContext` | Private authenticated-boundary output for one call/session; jobs cannot replace its owner from body data |
| `ResourceFacts<'a>` | Capability-supplied resource kind, ID and stored owner; borrowed only for the policy decision |
| `Permit` | Local decision result, not a bearer credential transferable to unrelated requests/resources |
| `AuthenticationError` / `Denied` | Bounded reason categories; evidence/secrets stay out of generic errors and logs |

The host owns `HostContext { auth, call_metadata, ready_ports }`. Its private
verified-ingress function builds immutable `call_metadata` from the **same**
envelope it dispatches, including a Header snapshot, target and local budget.
Its `AdmissionPolicy<HostContext>` compares that snapshot to the received Header,
checks policy/budget, then lets the unchanged bridge check the payload variant.
Handlers can read needed call metadata from their supplied context without hidden
thread-local state or changing the existing `Fn(P, &C)` signature. Never accept a
caller-built HostContext on a public network path.

Resource decisions happen again inside capability services. This is intentionally
not a “permission checked once at the menu” design. The discovery handler filters
successfully registered descriptors with `AccessMode::Discover`; invocation uses
`Invoke` and the capability's actual resource facts. The external adapter may
coarsen unknown/hidden-route errors without changing the bridge's internal error
model. Verified identity alone grants no default access.

## B. Jobs: a program is not an ordinary blocking Task

```rust
// Proposed capability/job contracts. All associated types are fixed at a host.
trait JobProgram {
    type Input;
    type Intent: Eq;
    type Output;
    type Failure;

    fn intent(
        &self,
        input: &Self::Input,
        limits: &ExecutionLimits,
    ) -> Result<Self::Intent, InvalidJob>;

    fn prepare(
        &self,
        input: Self::Input,
    ) -> Result<Box<dyn JobStepper<Output = Self::Output, Failure = Self::Failure>>, PrepareError>;
}

trait JobStepper {
    type Output;
    type Failure;
    fn step(&mut self, context: &StepContext) -> Step<Self::Output, Self::Failure>;
}

enum Step<R, E> {
    Yielded(Progress),
    Succeeded(R),
    Failed(E),
    Cancelled,  // only after this program has actually stopped its work
}

struct StepContext {
    max_work_units: std::num::NonZeroU32,
    cancellation: Option<CancelReason>,
}

trait JobService {
    type Input;
    type ResultView;

    fn submit(&self, auth: &AuthContext, request: Submit<Self::Input>)
        -> Result<Receipt, SubmitError>;
    fn inspect(&self, auth: &AuthContext, id: &JobId)
        -> Result<JobSnapshot, LookupError>;
    fn cancel(&self, auth: &AuthContext, id: &JobId)
        -> Result<CancelAck, LookupError>;
    fn result(&self, auth: &AuthContext, id: &JobId)
        -> Result<Self::ResultView, LookupError>;
}
```

`JobProgram::intent` produces a bounded semantic value including execution limits;
its equality must distinguish changed business intent. `prepare` creates a program
state without running it or performing I/O. Neither method may be invoked while
holding an engine `RefCell` borrow. The lifetime of the returned boxed stepper is
owned by the host (the sketch uses Rust's default `'static` trait-object lifetime);
it cannot capture a temporary borrowed provider lease. An explicitly shorter
host lifetime is possible later but must be modeled, not inferred.

A host fixes all program output/failure types; a genuinely heterogeneous host may
use its **application-owned** result/failure sums. It cannot put unrelated typed
programs into one homogeneous map without this unification. No `Any`, downcast,
JSON-value fallback or framework-wide job enum is implied.

`JobService::ResultView` is an **owned, bounded capability projection**. The first
counter can return a small integer/result record. Implementations must copy or
project under a short borrow and release it before returning; they cannot return
a reference into a dropped `RefCell` guard. No `Clone` bound is added to the
existing Task trait. Returning shared allocations that clients can retain forever
would require separate response-retention accounting before claiming a global
memory bound. `inspect` returns bounded summary/progress, not an unbounded event log.

`dyn JobService<Input = CounterInput, ResultView = CounterResult>` and
`dyn JobStepper<Output = CounterResult, Failure = CounterFailure>` are intended
object-safe seams; a concrete generic engine is equally valid. There is no `Send`
or `Sync` promise. The actual [provider GAT](../../src/rust/forge-provider/src/lib.rs)
is a different contract and is not made dyn-compatible by these sketches.
[Rust's dyn-compatibility rules](https://doc.rust-lang.org/reference/items/traits.html#dyn-compatibility)
explain why generic methods/associated type families cannot simply be hidden behind
an arbitrary trait object.

### Job values and error semantics

| Value | Required meaning |
| --- | --- |
| `Submit<I>` | Typed input, caller IntentKey and requested ExecutionLimits; no caller-selected owner field |
| `ExecutionLimits` | Validated queue TTL, runtime budget and program-work bound, clamped/rejected against server config; not the submit RPC wait timeout |
| `JobId` | Server node + application incarnation + never-reused-in-incarnation sequence; validated bounded fields |
| `Receipt` | JobId + acceptance revision + `Volatile` level + advertised intent-retention policy; no fabricated Durable variant in the first implementation |
| `JobSnapshot` | JobId, revision, state, bounded progress, cancel-request flag/reason and relevant timing observations |
| `CancelAck` | `CancelledBeforeStart`, `Requested { revision }`, or `AlreadyTerminal { state, revision }`; never equate Requested with stopped |
| `SubmitError` | Denied, invalid/unsupported input, expired admission, quiescing, capacity category, intent conflict, preparation failure or unavailable |
| `LookupError` | NotVisible/NotKnown, wrong incarnation, retained receipt but result expired, not terminal, or unavailable; outward disclosure policy may coarsen categories |

Result inspection must not rerun the program. Error/result values must be bounded,
redactable domain data; copying arbitrary panic strings or native exceptions into
remote views is not a contract.

### One-owner integration sketch

```text
submit handler:
  authorize and derive bounded intent (no engine borrow held)
  reserve capacity + key, or return existing receipt (short mutable borrow)
  prepare owned stepper (borrow released; no work/I/O)
  commit queue + record + intent, or release reservation (short mutable borrow)

host pump:
  take one eligible work token + expected revision (short mutable borrow)
  step program outside all engine borrows
  commit yield/terminal result and return/drop program state (new short borrow)
```

An in-flight token reserves its active slot; “take” must not make capacity appear
free. Commits verify job ID/revision and do not overwrite a newer terminal outcome.
A broken commit invariant stops admission and reports host failure; it does not
silently lose state or acknowledge success. The first owner loop processes control
requests between steps; a cancel queued during a step is observed at the next
turn, so completion may win. Preparation/step panics use the documented fail-stop
policy: same-host error reporting is best effort, process observation belongs to
an external observer if one exists, and no recovery is assumed merely because
values have Drop implementations.

### Worked local flow

1. A verified local session submits `jobs.submit @ 1` with `CounterInput`, key K
   and limits. The application wraps it in its typed payload sum and Header.
2. Shared bridge admits the route/context, checks the Submit variant; JobService
   authorizes the owner and commits volatile acceptance atomically. Return J.
3. Pump starts J after permission/deadline recheck, runs a bounded step and yields.
   Denied or unavailable execution policy instead commits Failed with a bounded
   reason before any step; the first engine does not silently retry that check.
4. `jobs.inspect(J)` independently authorizes against J's stored owner and returns
   a copied summary. A different unprivileged owner cannot learn J's result.
5. `jobs.cancel(J)` returns Requested while Running. A later step sees the signal,
   releases its work and returns Cancelled. If it already completed, preserve
   Succeeded and answer AlreadyTerminal instead. Repeated K returns J, not new work.

No ordinary `Task::run` signature changes. The initial program can have no Task
implementation at all; an adapter for a known-bounded Task is a separate choice.

## C. Remote: address wrapper, typed client, explicit uncertainty

```rust
// Proposed values; existing Header/Envelope remain unchanged.
struct AddressedCall<P> {
    target: TargetNode,                 // configured NodeId + optional expected incarnation
    request: Envelope<P>,              // caller-provided request/correlation IDs
    options: CallOptions,               // local wait deadline and operation-supported intent
}

// No extra execution trait: fix the existing IoClient's associated types.
type PeerClient<P, R, E> = dyn IoClient<
    Request = AddressedCall<P>,
    Response = RemoteReply<R, E>,
    Error = RemoteFault,
>;

struct RemoteReply<R, E> {
    responder: TargetNode,
    response: Response<Result<R, RemoteRejection<E>>>,
}

enum RemoteFault {
    NotSent(NotSentReason),
    OutcomeUnknown(UnknownReason),
}
```

A `RemoteRejection<E>` distinguishes authenticated pre-effect rejection from a
handler failure `E` that may follow effects. The name is provisional: the important
contract is that a business error is not recast as “nothing happened”. The wrapper
preserves actual `ResponseHeader` identity; adapters verify it before returning a
RemoteReply. A malformed/mismatched response after transmission is OutcomeUnknown,
not NotSent. If no authenticated matching receipt exists after a possible send,
there is no success/failure certainty to invent.

`CallOptions` contains a local monotonic wait deadline and optional intent token
only for an operation with a defined idempotency contract. The adapter owns the
wire timeout/credential representation. A job submit's typed IntentKey must agree
with any outer intent token; reject a conflict, never choose one silently.
A simple first node-read protocol can omit idempotency entirely and use no retry.
`P`, `R` and `E` are fixed application/capability types, not serialized-value trees.

### Selected-adapter internal interfaces

| Module/port | Contract, without selecting a protocol |
| --- | --- |
| Peer resolver | `NodeId → configured endpoint + expected identity`; bounded immutable table initially, no caller URL or automatic discovery |
| Framer | Enforces header/body/reply byte limits before unbounded allocation; malformed input is rejected before business decode |
| Identity verifier | Converts selected evidence into verified facts; exact evidence type fixed for this adapter, not a universal credential enum |
| Capability codec | Known route/version → bounded side-effect-free typed decode/encode; unknown routes/versions do not invoke arbitrary type loading |
| Verified ingress | Verifies target/incarnation/audience, builds trusted context from this envelope, calls existing bridge only |
| Peer client | Owns connect/send/receive outcome classification, budget consumption and response matching; no hidden business retry |

Keep protocol DTOs in the adapter. A reusable adapter may be generic over one
application codec type; it need not import every business capability. Credential
mechanisms and wire encoding require conformance tests once selected, not a fake
trait implementation presented as production security.

### Worked remote read flow

1. Local application prepares typed `node.info @ 1`, request Q, correlation C,
   target B and a finite wait budget. The resolver picks B's configured endpoint
   and expected identity; it does not interpret `node.info` as a network address.
2. Outbound adapter authenticates B, encodes the chosen version and carries the
   remaining timeout plus caller evidence. No new arbitrary provider lookup occurs.
3. B bounds the frame, verifies peer/actor/audience and target, decodes the enum,
   constructs its trusted per-call context and invokes its normal shared bridge.
4. B authorizes the actual operation, checks variant consistency and executes the
   read. It returns its identity/incarnation and matching response metadata.
5. A validates all reply identities before accepting the typed result. Timeout
   after possible delivery becomes OutcomeUnknown/unknown observation. It does
   not prove B is dead or make a future effectful call safe to retry elsewhere.

This read path adds neither remote disk navigation nor job submission. They need
additional resource/session and retained-intent contracts before exposure.

## D. Migration and explicit compatibility costs

1. Keep today's Task, DescribeCommand, IoClient, Header, provider GAT and bridge
   signatures. Introduce private job/access modules and test their semantics first.
2. Select new capabilities in an application-owned payload/result facade and add
   descriptors with exact header/name mappings, never a core business enum.
   **Compatibility cost:** existing `forge-local::Command`, `CommandKind` and
   `Output` are public exhaustive enums. Adding variants can break downstream
   exhaustive matches even while every core trait stays unchanged. Prefer a new
   host-owned wrapper/facade when retaining the existing local API, or explicitly
   version and coordinate that application API change with all callers/tests.
   Do not call such an enum extension silently additive or retroactively assume
   `#[non_exhaustive]` was already present.
3. Add a new secured host facade/context and explicit configuration mode. Preserve
   the existing local convenience API as documented local-trust mode; it is not
   exported unchanged as a remote authenticated endpoint. New remote APIs require
   caller request IDs rather than calling `Application::invoke` and regenerating them.
4. Put cross-node addressing in the wrapper, not a silent Header ABI/wire change.
   Version any actual future wire schema independently from capability versions;
   reject unsupported versions rather than guessing old defaults.
5. Extract packages only when useful. Keep in-memory state private; do not expose
   its maps/locks as public API. A durable adapter later needs a real atomic accept/
   transition/recovery port, not separate generic CRUD calls pretending to be a
   transaction. Async support gets separately versioned hosting/lifecycle seams;
   do not relabel synchronous blocking contracts as async-safe.

A stability review requires at least two consumers (e.g. in-process driver and
chosen adapter), the failure/ownership tests in the [proof sequence](foundation-interfaces.md#8-ordered-proofs-before-calling-the-foundation-stable),
and a compatibility test retaining the current pure-task and optional-consumer
builds. Diagrams and signature sketches alone are not that evidence.
