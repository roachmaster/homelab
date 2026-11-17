//! Scalar WebAssembly interface for the local playtest room.
//!
//! The lint allowances apply only to linker export names. All calls use
//! ordinary Rust references, and the browser supplies no memory addresses.

use forge_game_runtime::{Action, Room};
use std::cell::RefCell;

thread_local! {
    static ROOM: RefCell<Room> = RefCell::new(Room::new());
}

fn read(get: impl FnOnce(&Room) -> u32) -> u32 {
    ROOM.with(|room| room.try_borrow().map_or(0, |room| get(&room)))
}

fn update(apply: impl FnOnce(&mut Room)) {
    ROOM.with(|room| {
        if let Ok(mut room) = room.try_borrow_mut() {
            apply(&mut room);
        }
    });
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_width() -> u32 {
    read(Room::width)
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_height() -> u32 {
    read(Room::height)
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_cell(x: u32, y: u32) -> u32 {
    read(|room| room.cell(x, y))
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_status() -> u32 {
    read(Room::status)
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
