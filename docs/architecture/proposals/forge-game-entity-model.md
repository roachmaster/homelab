# Entity Model for the Forge Game Runtime

Give the game a typed, bounded entity model — identity, lifecycle,
update phases, and event targets — so the second game feature does
not invent them ad hoc. The player, the key, the exit, and every
future moving thing are entities; the map is terrain.

**Planning baseline** — GitHub `main` at `e402c23` on 2026-11-17 (the
snapshot design merged). This design builds on the [debug and trace
layer](forge-game-debug.md) for phase vocabulary and tracing.
The design applies Gregory chapters
14 (§§14.1–14.4) and 15 (§§15.1–15.9)
and the `design-gameplay-runtime` catalog entry, and it follows the
Forge package-boundary conventions (§11 below).

## The problem

Today the world is one player plus a static map, and the key, the
exit, and the player are all cell codes in the board. There is no
entity concept: nothing has an identity that survives a frame, no
lifecycle (spawn, despawn, reuse), no update order beyond "the player
moves," and no way for an event to name its target. An enemy, a
second key, or a moving platform would each need all of that, invented
separately. Gregory §14.2 says what the model must supply: identity,
behavior, and relationships — "a stable object ID ties
representations together for debugging and save/restore."

## Representative scenario

Room One plus one patrolling guard: it walks a fixed route along the
corridor, blocks the player's path, and despawns on restart. The
scenario exercises spawn (the guard appears from a descriptor),
movement (it moves before collision is resolved), phase ordering (the
player's collision sees the guard's current-tick position, not last
tick's), event targeting (the block delivers an event naming the
guard), and handle staleness (an event queued for the guard before a
restart is dropped, not delivered to whatever reuses its slot).

## Identity: three distinct things

§15.5 and the synthesis ("Separate address, identity and readiness")
insist these are not the same fact:

| Concept | Form | Lifetime | Purpose |
| --- | --- | --- | --- |
| Stable ID | `u16`, authored | Persists through load and save | Ties representations together: the same ID names the entity in a descriptor, the snapshot, the trace, and a save file (§14.2) |
| Runtime handle | Slot `u8`, generation `u32`, run epoch `u32` | One loaded run | Detects deletion, slot reuse, and restart. Resolved at the point of use (§15.5). |
| Readiness | Spawn completes at a tick boundary | Per spawn | A resolved handle is not the same as a ready entity; nothing observes a half-spawned entity |

The snapshot's entity `id` field is the stable ID. Runtime handles stay
inside Rust. Despawn increments the slot generation. If it exhausts the
counter, the slot is retired for the rest of the run. Restart creates
a new epoch, invalidating handles from the previous run.

## Lifecycle: descriptors versus instances

§15.3 separates a spawn definition from its live instance. A
**descriptor** is a typed Rust struct — stable ID, kind, initial
position, initial state — validated at construction (position in
bounds, kind known, references valid), with a validation error naming
the offending object and field (§14.3). An **instance** is the live
record: handle, current position, current state. Despawn retires the
slot and bumps the generation; the slot is reusable only after the
bump, so no event can reach a recycled entity.

**Static versus dynamic** (§14.1): the static/dynamic distinction is
a guarantee about change, and late reclassification is expensive.
The board is static terrain (floor, wall). The key and the exit are
dynamic — they participate in gameplay, change state, and trigger
events — so they become entities (kinds `Key`, `Exit`), not map
data. The runtime composites them onto the board at publication.
Cell code 6 represents the guard on the snapshot path.

## Data layout

