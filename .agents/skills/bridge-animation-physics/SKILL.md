---
name: bridge-animation-physics
description: Wire a character's authored clips to the physics simulation so the pose follows the simulated body. Use for clip selection from physics state, root-motion ownership, hitbox-to-collider mapping, and landing transitions.
---

# Bridge animation and physics

Start from the [physics design](../../../docs/architecture/proposals/forge-game-physics.md)
and an approved recipe made with
[build-pixel-character-base](../build-pixel-character-base/SKILL.md).
The bridge has one rule: **physics owns world position; animation
owns the pose.** Every decision below follows from it. Gregory
11.7 asks "which component gets the final say about a given
joint" — for Forge, the answer is written down in advance, and
no system writes a joint it does not own.

## The ownership contract

| What | Owner | Reads | Writes |
| --- | --- | --- | --- |
| Entity position, velocity, grounded flag | Physics (Move/Collide) | Input targets, terrain, colliders | Entity state, contact events |
| Clip selection | Animation (Animate) | Same tick's physics state | Active clip, clip clock |
| Joint poses, limb positions | Animation (Animate) | Active clip, frame time | The pose, drawn relative to the physics root |
| Collider | Physics (load time) | Recipe body hitbox | The body's AABB |
| Action boxes | Animation (per frame) | Clip frame markers | Phantom overlap queries, never contacts |

The snapshot is the only publication path. Physics writes entity
positions; the snapshot publishes them; the renderer reads the
snapshot. Animation never writes a position, and physics never
writes a joint.

## Map clips to physics states

The Animate phase reads the same tick's physics results —
grounded flag, horizontal speed, vertical velocity, and the
landing event — and selects a clip. Define the mapping before
wiring anything, using the Scout's clips as the example:

| Physics state | Clip |
| --- | --- |
| Grounded, speed ≈ 0 | `idle` |
| Grounded, speed > 0 | `walk` / `run` by speed band |
| Airborne, rising | `jump` (rise frames) |
| Airborne, falling | `jump` (fall frames) |
| Landing event this tick | `land` / `crouch`, then `idle` |

A clip change mid-air (rising to falling) is a phase change
within the `jump` clip, not a clip swap, so the legs do not
snap. A landing is an event-driven transition: the `Landed`
event selects the land clip on the landing tick, and the clip's
duration (from the manifest) decides when control returns to
idle. Record the impact speed with the event — a step down and
a hard fall land differently.

## Zero the airborne root offsets

The recipe's root offsets are a preview device for the
authoring tool. For a game with its own vertical motion, set
the airborne root offsets to `[0, 0]` before export (see the
[pixel-art README](../../../src/python/pixel-art/README.md)).
Keep the knee and arm rotations — those are the pose — and let
the runtime trajectory own world position. A jump looks like a
jump because the body rises (physics) while the legs tuck
(animation), not because the clip moves the body.

Verify the rule after export: the exported manifest's frames
for airborne clips must show zero root offsets, and the
greybox, final, and illustrated manifests must agree on frame
counts, durations, pivots, and hitboxes. If an airborne frame
still carries a root offset, the animation is fighting physics
for the root, and the character will visibly double-move.

## Map hitboxes to colliders and queries

The recipe's body hitbox IS the physics collider: one AABB,
defined once, used by both systems. Its `at` point is the
offset from the entity anchor (the recipe pivot); its `size`
is the box. A body-hitbox change is a physics change —
re-validate the collider at load, and reject a zero-size or
out-of-cell box with the part and frame named.

The recipe's action boxes are phantom overlap queries
(Gregory 12.3): they detect what an attack touches without
participating in dynamics, and they exist only on the frames
where the clip marks them active. An action-box change is an
animation change — re-check the query frames, not the collider.
Never let an action box push a body; if a hit should knock
something back, the query reports the hit and gameplay applies
the impulse through the physics event.

## Respect the time domains

Clip time and simulation time are different clocks. The
animation clock advances by tick dt (16,667 µs per tick);
frame durations come from the manifest's `duration_ms`. A frame
boundary is crossed when the accumulated clip time passes the
frame's duration — the frame changes at a tick boundary, never
mid-tick.

Loop endpoint semantics (Gregory 11.4): the cycle's last frame
is followed by its first frame, and the boundary pose is counted
once. If the authoring tool stores both a starting pose and its
duplicate at the cycle boundary, counting both as independent
intervals inserts a hitch — check the loop's frame count
against the manifest before wiring it.

