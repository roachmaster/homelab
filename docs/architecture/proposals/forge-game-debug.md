# Debug and Trace Layer for the Forge Game Runtime

Make the running game inspectable inside Forge IDE: a bounded trace
that records what happened and when, a live state inspector, frame
timing against a budget, and pause/step control — so a bug can be
explained, not just reproduced.

**Planning baseline** — GitHub `main` at `6e1e70e` on 2026-11-17. This
design applies Gregory chapter 9 (tools for debugging and
development) and the synthesis's runtime-to-debugger boundary to the
[game engine proposal](forge-game-engine.md). It builds on the
[snapshot design](forge-game-snapshot.md). The inspector reads the
snapshot buffer, and the trace ring is the only new runtime interface.
Slices 1–2 can land before the snapshot implementation. Slice 3 requires it.

## The problem

Today a bug in the playtest leaves only its visible state. There is no
record of the input edges that led to it, the tick on which an event
fired, or whether a hitch dropped ticks — and no way to freeze a
transient state while inspecting it. The QA journey's screenshots are
evidence of the visible result, not of the simulation events behind
it. Gregory §9.7 says it directly: for a temporal issue, collect the
recording alongside input or diagnostic logs, so what appeared can be
related to a simulation event. The trace is that log, and the
screenshot is already captured.

## Representative scenario

A playtester reports that the exit sometimes opens but the win does
not register. With this layer, the trace shows the key-collected
event's tick, the exit-unlocked event's tick, and whether a
completion event was ever delivered; the inspector shows the current
status and the player's tile; pause freezes the state at the suspect
tick so the step control can advance it one tick at a time until the
divergence appears. Without it, the report is a screenshot and a
guess.

## What the layer is

Five pieces, each answering a separate question about the same
incident (§9, "Applications and misconceptions"):

1. **Trace ring** — what happened, in order, with tick and phase.
2. **State inspector** — what is true right now.
3. **Frame timing** — how long each stage took, against a budget.
4. **Pause/step** — freeze simulation time while the investigative
   tools keep running.
5. **Debug drawing and console** — see spatial relationships in the
   world, and drive the tools by command.

## Trace ring (runtime)

The runtime publishes one versioned trace image in WASM memory. Each
16-byte record contains sequence `u32`, tick `u32`, phase `u8`, channel
`u8`, stable target ID `u16`, and data `u32`. The 24-byte header stores
`FGT1`, version `u16`, count `u16`, run epoch `u32`, latest sequence
`u32`, dropped count `u32`, and four flag bytes. Bytes 20–22 carry the
channel mask and bit 0 of byte 23 indicates replay completeness. Up to
512 records follow in oldest-first order. The total size is 8,216 bytes.
The run epoch and sequence distinguish new records from repeated reads.

**Channels** (§9.1 — a channel selects the message's domain, verbosity
selects the amount of detail): input, simulation, events, level,
hitch, error. The adapter sets an enabled-channel mask through an
export; the runtime writes only enabled channels. Default: input,
events, hitch, and error on; simulation detail off. §9.1 warns that a
system emitting every routine update buries the relevant sequence and
can disturb the behavior being investigated — the mask is the answer.

**Overflow:** the ring drops the oldest record and increments the
dropped count in the header, so a flood is visible rather than silent
— the same policy as the snapshot's event ring.

**What each record must name** (synthesis): the resource, tick, phase,
and time domain that produced it. The payload carries the
channel-specific value. The input channel records the direction applied
at the tick, including repeats. The hitch channel records discarded ticks.

**Phases.** The phase byte introduces the phase schedule the engine
design still lacks: `Input → Simulate → Publish → Present → Drain`.
The runtime has one simulation phase today; these are the slots the
entity, animation, and physics designs will fill. The trace makes the
schedule explicit and measurable before the phases exist, so the
first multi-phase bug is diagnosable instead of a mystery.

**Exports:** `forge_playtest_trace()` and
`forge_playtest_trace_len()` return the buffer's offset and length,
mirroring the snapshot exports; `forge_playtest_trace_channels(mask)`
sets the enabled channels. The adapter drains the ring each frame
while the debug drawer is open, and on demand for export.

## State inspector (adapter and Forge IDE)

The inspector reads the snapshot buffer — no new runtime interface —
and shows tick, status, player position, input queue depth, event
counts, and hitch stats, live. For v1 it lives in a debug drawer on
the `/playtest` page (same origin, no new route). A Forge IDE panel
reading the same data through the dispatch gateway is a later step;
the inspector's data contract is the snapshot, so the panel is a
second consumer, not a second design.

## Frame timing (adapter)

Instrumented scopes (§9.8) around the frame loop: the frame scope is
inclusive; `advance`, snapshot read, and paint are measured children,
so exclusive time is visible and child intervals are never
double-counted. The adapter records invocation counts (a costly single
call looks different from an inexpensive call repeated many times)
and keeps p50/p95/worst over the last 120 frames, shown as a small
graph in the drawer with a JSON export for comparison (§9.8: capture
multiple frames and export the results).

