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

## The problem, measured

The current interface is eight scalar WASM exports (`width`, `height`,
`cell`, `status`, `input`, `advance`, `clear_input`, `restart`). Every
frame the adapter calls `cell(x, y)` once per tile: 117 boundary
crossings per frame at Room One's 13×9, and the adapter caps boards at
64×64 (`max_dimension`) precisely because the read cost is O(board).
The adapter already redraws only changed cells, so the draw cost is
fine; the read cost is the bottleneck. The interface also carries no
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

One flat, versioned, little-endian buffer, written by the runtime
during `advance` and read by the adapter during paint. All integers are
fixed-width; there are no pointers, no strings, and no variable-length
sections except the count-bounded arrays below.

| Section | Contents |
| --- | --- |
| Header (20 bytes) | Magic `FGS1`, schema version `u16`, tick index `u32`, status `u32`, board width/height `u8`, entity count `u8`, event count `u8`, flags `u8`, reserved `u8` |
| Board | One byte per cell, row-major, `Cell` codes as today (floor, wall, key, locked exit, open exit, player) |
| Entities | Fixed-capacity array of 256 records: id `u16`, kind `u8`, state `u8`, current x/y `i32` fixed-point, previous x/y `i32` fixed-point. Room One publishes one record (the player); the capacity is reserved, not used. |
| Events | Ordered ring of 64 records: tick `u32`, sequence `u16`, kind `u8`, payload `u32`, reserved `u8`. Kinds for Room One: key collected, exit unlocked, completed. |
| Stats | Ticks run `u8`, ticks dropped `u8`, input edges dropped `u8`, events overflowed `u8` (see below) |

Total size is fixed: 20 + 4,096 (board at the 64×64 cap) + 256 × 20
(entities) + 64 × 12 (events) + 4 (stats) = 10,008 bytes every frame.
The buffer is fixed-size by design: the header counts say how much of
each array is live, the adapter allocates its read view once and reuses
it, and the runtime never allocates per frame.

## Transport: one stable buffer in WASM memory

The runtime exports `forge_playtest_snapshot() -> u32`, returning the
byte offset of the snapshot buffer in WASM linear memory, and
`forge_playtest_snapshot_len() -> u32`. The adapter creates one
`Uint8Array` view over the buffer and reads the snapshot from it each
frame: two scalar calls per frame instead of O(board). The buffer
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

Adapter safety rules: treat the memory as read-only; validate the
header (magic, supported version, width/height within the cap, counts
within their bounds) before reading anything else; on any validation
failure, show the existing failure message and disable the controls
rather than rendering a partial snapshot. A version the adapter does
not know is a failure, not a best-effort read.

## Phases and ownership

| Handoff | Producer → consumer | Phase | Data and owner | On failure |
| --- | --- | --- | --- | --- |
| Tick input | Adapter → runtime | Input, before `advance` | Queued edges; runtime owns the buffer | Overflowing edges are dropped and counted in stats |
| Snapshot | Runtime → adapter | Simulation, during `advance`; read during paint | Snapshot buffer; runtime owns the write, adapter owns the read view | Invalid header → failure message, controls disabled |
| Events | Runtime → adapter | Simulation writes; presentation drains once per frame | Event ring; runtime owns the write order, adapter owns the drain | Overflow drops the oldest event and sets the overflow count; an event is never delivered twice |
| Catch-up report | Runtime → adapter | Simulation, during `advance` | Stats section; runtime owns the counts | Dropped ticks are reported, never silently absorbed |

A single buffer suffices while the runtime and the adapter run on the
same thread: `advance` and paint are sequential inside one frame
callback, so the adapter can never observe a half-written snapshot.
If rendering ever moves to a worker, the snapshot becomes double
buffered (the runtime publishes to the back buffer and swaps at the
tick boundary); the layout and the adapter's read path do not change.

Interpolation: the snapshot carries previous and current positions
plus the tick index, and the adapter interpolates between them for
display using the frame's fractional time. Gameplay and collision keep
reading authoritative tick state, never the interpolated display
position. Room One's movement is tile-discrete, so interpolation is a
no-op there; the fields exist so the first smooth-moving entity needs
no interface change.

## Coordinates

The snapshot uses y-up world coordinates, per the proposal: y
increases upward, tiles are the world unit, and the canvas adapter
converts to y-down pixels. The runtime's internal map is y-down (row 0
is the top row), so the runtime flips y once, at publication, in the
single place that writes the snapshot. Baking the flip into the
interface now costs one conversion; baking y-down into the interface
would push the flip onto every future consumer (desktop host, physics,
tools). The adapter's tile size and dirty-cell painting are unchanged.

## Events: ordered, exactly once

Events are identified by (tick, within-tick sequence), per the
proposal. The adapter drains the ring once per frame, in order, and
each event fires its effect exactly once — the property audio cues
depend on, since a duplicated cue is audible. The ring holds 64
events; on overflow the oldest event is dropped and the overflow count
in the stats section increments, so a flood is visible rather than
silent. Room One generates at most three events per playthrough, so
overflow is a robustness bound, not an expected path.

## Catch-up reporting

`advance` gains a `FrameOutcome`: ticks run, ticks dropped (whole-tick
debt discarded after the four-tick cap), and fractional time preserved.
The counts land in the stats section each frame. This closes the gap
where the current `advance` returns nothing: a 250 ms hitch now
reports its dropped ticks instead of absorbing them silently. The
adapter surfaces a hitch only as a diagnostic for now; no UI change is
part of this design.

## Migration, in behavior-preserving slices

1. Add the snapshot buffer and its two exports alongside the scalar
   exports. No adapter change. Unit tests: layout offsets, header
   validation, version rejection.
2. Adapter reads the snapshot; the scalar `cell` export stays as a
   fallback for one release. The journey plays Room One to completion
   through the snapshot path. New assertion: at most a small constant
   number of WASM calls per frame.
3. Remove the scalar `cell` export and the adapter's `max_dimension`
   cap, replaced by the snapshot's bounds. The `width`/`height`
   exports go with it; dimensions come from the header.
4. Add events and stats to the snapshot and drain them in the
   adapter. Journey assertion: the key-collected and completed events
   each fire exactly once per playthrough.

Each slice keeps the game playable and the journey green; no slice
changes the simulation rules.

## Acceptance checks

- **Representative scenario:** the existing journey plays Room One to
  "You won!" and restarts, through the snapshot path, with no
  behavior change.
- **Failure case:** an unknown snapshot version (or a corrupt header)
  shows the failure message and disables the controls; nothing
  renders partially and nothing throws uncaught.
- **Hitch case:** a 250 ms stall runs at most four ticks and reports
  the dropped ticks in stats.
- **Performance check:** WASM calls per frame are constant (`advance`
  plus two snapshot calls, plus input on presses), independent of
  board size, at the 64×64 cap.

## Open decisions

- Whether the entity record needs a facing/direction field before the
  first moving entity, or whether it can wait for the entity design.
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
