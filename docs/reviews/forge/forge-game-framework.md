# forge-game-framework — focused session capability, not the application kernel

## Decision

**Adapt its session semantics and composition pattern; keep the game package optional.** This is a small, coherent example of a capability layered above a minimal shared contract. It should inform the new framework, not become its universal runtime. The strongest transferable idea is a typed command producing an authoritative revisioned state and explicit ordered effects. The game vocabulary, screen categories, scene hotspots, and presentation contract belong outside the generic ingress and dispatch packages.

**Snapshot:** default branch `main`, privately pinned revision, inspected 2026-10-30. Source is private. This public-safe report contains original analysis, not source excerpts; precise private permalinks remain in the private evidence inventory. Only `main` was returned by the branch inventory. No divergent branch was available to review.

## What is actually present

The workspace has three packages: `forge-game-core`, `forge-game-runtime`, and `forge-game-integration-tests`. Core defines dispatch, snapshot, transition, reducer, session-port, factory, and renderer-neutral screen contracts. Runtime implements an in-process session. The tests package exercises public contracts with an in-memory reducer and a small projection implementation. The only upstream runtime dependency is `forge-core`, pinned to a particular revision; there is no HTTP, Kafka, database, rendering toolkit, async runtime, or plugin loader in these manifests. **Evidence G1–G3.**

The README's boundary claim is substantially supported by those manifests: platform process, transport, persistence, and actual game rules are downstream concerns. Its references to sibling UI, FSM, and code-generation capabilities are architectural context, not proof that all those systems are composed here. In particular, this repository does not ship a production screen projector: it ships the projector interface, validation utilities, and a test implementation. **Evidence G2, G5, G6.**

The actual dependency graph is deliberately small:

```text
application composition root
    -> forge-game-runtime -> forge-game-core -> forge-core
                         -> forge-core
    -> application reducer and screen projector
contract-test package -> game core + runtime + forge-core
```

Factories are ordinary typed composition boundaries. There is no dependency-injection container, runtime module discovery, or dynamic ABI plugin mechanism. That is a strength for the requested plug-and-play system: a capability can be replaceable without turning every implementation into a service or a shared library. **Evidence G3, G4.**

## Representative implemented flow

1. The caller creates a dispatch containing a request identifier, expected state revision, and typed intent.
2. The session locks its state and checks its receipt map. An identical prior request returns the stored transition with a replay marker; reusing an identifier for different input is rejected.
3. A new request with a stale expected revision is rejected.
4. The reducer computes a new state and ordered effects. If it returns an error, the stored snapshot is not replaced.
5. On success, the revision increments, the new snapshot is installed, and the transition is cached under the request identifier.
6. A separately supplied projector can turn the authoritative snapshot into a validated screen; a platform renderer is still the application's concern. **Evidence G3–G6.**

This is real implementation rather than a method-shaped placeholder. However, a reducer's purity and determinism are a programming contract, not something its trait can enforce. Returned effects are values; this runtime does not execute, enqueue, persist, acknowledge, or fan them out. Its replay behavior is therefore **not an exactly-once side-effect guarantee**.

## What should transfer to the new framework

- **Explicit command identity and optimistic concurrency.** A generic command boundary can carry idempotency and revision metadata without importing game types.
- **Separate state transition from effects.** Handlers should decide business outcomes; an explicit internal event/fan-out facility can deliver consequences afterward.
- **Core contract versus implementation.** Keep the session policy independent of storage and scheduling, and make memory implementations useful for contract tests.
- **Composition-root factories.** Select implementations once during application assembly instead of scattering construction and global registries throughout handlers.
- **Stable presentation identifiers.** This is useful for optional UI projections, but not a reason to put UI models in the common ingress envelope.

