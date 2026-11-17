# Physics for the Forge Game Runtime

Give the game gravity, velocity, and collision response — so the
Scout can jump and fall under simulated motion instead of tile hops
and baked root offsets — while keeping the fixed tick,
determinism, and the snapshot interface exactly as they are.

**Planning baseline** — GitHub `main` at `7ba1235` on 2026-11-17.
This design applies Gregory chapter 12 (§§12.1–12.6), chapter 11
(§§11.4, 11.7, 11.11) for the animation bridge, chapter 7 (§7.5)
for the fixed step, and chapter 15 (§15.6) for the phase order,
and it follows the Forge package-boundary conventions.
It implements the proposal's staging: "Use simple tile collision
first. Add Rapier for a moving-platform or rigid-body milestone."

## The problem

Movement today is tile-discrete. There is no velocity, gravity,
falling, or jump — the Scout's jump arc exists only as root offsets
baked into the animation recipe. The snapshot already carries
sub-tile positions; the simulation does not yet produce them.

## The §12.1 decision: what gets physics

| Subject | Treatment | Reason |
| --- | --- | --- |
| Player | Character controller: kinematic, gameplay-driven velocity, tile collision with step-up | Tightly controlled movement |
| Guard, moving platforms (later) | Kinematic: script supplies target before the step | Authored, not simulated |
| Props (later) | Dynamic rigid bodies: gravity, impulse response, sleep | First real dynamics |
| Terrain | Static tile grid | Already the static world |
| Animation ground checks | Queries: downward ray from the feet | Phantom, no dynamics |

Deferred: joints, stacking, cloth, fluids, ragdolls, slopes.

## Bodies and shapes

Collider is an AABB from the recipe body hitbox (`at` offset from
pivot, `size` the box). Motion kinds: `Static`, `Kinematic`,
`Dynamic`. Filters: terrain/characters/props matrix; projectiles
never contact their owner. Shared shapes (shape ID table, bound 64)
arrive when many instances share one definition. Compounds deferred
to Rapier.

## Units and spaces (authoritative)

| Quantity | Representation | Space |
| --- | --- | --- |
| Entity position | Q16 (1/65536 tile) | Tile space, origin top-left |
| Snapshot position | Q8 (1/256 tile) | Truncation of Q16 |
| Velocity | Q16 per tick | Same tile space |
| Collider offset | Q8 relative to pivot | Recipe body-hitbox `at` |
| Recipe pivot | Pixel cell origin | Authoring space |
| Contact normal | Axis-aligned unit | +X/−X/+Y/−Y |
| Time-of-impact | Q16 fraction of a tick | Swept tests and rays |

No other spaces are admitted into the simulation step.

## The step: one physics step per tick

| Phase | Physics work |
| --- | --- |
| InputApply | Velocity targets (walk/run, jump impulse) |
| Move | Integrate (semi-implicit Euler); kinematic targets |
| Collide | Broad/narrow phase, resolution, grounded flags, contacts |

Tuning (discrete, pinned by test): gravity 32 tiles/s², jump 8 tiles/s
(apex 0.93 tiles at tick 14, lands tick 29), run cap 4, fall cap 12.

## Phase ordering with the entity model

| Entity-model phase | Physics work |
| --- | --- |
| InputApply | Velocity targets |
| Move | Integrate; kinematic targets |
| Collide | Broad/narrow, resolution, grounded, contacts |
| Animate | Clip selection from same tick's physics |
| Effects | Deliver `Landed`, `HeadBump`, `WallContact` |

Consumers read the current tick's generation tag. No attachment trails.

## Collision: tile grid, axis-separated resolution

**Collision world.** Named structure: collidable set, static tile grid,
persistent contact set. Operations: add/remove/update. V1 rebuilds from
the entity table each tick; mid-tick spawns visible next tick.

**Broad phase.** Tile grid for terrain; entity sweep for Dynamic/Kinematic
(bound by body count ≤ 256).

**Narrow phase.** AABB vs tile / AABB vs AABB, exact fixed-point SAT.
Touching is exact — no epsilon.

**Resolution.** X then Y; push-out + velocity projection; restitution 0
for characters. Grounded = Y push-up or resting near-zero velocity.

**Step-up.** Character controller only; 1-tile rise.

**Sleep.** Dynamic bodies after 30 ticks at rest; deterministic tick count.
Characters never sleep. Islands deferred.

## Tunneling

Characters capped well below 60 tiles/s. Bodies above 60 tiles/s must use
swept AABB. Acceptance: 90 tiles/s body reports correct sub-tick TOI and
never appears past the wall; 60 tiles/s threshold never crosses undetected.

## Queries: rays, sweeps, and phantoms

