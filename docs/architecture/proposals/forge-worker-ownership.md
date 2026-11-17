# Forge worker and main-thread ownership

**Proposal** based on Homelab `634f005`. The Forge IDE already exchanges
typed input and view messages through a dedicated browser worker. The current
Room One page instantiates WASM directly. This design specifies the
responsibilities and messages for a worker-backed playtest and Studio.
[Existing IDE worker](../../../src/rust/forge-ide-web/src/generate/javascript.rs) ·
[Current playtest adapter](../../../src/rust/forge-ide-web-host/src/design/playtest_browser.rs)

## Who owns what

| Owner | Responsibilities | Boundary output |
| --- | --- | --- |
| Main JavaScript thread | Capture input and forward actions to Rust. Receive completed render data and draw Canvas or update DOM. | Ordered actions and render-frame time samples to Rust |
| Rust main thread | Own the fixed clock, input application, active level, simulation, animation, replay, and publication. Validate all completions before changing state. | Typed work requests to providers and immutable views for rendering |
| JavaScript support workers | Carry out bounded I/O and support computations. Fetch bytes, calculate a digest, decode an image, or call a configured Rust worker backend. Relay typed results to Rust and views to main JavaScript. | A correlated result or failure per request |
| Rust support backend | Run independent computation selected by a factory, such as validating a cooked level or encoding a sealed replay. Work on received data or owned resources. | A candidate value or failure for Rust main to evaluate |
| Forge host | Admit Studio commands through the header-routed bridge. Authorize project access, inject the producer store, and return receipts and status. | Durable build ID, ordered status, and authorized artifact reference |
| Studio build worker | Claim a durable job using its own worker-scoped provider. Build the pinned revision under an approved profile and publish a verified artifact. | Attempt-fenced job events and immutable artifact metadata |

**Rust main thread** names the authoritative Rust execution context. A dedicated
browser worker can host that WASM instance with a thin JavaScript message adapter.
The same Rust authority applies while the current implementation hosts WASM on
the page. In either placement, main JavaScript forwards actions and draws the
views Rust publishes.

[Ownership diagram](../diagrams/mermaid/images/forge-worker-ownership.svg) ·
[Editable Mermaid](../diagrams/mermaid/forge-worker-ownership.mmd)

## Factory and provider scope

Each process or execution context bootstraps its own typed registrations,
validates and freezes them, then constructs only its selected providers. Rust
main receives narrow asset and replay ports. Support workers receive the
I/O or compute adapters for their assigned work. The Forge host gets a Studio
producer store, and the Studio worker gets a claim-and-complete store and a
build executor. Instances live with the assembly that created them.

Across a boundary, a client exchanges asset IDs, request generations, byte
buffers, or build receipts. The receiver resolves those values using its
injected provider. The startup configuration selects the implementation.
Forge's runtime command header selects an admitted operation and its typed
payload. [Bootstrap registrars](../bootstrap-registrars.md) ·
[Provider lifetime](../dependency-injection.md)

Playtest actions enter through the local Rust adapter. Studio commands cross
the host's shared Forge gateway. The simulation and its local providers stay
inside the player.

## Playtest actions and views

1. The runtime host starts a Rust instance for the playtest and announces
   `Ready` with the supported protocol and session generation. Main JavaScript
   enables the controls when that handshake succeeds.
2. Main JavaScript forwards ordered press and release edges and a monotonic
   render-frame time sample. With one frame request outstanding, the adapter
   retains edges in order and coalesces time samples. Rust decides how many
   ticks are due and accounts for elapsed time it drops under the catch-up cap.
3. Rust applies actions at a fixed tick, resolves motion and contacts, selects
   the pose, and publishes one complete view for that tick. The view pairs the
   existing world snapshot with the same-epoch, same-tick pose. A browser
   worker can transfer an owned copy of the published packet.
4. Main JavaScript checks the session and view identity, draws the newest
   complete view, and interpolates only the root for presentation. It can
   skip drawing an intermediate view. Rust retains the ordered simulation
   inputs and events independently of what the page drew.

Pause, step, restart, and focus loss use the same ordered action channel.
Rust owns the held-input reset and the clock reset on these boundaries.
JavaScript drops unsent stale input and disregards replies that precede the
boundary. Restarting the Rust host creates a new session generation.
Restarting a support worker invalidates that worker's outstanding requests.

