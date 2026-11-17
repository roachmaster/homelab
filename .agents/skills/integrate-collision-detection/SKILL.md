---
name: integrate-collision-detection
description: Add collision shapes, queries, and contacts to the game through the collision world. Use for colliders, ray casts, swept tests, phantom overlap queries, and contact events.
---

# Integrate collision detection

Start from the [physics design](../../../docs/architecture/proposals/forge-game-physics.md).
The collision world is the structure: it holds the collidable
set, the static tile grid, and the persistent contact set, and
every shape, query, and contact goes through it. Nothing
queries the entity table directly for collision — the world is
the single source of truth for what is where.

## Register a collidable

1. **Define the shape.** A collider is an AABB derived from the
   recipe's body hitbox: the hitbox's `at` point is the offset
   from the entity anchor (the recipe pivot), and its `size` is
   the box. A shape with zero size, or one that extends outside
   the cell, is rejected at load with the part and frame named.
   No circles, capsules, or polygons for v1 — the tile world is
   rectangular, and an AABB against a tile grid is exact.
2. **Choose the motion kind.** Static (terrain, never moves),
   Kinematic (script or patrol supplies a target before the
   step; pushes but takes no impulses), or Dynamic (integrates
   gravity, takes impulses). The kind is a typed enum, not a
   flag, and it decides what the step does with the body.
3. **Set the filter.** The collision filter is a typed matrix:
   terrain collides with everything; characters collide with
   terrain and each other; props collide with terrain,
   characters, and props. A projectile never contacts its owner
   — the filter expresses that, not a special case in the
   narrow phase. Triggers (pickups, goal volumes) are phantom
   queries, never contacts.
4. **Register in the world.** Add the collidable with its shape
   reference, transform, motion kind, and filter. For v1 the
   world is rebuilt from the entity table each tick, so a body
   spawned mid-tick is visible to queries only from the next
   tick. Do not work around this with a mid-tick query against
   the entity table.

## Choose the query

Each query has one job. Pick by the question, not by habit:

| Need | Query | What you get |
| --- | --- | --- |
| Is there ground below these feet? | Ray (short, downward) | `Some(Hit)` with the floor tile and distance, or `None` |
| What does this shot hit first? | Ray (long, aimed) | The nearest hit point, surface normal, distance, target |
| Will this fast body hit a wall this tick? | Swept shape | First time-of-impact (Q16 fraction) + contact |
| What does this action box overlap? | Phantom / overlap | Bounded set of stable IDs (≤ 8), no motion |

**Ray.** `raycast(origin, direction, max_distance)` walks tiles
and tests bodies, returning the nearest hit. Empty space returns
`None` — a normal answer, not an error. The hit point is exact
(Q16); the normal is the entry face. A ray never moves a body
and never creates a contact. The ground probe is a short
downward ray from the feet; it is what future foot planting
reads.

**Swept shape.** Issued only by the step for bodies above the
speed bound. Finds the first impact within the tick and resolves
there. See the physics design's tunneling section for acceptance
numbers (90 tiles/s and 60 tiles/s cases).

**Phantom.** Overlap against a volume with the same filters as
contacts but no response. Result is bounded (≤ 8; overflow drops
the farthest and counts it). Action boxes and triggers are
phantom queries: they detect, they never push.

All queries read the world as of the last completed Collide
phase. A query issued mid-step sees the previous tick's world;
a query issued in the Animate phase sees the current tick's.
The phase a query runs in is part of its contract — record it
per use.

## Contact identity

A contact is identified by `(body-A, body-B-or-tile, axis,
first-seen-tick)`. The first-seen tick emits the event once;
subsequent ticks keep the contact for resolution and for the
grounded-flag calculation but do not re-emit. The grounded flag
is set when the Y-resolution pushes the body upward this tick,
or when the body was already resting and its vertical velocity
remains near zero. A contact that persists across ticks never
fires twice.

## Verify the integration

- **Ray.** A downward ray from above the floor returns the
  floor tile, an upward normal, and the exact distance. A ray
  through empty space returns `None`. A ray at a body returns
  the body's stable ID and the entry face.
