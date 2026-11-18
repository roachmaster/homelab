# Entity Model for the Forge Game Runtime

**Implementation reference — reviewed 18 November 2026.** The bounded entity
table, stable IDs, epoch/generation handles, guard patrol and targeted events
are implemented. The old no-entity baseline is historical, not the current game.

## Current types and bounds

`entity.rs` is internal to `forge-game-runtime`. It defines `EntityId`, `Kind`,
`Descriptor`, `Patrol`, `Handle`, `Entity`, `Slot` and `Table`; only `EntityView`
is part of the public presentation surface. These internal names are not a
public ECS or a browser spawn API.

| Concept | Current representation | Lifetime |
| --- | --- | --- |
| Stable identity | Authored nonzero `u16` entity ID. | Joins level descriptor, world snapshot, events, diagnostics and trace. A general save format is future work. |
| Runtime validity | Slot `u8`, generation `u32`, room epoch `u32`. | Detects stale access after deletion, slot reuse or restart. Handles remain inside Rust. |
| Table storage | Fixed array of 256 slots with optional entity records. | Bounded to one room; iteration follows stable slot order. |
| Supported level content | Player, key, exit and optional guard. | The current `Level::validate` accepts 3–4 spawns, despite the generic table capacity of 256. |

Source: [entity table](../../../src/rust/forge-game-runtime/src/entity.rs) ·
[level validation](../../../src/rust/forge-game-runtime/src/level.rs)

## Construction and lifecycle

A validated level supplies descriptors rather than live entity memory. The
table rejects invalid IDs, duplicates, invalid positions/patrols and capacity
exhaustion. Errors are typed variants; they are not the earlier proposed rich
source-file/field-path diagnostic API.

Spawn constructs a complete record before returning its handle. Despawn clears
the entity and increments the slot generation. An exhausted generation retires
the slot. Restart changes the room epoch and rebuilds descriptors, invalidating
previous-run handles. Events retain handles until delivery, so a recycled slot
cannot receive another entity's event.

Room One's built-in IDs are player 1, key 2, exit 3 and guard 4. The key despawns
after its pickup event is delivered. The exit changes state after unlock; the
map remains terrain. The published board composites live entities on the
validated floor/solid grid rather than treating the map as the entity store.

## Actual tick ordering

The outer runtime trace uses input, simulation and publication phases. There is
no public independently registered InputApply/Move/Collide/Trigger/Goal scheduler.
Within `Room::apply_tick`, fixed methods execute a stable sequence:

- Tile mode moves the guard, resolves player movement against current room
  state, evaluates pickups/goals, advances animation, delivers queued events,
  and then applies the completed despawn.
- Platform mode runs `Character::step`, writes the resolved player anchor,
  evaluates objectives, advances animation from the same `MotionFrame`, delivers
  new contact edges and objective events, and applies completed despawns.
- Publication exposes one completed room epoch/tick. Presentation reads that
  result and cannot change simulation ordering.

Source: [`Room` tick paths](../../../src/rust/forge-game-runtime/src/lib.rs) ·
[physics](forge-game-physics.md)

## Events and presentation

`PendingEvent` carries the target handle before delivery. Delivered `Event`
records retain stable target ID, sequence, tick, kind and event tile. The
64-record event ring evicts the oldest record on overflow and counts loss;
`(epoch, sequence)` distinguishes events from repeated frame reads. New landing,
head and wall contacts emit edge events rather than repeating a landing on each
supported tick.

`EntityView` provides stable ID, kind, state/facing and previous/current Q8 y-up
positions. It contains no native pointer or runtime handle. The version-1 FGS1
entity record stays 20 bytes. Platform animation uses the separate same-tick
FGP1 player pose, and diagnostics use FGC1. The renderer verifies matching view
identity before combining them.

[Snapshot contract](forge-game-snapshot.md) · [debug tools](forge-game-debug.md)

## Queries and authored variation

Validated descriptors supply spawn positions and optional guard patrol endpoints
and tick period. Current game rules remain Rust code. `Room::cast_ray`,
`collision_bodies` and `phantom_overlap` expose bounded filtered queries over the
last completed room state; they do not expose mutable table storage.

Level JSON decoding is implemented at the web asset boundary and constructs the
same pure `LevelSpec`/`Level` values. It is no longer a future loading seam.
Game saves, arbitrary entity kinds, script behavior, multiple simultaneous
objectives and playable combat/climbing remain future contracts.

[Cooked level decoder](../../../src/rust/forge-ide-web/src/playtest_level.rs) ·
[query implementation](../../../src/rust/forge-game-runtime/src/queries.rs)

## Forge ownership and verification

The dependency-free runtime owns pure simulation objects and uses ordinary Rust
construction. Device/byte-source effects belong to the native host and generated
browser adapter, with Forge factory/provider assembly at the effectful native
boundary. The simulation does not dispatch bridge commands per entity update.

Native entity tests cover slot reuse, epoch/generation staleness, duplicate IDs,
patrol validation and bounded storage. Runtime tests cover current-tick guard
movement, event targets and replay behavior. Actual-WASM and browser checks
observe the published entity records. Current QA output is the evidence of a
fresh run; this reference does not certify unexecuted future entity features.

The rationale applies Gregory, *Game Engine Architecture*, second edition,
chapters 14–15. The original larger proposal remains available in Git history.