A time step that crosses a motion marker — a frame with an
action box, or the landing frame — processes the marker exactly
once, even if the step wraps the loop. A marker fired twice is
a double hit; a marker skipped is a missed hit. Both are
test failures, not tuning notes.

## Wire the Animate phase

Selection runs in the Animate phase, after Collide, so the clip
always matches the current tick's physics. The phase reads only
state published by earlier phases in the same tick — the
entity design's rule. A consumer that needs a peer's position
reads the current tick's generation tag (the snapshot tick
index), never last tick's value, so no attachment trails by a
tick.

There is no IK for a 2D sprite game: the feet are drawn, not
solved. If a future game needs foot planting on uneven ground,
the solver runs in the Animate phase after clip selection,
reads the ground query, and writes only the leg joints. The
policy stays: one writer per joint, named in advance.

## Verify the bridge

- **Same-tick selection.** The clip matches the physics state
  within the same tick: rising selects the jump rise frames on
  the tick the jump starts; the landing event selects the land
  clip on the landing tick.
- **No double motion.** With airborne root offsets zeroed, the
  character's world position comes only from physics. Play the
  jump in the browser and confirm the apex and landing tick
  match the physics acceptance numbers (0.93 tiles at tick 14,
  lands tick 29).
- **No trailing frame.** An attachment (the probe, a carried
  item) uses the current tick's generation tag. Pause the game
  mid-jump and confirm the attachment sits on the current
  pose, not the previous one.
- **Replay.** The same inputs, chunked differently, reach the
  same positions, landing tick, and clip selections. A physics
  divergence is a test failure; a clip-selection divergence is
  a bridge failure.
- **Boundary.** A body hitbox change re-validates the collider;
  an action-box change re-checks the query frames. Changing
  one must not silently change the other.

## Migrating an existing Scout recipe

Before a recipe can be driven by the physics design, perform these
checks in order. Do not skip any step; a missed step produces
visible double-motion or a silent hitbox mismatch.

1. **Zero airborne root offsets.** In the authoring tool set every
   airborne frame's root offset to `[0, 0]` and re-export. Keep
   limb rotations and action boxes; discard only the root
   translation that previously simulated the jump arc.
2. **Manifest parity.** Greybox, final, and illustrated manifests
   must agree on frame counts, `duration_ms` values, pivots, and
   body hitboxes. A mismatch is a recipe error, not a runtime
   tolerance.
3. **Hitbox re-validation.** At load, reject a zero-size box, a
   box that extends outside the cell, or a pivot offset that
   disagrees with the recipe. Name the part and frame in the
   error.
4. **Bridge acceptance.** Run the verification list below. A
   recipe that still carries airborne root offsets will fail the
   "no double motion" check and must not be shipped.

## Caveats and things to consider

- **One writer per joint.** Physics owns the root; animation owns
  the limbs. Nothing else may write a joint. If a future feature
  (foot planting, look-at, recoil) needs to adjust a joint, name
  the writer in advance and run it in the Animate phase after
  clip selection. An unnamed second writer is a contract violation.
- **Clip time vs simulation time.** These are different clocks.
  Never advance the animation clock by wall-clock time or by a
  variable render delta. A frame boundary occurs only at a tick
  boundary.
- **Landing is an event, not a state.** The land/crouch clip is
  selected by the `Landed` event on the landing tick. Do not
  infer landing solely from "grounded this tick and not grounded
  last tick" — the event carries impact speed and is the single
  source of truth.
- **Action boxes never push.** They are phantom queries only.
  Knock-back or hit reactions must be applied through a physics
  impulse or gameplay event, never by treating an action box as
  a contact.
- **Future IK.** Deferred for v1. When added, the solver must
  read the ground query, write only the leg joints, and leave
  the root under physics ownership. The policy does not change.
- **Replay surface.** Clip selection is part of the deterministic
  surface. The same inputs must produce the same clip sequence.
  A clip-selection divergence is a bridge failure, not a visual
  polish issue.

## Cross-references

- Physics design: [forge-game-physics.md](../../../docs/architecture/proposals/forge-game-physics.md)
  (ownership table, units/spaces, contact identity, phase mapping)
- Entity model: [forge-game-entity-model.md](../../../docs/architecture/proposals/forge-game-entity-model.md)
  (phase names, generation tags)
- Gregory study notes (study-notes repo): chapter 11 §§11.4, 11.7,
  11.11 and chapter 12 §§12.3, 12.5
- Pixel-art authoring: [README](../../../src/python/pixel-art/README.md)
  (root-motion rule, hitboxes, manifest)
