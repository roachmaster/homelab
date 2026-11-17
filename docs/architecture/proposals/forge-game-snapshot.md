# Simulation-to-Presentation Snapshot for the Forge Game Runtime

Replace the scalar per-cell WASM interface with one versioned, batched
snapshot per frame, so the playtest can grow past Room One without
reworking the runtime-to-presentation boundary for every new feature.

**Planning baseline** — GitHub `main` at `6e1e70e` on 2026-11-17. This
design follows the [game engine proposal](forge-game-engine.md) and its
`WorldSnapshot` / `FrameOutcome` / `GameEvent` contracts, and applies the
simulation-to-presentation trace from the `design-game-engine` skill.
It covers the snapshot interface only: the entity model, level loading,
camera, and audio belong to their own designs.

## The original interface

The original interface had eight scalar WASM exports (`width`, `height`,
`cell`, `status`, `input`, `advance`, `clear_input`, `restart`). Every
frame the adapter calls `cell(x, y)` once per tile: 117 boundary
crossings per frame at Room One's 13×9. The adapter already redrew only
changed cells. The call count comes from inspecting the loop. Relative
cost requires a browser profile. The scalar interface carried no
interpolation data, no events, no dynamic values (score, health,
timers), no entity data, and no report of dropped ticks after a hitch
(`advance` returns nothing, although the proposal requires discarded
time to be reported). Each of those would today need another scalar
export. The snapshot fixes the shape once.

## Representative scenario

Room One, played to completion in the browser: the player moves with
keyboard or buttons, collects the key, reaches the exit, and restarts.
The frame loop is unchanged (requestAnimationFrame drives `advance`,
then paint). What changes is what paint reads: one snapshot instead of
117 scalar calls. The existing journey already plays this scenario end
to end; it becomes the acceptance check for the new interface.

## Snapshot layout

One flat, versioned, little-endian buffer, published on initialization,
restart, and runtime updates, then read by the adapter during paint. All integers are
fixed-width; there are no pointers, no strings, and no variable-length
sections except the count-bounded arrays below.

| Section | Contents |
| --- | --- |
| Header (offset 0, 32 bytes) | `FGS1`, schema version `u16`, width/height `u16` each, entity count `u16`, event count `u16`, input depth `u16`, run epoch `u32`, tick `u32`, interpolation numerator `u32`, status `u8`, ticks run `u8`, reserved `u16` |
| Board (offset 32) | 4,096 bytes reserved. One byte per active cell, row-major, y-down. Cell codes are floor, wall, key, locked exit, open exit, player, and guard, in that order. |
| Entities (offset 4,128) | Up to 256 records at 20 bytes each. Stable ID `u16`, kind `u8`, state `u8`, current x/y and previous x/y as four `i32` values in 1/256-tile units, y-up. The state byte puts moving entities' facing in its upper two bits, clockwise from north. |
| Events (offset 9,248) | Up to 64 records at 16 bytes each, ordered oldest first. Sequence `u32`, tick `u32`, stable target ID `u16`, kind `u8`, reserved `u8`, event tile x/y `u16` each, y-up. |
| Stats (offset 10,272) | Ticks dropped, input edges dropped, and events dropped, each `u32`. |

Total size is fixed at 10,284 bytes.
The buffer is fixed-size by design: the header counts say how much of
each array is live, the adapter allocates its read view once and reuses
it, and the runtime never allocates per frame.

## Transport: one stable buffer in WASM memory

The runtime exports `forge_playtest_snapshot() -> u32`, returning the
byte offset of the snapshot buffer in WASM linear memory, and
`forge_playtest_snapshot_len() -> u32`. The adapter creates one
`Uint8Array` view over the buffer and reads the snapshot from it each
frame. The layout removes the per-cell WASM calls. The buffer
address is stable for the module's lifetime; the adapter re-creates its
view only if the memory grows.

Rejected alternatives:

- **JSON serialization per frame.** Allocates and parses every frame,
  and turns a typed interface into stringly data at the one boundary
  that runs hottest. JSON stays an external format, not an internal
  one.
- **Row batching (one call per row).** Cuts the call count 9× but keeps
  the O(board) shape and carries no entities, events, or interpolation.
  It postpones the same redesign by one feature.

