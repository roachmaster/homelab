//! Fixed-step simulation for the first playable Forge room.
//!
//! An adapter supplies elapsed microseconds and directional input edges, then
//! reads the complete room after advancing it. The simulation owns no clock,
//! display, input device, or platform resources.

mod animation;
mod entity;
mod input;
mod level;
pub mod physics;
mod queries;
mod ring;

use animation::Animator;
pub use animation::{AnimationTiming, PlayerPose};
pub use entity::EntityView;
use entity::{Descriptor, Entity, EntityId, Handle, Kind, Patrol, Position, Table};
use input::InputBuffer;
pub use level::{
    Level, LevelError, LevelId, LevelMode, LevelSpec, PatrolSpec, SpawnKind, SpawnSpec, SurfaceSpec,
};
use physics::{Character, Contact, Horizontal, MotionFrame, MotionInput, TileGrid, Vector, TILE};
pub use queries::{CollisionFilter, CollisionTargets, PhantomHits};
use ring::Ring;
use std::sync::Arc;

const TICKS_PER_SECOND: u64 = 60;
const MICROS_PER_SECOND: u64 = 1_000_000;
const MAX_TICKS_PER_ADVANCE: u64 = 4;
const WIDTH: u32 = 13;
const HEIGHT: u32 = 9;
const PLAYER: Descriptor = Descriptor {
    id: EntityId(1),
    kind: Kind::Player,
    spawn: Position { x: 1, y: 1 },
    patrol: None,
};
const KEY: Descriptor = Descriptor {
    id: EntityId(2),
    kind: Kind::Key,
    spawn: Position { x: 1, y: 7 },
    patrol: None,
};
const EXIT: Descriptor = Descriptor {
    id: EntityId(3),
    kind: Kind::Exit,
    spawn: Position { x: 11, y: 4 },
    patrol: None,
};
const GUARD_PATROL: Patrol = Patrol {
    first_x: 5,
    last_x: 10,
    period_ticks: 12,
};
const GUARD: Descriptor = Descriptor {
    id: EntityId(4),
    kind: Kind::Guard,
    spawn: Position { x: 6, y: 1 },
    patrol: Some(GUARD_PATROL),
};
const INPUT_PHASE: u8 = 0;
const SIMULATE_PHASE: u8 = 1;
const PUBLISH_PHASE: u8 = 2;
const INPUT_CHANNEL: u8 = 0;
const SIMULATION_CHANNEL: u8 = 1;
const EVENT_CHANNEL: u8 = 2;
const LEVEL_CHANNEL: u8 = 3;
const HITCH_CHANNEL: u8 = 4;
const ERROR_CHANNEL: u8 = 5;
const TRACE_CHANNEL_MASK: u32 = (1 << (ERROR_CHANNEL + 1)) - 1;
const DEFAULT_TRACE_MASK: u32 = TRACE_CHANNEL_MASK & !(1 << SIMULATION_CHANNEL);
const MAP: [&[u8; WIDTH as usize]; HEIGHT as usize] = [
    b"#############",
    b"#...#.......#",
    b"###.#.#####.#",
    b"#...#...#...#",
    b"#.#####.#.#.#",
    b"#.....#.#...#",
    b"###.#.#.###.#",
    b"#...#.......#",
    b"#############",
];

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Up,
    Down,
    Left,
    Right,
    Jump,
}

/// Input actually applied by the simulation at a tick, after resolving edges
/// and held directions. A replay does not need the original browser timing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectiveInput {
    Completed,
    Tile(Option<Action>),
    Platform(MotionInput),
}

/// Stable values for the room's presentation adapter.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cell {
    Floor = 0,
    Wall = 1,
    Key = 2,
    LockedExit = 3,
    OpenExit = 4,
    Player = 5,
    Guard = 6,
}

/// Stable values for the room's current objective.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    FindKey = 0,
    ExitUnlocked = 1,
    Completed = 2,
}

#[repr(u8)]
#[derive(Clone, Copy)]
enum EventKind {
    KeyCollected = 1,
    ExitUnlocked = 2,
    Completed = 3,
    GuardBlocked = 4,
    Landed = 5,
    HeadBump = 6,
    WallContact = 7,
}

#[derive(Clone, Copy)]
struct PendingEvent {
    kind: EventKind,
    target: Handle,
    position: Position,
}

