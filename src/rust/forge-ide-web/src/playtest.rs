//! WebAssembly interface for the local playtest room.
//!
//! The lint allowances apply only to linker export names. All calls use
//! ordinary Rust references. The browser reads producer-owned byte buffers
//! after each call returns and supplies no memory addresses.

use crate::playtest_wire::{self, SNAPSHOT_SIZE, TRACE_SIZE};
use forge_game_runtime::{Action, Cell, Room};
use std::cell::RefCell;

#[path = "playtest_level.rs"]
mod level;

const POSE_SIZE: usize = 32;

struct Playtest {
    room: Room,
    snapshot: Box<[u8; SNAPSHOT_SIZE]>,
    trace: Box<[u8; TRACE_SIZE]>,
    pose: Box<[u8; POSE_SIZE]>,
    level_publication: Box<[u8; level::PUBLICATION_SIZE]>,
    loader: level::Loader,
    trace_publication: Option<(u32, u32, u32, u32, bool)>,
}

impl Playtest {
    fn new() -> Self {
        let mut playtest = Self {
            room: Room::new(),
            snapshot: Box::new([0; SNAPSHOT_SIZE]),
            trace: Box::new([0; TRACE_SIZE]),
            pose: Box::new([0; POSE_SIZE]),
            level_publication: Box::new([0; level::PUBLICATION_SIZE]),
            loader: level::Loader::default(),
            trace_publication: None,
        };
        playtest.publish();
        playtest
    }

    fn publish(&mut self) {
        playtest_wire::snapshot(&self.room, &mut self.snapshot);
        self.publish_pose();
        self.loader
            .publication(self.room.epoch(), &mut self.level_publication);
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

    fn publish_pose(&mut self) {
        let pose = self.room.pose();
        self.pose.fill(0);
        self.pose[..4].copy_from_slice(b"FGP1");
        self.pose[4..8].copy_from_slice(&self.room.epoch().to_le_bytes());
        self.pose[8..12].copy_from_slice(&self.room.tick().to_le_bytes());
        self.pose[12..14].copy_from_slice(&pose.entity_id.to_le_bytes());
        self.pose[14] = pose.phase as u8;
        self.pose[15] = u8::from(pose.left);
        self.pose[16..20].copy_from_slice(&pose.frame.to_le_bytes());
        self.pose[20] = u8::from(pose.grounded);
        self.pose[21] = pose.contacts;
        if let Some(contact) = self.room.motion().and_then(|motion| {
            motion
                .landed
                .or(motion.head_bump)
                .or(motion.wall_contact)
                .or(motion.contacts.ground)
        }) {
            self.pose[22..24].copy_from_slice(&(contact.tile_x as i16).to_le_bytes());
            let y = self.room.height() as i32 - 1 - contact.tile_y;
            self.pose[24..26].copy_from_slice(&(y as i16).to_le_bytes());
            self.pose[26] = contact.normal as u8;
            self.pose[28..32].copy_from_slice(&contact.impact_speed.to_le_bytes());
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
    update_playtest(|playtest| apply(&mut playtest.room));
}

fn update_playtest(apply: impl FnOnce(&mut Playtest)) {
    PLAYTEST.with(|playtest| {
        if let Ok(mut playtest) = playtest.try_borrow_mut() {
            apply(&mut playtest);
            playtest.publish();
        }
    });
}

fn deliver(apply: impl FnOnce(&mut Playtest) -> bool) -> u32 {
    PLAYTEST.with(|playtest| {
        playtest
            .try_borrow_mut()
            .map_or(0, |mut playtest| u32::from(apply(&mut playtest)))
    })
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
        4 => Action::Jump,
        _ => return,
    };
    if pressed <= 1 {
        update(|room| room.input(action, pressed == 1));
    }
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_advance(elapsed_micros: u32) {
    update_playtest(|state| {
        if !state.room.activation_boundary(elapsed_micros)
            || !state.loader.activate(&mut state.room)
        {
            state.room.advance(elapsed_micros);
        }
    });
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_clear_input() {
    update(Room::clear_input);
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_restart() {
    update_playtest(|state| {
        state.loader.restart();
        state.room.restart();
    });
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
    update_playtest(|state| {
        if !state.loader.activate(&mut state.room) {
            state.room.step_one_tick();
        }
    });
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_pose_snapshot() -> u32 {
    read(|playtest| playtest.pose.as_ptr() as u32)
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_pose_snapshot_len() -> u32 {
    POSE_SIZE as u32
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_level_snapshot() -> u32 {
    read(|playtest| playtest.level_publication.as_ptr() as u32)
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_level_snapshot_len() -> u32 {
    level::PUBLICATION_SIZE as u32
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_level() -> u32 {
    read(|playtest| playtest.loader.active_slot())
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_level_begin(request: u32, manifest_tag: u32) -> u32 {
    deliver(|state| state.loader.begin(request, manifest_tag))
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_level_manifest_word(request: u32, index: u32, word: u32) -> u32 {
    deliver(|state| state.loader.manifest_word(request, index, word))
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_level_byte(request: u32, byte: u32) -> u32 {
    deliver(|state| state.loader.byte(request, byte))
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_level_prepare(request: u32) -> u32 {
    deliver(|state| state.loader.prepare(request, &state.room))
}