- **Swept.** A body at 90 tiles/s crossing a 1-tile wall
  reports the contact at the correct sub-tick time-of-impact.
  A body at the 60 tiles/s threshold never crosses a wall
  undetected.
- **Phantom.** An action box active on a frame reports the
  bodies it overlaps and moves none of them. A trigger volume
  reports entries without pushing the entrant.
- **Contact identity.** A resting contact emits exactly one
  event on its first tick and none after. A body that leaves
  and re-enters emits again, with a new first-seen tick.
- **Mid-tick spawn.** A body spawned mid-tick is invisible to
  queries until the next tick's Collide phase, and its first
  contact emits with the correct first-seen tick.
- **Replay.** The same inputs, chunked differently, reach the
  same contacts, the same first-seen ticks, and the same
  events. A contact divergence is a test failure.

## Caveats and things to consider

- **The world is the only source of truth.** Never query the
  entity table directly for collision. A query against the
  table bypasses the world's filters, its contact set, and its
  phase guarantees.
- **A query never moves a body.** Rays and phantom queries
  observe; only the step resolves contacts. A query result
  that needs to change motion goes through a physics event.
- **Touching is exact.** Fixed-point arithmetic makes two
  bodies whose faces share a tile boundary touching,
  deterministically, on every platform. There is no epsilon
  to tune — do not add one.
- **Shared shapes are one definition, many bodies.** Changing
  a shared shape changes every collidable that references it.
  A per-body tweak is a new shape, not an edit to the shared
  one.
- **A phantom result is bounded.** At most 8 collidables;
  overflow drops the farthest and counts it. A query that
  needs more results is asking the wrong question — narrow
  the volume or split the query.
- **Sleep is deterministic.** A tick count, never a
  wall-clock timer. Characters never sleep.

## Surface properties and materials

Contact response eventually needs restitution and friction. Those
numbers come from a surface-properties table delivered by the
asset pipeline (a cooked level chunk or a resident table), keyed
by surface ID. For v1:

- Characters use restitution 0 and a single default friction —
  no table lookup required for the player controller.
- Dynamic props (when they land) look up the surface ID of the
  contact and apply the table. Missing IDs are load-time
  validation errors, not contact-time surprises.
- Do not hard-code coefficients inside the narrow phase or the
  resolver. The table is the single source.

## Diagnostic overlays

When verifying contacts and queries, enable the debug overlays
defined in the physics design:

- Body AABBs (collision world)
- Contact normals and depths (persistent contact set)
- Grounded and sleep flags
- Last ray hit point

Overlays are presentation-only. Use them to confirm that a
resting contact's normal points out of the floor, that a swept
hit reports the entry face, and that a phantom overlap moves
nothing. A wrong normal that is invisible without overlays is
still a test failure.

## Additional caveats

- **No mid-tick entity-table queries for collision.** The world
  is rebuilt each tick in v1. A body spawned this tick is
  invisible until the next Collide phase.
- **Tile cells are closed solids.** There is no open-mesh
  winding policy. Inside a solid tile is always a penetration
  to resolve.
- **Shared shape edits affect every collidable.** A per-body
  tweak requires a new shape ID, not an edit to the shared
  definition.

## Cross-references

- Physics design: [forge-game-physics.md](../../../docs/architecture/proposals/forge-game-physics.md)
  (collision world, queries, contact identity, bounds, caveats)
- Entity model: [forge-game-entity-model.md](../../../docs/architecture/proposals/forge-game-entity-model.md)
  (phase names, generation tags, event delivery)
- Animation bridge: [bridge-animation-physics](../bridge-animation-physics/SKILL.md)
  (ground probe, action boxes as phantom queries)
- Gregory study notes (study-notes repo): chapter 12 §12.3
  (collision detection) and §12.4 (rigid body dynamics)
- Pixel-art authoring: [README](../../../src/python/pixel-art/README.md)
  (body hitboxes, action boxes, manifest)