/// An event refers to an authored entity ID and gives the occurrence tile in y-up coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Event {
    pub sequence: u32,
    pub tick: u32,
    pub target: u16,
    pub kind: u8,
    pub x: u16,
    pub y: u16,
}

/// A trace record carries an epoch-scoped sequence. Input records carry the
/// effective movement action applied on this tick, including held repeats.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TraceRecord {
    pub sequence: u32,
    pub tick: u32,
    pub phase: u8,
    pub channel: u8,
    pub target: u16,
    pub data: u32,
}

/// An independent authored room with a fixed simulation clock.
pub struct Room {
    level: Arc<Level>,
    table: Table,
    player: Handle,
    key: Option<Handle>,
    exit: Handle,
    guard: Option<Handle>,
    character: Option<Character>,
    animator: Animator,
    status: Status,
    epoch: u32,
    tick: u32,
    tick_fraction: u64,
    ticks_run: u32,
    ticks_dropped: u32,
    input: InputBuffer,
    input_dropped: u32,
    events: Ring<Event, 64>,
    stale_events: u32,
    trace: Ring<TraceRecord, 512>,
    trace_mask: u32,
    replay_complete: bool,
}

impl Room {
    pub fn new() -> Self {
        Self::from_level(Arc::new(Level::builtin()), 1, DEFAULT_TRACE_MASK)
    }

    pub fn from_validated(level: Level, epoch: u32, trace_mask: u32) -> Self {
        Self::from_level(Arc::new(level), epoch, trace_mask & TRACE_CHANNEL_MASK)
    }

    fn from_level(level: Arc<Level>, epoch: u32, trace_mask: u32) -> Self {
        let mut table = Table::new(epoch);
        let player = table.spawn(&level, level.player).expect("validated player");
        let key = table.spawn(&level, level.key).expect("validated key");
        let exit = table.spawn(&level, level.exit).expect("validated exit");
        let guard = level
            .guard
            .map(|guard| table.spawn(&level, guard).expect("validated guard"));
        let character = (level.mode == LevelMode::Platform)
            .then(|| level.spawn_character().expect("validated platform spawn"));
        let mut room = Self {
            level,
            table,
            player,
            key: Some(key),
            exit,
            guard,
            character,
            animator: Animator::new(),
            status: Status::FindKey,
            epoch,
            tick: 0,
            tick_fraction: 0,
            ticks_run: 0,
            ticks_dropped: 0,
            input: InputBuffer::new(),
            input_dropped: 0,
            events: Ring::new(),
            stale_events: 0,
            trace: Ring::new(),
            trace_mask,
            replay_complete: true,
        };
        room.record_trace(PUBLISH_PHASE, LEVEL_CHANNEL, 0, u32::from(epoch != 1));
        room
    }

    pub fn width(&self) -> u32 {
        self.level.width
    }

    pub fn height(&self) -> u32 {
        self.level.height
    }

    /// Returns the cell code with x increasing rightward and y increasing
    /// downward. The player occupies its tile visually. Outside cells are walls.
    pub fn cell(&self, x: u32, y: u32) -> u32 {
        let position = Position { x, y };
        let cell = if position == self.entity(self.player).position {
            Cell::Player
        } else if self
            .guard
            .is_some_and(|guard| position == self.entity(guard).position)
        {
            Cell::Guard
        } else if self
            .key
            .is_some_and(|key| self.entity(key).position == position)
        {
            Cell::Key
        } else if position == self.entity(self.exit).position {
            if self.status == Status::FindKey {
                Cell::LockedExit
            } else {
                Cell::OpenExit
            }
        } else if self.level.solid(position) {
            Cell::Wall
        } else {
            Cell::Floor
        };
        cell as u32
    }