| Query | Returns | Used by |
| --- | --- | --- |
| Ray | Option<Hit> (point, normal, distance, target) | Ground probe, aiming |
| Swept shape | First TOI + contact | Fast bodies above speed bound |
| Phantom / overlap | Bounded set of stable IDs (≤ 8) | Action boxes, triggers |

Queries read the world as of the last completed Collide phase. A query
never moves a body.

## Transform authority (§12.5)

| Object | Writer | When |
| --- | --- | --- |
| Dynamic body | Physics (Move/Collide) | During the tick |
| Kinematic body | Gameplay (before step) | Before Move |
| Static terrain | Nobody (level data) | Load time |
| Animated pose | Animation (Animate) | After Collide |

Explicit offsets: entity anchor = recipe pivot; collider = hitbox `at`;
sprite from pivot. Snapshot is the only publication path.

## Events

`Landed` (with impact speed), `HeadBump`, `WallContact`. Contact identity:
`(body-A, body-B-or-tile, axis, first-seen-tick)`. First-seen emits once;
persistence does not re-emit.

## The animation bridge

Physics owns world position; animation owns the pose. Airborne root offsets
must be zeroed. Clip selection from same-tick physics state. Body hitbox IS
the collider; action boxes are phantom queries. One writer per joint.

## Migrating an existing Scout recipe

1. Zero airborne root offsets and re-export.
2. Manifest parity (greybox/final/illustrated).
3. Hitbox re-validation at load.
4. Bridge acceptance tests.

## Determinism and replay

Fixed point only; no floats; fixed iteration order. Same inputs → same
positions, landing tick, events, and clip selections.

## Bounds

| Bound | Value |
| --- | --- |
| Bodies | ≤ 256 |
| Body size | ≤ 4×4 tiles |
| Speed | ≤ 60 tiles/s (swept above) |
| Contacts per body per tick | ≤ 8 |
| Sleep | After 30 ticks at rest |

## Middleware: Rapier later

Trigger: non-tile shapes, joints, or stacking. Seam is the physics `step`
function. Repeatability check gates the swap.

## How this integrates with the Forge philosophy

Typed contracts, bounded everything, package boundaries (stays in
`forge-game-runtime`, zero new dependencies), determinism.

## Surface properties (seam with the asset pipeline)

§12.3 and the asset pipeline share a surface-properties table.
A surface ID (from a cooked level chunk or resident table) names
restitution and friction for a tile or body. For v1:

- Characters use restitution 0 and a single default friction.
- Dynamic props read the surface ID of the contact when they land.
- Physics never invents surface numbers; missing IDs fail at load.

This is the only physics-visible output of the asset pipeline beyond
collider shapes and the tile grid.

## Diagnostic visualization

Gregory's applications section asks for overlays of broad-phase bounds,
contact normals, centers of mass, and sleep state. For Forge:

| Overlay | Source | When visible |
| --- | --- | --- |
| Body AABB | Collision world | Debug draw toggle |
| Contact normal + depth | Persistent contact set | Debug draw toggle |
| Grounded flag | Body state | Debug draw toggle |
| Sleep state | Body state (dynamic only) | Debug draw toggle |
| Ray hit point | Last ray query result | Debug draw toggle |

Overlays are presentation-only; they must not affect the step or allocate
on the tick path. Acceptance tests for contacts and rays should be runnable
with overlays on so a wrong normal is visible immediately.

**Static tile geometry.** Tile cells are closed solids. There is no open
triangle mesh and therefore no one-sided vs two-sided contact policy.
A body that enters a solid tile is pushed out; there is no "inside" state.

## Caveats and things to consider

- **Units are a contract.** New spaces must update the units table first.
- **No sub-stepping in v1.** One physics step per tick.
- **Restitution is zero for characters.** Landings are not bounces.
- **Sleep is deterministic.** Tick count, never a wall-clock timer.
- **Rapier is a later adapter, not a rewrite.** Seam is `step`.
- **Animation bridge is mandatory for airborne motion.** Zero root offsets.
- **Collision world rebuilt per tick in v1.** Mid-tick spawns visible next tick.
- **A query never moves a body.** Only the step resolves contacts.
- **Surface properties are looked up by ID.** Missing IDs fail at load.
- **Debug overlays are presentation-only.** No tick-path allocation.

## Open decisions

- **Slopes — deferred.** Tile type and controller rule arrive together.
- **Moving platforms — mechanism ready, first use deferred.**
- **Prop restitution and friction — tuned when props land.**

## References

- Gregory, *Game Engine Architecture*, 2nd ed., chapters 7, 11, 12, 15.
  Study notes in `roachmaster/study-notes` under
  `studies/gregory-game-engine-architecture-2e-2015/`.
- forge-game-engine.md, forge-game-entity-model.md, forge-game-snapshot.md,
  pixel-art README, design-package-boundaries skill.
