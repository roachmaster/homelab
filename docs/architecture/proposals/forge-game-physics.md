# Physics for the Forge Game Runtime

Give the game gravity, velocity, and collision response — so the
Scout can jump and fall under simulated motion instead of tile hops
and baked root offsets — while keeping the fixed tick,
determinism, and the snapshot interface exactly as they are.

**Planning baseline** — GitHub `main` at `7ba1235` on 2026-11-17.
This design applies Gregory chapter 12 (§§12.1–12.6), chapter 11
(§§11.4, 11.7, 11.11) for the animation bridge, chapter 7 (§7.5)
for the fixed step, and chapter 15 (§15.6) for the phase order,
and it follows the Forge package-boundary conventions (see "How this
integrates with the Forge philosophy" below).
It implements the proposal's staging: "Use simple tile collision
first. Add Rapier for a moving-platform or rigid-body milestone."

## The problem

Movement today is tile-discrete: an entity occupies one tile, and
a move is a one-tile step that either succeeds or is blocked. There
is no velocity, no gravity, no falling, and no jump — the Scout's
jump arc exists only as root offsets baked into the animation
recipe, and the pixel-art README already says that a game with its
own vertical motion must zero those offsets and use "the runtime
trajectory for world position." There is no runtime trajectory yet.
The snapshot already carries sub-tile positions (1/256-tile units)
and previous/current pairs for interpolation, but every published
position is a whole tile: the interface is ready, the simulation
is not.

## The §12.1 decision: what gets physics

§12.1 says the decision is granular: "A title can use queries for
aiming and animation, use rigid bodies for props, and reserve
tightly controlled movement for the player." For Forge's first
physics:

| Subject | Treatment | Reason |
| --- | --- | --- |
| Player | Character controller (§12.5): kinematic, gameplay-driven velocity, tile collision with a step-up rule | Tightly controlled movement; a full rigid body would fight the input model and make stairs and jumps unpredictable |
| Guard, moving platforms (later) | Kinematic: script or patrol supplies a target transform before the step; affects contacts, is not affected by them | The guard's patrol is authored, not simulated; the proposal already says gameplay supplies kinematic targets before stepping |
| Props (crates, debris — later slice) | Dynamic rigid bodies: gravity, impulse response, sleep | The first thing that needs real dynamics; nothing in Room One needs it yet |
| Terrain | Static: the tile grid, no bodies, no transforms | The board is already the static world |
| Animation ground checks | Queries: a downward ray/probe from the feet | The animation bridge needs "grounded?" without participating in dynamics (§12.3 phantom queries) |

What this design does not add: joints, stacking, cloth, fluids,
ragdolls (§12.6 — deferred, with the Rapier trigger in §11), and
slopes (the tile grid has no slope tile; a slope type is a future
level-design decision, not a physics one).

## Bodies and shapes

A body is an entity with a collider and a motion kind. The
collider is an axis-aligned box (AABB) derived from the recipe's
body hitbox: the hitbox's `at` point is the offset from the
entity's anchor (the recipe pivot), and its `size` is the box.
No circles, capsules, or polygons for v1 — the tile world is
rectangular, and an AABB against a tile grid is exact, not an
approximation (§12.3: a proxy should be efficient and stable for
gameplay contact; the render mesh's detail is irrelevant here).

Motion kinds are a typed enum, not a flag:

```rust
enum Motion { Static, Kinematic, Dynamic }
```

- **Static** bodies contribute shape only. They never move and
  never appear in the broad phase as movers.
- **Kinematic** bodies move only by authored or scripted targets,
  supplied before the physics step. They push dynamic bodies but
  take no impulses.
- **Dynamic** bodies integrate forces (gravity only, for v1) and
  take impulses from contacts.

Collision filters are a small typed matrix, not per-pair
configuration: terrain collides with everything; characters
collide with terrain and each other; props collide with terrain,
characters, and props; triggers (pickups, goal volumes) are
phantom queries, never contacts (§12.3). A projectile never
contacts its owner — the filter, not a special case in the
narrow phase.

## Units and spaces (authoritative)

Gregory §4 requires every vector to name its source space, destination space, and units. The following table is the only admitted contract inside the simulation:

| Quantity            | Representation          | Space / meaning                                      |
| ------------------- | ----------------------- | ---------------------------------------------------- |
| Entity position     | Q16 (1/65 536 tile)     | Tile space; origin at the top-left of the board      |
| Snapshot position   | Q8 (1/256 tile)         | Truncation of Q16; the runtime keeps the Q16 remainder so truncation never accumulates |
| Velocity            | Q16 per tick            | Same tile space                                      |
| Collider offset     | Q8 relative to pivot    | Recipe body-hitbox `at` point                        |
| Recipe pivot        | Pixel cell origin       | Authoring space; converted once at load              |
| Contact normal      | Axis-aligned unit       | +X, −X, +Y, or −Y only for v1                        |

No other spaces are admitted into the simulation step. Screen-space conversion lives only in the presentation layer. A future change that introduces a new space must update this table first.

## The step: one physics step per tick

The tick IS the physics step. There is no substepping for v1:
at 60 Hz, the fastest character moves 0.2 tiles per tick (see
§6), so one step per tick cannot miss a tile. The physics runs
inside `Simulate`, in the Move and Collide phases the entity
design already defines:

| Phase | Physics work |
| --- | --- |
| InputApply | Intended moves become velocity targets (walk/run speed, jump impulse on the jump edge) |
| Move | Integrate: velocity += gravity × dt; position += velocity × dt (semi-implicit Euler); kinematic targets applied |
| Collide | Broad phase (tile grid + entity AABBs), narrow phase (AABB vs tile, AABB vs AABB), resolution (positional push-out + velocity projection), grounded flags, contact events |

Integration is semi-implicit Euler (velocity first, then
position) in fixed point. All state is integer: positions in
Q16 (1/65,536 tile) internally, published as Q8 (1/256 tile) in
the snapshot by truncation — the runtime keeps the Q16 remainder,
so truncation never accumulates error. Velocities are Q16 per
tick. There are no floats anywhere in the runtime, so the replay
guarantee (same inputs, same state, across frame partitions)
extends to physics unchanged. The proposal's warning applies to
any future middleware: "verify repeatability under the selected
build and platform before relying on it."

Tuning constants, with the discrete behavior pinned by test
rather than by the continuous formula:

| Constant | Value | Discrete result (verified) |
| --- | --- | --- |
| Gravity | 32 tiles/s² (583 Q16/tick²) | — |
| Jump velocity | 8 tiles/s (8,738 Q16/tick) | Apex 0.93 tiles at tick 14, lands tick 29 |
| Run speed cap | 4 tiles/s | 0.067 tiles/tick |
| Fall speed cap | 12 tiles/s | 0.20 tiles/tick |

The continuous formula says apex 1 tile, airtime 30 ticks;
semi-implicit Euler gives 0.93 tiles and 29 ticks. The test
asserts the discrete numbers — the simulation's answer, not the
textbook's.

## Phase ordering with the entity model

Physics runs inside the phases already defined by the
[entity model](forge-game-entity-model.md). The mapping is fixed:

| Entity-model phase | Physics work |
| --- | --- |
| InputApply | Intended moves become velocity targets (walk/run speed, jump impulse on the jump edge) |
| Move | Integrate velocity and position; apply kinematic targets |
| Collide | Broad/narrow phase, resolution, grounded flags, contact events |
| Animate | Clip selection from the *same tick's* physics results (see the animation bridge) |
| Effects | Queued physics events (`Landed`, `HeadBump`, `WallContact`) are delivered |

A consumer that needs a peer's position reads the current tick's
generation tag (the snapshot tick index). No attachment may trail
by a tick. The physics design does not introduce new phase names;
it only fills the work that belongs inside the existing ones.

## Collision: tile grid, axis-separated resolution

**Broad phase.** The tile grid is the broad phase for terrain:
a body's AABB overlaps a bounded set of tiles (a 1-tile body
touches at most 4 tiles; the bound is max body size + 1 per
axis). Entity-vs-entity pairs use a simple sweep over the live
bodies — at most 256 entities, and only bodies with the Dynamic
or Kinematic kinds participate, so the pair count is bounded by
the body count, not the entity count. No spatial hash for v1;
the revisit trigger is a measured broad-phase cost, not a
feared one.

**Narrow phase.** AABB vs tile AABB, and AABB vs AABB, both
exact in fixed point. Contact information is the minimum
penetration axis and depth, plus the contact normal (axis-aligned
by construction).

**Resolution.** Axis-separated, in a fixed order: resolve X,
then Y. Each axis pushes the body out along the minimum
penetration and projects the velocity: the velocity component
into the surface becomes zero (restitution 0 for characters —
a landing is not a bounce). Resolving X before Y means a body
moving diagonally into a corner slides along the wall instead
of sticking, deterministically. There is no iterative solver
for v1 — one pass per axis, because tile contacts are
axis-aligned and non-penetrating after one correction. Stacking
and resting contacts need persistent resolution (§12.4); v1 has
no stacks, and the grounded flag (set when the Y resolution
pushes the body up out of a floor) is the resting-contact
answer for a character.

**Step-up.** The character controller steps up a 1-tile rise
automatically: if horizontal motion is blocked by a tile whose
top is exactly one tile above the body's feet, the body rises
onto it instead of stopping. This is the §12.5 "predictable
stairs" behavior, and it is a controller rule, not a physics
rule — a dynamic prop does not step up.

**Resting and sleep.** A dynamic body at rest on a surface with
near-zero velocity for N consecutive ticks sleeps: it is
excluded from integration until a contact or impulse wakes it.
Sleep is deterministic (a tick count, not a timer) and visible
in the trace. Characters never sleep — input can arrive on any
tick.

## Tunneling: bound the speed, sweep what exceeds it

§12.3: discrete tests observe snapshots, so a body can cross an
obstacle between them. The worked example (a 30 m/s ball at 60 Hz
moving 0.5 m per step, with both endpoints clear) applies
directly: at 60 ticks per second, a body moving 60 tiles/s
travels exactly 1 tile per tick — the thickness of the thinnest
wall. The design's answer is a bound plus a rule:

- Character speeds are capped far below the threshold (run 4,
  fall 12 tiles/s → at most 0.2 tiles per tick). Characters
  cannot tunnel, by construction.
- Any body faster than 60 tiles/s — a projectile — MUST use a
  swept AABB test (the volume the body occupies during the tick,
  §12.3), which finds the first time of impact within the step
  and resolves there. The test for this is the worked example
  restated in Forge units: a body at 90 tiles/s crossing a
  1-tile wall reports the contact at the correct sub-tick time.

The bound is a typed constant, and spawning a body above it
without the swept flag is a validation error, not a runtime
surprise.

**Swept acceptance (Forge units).** A body at 90 tiles/s that
starts just before a 1-tile wall must report contact at the
correct sub-tick time-of-impact and must never appear on the
far side of the wall in any published snapshot. A body at the
60 tiles/s threshold itself must also never cross a wall
undetected. The time-of-impact is recorded as a fraction of the
current tick (Q16) so the resolution can place the body at the
exact contact plane before the remaining velocity is applied.

## Transform authority (§12.5)

§12.5: "Assign transform authority per object and per update
phase." The authority table for Forge:

| Object | Who writes its transform | When | Who reads it |
| --- | --- | --- | --- |
| Dynamic body | Physics (Move/Collide) | During the tick | Snapshot, animation (Animate), trace |
| Kinematic body | Gameplay/script (before the step) | Before Move | Physics (as an obstacle), snapshot |
| Static terrain | Nobody (level data) | Load time | Physics (broad phase) |
| Animated pose | Animation (Animate) | After Collide | Snapshot (pose fields), renderer |

The entity, its rendered sprite, and its collider have
different local origins, and §12.5 warns that this is "a common
class of bugs in which the correct body motion appears in the
wrong place." The offsets are explicit and typed: the entity
anchor is the recipe pivot; the collider is the body hitbox's
`at` offset from the pivot; the sprite is drawn from the pivot
minus the cell offset (per the pixel-art README). No implicit
"center" anywhere.

The snapshot is the only publication path: physics writes
entity positions, the snapshot publishes them, the renderer
reads the snapshot. The renderer never sees a mid-step state,
and a gameplay query always sees the last published tick — the
"which snapshot" question §12.5 raises is answered by the
existing generation tag (the snapshot tick index).

## Events: physics speaks through the ring

Physics emits events through the existing event ring, with new
kinds: `Landed` (with impact speed, so the animation bridge
can distinguish a step down from a hard fall), `HeadBump`
(ceiling contact while rising), `WallContact` (sustained
horizontal block — for wall-slide feedback later). Each event
carries the stable target ID and the tile, as the existing
events do. Delivery is queued and exactly-once, at the Effects
phase, as the entity design requires. No event fires twice for
one contact: a contact that persists across ticks emits on its
first tick only.

A contact is identified by `(body-A, body-B-or-tile, axis,
first-seen-tick)`. The first-seen tick emits the event once;
subsequent ticks keep the contact for resolution and for the
grounded-flag calculation but do not re-emit. The grounded flag
is set when the Y-resolution pushes the body upward this tick,
or when the body was already resting and its vertical velocity
remains near zero. This is the resting-contact answer for a
character in the absence of an iterative solver.

## The animation bridge

This section is the contract the
[`bridge-animation-physics`](../../../.agents/skills/bridge-animation-physics/SKILL.md)
skill implements. The bridge answers one question per §11.7:
"which component gets the final say about a given joint" — and
per §11.11: who owns root motion.

**Root-motion ownership.** Physics owns world position;
animation owns the pose. The recipe's root offsets are a
preview device for the authoring tool: the pixel-art README
already requires zeroing airborne root offsets for a game with
its own vertical motion, and this design makes that a rule —
runtime animation never writes the entity's position. The
animation system receives the physics state and selects a clip;
the clip's joint rotations and limb poses are drawn relative
to the physics-owned root. A jump looks like a jump because
the body rises (physics) while the legs tuck (animation), not
because the clip moves the body.

**Clip selection from physics state.** The Animate phase reads
the same tick's physics results — grounded flag, horizontal
speed, vertical velocity, and the landing event — and selects
a clip:

| Physics state | Clip |
| --- | --- |
| Grounded, speed ≈ 0 | idle |
| Grounded, speed > 0 | walk / run by speed band |
| Airborne, vy > 0 (rising) | jump (rise frames) |
| Airborne, vy < 0 (falling) | jump (fall frames) or fall |
| Landing event this tick | land / crouch, then idle |

Selection runs in the Animate phase, after Collide, so the
clip always matches the current tick's physics — the synthesis's
pass-4 check ("physics and animation use the intended state
generation and the attachment does not trail by a tick") holds
by construction. A clip change mid-air (rising → falling) is a
phase change within the jump clip, not a clip swap, so the legs
don't snap.

**Time domains.** Clip time and simulation time are different
clocks (synthesis: "Make time domains explicit"). The animation
clock advances by tick dt (16,667 µs per tick); frame durations
come from the manifest's `duration_ms`. A frame boundary is
crossed when the accumulated clip time passes the frame's
duration — the frame changes at a tick boundary, never
mid-tick. Loop endpoint semantics (§11.4): the cycle's last
frame is followed by its first frame; the boundary pose is
counted once, so a loop has no hitch. A time step that crosses
a motion marker (a frame with an action box, or the landing
frame) processes the marker exactly once, even if the step
wraps the loop.

**Hitboxes vs colliders.** The recipe's body hitbox IS the
physics collider (one AABB, defined once, used by both
systems). The recipe's action boxes are phantom overlap
queries (§12.3): they detect what the attack touches without
participating in dynamics, and they exist only on the frames
where the clip marks them active. The bridge's rule: a body
hitbox change is a physics change (re-validate the collider);
an action-box change is an animation change (re-check the
query frames).

**Post-processing policy.** Physics owns the root; animation
owns the limbs; nothing else writes joints. There is no IK for
v1 — a 2D sprite's feet are drawn, not solved, and a foot
solver without a ground query and planted-foot knowledge would
"lock a swinging foot or sharply extend a knee" (§11.7). If a
future game needs foot planting on uneven ground, the solver
runs in the Animate phase after clip selection, reads the
ground query, and writes only the leg joints — the policy
stays: one writer per joint, named in advance.

**Interpolation.** The snapshot's previous/current positions
already interpolate the root between ticks; the animation frame
is selected per tick and does not blend sub-tick for v1. A
frame change is a discrete event at a tick boundary, and the
renderer's interpolation applies to the root only.

## Migrating an existing Scout recipe

Before a recipe can be driven by this physics design:

1. Zero every airborne root offset (`[0, 0]`) in the authoring
   tool and re-export. Keep limb rotations; discard only the
   root translation that previously simulated the jump arc.
2. Confirm greybox, final, and illustrated manifests agree on
   frame counts, durations, pivots, and body hitboxes.
3. Re-validate the body hitbox at load: non-zero size, entirely
   inside the cell, pivot offset consistent with the recipe.
4. Run the bridge acceptance tests (same-tick selection, no
   double motion, no trailing attachment, replay identity of
   clip selection).

A recipe that still carries airborne root offsets will produce
visible double-motion and is rejected by the bridge skill.

## Determinism and replay

Fixed point only; no floats; no wall-clock input to the step;
iteration order by entity index; axis resolution in a fixed
order. The replay test extends to physics: the same inputs,
chunked differently, reach the same positions, the same
landing tick, and the same events. A physics divergence is a
test failure, not a tuning note. The trace records physics
events (contacts, landings, sleep/wake) on the simulation
channel with the phase tag, so a divergence is diagnosable
from the log.

## Bounds

| Bound | Value | Reason |
| --- | --- | --- |
| Bodies | ≤ 256 (MAX_ENTITIES) | The entity table's size; bodies are entities |
| Body size | ≤ 4×4 tiles | Bounds the broad-phase tile set (≤ 25 tiles) |
| Speed | ≤ 60 tiles/s, swept above | The tunneling threshold (§6) |
| Contacts per body per tick | ≤ 8 | Tile contacts (≤ 4) + entity contacts (≤ 4); overflow drops the deepest and counts it |
| Sleep | After 30 ticks at rest | Deterministic, visible in the trace |

The tick path allocates nothing; all physics state lives in
fixed arrays beside the entity table.

## Middleware: Rapier later, behind an adapter

§12.2: evaluate middleware "on the actual game's collision
geometry, active object count, controller behavior and query
patterns," and confine SDK behavior behind "a shared engine
adapter." The trigger for Rapier is a concrete need this design
cannot meet: non-tile shapes (ramps, circles), joints (a hinged
door, a rope), or stacking (crates that pile). Until one of
those arrives, custom tile physics is the whole engine — it is
simpler, exactly deterministic, and zero-dependency, and the
proposal stages Rapier the same way.

When the trigger fires, the integration follows the proposal:
gameplay supplies kinematic targets before stepping, the solver
owns dynamic body transforms, the renderer reads the snapshot.
The seam is the physics step itself: the runtime's `step`
function takes the entity table and the input state and returns
the resolved positions and events. Replacing the tile solver
with a Rapier-backed solver replaces that module, not the game
— and the repeatability check (same inputs, same state, on the
target build and platform) gates the swap. The adapter expresses
Forge concepts (stable IDs, tile coordinates, the event ring)
and confines Rapier types to the adapter module.

## How this integrates with the Forge philosophy

- **Typed contracts.** Motion kinds, contact normals, and event
  kinds are enums, never strings or flags; colliders are typed
  AABBs with explicit anchor offsets. No untyped payloads
  anywhere in the step — the package-boundary rule applies
  inside the sim, not just at the Forge bridge.
- **Bounded everything.** Bodies, contacts, speeds, and sizes
  are capped in the table in "Bounds" above; the tick path allocates
  nothing; iteration is by index, never by hash map, so the
  step order is deterministic.
- **Package boundaries.** Physics lives in `forge-game-runtime`,
  which stays zero-dependency: no new crate, no new
  dependencies, no change to the architecture guard. It stays
  in the runtime crate because bodies share the tick invariant
  with the room — a separate crate would create pass-through
  types and coordinated versioning for no isolation gained.
  The Forge side (adapter, IDE) sees only the snapshot and the
  trace, never runtime internals. The runtime does not use the
  Forge bridge: per the proposal it uses ordinary Rust
  construction, and the bridge pattern applies at the Forge
  dispatch boundary, not inside the simulation.
- **Determinism.** The step is a pure function of (state, inputs)
  for a given tick; no wall-clock, no floats, fixed iteration
  order. Replay identity extends to physics events and clip
  selection.

## Caveats and things to consider

- **Units are a contract.** Introducing a new space or representation
  without updating the "Units and spaces" table is a design error.
- **No sub-stepping in v1.** One physics step per tick is intentional.
  If a future body needs finer resolution, add sub-stepping behind
  the same `step` seam so the snapshot interface stays unchanged.
- **Restitution is zero for characters.** Landings are not bounces.
  Prop restitution is tuned only when the first dynamic prop lands.
- **Sleep is deterministic.** A tick count, never a wall-clock timer.
  Characters never sleep.
- **Rapier is a later adapter, not a rewrite.** The seam is the
  physics `step` function. The repeatability check on the target
  build gates any middleware swap.
- **Animation bridge is mandatory for any airborne motion.** A
  recipe that still carries airborne root offsets must not be
  published; the bridge skill owns the rejection criteria.

## Open decisions

- **Slopes — deferred.** The tile grid has no slope tile. If a
  level needs one, the tile type and the controller's slope
  rule arrive together, as a level-design decision.
- **Moving platforms — mechanism ready, first use deferred.**
  Kinematic targets are the mechanism; the first platform
  arrives with the first level that needs one.
- **Prop restitution and friction — tuned when props land.**
  The constants are named in slice 4, not guessed now.

## References

- Gregory, *Game Engine Architecture*, 2nd ed., chapter 12
  (§§12.1–12.6: whether to have physics, middleware,
  collision detection, rigid body dynamics, integration,
  advanced features) and chapter 11 (§§11.4, 11.7, 11.11:
  clips, post-processing, action state machines), plus
  chapter 7 (§7.5: the fixed step) and chapter 15 (§15.6:
  phase ordering). Study notes in `roachmaster/study-notes`
  under `studies/gregory-game-engine-architecture-2e-2015/`.
- [Extending Homelab with a Rust Game Engine](forge-game-engine.md)
  (physics staging, Rapier trigger), the
  [entity model](forge-game-entity-model.md) (phase schedule,
  events), the
  [simulation-to-presentation snapshot](forge-game-snapshot.md)
  (Q8 positions, interpolation), and the
  [pixel-art authoring README](../../../src/python/pixel-art/README.md)
  (root-motion rule, hitboxes, manifest).
- The `design-package-boundaries` skill (`.agents/skills/`) for
  the typed-contract, boundedness, and dependency-direction
  conventions applied throughout, summarized in "How this
  integrates with the Forge philosophy" above.