    pub fn status(&self) -> u32 {
        self.status as u32
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    pub fn tick(&self) -> u32 {
        self.tick
    }

    /// The residual simulation time in millionths of one fixed tick.
    pub fn interpolation(&self) -> u32 {
        self.tick_fraction as u32
    }

    /// Prepared rooms can replace this run only before a simulation tick.
    /// A completed room is already resting at a published boundary.
    pub fn activation_boundary(&self, elapsed_micros: u32) -> bool {
        elapsed_micros != 0
            && (self.status == Status::Completed
                || self.tick_fraction + u64::from(elapsed_micros) * TICKS_PER_SECOND
                    >= MICROS_PER_SECOND)
    }

    pub fn ticks_run(&self) -> u32 {
        self.ticks_run
    }

    pub fn ticks_dropped(&self) -> u32 {
        self.ticks_dropped
    }

    pub fn input_depth(&self) -> u32 {
        self.input.len()
    }

    pub fn input_dropped(&self) -> u32 {
        self.input_dropped
    }

    pub fn events(&self) -> impl Iterator<Item = Event> + '_ {
        self.events.iter()
    }

    /// Count of evicted published events and rejected stale targets in this epoch.
    pub fn events_dropped(&self) -> u32 {
        self.events.dropped().saturating_add(self.stale_events)
    }

