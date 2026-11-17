//! WebAssembly interface for the local playtest room.
//!
//! The lint allowances apply only to linker export names. All calls use
//! ordinary Rust references. The browser reads producer-owned byte buffers
//! after each call returns and supplies no memory addresses.

use crate::playtest_wire::{self, SNAPSHOT_SIZE, TRACE_SIZE};
use forge_game_runtime::{Action, Cell, Room};
use std::cell::RefCell;

struct Playtest {
    room: Room,
    snapshot: Box<[u8; SNAPSHOT_SIZE]>,
    trace: Box<[u8; TRACE_SIZE]>,
    trace_publication: Option<(u32, u32, u32, u32, bool)>,
}

impl Playtest {
    fn new() -> Self {
        let mut playtest = Self {
            room: Room::new(),
            snapshot: Box::new([0; SNAPSHOT_SIZE]),
            trace: Box::new([0; TRACE_SIZE]),
            trace_publication: None,
        };
        playtest.publish();
        playtest
    }

    fn publish(&mut self) {
        playtest_wire::snapshot(&self.room, &mut self.snapshot);
        let trace_publication = (
            self.room.epoch(),
            self.room.trace_latest_sequence(),
            self.room.trace_dropped(),
            self.room.trace_mask(),
            self.room.trace_replay_complete(),
        );
        if self.trace_publication != Some(trace_publication) {
            playtest_wire::trace(&self.room, &mut self.trace);
            self.trace_publication = Some(trace_publication);
        }
    }
}

thread_local! {
    static PLAYTEST: RefCell<Playtest> = RefCell::new(Playtest::new());
}

fn read(get: impl FnOnce(&Playtest) -> u32) -> u32 {
    PLAYTEST.with(|playtest| playtest.try_borrow().map_or(0, |playtest| get(&playtest)))
}

fn update(apply: impl FnOnce(&mut Room)) {
    PLAYTEST.with(|playtest| {
        if let Ok(mut playtest) = playtest.try_borrow_mut() {
            apply(&mut playtest.room);
            playtest.publish();
        }
    });
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_width() -> u32 {
    read(|playtest| playtest.room.width())
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_height() -> u32 {
    read(|playtest| playtest.room.height())
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_cell(x: u32, y: u32) -> u32 {
    read(|playtest| {
        let cell = playtest.room.cell(x, y);
        if cell == Cell::Guard as u32 {
            // Older Canvas adapters already know how to paint an obstacle.
            Cell::Wall as u32
        } else {
            cell
        }
    })
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_status() -> u32 {
    read(|playtest| playtest.room.status())
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_input(action: u32, pressed: u32) {
    let action = match action {
        0 => Action::Up,
        1 => Action::Down,
        2 => Action::Left,
        3 => Action::Right,
        _ => return,
    };
    if pressed <= 1 {
        update(|room| room.input(action, pressed == 1));
    }
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_advance(elapsed_micros: u32) {
    update(|room| room.advance(elapsed_micros));
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_clear_input() {
    update(Room::clear_input);
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_restart() {
    update(Room::restart);
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_snapshot() -> u32 {
    read(|playtest| playtest.snapshot.as_ptr() as u32)
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_snapshot_len() -> u32 {
    SNAPSHOT_SIZE as u32
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_trace() -> u32 {
    read(|playtest| playtest.trace.as_ptr() as u32)
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_trace_len() -> u32 {
    TRACE_SIZE as u32
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_trace_channels(mask: u32) {
    update(|room| room.set_trace_mask(mask));
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_step() {
    update(Room::step_one_tick);
}