The adapter treats memory as read-only and validates the exported range,
magic, version, dimensions, and counts before consuming records. On validation
failure, show the existing failure message and disable the controls
rather than rendering a partial snapshot. A version the adapter does
not know is a failure, not a best-effort read.

## Phases and ownership

| Handoff | Producer → consumer | Phase | Data and owner | On failure |
| --- | --- | --- | --- | --- |
| Tick input | Adapter → runtime | Input, before `advance` | Queued edges; runtime owns the buffer | Overflowing edges are dropped and counted in stats |
| Snapshot | Runtime → adapter | Publication follows a runtime update. Canvas reads it before painting. | Snapshot buffer; runtime owns the write, adapter owns the read view | Invalid header → failure message, controls disabled |
| Events | Runtime → adapter | Simulation publishes; presentation can read the retained history. | Runtime owns write ordering and the sequence. | Overflow evicts the oldest event and increments the loss count. |
| Catch-up report | Runtime → adapter | Simulation, during `advance` | Stats section; runtime owns the counts | Dropped ticks are reported, never silently absorbed |

A single buffer suffices while the runtime and the adapter run on the
same thread: `advance` and paint are sequential inside one frame
callback, so the adapter can never observe a half-written snapshot.
If rendering ever moves to a worker, the snapshot becomes double
buffered (the runtime publishes to the back buffer and swaps at the
tick boundary); the layout and the adapter's read path do not change.

The interpolation numerator uses a denominator of 1,000,000. The
snapshot carries previous and current transforms for future smooth
movement. Room One draws tile-discrete positions. Collision reads
authoritative tick state.

## Coordinates

Entity and event positions are y-up. The board remains row-major,
with row zero at the top, so Canvas can paint it directly. The adapter
converts event coordinates to board rows for debug markers.

## Event identity and retention

The pair (run epoch, sequence) identifies an event across repeated reads
and restarts. A new consumer can use that pair as a cursor. The history
holds 64 events. Overflow evicts the oldest and increments the loss
counter. Events identify targets by stable ID and carry the tile where
they occurred even after the target despawns.

## Catch-up reporting

`advance` publishes ticks run, ticks dropped (whole-tick
debt discarded after the four-tick cap), and fractional time preserved.
The counts land in the stats section each frame. This closes the gap
where the current `advance` returns nothing: a 250 ms hitch now
reports its dropped ticks instead of absorbing them silently. The
debug inspector displays the clock diagnostics.

## Compatibility

The Room One adapter selects the snapshot when its exports are available.
An older bundle can still use the scalar playtest. Debug tools require
the snapshot and trace exports. Retire the compatibility path once
every active portal release uses the snapshot.

## Acceptance checks

- **Representative scenario:** the existing journey plays Room One to
  "You won!" and restarts, through the snapshot path, with no
  behavior change.
- **Failure case:** an unknown snapshot version (or a corrupt header)
  shows the failure message and disables the controls; nothing
  renders partially and nothing throws uncaught.
- **Hitch case:** a 250 ms stall runs at most four ticks and reports
  the dropped ticks in stats.
- **Performance check:** WASM calls per frame are constant with respect
  to board size. Measure the full frame in a browser to evaluate cost.

## Open decisions

- Whether the desktop host reads the same snapshot buffer (recommended:
  yes, one interface for both hosts) or gets its own presentation
  path.

## References

- [Extending Homelab with a Rust Game Engine](forge-game-engine.md) —
  the `WorldSnapshot`, `FrameOutcome`, and `GameEvent` contracts this
  design implements.
- Gregory, *Game Engine Architecture*, 2nd ed., §§7.4–7.6 (game-loop
  timing and the fixed-step accumulator), §15.6 (the game loop in the
  runtime foundation), §§10.1–10.4 (rendering engine and CPU–GPU
  synchronization). Study notes in `roachmaster/study-notes` under
  `studies/gregory-game-engine-architecture-2e-2015/`.
- The `design-game-engine` skill (simulation-to-presentation trace)
  and its `design-game-rendering-pipeline` catalog entry for the
  follow-up camera and GPU work.
