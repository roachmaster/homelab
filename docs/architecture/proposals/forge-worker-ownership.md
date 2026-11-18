# Forge worker and main-thread ownership

The Forge IDE exchanges typed input and view messages through a dedicated
browser worker. The playtest hosts WASM on the page and uses a JavaScript I/O
worker. Studio uses a separate worker to send control requests to the Forge host.
[Existing IDE worker](../../../src/rust/forge-ide-web/src/generate/javascript.rs) ·
[Current playtest adapter](../../../src/rust/forge-ide-web-host/src/design/playtest_browser.rs)

The playtest implementation hosts Rust on the page and uses a generated
worker to fetch WASM and cooked assets. The I/O worker returns bounded bytes
and the Rust runtime verifies them before staging a level. Rust also records
effective inputs across room transitions for **Export replay** in Debug tools.
The WASM playback API verifies exported JSON against the retained cooked
levels and publishes a separate replay view.
[I/O worker](../../../src/rust/forge-ide-web-host/src/design/playtest_asset_worker.rs) ·
[Replay journal](../../../src/rust/forge-ide-web/src/playtest_replay.rs)

## Who owns what

| Owner | Responsibilities | Boundary output |
| --- | --- | --- |
| Main JavaScript thread | Capture input and forward actions to Rust. Receive completed render data and draw Canvas or update DOM. | Ordered actions and render-frame time samples to Rust |
| Rust main thread | Own the fixed clock, input application, active level, simulation, animation, replay, and publication. Validate all completions before changing state. | Typed work requests to providers and immutable views for rendering |
| JavaScript I/O workers | Execute typed, bounded I/O requests from Rust. Fetch bytes, return replies, and relay messages across browser contexts. | A correlated I/O result or failure per request |

**Rust main thread** names the authoritative Rust execution context. A dedicated
browser worker can host that WASM instance with a thin JavaScript message adapter.
The same Rust authority applies while the current implementation hosts WASM on
the page. In either placement, main JavaScript forwards actions and draws the
views Rust publishes.

[Ownership diagram](../diagrams/mermaid/images/forge-worker-ownership.svg) ·
[Editable Mermaid](../diagrams/mermaid/forge-worker-ownership.mmd)

## Factory and provider scope

Each Rust assembly bootstraps its typed registrations, validates and freezes
them, then constructs the providers it uses. Rust main receives narrow asset
and replay ports. When those ports need browser I/O, they emit typed requests
for the configured JavaScript worker adapter. Each provider instance lives
with the assembly that created it.

Across the worker boundary, callers exchange asset IDs, request generations,
and owned byte buffers. The startup configuration selects the I/O adapter.
For Studio, the Forge runtime command header selects an admitted operation
and its typed payload. [Bootstrap registrars](../bootstrap-registrars.md) ·
[Provider lifetime](../dependency-injection.md)

Playtest actions enter through the local Rust adapter. The simulation and
its providers stay inside the player.

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
Restarting an I/O worker invalidates that worker's outstanding requests.

| Message | Sender → receiver | Ownership or check |
| --- | --- | --- |
| `ActionBatch`, `FramePulse`, `ControlAction` | Main JavaScript → Rust main | Ordered actions and observed time. Rust validates and applies them. |
| `AssetWork` | Rust main → JavaScript I/O worker | Asset ID, response byte limit, and request generation. Rust retains the expected digest. |
| `AssetResult` | JavaScript I/O worker → Rust main | Bounded response bytes or an I/O error tagged with the request identity and generation. Rust matches pending work, verifies the digest, and validates the format. |
| `FrameView` | Rust main → main JavaScript, optionally relayed by a worker | Immutable snapshot, pose, and render metadata tagged with session, publication sequence, room epoch, and tick. |

Worker replies can arrive in any order. Rust matches each reply to pending
work in the current session. Main JavaScript accepts complete views in
publication order within that session, including when a worker relays them.