These are proposed adaptations, not claims that the generic mechanisms already exist in this repository. The REST and Kafka adapters should still converge on the **same header-normalizing ingress bridge**, which dispatches to one selected command handler. A game session may be one such handler-backed capability. Its effect values can feed internal fan-out after a successful command; they should not create a competing transport-specific dispatcher.

## Important limitations and coupling

**Receipt lifetime is unbounded.** Every successful request retains its input and complete transition in a map. Long-lived or large-state sessions will accumulate memory; persistence, retention, eviction, and idempotency scope require an explicit policy. **G3, lines 20–35 and 112–120.**

**A replay includes the original effect list.** The replay flag is the only distinction in the returned transition. A downstream consumer that blindly executes effects again can duplicate them. Define effect-delivery identity, replay suppression, and—where durability matters—transactional state/outbox boundaries before generalizing this to business commands. **G3, lines 79–90.**

**The reducer executes under a synchronous mutex.** This is understandable for a small deterministic session, but unsuitable for arbitrary slow I/O handlers. It serializes all work in one session and has no async cancellation or backpressure policy. Revision increment also lacks explicit overflow handling. **G3, lines 74–121.**

**Validation is opt-in at construction boundaries.** Dispatch and presentation fields are public. A caller can construct invalid dispatch data without using the checked constructor; the session does not repeat blank-identifier validation. Presentation types similarly require callers to invoke validation after mutation. Treat these as trusted typed APIs, not fully validated wire contracts. **G4, lines 16–42; G5, lines 208–307.**

**Identifier normalization deserves a test.** Screen identifier validation checks a trimmed view but retains the original string; whitespace-surrounded values can pass while equality and uniqueness operate on the stored strings. Action and hotspot uniqueness are checked separately, not across one shared identifier namespace. Decide the desired contract rather than assuming stronger invariants. **G5, lines 289–306 and 349–364.**

**Developer checks depend on another checkout.** The Makefile runs formatting, Clippy, tests, and a code-generation check through a sibling repository path. The generated contracts are checked in, but clean-checkout regeneration is not self-contained. Adapt the reproducibility/provenance idea, not the adjacent-directory assumption. **G7–G8.**

## Verification and next step

Observed source contains **four Rust test functions**: two presentation unit tests and two integration contract tests. They cover valid screen construction, duplicate action rejection, projection revision/intents, successful transition, request replay, conflicting request identity, stale revision, and reducer failure preserving state. They do not demonstrate durability, concurrent stress, effect delivery, bounded receipts, invalid direct struct construction, or transport equivalence. **G5–G6.**

No tracked GitHub Actions workflow was present, and the Actions query returned zero runs for this exact commit. The Makefile is a proposed local check entry point, not evidence of a passed run. **No build, tests, scripts, or generators were executed during this read-only review.** No repository-specific `AGENTS.md` or local skills were present in the pinned tree.

The smallest useful next step is to reproduce the revision/replay contract behind the new generic handler interface with an in-memory implementation and a fake effect sink. Add tests proving that an identical command arriving through REST and Kafka is routed to the same handler and does not repeat side effects. Keep the game crates as an optional example capability demonstrating that the application kernel has not absorbed its domain.

## Source evidence index

All locations are relative to this repository at the pinned commit; exact private URLs are retained outside the public portfolio.

| ID | Inspected location |
|---|---|
| G1 | `Cargo.toml:1–25`; all three package manifests |
| G2 | `README.md:6–58` |
| G3 | `crates/forge-game-runtime/src/lib.rs:11–156` |
| G4 | `crates/forge-game-core/src/lib.rs:10–74`; `src/generated/{reducer,session_port,session_factory}.rs` |
| G5 | `crates/forge-game-core/src/presentation.rs:11–405` |
| G6 | `crates/forge-game-integration-tests/tests/session_contract_bdd.rs:37–78`; `presentation_contract_bdd.rs:17–80` |
| G7 | `Makefile:1–7` |
| G8 | `forge/codegen.yaml`; `forge/codegen.lock.yaml` |