    pub fn entities(&self) -> impl Iterator<Item = EntityView> + '_ {
        self.table.views(self.height()).map(|mut entity| {
            if entity.kind == Kind::Player as u8 {
                if let Some(motion) = self.motion() {
                    (entity.x, entity.y) = self.wire_position(motion.position);
                    (entity.previous_x, entity.previous_y) = self.wire_position(motion.previous);
                }
            }
            entity
        })
    }

    pub fn pose(&self) -> PlayerPose {
        let player = self.entity(self.player);
        self.animator
            .view(player.id.0, player.facing == Action::Left, self.motion())
    }

    pub fn motion(&self) -> Option<MotionFrame> {
        self.character.as_ref().map(Character::frame)
    }

    fn wire_position(&self, anchor: Vector) -> (i32, i32) {
        // v1 publishes tile anchors in y-up space. Physics owns a center-x,
        // foot-y anchor in the grid's continuous y-down space.
        (
            (anchor.x - TILE / 2) / 256,
            (self.height() as i32 * TILE - anchor.y) / 256,
        )
    }

    pub fn trace(&self) -> impl Iterator<Item = TraceRecord> + '_ {
        self.trace.iter()
    }

    pub fn trace_latest_sequence(&self) -> u32 {
        self.trace.latest()
    }

    pub fn trace_dropped(&self) -> u32 {
        self.trace.dropped()
    }

    pub fn trace_replay_complete(&self) -> bool {
        self.replay_complete && self.trace.dropped() == 0
    }

    pub fn trace_mask(&self) -> u32 {
        self.trace_mask
    }

    pub fn set_trace_mask(&mut self, mask: u32) {
        self.trace_mask = mask & TRACE_CHANNEL_MASK;
    }

    /// Queues a press or release for playback at the next simulation tick.
    pub fn input(&mut self, action: Action, pressed: bool) {
        if self.status != Status::Completed && (action != Action::Jump || self.character.is_some())
        {
            let dropped = self.input.enqueue(action, pressed);
            if dropped != 0 {
                self.input_dropped = self.input_dropped.saturating_add(dropped);
                self.record_trace(INPUT_PHASE, ERROR_CHANNEL, self.level.player.id.0, dropped);
            }
        }
    }

    /// Runs up to four 60 Hz ticks and drops any remaining whole-tick debt,
    /// preserving fractional time. Each tick can move the player one tile.
    pub fn advance(&mut self, elapsed_micros: u32) {
        self.advance_observed(elapsed_micros, |_, _| {});
    }

    /// Advance the fixed clock and report each tick's effective input in order.
    pub fn advance_observed(
        &mut self,
        elapsed_micros: u32,
        mut observe: impl FnMut(u32, EffectiveInput),
    ) {
        self.ticks_run = 0;
        self.ticks_dropped = 0;
        if self.status == Status::Completed {
            return;
        }
        let time = self.tick_fraction + u64::from(elapsed_micros) * TICKS_PER_SECOND;
        let due = time / MICROS_PER_SECOND;
        let ticks = due.min(MAX_TICKS_PER_ADVANCE);
        self.ticks_dropped = (due - ticks) as u32;
        self.tick_fraction = time % MICROS_PER_SECOND;
        for _ in 0..ticks {
            if let Some(input) = self.run_tick() {
                observe(self.tick, input);
            }
            if self.status == Status::Completed || self.tick == u32::MAX {
                break;
            }
        }
        if self.ticks_dropped != 0 {
            self.record_trace(PUBLISH_PHASE, HITCH_CHANNEL, 0, self.ticks_dropped);
        }
    }

    /// Advance exactly one simulation tick outside the wall-clock accumulator.
    pub fn step_one_tick(&mut self) {
        self.step_observed(|_, _| {});
    }

    /// Step exactly once and report the effective input, if the clock advanced.
    pub fn step_observed(&mut self, observe: impl FnOnce(u32, EffectiveInput)) {
        self.ticks_run = 0;
        self.ticks_dropped = 0;
        if let Some(input) = self.run_tick() {
            observe(self.tick, input);
        }
    }

    /// Apply one recorded tick without interpreting device edges or elapsed time.
    /// Reject records that do not match the room's state or simulation mode.
    pub fn replay_tick(&mut self, input: EffectiveInput) -> bool {
        let valid = match input {
            EffectiveInput::Completed => self.status == Status::Completed,
            EffectiveInput::Tile(action) => {
                self.status != Status::Completed
                    && self.character.is_none()
                    && action != Some(Action::Jump)
            }
            EffectiveInput::Platform(_) => {
                self.status != Status::Completed && self.character.is_some()
            }
        };
        let Some(tick) = self.tick.checked_add(1).filter(|_| valid) else {
            return false;
        };
        self.ticks_run = 0;
        self.ticks_dropped = 0;
        self.apply_tick(tick, input);
        true
    }

    /// Drops pending input, held directions, and fractional clock time on focus loss.
    pub fn clear_input(&mut self) {
        self.input.clear();
        self.tick_fraction = 0;
    }

    /// Rebuilds the room under a fresh epoch. Exhausting the epoch counter
    /// leaves the final run intact, so handles and event IDs never alias.
    pub fn restart(&mut self) {
        if let Some(epoch) = self.epoch.checked_add(1) {
            *self = Self::from_level(Arc::clone(&self.level), epoch, self.trace_mask);
        }
    }

    fn run_tick(&mut self) -> Option<EffectiveInput> {
        let tick = self.tick.checked_add(1)?;
        let input = match (self.status, self.character.is_some()) {
            (Status::Completed, _) => EffectiveInput::Completed,
            (_, true) => EffectiveInput::Platform(self.input.next_motion()),
            (_, false) => EffectiveInput::Tile(self.input.next_step()),
        };
        self.apply_tick(tick, input);
        Some(input)
    }

    fn apply_tick(&mut self, tick: u32, input: EffectiveInput) {
        self.tick = tick;
        self.ticks_run += 1;
        self.table.begin_tick();
        match input {
            EffectiveInput::Completed => (),
            EffectiveInput::Tile(action) => self.run_tile_tick(action),
            EffectiveInput::Platform(motion) => self.run_platform_tick(motion),
        }
    }

    fn run_tile_tick(&mut self, action: Option<Action>) {
        // Input, movement, collisions, animation, then event effects.
        if let Some(action) = action {
            self.replay_complete &= self.trace_mask & (1 << INPUT_CHANNEL) != 0;
            self.record_trace(
                INPUT_PHASE,
                INPUT_CHANNEL,
                self.level.player.id.0,
                action as u32,
            );
        }
        self.move_guard();
        let previous = self.entity(self.player).position;
        let blocked = action.and_then(|action| self.move_player(action));
        let (pickup, objective) = self.update_objective();
        self.animator.advance_tile(
            self.entity(self.player).position != previous,
            self.level.timing,
        );
        for event in [blocked, pickup, objective].into_iter().flatten() {
            self.deliver(event);
        }
        if pickup.is_some() {
            if let Some(key) = self.key.take() {
                self.table.despawn(key);
            }
        }
    }

    fn run_platform_tick(&mut self, input: MotionInput) {
        let horizontal = match input.horizontal {
            Horizontal::Left => Some(Action::Left),
            Horizontal::Right => Some(Action::Right),
            Horizontal::Neutral => None,
        };
        let player_id = self.level.player.id.0;
        for action in [horizontal, input.jump.then_some(Action::Jump)]
            .into_iter()
            .flatten()
        {
            self.replay_complete &= self.trace_mask & (1 << INPUT_CHANNEL) != 0;
            self.record_trace(INPUT_PHASE, INPUT_CHANNEL, player_id, action as u32);
        }

        let grid = RoomCollision {
            level: &self.level,
            closed_exit: (self.status == Status::FindKey).then(|| self.entity(self.exit).position),
        };
        let motion = self
            .character
            .as_mut()
            .expect("platform character")
            .step(&grid, input);
        let position = self.anchor_tile(motion.position);
        let player = self.table.get_mut(self.player).expect("live player");
        player.position = position;
        if let Some(direction) = horizontal {
            player.facing = direction;
        }

        let (pickup, objective) = self.update_objective();
        self.animator.advance(motion, self.level.timing);
        for (kind, contact) in [
            (EventKind::Landed, motion.landed),
            (EventKind::HeadBump, motion.head_bump),
            (EventKind::WallContact, motion.wall_contact),
        ] {
            if let Some(contact) = contact {
                self.deliver_contact(kind, contact);
            }
        }
        for event in [pickup, objective].into_iter().flatten() {
            self.deliver(event);
        }
        if pickup.is_some() {
            if let Some(key) = self.key.take() {
                self.table.despawn(key);
            }
        }
    }

    fn anchor_tile(&self, anchor: Vector) -> Position {
        // Key and exit are tile triggers. Test the player's body center.
        Position {
            x: anchor.x.div_euclid(TILE).clamp(0, self.width() as i32 - 1) as u32,
            y: (anchor.y - TILE * 2 / 5)
                .div_euclid(TILE)
                .clamp(0, self.height() as i32 - 1) as u32,
        }
    }

    fn deliver_contact(&mut self, kind: EventKind, contact: Contact) {
        self.deliver(PendingEvent {
            kind,
            target: self.player,
            position: Position {
                x: contact.tile_x.clamp(0, self.width() as i32 - 1) as u32,
                y: contact.tile_y.clamp(0, self.height() as i32 - 1) as u32,
            },
        });
    }

    fn move_guard(&mut self) {
        let Some(handle) = self.guard else { return };
        let guard = self.entity(handle);
        let patrol = guard.patrol.expect("validated guard patrol");
        if !self.tick.is_multiple_of(patrol.period_ticks) {
            return;
        }
        let player_position = self.entity(self.player).position;
        let direction = match guard.position.x {
            x if x == patrol.first_x => Action::Right,
            x if x == patrol.last_x => Action::Left,
            _ => guard.facing,
        };
        let Some(target) = guard.position.next(direction) else {
            return;
        };
        if target != player_position {
            self.move_entity(handle, target, direction);
        }
    }

    fn move_player(&mut self, action: Action) -> Option<PendingEvent> {
        let target = self.entity(self.player).position.next(action)?;
        if self.level.solid(target)
            || (target == self.entity(self.exit).position && self.status == Status::FindKey)
        {
            return None;
        }
        if let Some(guard) = self.guard {
            if target == self.entity(guard).position {
                return Some(PendingEvent {
                    kind: EventKind::GuardBlocked,
                    target: guard,
                    position: target,
                });
            }
        }
        self.move_entity(self.player, target, action);
        None
    }

    fn move_entity(&mut self, handle: Handle, position: Position, direction: Action) {
        let entity = self.table.get_mut(handle).expect("live movement entity");
        entity.position = position;
        entity.facing = direction;
        let id = entity.id;
        self.record_trace(
            SIMULATE_PHASE,
            SIMULATION_CHANNEL,
            id.0,
            position.x | (position.world_y(self.height()) << 16),
        );
    }

    fn update_objective(&mut self) -> (Option<PendingEvent>, Option<PendingEvent>) {
        let exit_position = self.entity(self.exit).position;
        if let Some(key) = self.key {
            let key_position = self.entity(key).position;
            if self.overlaps_objective(key_position) {
                self.status = Status::ExitUnlocked;
                self.table.get_mut(self.exit).expect("live exit").state = 1;
                return (
                    Some(PendingEvent {
                        kind: EventKind::KeyCollected,
                        target: key,
                        position: key_position,
                    }),
                    Some(PendingEvent {
                        kind: EventKind::ExitUnlocked,
                        target: self.exit,
                        position: exit_position,
                    }),
                );
            }
        } else if self.overlaps_objective(exit_position) {
            self.status = Status::Completed;
            self.clear_input();
            return (
                None,
                Some(PendingEvent {
                    kind: EventKind::Completed,
                    target: self.exit,
                    position: exit_position,
                }),
            );
        }
        (None, None)
    }

    fn overlaps_objective(&self, target: Position) -> bool {
        match self.motion() {
            None => self.entity(self.player).position == target,
            Some(motion) => {
                let (shape, anchor) = queries::tile_collider(target);
                self.level.collider.overlaps(motion.position, shape, anchor)
            }
        }
    }

    fn deliver(&mut self, pending: PendingEvent) {
        let Some(entity) = self.table.get(pending.target) else {
            self.stale_events = self.stale_events.saturating_add(1);
            self.record_trace(SIMULATE_PHASE, ERROR_CHANNEL, 0, 1);
            return;
        };
        let target = entity.id.0;
        let kind = pending.kind as u8;
        let y = pending.position.world_y(self.height()) as u16;
        self.events.push(|sequence| Event {
            sequence,
            tick: self.tick,
            target,
            kind,
            x: pending.position.x as u16,
            y,
        });
        self.record_trace(SIMULATE_PHASE, EVENT_CHANNEL, target, u32::from(kind));
    }

    fn record_trace(&mut self, phase: u8, channel: u8, target: u16, data: u32) {
        if self.trace_mask & (1 << channel) != 0 {
            self.trace.push(|sequence| TraceRecord {
                sequence,
                tick: self.tick,
                phase,
                channel,
                target,
                data,
            });
        }
    }

    fn entity(&self, handle: Handle) -> &Entity {
        self.table.get(handle).expect("Room One live entity")
    }
}

