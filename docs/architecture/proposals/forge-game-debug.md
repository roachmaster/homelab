# Debug and Trace Layer for the Forge Game Runtime

**Implementation reference — reviewed 18 November 2026.** `/playtest` has a
Debug tools drawer with inspection, pause/step, channels, overlays, trace/timing
exports and full replay controls. The original no-debug baseline is historical.
This is a local game debugger, not a new remote Forge command endpoint.

## Implemented pieces

| Piece | Current responsibility |
| --- | --- |
| Rust trace ring | Bounded ordered records for input, simulation, events, level, hitch and error channels. |
| State inspector | Reads committed world/pose/diagnostic data and shows room/tick, position, status, contact and loss information. |
| Timing view | Generated browser adapter measures advance, snapshot, paint and inclusive frame scopes and a rolling summary. |
| Pause and step | Rust owns the pause gate and explicit one-tick operation; browser controls remain available. |
| Debug drawing | Canvas overlays body boxes, contacts, ray hits and event/location information without mutating the game. |
| Capture and replay | Separate trace/timing JSON exports and the Rust-owned full session recorder/import/playback path. |

Source: [runtime trace](../../../src/rust/forge-game-runtime/src/lib.rs),
[wire writer](../../../src/rust/forge-ide-web/src/playtest_wire.rs),
[debug adapter](../../../src/rust/forge-ide-web-host/src/design/playtest_debug_browser.rs),
[typed adapter declarations](../../../src/rust/forge-ide-web-host/src/design/playtest_browser.rs).

## Trace contract

FGT1 is a fixed 8,216-byte little-endian buffer: a 24-byte header and up to
512 records of 16 bytes. Records contain sequence, tick, phase, channel, stable
target ID and bounded numeric data. The header carries version, count, room
epoch, latest sequence, loss count, channel mask and replay-completeness flag.
The runtime owns writes; the generated browser reader validates the view.

A channel mask controls recording. The default enables input, events, level,
hitch and error while omitting routine simulation detail. Ring overflow evicts
the oldest record and increments loss. The Rust phase codes used here are
input, simulation and publication; browser timing/presentation scopes are not
additional independently scheduled Rust workers.

`forge_playtest_trace`, `forge_playtest_trace_len` and
`forge_playtest_trace_channels` expose this contract. The drawer reads retained
records while visible and on export. Trace completeness can be lost through
filtering or eviction; it is not the same as full session recorder completeness.

## State, timing and memory

The inspector reads the FGS1 world plus matching FGP1 pose and FGC1 collision
buffers. It includes tick/epoch, level identity, status, player/pose, clock/input/
event loss, bodies, contacts and ray results. Diagnostic drawing consumes
committed state after the tick and cannot change replay results.

The generated adapter keeps a rolling 120-frame sample window and displays
p50/p95/worst for its measured scopes with a 16.67 ms frame-budget line. Those
measurements are browser observations, not guaranteed simulation/GPU deadlines.
The source does not establish a 1 ms physics budget or a target-hardware
performance result. Timing samples can be exported as JSON for a controlled run.

The inspector shows WASM linear-memory size and level-handoff start/peak pages.
Linear memory is reserved WASM memory, not total browser RSS or a complete native
heap/GPU-memory measurement. Fixed publication buffers are 10,284 bytes (FGS1),
8,216 (FGT1), 32 (FGP1), and 5,216 (FGC1).

## Pause, controls and commands

Rust's playtest host owns the paused flag, clears applicable input/clock debt on
lifecycle boundaries and supports one explicit tick while paused. Resume starts
from a fresh browser time sample. Restart creates the appropriate new room epoch
and records its replay boundary.

The drawer supplies pause/resume, step, restart, channel controls, overlays and
named capture actions. Its bounded command vocabulary includes `pause`,
`resume`, `step`, `restart`, `trace`, `channels <mask>` and `timing`. It does not
execute arbitrary source or expose an `eval`/script console. Input while paused
can be applied by Step without allowing background frame time to run ticks.

[Playtest host](../../../src/rust/forge-ide-web/src/playtest.rs) ·
[collision diagnostics](../../../src/rust/forge-ide-web/src/playtest_collision.rs)

## Full replay and historical assets

The 512-record ring is an incident/debug history and may evict earlier input.
The separate Rust `Timeline` records effective per-tick input, room segments,
manifest digests and activation/restart boundaries. Complete session exports are
bounded to 36,000 ticks, 128 segments and 2 MiB. Overflow marks an export
incomplete while gameplay continues.

Import validates the full session, obtains any missing retained historical
manifest/level bytes through a generation-guarded archive port, then creates a
separate replay view. Step, play and restart of that replay do not replace live
state. The configured host archive is required when an older cooked generation
is no longer resident; no missing historical data is silently substituted.

[Replay state](../../../src/rust/forge-ide-web/src/playtest_replay.rs) ·
[replay browser adapter](../../../src/rust/forge-ide-web-host/src/design/playtest_replay_browser.rs) ·
[asset retention](forge-game-asset-pipeline.md#replay-retention)

## Verification and remaining work

Native tests cover ring bounds, input/clock behavior and pause semantics.
Actual-WASM tests cover snapshot/trace validation, effective input recording,
incomplete captures, level transitions and replay. The browser journey exercises
pause/step, export/import and the visible drawer. Fresh results belong to the
current QA run; a timing chart alone is not a performance certification.

A remote IDE debugger, GPU profiler, memory allocator inspector, video capture,
cheat system, editor and script console are not implemented by this slice.
Browser screenshots remain QA artifacts rather than a new runtime recording
service. The architecture uses pure Rust simulation values and the existing
generated browser effect/presentation boundaries.

The rationale applies Gregory, *Game Engine Architecture*, second edition,
chapter 9. Earlier proposed migration steps remain in Git history.
