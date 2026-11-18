# Forge worker and main-thread ownership

**Implementation reference — reviewed 18 November 2026.** The IDE, playtest and
Studio currently use different browser placements. This document describes
those actual boundaries, including Studio's remaining legacy client exception.
“Rust authority” names the state owner; it does not mean every Rust instance runs
on the browser main thread.

## Current execution contexts

| Screen | Main JavaScript | Worker | Authoritative application state |
| --- | --- | --- | --- |
| IDE Chat/Mail/Workbench | DOM, focus, lifecycle, local presentation and typed action forwarding. | Generated adapter hosts Rust/WASM and executes its prepared admitted I/O effects. | Rust/WASM model in that dedicated worker. |
| Playtest | Input capture, elapsed frame samples, Canvas/image decoding and direct WASM calls. | Generated I/O-only worker reads bounded WASM, manifest, level and atlas bytes. | Page-hosted Rust/WASM owns clock, input, simulation, asset validation, activation, animation and replay. |
| Studio | Legacy JavaScript client owns orchestration, polling, rendering and tab-local selected build. | Legacy Studio I/O worker sends control requests and correlates replies. | Rust web host owns admission; Python Studio/Store owns durable build state. No Studio Browser Rust controller exists yet. |

[IDE generator](../../../src/rust/forge-ide-web/src/generate/javascript.rs) ·
[playtest adapter](../../../src/rust/forge-ide-web-host/src/design/playtest_browser.rs) ·
[asset worker](../../../src/rust/forge-ide-web-host/src/design/playtest_asset_worker.rs) ·
[Studio client](../../../src/rust/forge-ide-web-host/src/design/studio.rs)

Workers exchange messages and owned buffers. They do not share a global registry
or obtain arbitrary native providers. There are no separate Rust computation
workers behind the current playtest I/O worker.

## Factory and provider scope

The native web host stages effect-free typed registrars, validates/freezes its
startup configuration, then creates the selected asset source, agent tasks and
optional Chat/Studio relays. Ready instances are injected into handlers and live
with the host. An unused optional relay has no runtime acquisition effect.
[Host assembly](../../../src/rust/forge-ide-web-host/src/service.rs) ·
[Studio relay factory](../../../src/rust/forge-ide-web-host/src/studio.rs)

Pure `Room`, `Level`, `Character`, `Assets`, `Loader` and replay values use normal
Rust construction; empty registrars would add no dependency boundary. The
generated page adapter constructs, owns and terminates its configured I/O worker
and pending requests. Cross-context messages carry fixed asset names, request
numbers, bounded owned buffers and failures, rather than a registry handle.

The Python Studio API/worker/preview are separately configured processes. Their
`Store` and artifact functions are Python service dependencies, not Rust
`Registry` entries. Their PostgreSQL roles and secret files have separate scopes.

[Ownership diagram](../diagrams/mermaid/images/forge-worker-ownership.svg)

## Playtest input and publication

Main JavaScript forwards press/release edges to `forge_playtest_input` and elapsed
microseconds to `forge_playtest_advance`. Rust buffers input, runs bounded fixed
ticks and publishes FGS1 world, FGP1 pose and FGC1 collision buffers. Publication
is sequential with Canvas reads on the same page thread. The renderer validates
magic, ranges and matching room epoch/tick/player before combining artwork and
root state.

The current implementation has no separate Ready/ActionBatch/FramePulse/FrameView
message protocol for a simulation worker. Those were illustrative proposal names,
not exported DTOs. A later worker-hosted simulation would need its own ordered
input, backpressure and complete-view handoff contract.

Pause, step, restart, focus loss and page teardown use the existing Rust/browser
lifecycle seam. The page clears held input; Rust owns the clock/input and replay
state transitions. I/O adapter closure aborts pending reads and terminates its
worker. A late reply cannot act after its owned pending request is removed.

[Snapshot contract](forge-game-snapshot.md) ·
[Playtest exports](../../../src/rust/forge-ide-web/src/playtest.rs)

## Level I/O, activation and replay

1. Rust records the requested level and generation. The generated adapter reads
   the required supported assets through the I/O worker's `read` message.
2. The worker performs bounded same-origin reads and returns `asset-result` with
   a transferable buffer or `asset-error`, retaining the request and asset name.
3. The page uploads bytes into a bounded WASM buffer. Rust `Assets` verifies
   manifest binding/digest and `Loader` checks the newest request, format and
   level constraints before preparing a candidate.
4. The active room continues until a valid activation boundary. Rust seals the
   outgoing replay segment and records the boundary before replacing the room;
   the incoming room starts at tick zero with a new epoch.
5. Main JavaScript draws the committed world/pose. Optional art failure uses
   primitives; required or stale data cannot replace the active room.

The Rust session recorder retains effective input independently of the small
debug ring. Complete exports are bounded to 36,000 ticks, 128 segments and 2 MiB.
Import validates the complete session and requests missing historical manifests
and level chunks through a Rust-owned archive-read generation. The host must
have a retained digest archive configured to serve an older build. Replay has a
separate view and does not overwrite live simulation state.

[Level/replay sequence](../diagrams/mermaid/images/forge-game-worker-handoff.svg) ·
[loader](../../../src/rust/forge-ide-web/src/playtest_level.rs) ·
[replay and archive port](../../../src/rust/forge-ide-web/src/playtest_replay.rs)

## Studio submission and durable state

The durable service is implemented for the `static-web` profile. A link to its
client does not establish that the private API, database roles, worker and
separate preview origin are enabled on a live portal.

1. The current Studio JavaScript client sends an action to its worker. The worker
   dispatches `game.build.submit` through `/forge/v1/dispatch`, carrying the
   existing session, CSRF, version and request correlation.
2. Rust host admission validates project grants and destination/body agreement
   before forwarding. The private Python API independently validates the
   configured project, exact revision and `static-web` profile.
3. `Store.submit` atomically commits the job and first event. Matching intent
   reuses the build ID; an idempotency key reused with different input conflicts.
4. The packaging worker polls/claims a 30-second lease and attempt token, reads
   the approved local Git subtree without running project scripts, verifies and
   publishes a digest-addressed artifact, then conditionally attaches it while
   the lease is current. Expired claims retry up to three total attempts.
5. Cancellation atomically clears a queued/running attempt. Reconnect restores
   the tab-local build identity and polls admitted status with an event cursor.
   The preview uses a signed link on the configured separate origin.

The first profile caps output at 256 files and 16 MiB. API and worker SQL roles
are separate; preview reads artifacts without a database connection. A stale
attempt may leave an unreferenced artifact, but cannot attach it to a job.
Artifact retention/reclamation and general project compilation remain future.

[Implemented Studio sequence](../diagrams/mermaid/images/forge-studio-worker-lease.svg) ·
[service/setup](../../../deploy/mission-control/studio/README.md) ·
[Store](../../../deploy/mission-control/studio/studio_store.py)

## Remaining authoring gap and verification

Studio's JavaScript `MAIN`/`WORKER` string controllers are a remaining exception
to Forge's Rust-owned intent and declarative generated-adapter direction. Moving
Studio client state/effect preparation into a typed Rust/WASM model is future
work. Do not draw a Browser Rust controller into the implemented Studio sequence.

Native, actual-WASM and browser tests cover bounded input, view identity, asset
staleness, fixed-tick transitions and replay. Disposable Studio integration tests
cover project admission, idempotency, cancellation, lease expiry and guarded
artifact completion. Fresh run and live-configuration evidence belong to the
current QA/deployment record; no deployment is certified by this document alone.
