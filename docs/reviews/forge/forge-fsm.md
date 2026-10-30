# forge-fsm — small, optional state-transition capability

## Verdict

**Reuse for bounded domain lifecycle logic; do not mistake it for a durable workflow engine or framework-wide orchestrator.** This repository is usefully small. Its runtime dependency is only the framework core, and its generic types do not depend on UI, transports, databases, async executors, or the generator that authored its source. It is a good example of a capability that applications select when needed rather than inherit universally.

## Review scope and evidence

Reviewed the current `main` snapshot on 2026-10-30: complete recursive inventory, package manifest, all eight runtime source files, generated BDD test declarations and fixture definitions, source-generation plan, README, and materialization helper as text. The only accessible branch was `main`; no alternate implementation branch was exposed. Exact source pins and private links are preserved in the private evidence inventory. References here identify repository-relative paths and lines.

No code generation, tests, build scripts or repository scripts were run. The reviewed tree has no GitHub Actions workflow and the pinned commit returned zero GitHub check runs. Test success is therefore **not established by this review**. Two integration scenarios and one inline trace test are present in the inspected source; those are authored checks, not executed results.

## Actual package and API

The single `forge-fsm` package declares Rust 1.86 and edition 2024. Its only runtime dependency is `forge-core`, pinned to the same framework revision used by codegen and UI at review time (`Cargo.toml:8–15`). There is no runtime dependency on `forge-codegen`, no build script, no dynamic plugin loader, and no serialization dependency. No manifest license field or license text was found; resolve distribution/license intent before public release.

The primary contract, `FsmDefinition`, has associated state, event and output types and three operations: initial state, terminal-state classification, and transition resolution (`src/definition.rs:12–29`). State must be cloneable and comparable. No framework-owned domain event vocabulary is imposed.

`FsmMachine` owns one definition and current state. It can start at the definition's initial state or an explicitly supplied state. Dispatch asks the definition for a transition, rejects a transition whose origin differs from the actual current state, then copies the destination into the machine (`src/machine.rs:21–56`). `FsmTransition` carries origin, destination and an arbitrary output; output mapping can adapt effects without changing the state pair (`src/transition.rs:16–40`).

`FsmDescriptor`, `FsmRole` and `FsmKind` label orchestration versus child/subflow definitions. Descriptor construction validates nonblank names and matching role/kind combinations (`src/descriptor.rs:12–49`). `FsmOrchestrator` adds only an associated subflow key and a query for the active child (`src/orchestrator.rs:10–17`). It does not create, schedule, await, persist or resume child machines automatically.

`FsmTraceRunner` replays an ordered event sequence and returns initial/final states, transitions and a final terminal flag (`src/trace.rs:25–58`). This is suitable for deterministic examples and regression traces, not a durable execution journal.

## Flow and framework boundary

A suitable assembled use is:

**canonical ingress → selected capability handler → capability-owned FSM definition → next state plus outputs → capability-owned persistence/effect ports**.

The FSM belongs behind the handler, not inside selected input adapters and not in a central bridge. Its outputs may describe follow-on work, but this crate does not dispatch that work. The domain capability decides whether to fan out, persist, emit events, retry an effect or expose a projection. The composition root supplies the required ports and state owner without knowing each transition rule.

This is fully compatible with header-deterministic ingress: the envelope header chooses a registered capability operation; the payload becomes a typed domain event inside that capability. The FSM never needs to inspect transport headers or choose a route. It also need not be used for every operation—stateless reads and simple commands should remain simple.

## Generated source is provenance, not architecture

The manifest, module root, runtime files and BDD declaration file are visible generated artifacts. The source plan explicitly lists them, their templates and input data (`forge/codegen.yaml:1–93`). Ordinary Cargo compilation consumes those checked-in files; code generation is a separate development step.

Keep this distinction in architecture diagrams. A dashed tooling/materialization relationship from codegen to FSM is appropriate; a runtime dependency arrow from FSM to codegen would be wrong. Consumer domain definitions should usually implement the generic traits in their own package rather than editing or copying the generated framework implementation.

The convenience materialization script is broader than its name implies: after planning/materializing/checking, it can stage, commit and push source changes (`scripts/materialize.sh:11–29`). It was not run. Separate local generation from publication if this workflow is adopted in homelab, and treat templates/specs—not generated output—as the authoritative edit surface.

## Implemented guarantees and important limits

The origin-state guard is real, and state assignment occurs only after transition creation and that guard succeed. This gives a useful narrow invariant for in-memory state progression. Descriptor validation and transition-output mapping are also implemented rather than placeholders. The BDD scenarios cover a successful transition and a manually coordinated child/subflow completion; the inline test records a two-step trace.

Several broader guarantees remain the consumer's responsibility:

- **Terminal does not mean blocked.** Dispatch does not consult `is_terminal`, and trace replay consumes every supplied event. A definition may intentionally permit terminal self-transitions, or must explicitly reject later events. Document the policy.
- **Explicit state loading is unchecked.** `from_state` accepts a value without a validation/migration hook. Persistent state needs its own schema/version and validity boundary.
- **Purity is a convention.** The trait describes deterministic, storage-free transitions but cannot prevent an implementation from using interior mutation or external effects. Keep transition definitions pure and interpret effects outside them if replay matters.
- **Trace errors lose the returned partial trace.** Replay returns early on the first error. Operational diagnostics may need a result that retains completed transitions separately from failure.
- **No concurrency or durability is supplied.** Revision checks, locking, event ordering, retry policy, idempotency, recovery, timers and durable effect delivery are not implemented by this crate. A single mutable machine is not a distributed workflow engine.

The reviewed tests do not establish those broader properties or exercise a deployed transport/persistence integration. Add capability-specific rejection and failure-path tests before relying on the FSM for operational state changes.

## Recommended disposition

Keep FSM as an independently selectable library or small starter. Reuse its generic transition model, origin guard and trace tooling; adapt terminal-state and restore semantics only where a real consumer requires them. Keep durable orchestration out of the initial foundation unless a concrete use case earns that complexity.

For the portfolio, demonstrate one bounded lifecycle with a transition diagram, executable event trace, rejected event, and an injected in-memory effect port. Label persistence and broker delivery separately if they are added. The strongest story here is disciplined scope: a small state capability that can plug into an application without dragging in a UI, transport stack or global workflow manager.