**Budget** (synthesis's critical-path method): at 60 Hz the frame is
16.67 ms. Room One's budget is simulation ≤ 1 ms, snapshot read
negligible, paint ≤ 4 ms, total CPU ≤ 13 ms before present — the
headroom is the margin a future feature spends, and the timing view
shows when it is spent. The snapshot's hitch stats feed the same view:
a hitch is a timing event, not a mystery. §9.8's warning applies —
instrument only the scopes needed for the question, since collecting
measurements adds work — so the timing scopes are the four above, no
more, until a question needs finer ones.

## Pause and step (§9.5)

Pausing stops the wall-clock calls to `advance` and clears held input
and fractional time. The inspector and trace remain available. Step
calls `forge_playtest_step()`, which executes one simulation tick
without adding elapsed time. A player can enter a movement while paused
and apply it with Step. Resume starts from a fresh wall-clock sample.
Restart starts a new run epoch and a fresh trace.

## Debug drawing and console

**Debug drawing** (§9.2) reuses the adapter's typed `Paint` config:
grid coordinates, a player-tile highlight, input queue depth as a
bar, and event markers at the tile where the event fired. Primitives
are drawn where the underlying event occurred — a marker on the key's
tile says more than a coordinate in a log — and world-space primitives
follow the board while screen-space text stays fixed. Off by default,
toggled in the drawer; the documented scale is one tile per cell.

**Console** (§9.4): a minimal typed command set in the drawer —
`pause`, `resume`, `step`, `restart`, `trace` (dump the ring),
`channels <mask>`, `timing` (export the frame timings). Discoverable
names, predictable arguments, no `eval`, no scripting — a script
console is chapter 15.8 territory, not this design. The drawer
distinguishes actions that alter simulation (pause, step, restart)
from overlays that only report a value (§9.3).

## Memory (§9.9)

The inspector shows WASM linear-memory size (`memory.buffer.byteLength`).
The fixed memory regions are 10,284 bytes for the snapshot and 8,216
bytes for the trace. Linear-memory size measures reserved pages.

## Trace export and replay

The drawer exports the retained trace and timing samples as JSON.
Applied movement records carry their simulation tick. A capture is
marked incomplete when movement went unrecorded through filtering or
when the ring evicted records. The WASM regression test replays a
complete capture and checks the resulting room and completion tick.

## What this design does not do

- **Cheats** (§9.6) — deferred. A shortcut that bypasses collision
  "cannot establish whether that gate blocks movement correctly," so
  cheats must never be used as QA evidence. When they arrive, the
  design must record which rules were modified.
- **Remote debugging, GPU profiling, a full editor, a script
  console** — out of scope. There is no GPU work to profile yet
  (Canvas 2D), and the drawer is an inspection tool, not an editor.
- **Screenshot/movie capture** (§9.7) — the journey already captures
  rendered screenshots; this design adds the trace that makes them
  interpretable, not a new capture path.

## Handoffs

| Handoff | Producer → consumer | Phase | Data and owner | On failure |
| --- | --- | --- | --- | --- |
| Trace record | Runtime → adapter | Any (record names its phase) | Trace ring; runtime owns the write head, adapter owns the drain | Overflow drops the oldest record and counts it; a corrupt header means the drawer shows "trace unavailable", never a partial log |
| State | Runtime → inspector | Publish → Present | Snapshot buffer (see the snapshot design) | Invalid header → the inspector shows the failure state, as the adapter does |
| Frame timing | Adapter → drawer | Present → Drain | Per-frame scope timings; adapter owns the samples | A missing sample is a gap in the graph, not a fabricated value |
| Pause/step | Drawer → runtime | Input | Wall-clock calls are withheld. Step calls the explicit one-tick operation. | The single-threaded adapter issues the call after the current frame completes. |

## Migration, in behavior-preserving slices

1. Trace ring in the runtime plus the three exports; the adapter
   drains to the console, no UI. Unit tests: ring wrap, overflow
   count, channel mask, header validation.
2. Frame timing in the adapter plus pause/step controls on the
   playtest page. No drawer yet — the timing is visible in the
   console and the controls are buttons.
3. Debug drawer UI: inspector, timing graph, trace view, console,
   debug drawing toggle. Requires the snapshot implementation.
4. Trace export (JSON) plus the journey's replay check.

Each slice keeps the game playable and the journey green.

## Acceptance checks

- **Representative scenario:** play Room One to completion with the
  drawer open. The trace records applied movement, event delivery,
  and the completion tick.
- **Hitch case:** a forced 250 ms stall appears in the trace with its
  dropped-tick count, matching the snapshot stats.
- **Pause/step:** pause, step one tick — the inspector shows tick + 1
  and the player moved exactly one tile; the snapshot stays readable
  while paused.
- **Timing:** a 5 ms synchronous stall raises the recorded scope
  time. Compare CPU p95 with the stated frame budget on a reference
  browser and device.
- **Overhead:** trace on versus off changes frame time by less than
  the measurement noise; trace writes are a few stores per record,
  bounded by the channel mask.

## Open decisions

- Whether the debug drawer stays on `/playtest` long-term or becomes
  a Forge IDE panel first — the snapshot is the contract either way.
- Whether trace export is JSON only, or the raw binary ring as well
  for exact replay tooling.

## References

- Gregory, *Game Engine Architecture*, 2nd ed., chapter 9
  (§§9.1–9.9: logging and tracing, debug drawing, in-game menus,
  console, debug cameras and pausing, cheats, screenshots and movie
  capture, in-game profiling, memory stats and leak detection), and
  the whole-book synthesis's frame-budget method and
  runtime-to-debugger boundary. Study notes in
  `roachmaster/study-notes` under
  `studies/gregory-game-engine-architecture-2e-2015/`.
- [Extending Homelab with a Rust Game Engine](forge-game-engine.md)
  and the [simulation-to-presentation snapshot](forge-game-snapshot.md)
  this layer builds on.
