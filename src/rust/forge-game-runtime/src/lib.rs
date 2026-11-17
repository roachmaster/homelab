//! Fixed-step simulation for the first playable Forge room.
//!
//! An adapter supplies elapsed microseconds and directional input edges, then
//! reads the complete room after advancing it. The simulation owns no clock,
//! display, input device, or platform resources.

mod input;

use input::InputBuffer;

const TICKS_PER_SECOND: u64 = 60;
const MICROS_PER_SECOND: u64 = 1_000_000;
const MAX_TICKS_PER_ADVANCE: u64 = 4;
const WIDTH: u32 = 13;
const HEIGHT: u32 = 9;
const START: Position = Position { x: 1, y: 1 };
const MAP: [&[u8; WIDTH as usize]; HEIGHT as usize] = [
    b"#############",
    b"#S..#.......#",
    b"###.#.#####.#",
    b"#...#...#...#",
    b"#.#####.#.#E#",
    b"#.....#.#...#",
    b"###.#.#.###.#",
    b"#K..#.......#",
    b"#############",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Up,
    Down,
    Left,
    Right,
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
}

/// Stable values for the room's current objective.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    FindKey = 0,
    ExitUnlocked = 1,
    Completed = 2,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Position {
    x: u32,
    y: u32,
}

impl Position {
    fn next(self, action: Action) -> Option<Self> {
        let (dx, dy) = match action {
            Action::Up => (0, -1),
            Action::Down => (0, 1),
            Action::Left => (-1, 0),
            Action::Right => (1, 0),
        };
        Some(Self {
            x: self.x.checked_add_signed(dx)?,
            y: self.y.checked_add_signed(dy)?,
        })
    }

    fn terrain(self) -> u8 {
        MAP.get(self.y as usize)
            .and_then(|row| row.get(self.x as usize))
            .copied()
            .unwrap_or(b'#')
    }
}

/// An independent room with an integer tile position and a fixed simulation clock.
pub struct Room {
    player: Position,
    status: Status,
    tick_fraction: u64,
    input: InputBuffer,
}

impl Room {
    pub fn new() -> Self {
        Self {
            player: START,
            status: Status::FindKey,
            tick_fraction: 0,
            input: InputBuffer::new(),
        }
    }

    pub fn width(&self) -> u32 {
        WIDTH
    }

    pub fn height(&self) -> u32 {
        HEIGHT
    }

    /// Returns the cell code with x increasing rightward and y increasing
    /// downward. The player occupies its tile visually. Outside cells are walls.
    pub fn cell(&self, x: u32, y: u32) -> u32 {
        let position = Position { x, y };
        let cell = if position == self.player {
            Cell::Player
        } else {
            match position.terrain() {
                b'#' => Cell::Wall,
                b'K' if self.status == Status::FindKey => Cell::Key,
                b'E' if self.status == Status::FindKey => Cell::LockedExit,
                b'E' => Cell::OpenExit,
                _ => Cell::Floor,
            }
        };
        cell as u32
    }

    pub fn status(&self) -> u32 {
        self.status as u32
    }

    /// Queues a press or release for playback at the next simulation tick.
    pub fn input(&mut self, action: Action, pressed: bool) {
        if self.status != Status::Completed {
            self.input.enqueue(action, pressed);
        }
    }

    /// Runs up to four 60 Hz ticks and drops any remaining whole-tick debt,
    /// preserving fractional time. Each tick can move the player one tile.
    pub fn advance(&mut self, elapsed_micros: u32) {
        let time = self.tick_fraction + u64::from(elapsed_micros) * TICKS_PER_SECOND;
        let ticks = (time / MICROS_PER_SECOND).min(MAX_TICKS_PER_ADVANCE);
        self.tick_fraction = time % MICROS_PER_SECOND;
        for _ in 0..ticks {
            if let Some(action) = self.input.next_step() {
                self.step(action);
            }
            if self.status == Status::Completed {
                break;
            }
        }
    }

    /// Drops pending input, held directions, and fractional clock time on focus loss.
    pub fn clear_input(&mut self) {
        self.input.clear();
        self.tick_fraction = 0;
    }

    /// Restores the initial room, including the key and the locked exit.
    pub fn restart(&mut self) {
        self.player = START;
        self.status = Status::FindKey;
        self.clear_input();
    }

    fn step(&mut self, action: Action) {
        let Some(target) = self.player.next(action) else {
            return;
        };
        match target.terrain() {
            b'#' => return,
            b'E' if self.status == Status::FindKey => return,
            b'E' => {
                self.status = Status::Completed;
                self.clear_input();
            }
            b'K' => self.status = Status::ExitUnlocked,
            _ => {}
        }
        self.player = target;
    }
}

impl Default for Room {
    fn default() -> Self {
        Self::new()
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
