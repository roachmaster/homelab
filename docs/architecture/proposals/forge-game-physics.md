# Physics for the Forge Game Runtime

**Implementation reference — reviewed 18 November 2026.** The initial physics
proposal has become a bounded character-controller slice. This document names
actual APIs; the broader dynamic-body design remains future work.

## Implemented scope

| Area | Current implementation |
| --- | --- |
| Room One | Discrete four-direction puzzle movement, key, exit and authored guard patrol. |
| Traversal Yard | Side-view `physics::Character` with gravity, grounded jump edges, X/Y swept tile contacts and bounded step-up. |
| Queries | `sweep_tiles`, `cast_ray`, `Room::cast_ray`, `Room::collision_bodies`, and bounded filtered `Room::phantom_overlap`. |
| Animation | `Animator` derives Idle, Walk, Rise, Fall and Land from the resolved `MotionFrame`. |
| Publication | Existing FGS1 world snapshot plus same-epoch/tick/player FGP1 pose and separate FGC1 collision diagnostics. |

Source: [runtime](../../../src/rust/forge-game-runtime/src/lib.rs),
[controller and sweeps](../../../src/rust/forge-game-runtime/src/physics.rs),
[room queries](../../../src/rust/forge-game-runtime/src/queries.rs),
[animation](../../../src/rust/forge-game-runtime/src/animation.rs),
[publication](../../../src/rust/forge-ide-web/src/playtest.rs).

## Fixed tick and motion

`Room::advance` accumulates elapsed microseconds at 60 Hz. It runs at most four
fixed ticks per call, retains fractional time and reports discarded whole-tick
debt. `Action` press/release edges are bounded; a short tap survives a frame with
no simulation tick. `EffectiveInput` records the input actually consumed at each
tick for replay. Pause, focus loss and restart reset the applicable input/clock
state through the Rust boundary.

`Character::step` chooses horizontal velocity from `Horizontal`, applies a jump
impulse only on a grounded pressed edge, advances gravity with a fall-speed cap,
then sweeps horizontal and vertical motion against the tile grid. A grounded
non-jumping character may perform the bounded step-up test. A blocked axis stops
at the contact plane and zeroes that velocity component. Spawning inside terrain
or outside valid bounds is rejected.

The actual room phases are input, simulation and publication. Move, collision,
animation and event operations occur in a stable order within simulation; they
are not a public scheduler of independently registered phase workers.

[Implemented fixed-tick sequence](../diagrams/mermaid/images/game-physics-tick.svg) ·
[Editable source](../diagrams/mermaid/game-physics-tick.mmd)

## Units and authority

Simulation uses signed Q16 tile units (`TILE = 65,536`), positive X right and Y
down. The collider is an axis-aligned foot-relative box derived from the approved
Scout body and logical pivot. The platform character owns its resolved root;
`Animator` selects local artwork. There is no second animation-authored root
trajectory in the active physics jump.

The publication adapter converts previous/current roots to y-up Q8 FGS1 entity
positions. The 32-byte FGP1 companion carries magic, room epoch, tick, player ID,
motion phase, facing, frame, grounded/contact flags and selected contact facts.
The generated renderer checks matching identity before joining the two buffers.
Atlas scale changes artwork resolution, not collision units.

[Authority diagram](../diagrams/mermaid/images/game-physics-authority.svg) ·
[Snapshot contract](forge-game-snapshot.md)

## Contacts, queries and diagnostics

`MotionFrame` retains ground/head/wall contacts plus edge-triggered `landed`,
`head_bump` and `wall_contact` values. Continuing support maintains grounded state;
new contact edges feed the bounded event ring rather than replaying a landing
on every tick. Contact data includes tile, normal, penetration and impact speed.

`Room::cast_ray` selects terrain or configured entity targets with stable tie
ordering. `phantom_overlap` returns bounded stable IDs and a dropped-hit count;
touching faces do not count as positive-area overlap. Queries observe the last
completed room state. The controller uses its own current candidate while
resolving motion. Combat hitbox timing can reuse this seam, but there is no
implemented punch/kick damage controller.

The FGC1 diagnostic buffer contains body bounds, contacts and a forward ray.
The generated Debug tools draw them outside the simulation and expose the pose,
clock, trace and replay state. No sleeping-body state is implemented.

[Landing and publication](../diagrams/mermaid/images/game-physics-animation.svg) ·
[Diagnostic writer](../../../src/rust/forge-ide-web/src/playtest_collision.rs)

## Authored data and art

`Level::validate` checks dimensions, solids, unique spawns, required player/key/
exit, optional guard patrol, named surface, timing and collider. Surface friction
and restitution fractions are validated metadata. The current character
controller uses fixed movement constants and stops blocked axes; it is not a
material-friction or restitution solver.

The cooker zeros the Scout jump's authored airborne root offsets in a temporary
recipe, preserves limb poses, and materializes timing/collider data in each
level. Optional atlas failure uses primitive drawing without changing physics
or animation timing. The 4x illustrated atlas retains the same logical footprint.

Source: [level validation](../../../src/rust/forge-game-runtime/src/level.rs) ·
[cooker](../../../src/python/game-assets/README.md)

## Factory/provider boundary

`Room`, `Level`, `Character`, `Animator` and query values are pure Rust simulation
objects without device or I/O ownership, so they use ordinary construction.
Native host dependencies use Forge registrars, freeze and injected factories/
providers. The page-scoped generated I/O adapter owns its worker; messages carry
bounded asset identity and request generation rather than a shared registry.

## Verification and remaining work

Native tests exercise sweeps, contact planes, step-up bounds, overlap filters and
ray ordering. The actual-WASM playtest checks pose identity, the 29-tick jump,
level admission and Traversal Yard completion. Browser acceptance exercises the
published adapter. These are test contracts; fresh run results belong to the
current QA output.

General dynamic bodies, body-body response, joints, stacking, sleeping, slopes,
a middleware adapter and playable ledge/rope/ladder/barrel/combat states remain
future work. Having those animation clips does not make them gameplay mechanics.
No new physics dependency is required by the implemented tile controller.

[Engine reference](forge-game-engine.md) ·
[Asset pipeline](forge-game-asset-pipeline.md) ·
[Actual-WASM tests](../../../src/rust/forge-ide-web/tests/playtest-wire.mjs)

The design rationale draws on Gregory, *Game Engine Architecture*, second
edition, chapter 12 and the study collection retained in Study Notes.