| Message | Sender → receiver | Ownership or check |
| --- | --- | --- |
| `ActionBatch`, `FramePulse`, `ControlAction` | Main JavaScript → Rust main | Ordered actions and observed time. Rust validates and applies them. |
| `AssetWork` | Rust main → support worker or injected asset service | Asset ID, expected digest, and request generation. |
| `AssetResult` | Support worker/backend → Rust main | Bounded bytes, prepared candidate, or error tagged with the request identity and generation. Rust matches it to pending work and checks asset identity, schema, and readiness. |
| `FrameView` | Rust main → main JavaScript, optionally relayed by a worker | Immutable snapshot, pose, and render metadata tagged with session, publication sequence, room epoch, and tick. |
| Optional decoded art | Support worker → main JavaScript | Presentation resource tagged with the asset identity and generation. The renderer draws it when those tags match the current Rust view and uses primitives while art is unavailable. |

Worker replies can arrive in any order. Rust matches each reply to pending
work in the current session. Main JavaScript accepts complete views in
publication order within that session, including when a worker relays them.

The JavaScript bridge queues messages using the platform's worker ports.
Its payloads are serializable or explicitly transferable. A transferred image
or byte buffer has one consuming owner. Rust keeps its working WASM memory.
[Existing snapshot contract](forge-game-snapshot.md) ·
[HTML worker messaging](https://html.spec.whatwg.org/multipage/workers.html#dedicated-workers-and-the-dedicatedworkerglobalscope-interface)

## Load a level and preserve replay

The main thread forwards a level selection. Rust issues asset work through
its provider. A support worker reads the cooked manifest and bytes, performs
the requested digest check, and can delegate bounded decoding and preparation
to the Rust support backend. The old room continues running during preparation.
Rust discards stale completions and keeps the active room on required asset
failure. A valid newest candidate waits at the activation boundary.

At that boundary, Rust seals the outgoing replay segment and records the
transition before replacing the room. Each segment identifies the exact
cooked manifest, level ID, room epoch, and effective inputs by tick. The
transition records the outgoing boundary tick and the incoming epoch.
The incoming room starts at tick zero. A worker transports or encodes sealed
segments in the order Rust publishes.

Use a bounded Rust-owned session recorder. The current room's 512-record
debug ring can lose earlier inputs, so a complete replay must capture
effective inputs while running. When the recorder exhausts its budget,
mark the export incomplete and continue the playtest. During replay,
load the recorded cooked generation before applying its transition at the
recorded boundary. I/O completion sets readiness. The recorded tick sets
when the replay transitions.

[Level handoff and replay sequence](../diagrams/mermaid/images/forge-game-worker-handoff.svg) ·
[Editable Mermaid](../diagrams/mermaid/forge-game-worker-handoff.mmd) ·
[Cooked asset lifecycle](forge-game-asset-pipeline.md#tick-boundary-activation-and-replay) ·
[Current room replacement](../../../src/rust/forge-ide-web/src/playtest_level.rs)

## Submit and report a Studio build

1. Main JavaScript forwards the build action to Rust. The IDE worker's
   transport sends the resulting typed effect through `/forge/v1/dispatch`.
   The Forge host admits `game.build.submit` for the authenticated project
   and configured profile.
2. Studio commits the job with its pinned revision and its first event in one
   PostgreSQL transaction and returns a build receipt. Matching retries get
   the same build ID. A reused key with different input gets a conflict.
3. The separate Studio worker claims a lease, builds the pinned revision in
   its isolated workspace, and verifies the output. Completion attaches the
   immutable artifact only while the attempt token is current. The durable
   store carries the job and receipt across page closure or worker restart.
4. The IDE transport requests status by build ID and event cursor through
   the admitted host route. Rust projects that status for main JavaScript
   to render. An authorized artifact link opens the preview on its separate
   origin.

[Studio worker sequence](../diagrams/mermaid/images/forge-studio-worker-lease.svg) ·
[Editable Mermaid](../diagrams/mermaid/forge-studio-worker-lease.mmd) ·
[Studio DAO and lifecycle](forge-game-engine.md#build-data-and-worker-lifecycle)

## Acceptance checks

- Match ticks, poses, and contacts for the same effective inputs across two
  render-frame partitions. Exercise a short tap, frame backpressure, focus loss,
  and late replies after restart.
- Deliver level completions out of order. Check that only the current verified
  candidate activates, at a tick boundary, with a complete matching view.
- Replay two rooms from one export using their exact cooked digests and
  transition tick. Exercise an exhausted recorder and verify the incomplete
  flag. Keep debug drawing and image decode outside the simulation tick.
- Deny a build for an unauthorized project before enqueue. Retry a submit
  idempotently, expire a worker lease, and verify that the stale attempt cannot
  attach an artifact. Reconnect Studio and read the same build receipt.