impl Default for Room {
    fn default() -> Self {
        Self::new()
    }
}

struct RoomCollision<'a> {
    level: &'a Level,
    closed_exit: Option<Position>,
}

impl TileGrid for RoomCollision<'_> {
    fn width(&self) -> u32 {
        self.level.width
    }

    fn height(&self) -> u32 {
        self.level.height
    }

    fn is_solid(&self, x: u32, y: u32) -> bool {
        self.closed_exit == Some(Position { x, y }) || self.level.is_solid(x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: u32 = 16_667;
    const TO_KEY: &[u8] = b"RRDDLLDDRRDDLL";
    const TO_LOCKED_EXIT: &[u8] = b"RRDDLLDDRRRRDDRRRRRRUUU";
    const KEY_TO_EXIT: &[u8] = b"RRUURRDDRRRRRRUUU";

    fn tap(room: &mut Room, letter: u8) {
        let action = match letter {
            b'U' => Action::Up,
            b'D' => Action::Down,
            b'L' => Action::Left,
            b'R' => Action::Right,
            _ => panic!("invalid direction"),
        };
        room.input(action, true);
        room.input(action, false);
    }

    fn walk(room: &mut Room, path: &[u8]) {
        for &letter in path {
            tap(room, letter);
            room.advance(TICK);
        }
    }

    #[test]
    fn walls_and_locked_exit_block_movement_until_the_key_is_collected() {
        let mut room = Room::new();
        assert_eq!((room.width(), room.height()), (13, 9));
        assert_eq!(room.cell(0, 1), Cell::Wall as u32);
        assert_eq!(room.cell(u32::MAX, 1), Cell::Wall as u32);
        walk(&mut room, b"ULD");
        assert_eq!(room.cell(1, 1), Cell::Player as u32);
        walk(&mut room, TO_LOCKED_EXIT);
        assert_eq!(room.cell(11, 5), Cell::Player as u32);
        assert_eq!(room.cell(11, 4), Cell::LockedExit as u32);
        assert_eq!(room.status(), Status::FindKey as u32);

        room.restart();
        walk(&mut room, TO_KEY);
        assert_eq!(room.cell(1, 7), Cell::Player as u32);
        assert_eq!(room.cell(11, 4), Cell::OpenExit as u32);
        assert_eq!(room.status(), Status::ExitUnlocked as u32);
        walk(&mut room, KEY_TO_EXIT);
        assert_eq!(room.cell(11, 4), Cell::Player as u32);
        assert_eq!(room.cell(1, 7), Cell::Floor as u32);
        assert_eq!(room.status(), Status::Completed as u32);
        walk(&mut room, b"DDD");
        assert_eq!(room.cell(11, 4), Cell::Player as u32);

        room.restart();
        assert_eq!(room.cell(1, 1), Cell::Player as u32);
        assert_eq!(room.cell(1, 7), Cell::Key as u32);
        assert_eq!(room.cell(11, 4), Cell::LockedExit as u32);
        assert_eq!(room.status(), Status::FindKey as u32);
    }

    #[test]
    fn short_taps_survive_sub_tick_frames_and_play_in_order_once() {
        let mut room = Room::new();
        for &letter in b"RRD" {
            tap(&mut room, letter);
        }
        room.advance(16_666);
        assert_eq!(room.cell(1, 1), Cell::Player as u32);
        room.advance(1);
        assert_eq!(room.cell(2, 1), Cell::Player as u32);
        room.advance(TICK);
        assert_eq!(room.cell(3, 1), Cell::Player as u32);
        room.advance(TICK);
        assert_eq!(room.cell(3, 2), Cell::Player as u32);
        room.advance(100_000);
        assert_eq!(room.cell(3, 2), Cell::Player as u32);
    }

    #[test]
    fn hitch_runs_at_most_four_ticks_and_removes_whole_tick_debt() {
        let mut room = Room::new();
        for &letter in b"RRDDLL" {
            tap(&mut room, letter);
        }
        room.advance(1_000_000);
        assert_eq!(room.cell(3, 3), Cell::Player as u32);
        room.advance(0);
        assert_eq!(room.cell(3, 3), Cell::Player as u32);
        room.advance(TICK);
        assert_eq!(room.cell(2, 3), Cell::Player as u32);
    }

    #[test]
    fn single_step_ignores_accumulated_time_and_hitch_counter_keeps_full_range() {
        let mut room = Room::new();
        room.advance(33_333);
        assert_eq!((room.tick(), room.interpolation()), (1, 999_980));
        room.step_one_tick();
        assert_eq!(
            (room.tick(), room.ticks_run(), room.ticks_dropped()),
            (2, 1, 0)
        );
        assert_eq!(room.interpolation(), 999_980);

        room.advance(u32::MAX);
        assert_eq!(room.ticks_run(), 4);
        assert!(room.ticks_dropped() > u8::MAX.into());
        let hitch = room.trace().last().unwrap();
        assert_eq!(hitch.channel, HITCH_CHANNEL);
        assert_eq!(hitch.data, room.ticks_dropped());
    }

    #[test]
    fn player_collides_with_the_guards_new_tick_position() {
        let mut room = Room::new();
        room.table.get_mut(room.player).unwrap().position = Position { x: 8, y: 1 };
        for _ in 1..GUARD_PATROL.period_ticks {
            room.step_one_tick();
        }
        assert_eq!(room.cell(6, 1), Cell::Guard as u32);
        tap(&mut room, b'L');
        room.step_one_tick();
        assert_eq!(room.cell(7, 1), Cell::Guard as u32);
        assert_eq!(room.cell(8, 1), Cell::Player as u32);
        assert_eq!(
            room.events().collect::<Vec<_>>(),
            [Event {
                sequence: 1,
                tick: 12,
                target: 4,
                kind: 4,
                x: 7,
                y: 7
            }]
        );
        let guard = room
            .entities()
            .find(|entity| entity.id == GUARD.id.0)
            .unwrap();
        assert_eq!(
            (guard.previous_x, guard.x, guard.y),
            (6 * 256, 7 * 256, 7 * 256)
        );
    }

    #[test]
    fn restart_publishes_new_epoch_and_drops_delivery_to_the_old_guard() {
        let mut room = Room::new();
        let prior = room.guard;
        room.restart();
        assert_ne!(room.guard, prior);
        assert_eq!((room.epoch(), room.tick()), (2, 0));
        assert_eq!(room.cell(6, 1), Cell::Guard as u32);
        assert_eq!(room.trace().next().unwrap().data, 1);
        room.deliver(PendingEvent {
            kind: EventKind::GuardBlocked,
            target: prior.unwrap(),
            position: GUARD.spawn,
        });
        assert_eq!(room.events().count(), 0);
        assert_eq!(room.events_dropped(), 1);
        assert_eq!(room.trace().last().unwrap().channel, ERROR_CHANNEL);
    }

    #[test]
    fn trace_replays_canonical_room_and_ordered_effects() {
        let mut room = Room::new();
        walk(&mut room, TO_KEY);
        walk(&mut room, KEY_TO_EXIT);
        assert_eq!(room.status(), Status::Completed as u32);
        assert!(room.trace_replay_complete());
        assert_eq!(
            room.events()
                .map(|event| (event.sequence, event.kind, event.target))
                .collect::<Vec<_>>(),
            [(1, 1, 2), (2, 2, 3), (3, 3, 3)]
        );
        let mut actions = room
            .trace()
            .filter(|record| record.channel == INPUT_CHANNEL)
            .peekable();
        let mut replay = Room::new();
        for tick in 1..=room.tick() {
            let action = if actions.peek().is_some_and(|record| record.tick == tick) {
                let record = actions.next().unwrap();
                Some([Action::Up, Action::Down, Action::Left, Action::Right][record.data as usize])
            } else {
                None
            };
            assert!(replay.replay_tick(EffectiveInput::Tile(action)));
        }
        assert_eq!(replay.status(), room.status());
        assert_eq!(
            replay.entities().collect::<Vec<_>>(),
            room.entities().collect::<Vec<_>>()
        );
        assert_eq!(
            replay.events().collect::<Vec<_>>(),
            room.events().collect::<Vec<_>>()
        );
    }

    #[test]
    fn masked_and_overflowed_traces_are_explicitly_incomplete() {
        let mut room = Room::new();
        room.set_trace_mask(room.trace_mask() & !(1 << INPUT_CHANNEL));
        tap(&mut room, b'U');
        room.step_one_tick();
        assert!(!room.trace_replay_complete());
        room.set_trace_mask(DEFAULT_TRACE_MASK);
        assert!(!room.trace_replay_complete());
        room.restart();
        assert!(room.trace_replay_complete());
        for _ in 0..600 {
            tap(&mut room, b'U');
            room.step_one_tick();
        }
        assert_eq!(room.trace().count(), 512);
        assert_eq!(room.trace_dropped(), 89);
        assert_eq!(room.trace().next().unwrap().sequence, 90);
        assert_eq!(room.trace_latest_sequence(), 601);
        assert!(!room.trace_replay_complete());
    }

    #[test]
    fn event_overflow_keeps_latest_sequences_in_delivery_order() {
        let mut room = Room::new();
        for _ in 0..70 {
            room.deliver(PendingEvent {
                kind: EventKind::GuardBlocked,
                target: room.guard.unwrap(),
                position: GUARD.spawn,
            });
        }
        assert_eq!(room.events_dropped(), 6);
        assert!(room.events().map(|event| event.sequence).eq(7..=70));
    }

    #[test]
    fn clearing_focus_drops_held_input_pending_input_and_fractional_time() {
        let mut room = Room::new();
        room.input(Action::Right, true);
        room.advance(TICK);
        assert_eq!(room.cell(2, 1), Cell::Player as u32);
        tap(&mut room, b'L');
        room.advance(16_665);
        room.clear_input();
        room.advance(1_000_000);
        assert_eq!(room.cell(2, 1), Cell::Player as u32);
        room.input(Action::Right, true);
        room.advance(1);
        assert_eq!(room.cell(2, 1), Cell::Player as u32);
        room.advance(TICK);
        assert_eq!(room.cell(3, 1), Cell::Player as u32);
    }

    #[test]
    fn same_input_produces_same_state_at_different_frame_partitions() {
        let mut normal = Room::new();
        let mut uneven = Room::new();
        for segment in [TO_KEY, KEY_TO_EXIT] {
            for group in segment.chunks(2) {
                for &letter in group {
                    tap(&mut normal, letter);
                    tap(&mut uneven, letter);
                }
                for _ in group {
                    normal.advance(TICK);
                }
                uneven.advance(TICK * group.len() as u32 / 3);
                uneven.advance(TICK * group.len() as u32 - TICK * group.len() as u32 / 3);
                assert_eq!(normal.status(), uneven.status());
                for y in 0..normal.height() {
                    for x in 0..normal.width() {
                        assert_eq!(normal.cell(x, y), uneven.cell(x, y));
                    }
                }
            }
        }
        assert_eq!(uneven.status(), Status::Completed as u32);
    }
}