The JavaScript bridge queues messages using the platform's worker ports.
Its payloads are serializable or explicitly transferable. A transferred byte
buffer has one consuming owner. Rust keeps its working WASM memory.
[Existing snapshot contract](forge-game-snapshot.md) ·
[HTML worker messaging](https://html.spec.whatwg.org/multipage/workers.html#dedicated-workers-and-the-dedicatedworkerglobalscope-interface)

## Load a level and preserve replay

Main JavaScript forwards a level selection. Rust issues asset requests through
its provider. The JavaScript I/O worker fetches the cooked manifest and level
bytes and returns bounded responses. Rust verifies digests, decodes the level,
and prepares the candidate between ticks. The old room stays active until a
valid newest candidate reaches its activation boundary. Failed or stale
responses leave the active room in place. The renderer uses primitives while
optional art is unavailable.

At that boundary, Rust seals the outgoing replay segment and records the
transition before replacing the room. Each segment identifies the exact
cooked manifest, level ID, room epoch, and effective inputs by tick. The
transition records the outgoing boundary tick and the incoming epoch.
The incoming room starts at tick zero. Rust encodes sealed segments and can
hand the resulting bytes to the browser for delivery.

Use a bounded Rust-owned session recorder. The current room's 512-record
debug ring can lose earlier inputs, so a complete replay must capture
effective inputs while running. When the recorder exhausts its budget,
mark the export incomplete and continue the playtest. During replay, Rust
loads and verifies the recorded cooked generation. The recorded tick sets
when the replay transitions.

[Level handoff and replay sequence](../diagrams/mermaid/images/forge-game-worker-handoff.svg) ·
[Editable Mermaid](../diagrams/mermaid/forge-game-worker-handoff.mmd) ·
[Cooked asset lifecycle](forge-game-asset-pipeline.md#tick-boundary-activation-and-replay) ·
[Current room replacement](../../../src/rust/forge-ide-web/src/playtest_level.rs)

## Submit and report a Studio build

The Studio browser worker carries requests and replies. The Rust web host
owns admission and forwards admitted requests to the private Studio API. The
Studio executor owns the durable build attempt, with a store scoped to claim,
renew, and finish operations.

1. Main JavaScript passes the build action to the I/O worker. The worker
   carries the request to `/forge/v1/dispatch`. The Rust host admits
   `game.build.submit` for the authenticated project and forwards it to Studio
   for profile and source validation.
2. Studio commits the job with its pinned revision and its first event in one
   PostgreSQL transaction and returns a build receipt. Matching retries get
   the same build ID. A reused key with different input gets a conflict.
3. The Studio worker claims a lease, reads the pinned Git subtree, and
   verifies the output. Completion attaches the immutable artifact only
   while the attempt token is current. The durable store carries the job and
   receipt across page closure or worker restart.
4. The browser I/O worker requests status by build ID through the admitted
   host route. Rust validates and projects status for main JavaScript to
   render. An authorized artifact link opens the preview on its separate
   origin. The Studio page remembers the build ID in the current tab.

[Studio worker sequence](../diagrams/mermaid/images/forge-studio-worker-lease.svg) ·
[Editable Mermaid](../diagrams/mermaid/forge-studio-worker-lease.mmd) ·
[Studio DAO and lifecycle](forge-game-engine.md#build-data-and-worker-lifecycle)

## Acceptance checks

- Match ticks, poses, and contacts for the same effective inputs across two
  render-frame partitions. Exercise a short tap, frame backpressure, focus loss,
  and late replies after restart.
- Deliver malformed, digest-mismatched, and out-of-order I/O results. Check
  that only the current verified candidate activates, at a tick boundary,
  with a complete matching view.
- Replay two rooms from one export using their exact cooked digests and
  transition tick. Exercise an exhausted recorder and verify the incomplete
  flag. Keep debug drawing and image decode outside the simulation tick.
- For Studio, deny an unauthorized project before
  enqueue. Retry a submit idempotently, expire a worker lease, and verify
  that the stale attempt cannot attach an artifact. Reconnect Studio and
  read the same build receipt.