Runtime storage is an array-of-structures: one fixed array of 256
entity structs, indexed by slot, matching the snapshot's entity
array. The reasoning, recorded per §15.2 ("Decide operations and
lifetime first, then choose layout based on observed access
patterns"): the entity count is at most 256, the dominant operations
are per-entity updates and snapshot publication, and both are
array-of-structures friendly. The structure-of-arrays alternative is
rejected for now — no pass yet scans one property across all
entities — and the revisit trigger is explicit: when culling or
broad-phase arrives, add a property index with an invalidation rule
(rebuilt at the Publish phase), not a rewrite. Editor-only data
(names, annotations) stays out of runtime records (§14.2).

## Update phases

§15.6: a direct loop invoking one large update per object only works
while the dependencies are trivial; with animation, physics, effects,
and attachments, an arbitrary loop can put a parent at tick *n* and
its dependent at tick *n−1*, which looks like an intermittent
trailing frame. The fix is a fixed phase order inside `advance`:

| Phase | Runs | Reads | Publishes |
| --- | --- | --- | --- |
| InputApply | Queued input edges | Input buffer | Intended moves |
| Move | Entities with velocity or patrol | Intended moves, current positions | New positions |
| Collide | Movement resolution | New positions, terrain, entity positions | Resolved positions, block events |
| Trigger | Volumes and pickups | Resolved positions | Pickup/trigger events |
| Goal | Win/lose evaluation | Trigger results, status | New status |
| Effects | Event delivery to handlers | Queued events | Handler outcomes |
| Animate | (Reserved slot; no-op today) | Resolved positions | Poses, complete before Publish |

The rule: a phase reads only state published by earlier phases in
the same tick. A consumer that needs a peer's position reads the
current tick's generation tag (the snapshot tick index), never last
tick's value. The phase order is a fixed sequence in `advance`, not a
per-object loop, so the trailing-frame bug is prevented by
construction rather than by convention. Phase tags go in the trace
(see the debug design), so a phase-order bug is diagnosable from the
log. The debug design's outer vocabulary — `Input → Simulate →
Publish → Present → Drain` — frames these phases; `Simulate`
contains the table above.

## Events: queued, targeted, exactly once

§15.7: an event envelope carries type, target, and arguments.
Immediate delivery lets nested handlers mutate objects mid-update
and recurse deeply, so this design delivers **queued only**: events
are enqueued during Collide/Trigger/Goal and delivered at the
Effects phase, at a known point in the tick. The runtime queue owns
its payloads (fixed `u32`, no allocation). A stale target is dropped
and counted; a valid target receives the event exactly once. Events
are deterministic functions of (tick, inputs), and the trace records
them, so replay reproduces them.

The internal queue carries full handles. Delivered events in the
snapshot carry a stable target ID and the tile where the event
occurred. The run epoch and event sequence identify the record
across repeated reads. This makes the pickup trace useful after
the key despawns.

## Game flow

§15.9: global rules such as completion and objectives build on the
object model. Room One's flow is a three-state finite state machine
over the runtime `Status` (`FindKey → ExitUnlocked → Completed`).
Progression state lives apart from chunk residency (§14.1: an
objective can span chunks, and backtracking must not reset progress)
— structurally, objectives are progression state, not entity flags,
even though Room One has one chunk and one objective. A dependency
graph for concurrent objectives is deferred to the first game that
has more than one objective at once.

## Data-driven behavior, without scripting yet

§14.3: put repeatable variation into schemas and validated
attributes; reserve scriptable behavior for rules that require
control flow, timing, or branching. Descriptors are the data surface:
a guard's patrol route, speed, and kind are validated descriptor
values, changeable without touching code. Game rules stay code-only
(the proposal's rule interface). Scripting (§15.8) is deferred, with
an explicit revisit trigger: when a second game needs authored
behavior that descriptors cannot express. A fault in content names
the offending object, field, and source (§14.3) — descriptor
validation errors carry the stable ID and the field path.

## Save and load: the seam, not the design

§15.4: "Keep logical state and resource discovery separate so a save
remains meaningful after the memory layout changes." This design
keeps that seam clean: a save serializes descriptors plus dynamic
state (positions, statuses, progression), never the runtime layout —
the entity table can be rebuilt from a descriptor list plus a state
list. The save/load design itself (serialization format, checkpoint
versus save-anywhere) is separate work; the runtime has no serde yet
and gains none here.

## How this integrates with the Forge philosophy

- **Typed contracts.** Kinds, handles, and events use Rust enums and
  newtypes. Room One descriptors are validated Rust structs. File
  decoding belongs to the future level-loading boundary.
- **Bounded everything.** `MAX_ENTITIES` is 256, matching the
  snapshot array; storage is fixed arrays; the tick path allocates
  nothing; iteration is by index, never by hash map, so update order
  is deterministic.
- **Package boundaries.** The entity model lives in
  `forge-game-runtime`, which stays zero-dependency: no new crate,
  no new dependencies, no change to the architecture guard. It stays
  in the runtime crate because entities share the tick invariant
  with the room — a separate crate would create pass-through types
  and coordinated versioning for no isolation gained. The Forge side
  (adapter, IDE) sees only the snapshot and the trace, never runtime
  internals. The runtime does not use the Forge bridge: per the
  proposal it uses ordinary Rust construction, and the bridge
  pattern applies at the Forge dispatch boundary, not inside the
  simulation.
- **Determinism.** Entity updates are pure functions of (state,
  inputs). The cross-partition replay test extends to entities: the
  same inputs, chunked differently, reach the same state, including
  the guard's position and the completion tick.
- **Verification is the deliverable.** Unit tests per acceptance
  criterion below; journey assertions on the snapshot's entity
  record (the player's snapshot position matches the visible
  position); the rendered playtest is the evidence, not the Rust
  source alone.

## Handoffs

| Handoff | Producer → consumer | Phase | Data and owner | On failure |
| --- | --- | --- | --- | --- |
| Spawn | Descriptor → entity table | Tick boundary | Descriptor (validated at construction); table owns the slot | Invalid descriptor is rejected at construction with the stable ID and field; no partial spawn |
| Position | Move → Collide → Trigger | Within Simulate | Entity positions; the table owns them | A phase reading a stale generation is a bug the trace exposes, not a state the design permits |
| Event | Collide/Trigger/Goal → Effects | Within Simulate | Runtime queue owns the payload; snapshot ring publishes delivered events | Stale target dropped and counted; queue overflow drops the oldest event and counts it |
| State | Runtime → adapter | Publish → Present | Snapshot buffer (see the snapshot design) | Invalid header → the failure state, as the adapter does |

## Migration, in behavior-preserving slices

1. Entity table plus handles in the runtime; the player becomes
   entity 0, the key and exit become entities, the board composites
   at publication. No behavior change; the journey stays green.
2. The phase schedule inside `advance`, plus event targets in the
   runtime queue and the published payload encoding.
3. Spawn descriptors plus the guard scenario in Room One.
4. Journey assertions on the snapshot entity record, and the replay
   check extended to entities.

Each slice keeps the game playable and the journey green; no slice
changes the simulation rules.

## Acceptance checks

- **Representative scenario:** the guard patrols its route, blocks
  the corridor, and despawns on restart; the player still completes
  Room One.
- **Handle staleness:** despawn an entity, then deliver an event to
  its old handle — the delivery is dropped and counted, and nothing
  else receives it.
- **Phase ordering:** the guard moves at tick *n* and the player's
  collision at tick *n* sees the new position — no trailing frame.
- **Event exactly-once:** the key-collected event fires once per
  playthrough, targeted at the key's handle.
- **Replay:** the same inputs, chunked differently, reach the same
  completion tick with entities present.
- **Boundary:** spawning past the 256-entity cap is rejected with a
  counted error, not a panic.

## Open decisions

- **Facing — resolved here.** The snapshot design left open whether
  the entity record needs a facing field. It does, for the first
  moving entity — and no layout change is needed: the state byte's
  upper two bits carry facing clockwise from north (north, east,
  south, west).
- **Descriptor file format — deferred** to the asset-pipeline design.
  Descriptors are Rust types for now; the file format arrives with
  level loading.

## References

- Gregory, *Game Engine Architecture*, 2nd ed., chapter 14
  (§§14.1–14.4: world anatomy, game objects, data-driven engines,
  the world editor) and chapter 15 (§§15.1–15.9: object model,
  references and queries, update phases, events, scripting, game
  flow), plus chapter 3 (data-oriented design) and the whole-book
  synthesis. Study notes in `roachmaster/study-notes` under
  `studies/gregory-game-engine-architecture-2e-2015/`.
- [Extending Homelab with a Rust Game Engine](forge-game-engine.md),
  the [simulation-to-presentation snapshot](forge-game-snapshot.md),
  and the [debug and trace layer](forge-game-debug.md) this design
  builds on.
- The `design-package-boundaries` skill (`.agents/skills/`) for the
  typed-contract, boundedness, and dependency-direction conventions
  applied in §11.
