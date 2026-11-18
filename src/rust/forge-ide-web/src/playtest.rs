//! WebAssembly interface for the local playtest room.
//!
//! The lint allowances apply only to linker export names. All calls use
//! ordinary Rust references. The browser reads producer-owned byte buffers
//! and fills bounded asset uploads allocated by Rust. It supplies no addresses.

use crate::playtest_wire::{self, SNAPSHOT_SIZE, TRACE_SIZE};
use forge_game_runtime::{Action, Cell, Room};
use std::cell::RefCell;

#[path = "playtest_asset.rs"]
mod asset;
#[path = "playtest_collision.rs"]
mod collision;
#[path = "playtest_level.rs"]
mod level;
#[path = "playtest_replay.rs"]
mod replay;

const POSE_SIZE: usize = 32;

struct Playtest {
    room: Room,
    snapshot: Box<[u8; SNAPSHOT_SIZE]>,
    trace: Box<[u8; TRACE_SIZE]>,
    pose: Box<[u8; POSE_SIZE]>,
    level_publication: Box<[u8; level::PUBLICATION_SIZE]>,
    loader: level::Loader,
    assets: asset::Assets,
    trace_publication: Option<(u32, u32, u32, u32, bool)>,
    replay: replay::Session,
    replay_bytes: Vec<u8>,
    replay_input: Vec<u8>,
    replay_import: Option<replay::Import>,
    replay_archive: replay::ArchivePort,
    replay_result: Option<ReplayView>,
    collision_view: Box<[u8; collision::SIZE]>,
    paused: bool,
}

struct ReplayView {
    snapshot: Box<[u8; SNAPSHOT_SIZE]>,
    pose: [u8; POSE_SIZE],
    import: replay::Import,
    timeline: replay::Timeline,
}

impl ReplayView {
    fn publish(&mut self) {
        playtest_wire::snapshot(&self.timeline.room, &mut self.snapshot);
        write_pose(&self.timeline.room, &mut self.pose);
    }
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
            assets: asset::Assets::default(),
            trace_publication: None,
            replay: replay::Session::new(1),
            replay_bytes: Vec::new(),
            replay_input: Vec::new(),
            replay_import: None,
            replay_archive: replay::ArchivePort::new(),
            replay_result: None,
            collision_view: Box::new([0; collision::SIZE]),
            paused: false,
        };
        playtest.publish();
        playtest
    }

    fn accept_asset(&mut self, asset: asset::Asset, request: u32) -> bool {
        let Some(bytes) = self.assets.take(asset) else {
            return false;
        };
        if asset == asset::Asset::Manifest {
            return (request == 0 || self.loader.requested_slot(request).is_some())
                && self.assets.install_manifest(&bytes);
        }
        if let Some(slot) = asset.slot() {
            return self.loader.awaiting(request, slot)
                && self.assets.verify(asset, &bytes).is_some_and(|digest| {
                    self.loader
                        .accept(request, slot, &bytes, digest, &self.room)
                });
        }
        if request != 0 || self.assets.verify(asset, &bytes).is_none() {
            return false;
        }
        match asset {
            asset::Asset::ScoutAtlas => self.assets.prepare_atlas(&bytes),
            asset::Asset::ScoutAtlasImage => self.assets.finish_atlas(&bytes),
            _ => false,
        }
    }

    fn activate(&mut self) -> bool {
        let boundary_tick = self.room.tick().saturating_add(1);
        self.loader.activate(&mut self.room, |slot, digest, epoch| {
            self.replay.activate(boundary_tick, epoch, slot, digest);
        })
    }

    fn advance(&mut self, elapsed_micros: u32) {
        if self.paused {
            return;
        }
        if self.room.activation_boundary(elapsed_micros) && self.activate() {
            return;
        }
        self.room.advance_observed(elapsed_micros, |tick, input| {
            self.replay.record(tick, input);
        });
    }

    fn step(&mut self) {
        if !self.activate() {
            self.room.step_observed(|tick, input| {
                self.replay.record(tick, input);
            });
        }
    }

    fn restart(&mut self) {
        self.loader.restart();
        if let Some(epoch) = self.room.epoch().checked_add(1) {
            self.replay.restart(self.room.tick(), epoch);
            self.room.restart();
        }
    }

    fn set_paused(&mut self, paused: bool) {
        if self.paused != paused {
            self.room.clear_input();
            self.paused = paused;
        }
    }

    fn publish(&mut self) {
        playtest_wire::snapshot(&self.room, &mut self.snapshot);
        write_pose(&self.room, &mut self.pose);
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

    fn reserve_replay(&mut self, length: usize) -> u32 {
        self.replay_input.clear();
        self.replay_import = None;
        self.replay_archive.clear();
        if !(1..=replay::MAX_BYTES).contains(&length)
            || self.replay_input.try_reserve(length).is_err()
        {
            return 0;
        }
        self.replay_input.resize(length, 0);
        self.replay_input.as_mut_ptr() as u32
    }

    fn prepare_replay(&mut self) -> bool {
        let bytes = std::mem::take(&mut self.replay_input);
        self.replay_archive.clear();
        self.replay_import = replay::Import::parse(&bytes, &self.loader);
        self.replay_import.is_some()
    }

    fn finish_replay(&mut self) -> bool {
        if self.replay_import.is_none() && !self.prepare_replay() {
            return false;
        }
        self.replay_archive.clear();
        let import = self.replay_import.take().expect("prepared replay");
        let Some(timeline) = import.validate() else {
            return false;
        };
        let mut result = ReplayView {
            snapshot: Box::new([0; SNAPSHOT_SIZE]),
            pose: [0; POSE_SIZE],
            import,
            timeline,
        };
        result.publish();
        self.replay_result = Some(result);
        true
    }
}

fn write_pose(room: &Room, buffer: &mut [u8; POSE_SIZE]) {
    let pose = room.pose();
    buffer.fill(0);
    buffer[..4].copy_from_slice(b"FGP1");
    buffer[4..8].copy_from_slice(&room.epoch().to_le_bytes());
    buffer[8..12].copy_from_slice(&room.tick().to_le_bytes());
    buffer[12..14].copy_from_slice(&pose.entity_id.to_le_bytes());
    buffer[14] = pose.phase as u8;
    buffer[15] = u8::from(pose.left);
    buffer[16..20].copy_from_slice(&pose.frame.to_le_bytes());
    buffer[20] = u8::from(pose.grounded);
    buffer[21] = pose.contacts;
    if let Some(contact) = room.motion().and_then(|motion| {
        motion
            .landed
            .or(motion.head_bump)
            .or(motion.wall_contact)
            .or(motion.contacts.ground)
    }) {
        buffer[22..24].copy_from_slice(&(contact.tile_x as i16).to_le_bytes());
        let y = room.height() as i32 - 1 - contact.tile_y;
        buffer[24..26].copy_from_slice(&(y as i16).to_le_bytes());
        buffer[26] = contact.normal as u8;
        buffer[28..32].copy_from_slice(&contact.impact_speed.to_le_bytes());
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
    update_playtest(|state| state.advance(elapsed_micros));
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_clear_input() {
    update(Room::clear_input);
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_restart() {
    update_playtest(Playtest::restart);
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
    update_playtest(Playtest::step);
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_set_paused(paused: u32) {
    if paused <= 1 {
        update_playtest(|state| state.set_paused(paused == 1));
    }
}

/// Encode a caller-requested export. Bytes remain valid until the next export.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_replay() -> u32 {
    PLAYTEST.with(|playtest| {
        playtest.try_borrow_mut().map_or(0, |mut state| {
            state.replay_bytes = serde_json::to_vec(&state.replay).unwrap_or_default();
            if state.replay_bytes.is_empty() {
                0
            } else {
                state.replay_bytes.as_ptr() as u32
            }
        })
    })
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_replay_len() -> u32 {
    read(|state| state.replay_bytes.len() as u32)
}

/// Reserve producer-owned WASM bytes for one complete replay document.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_replay_reserve(length: u32) -> u32 {
    PLAYTEST.with(|playtest| {
        playtest
            .try_borrow_mut()
            .map_or(0, |mut state| state.reserve_replay(length as usize))
    })
}

/// Validate the replay document and prepare its historical asset requests.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_replay_prepare() -> u32 {
    deliver(Playtest::prepare_replay)
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_replay_need_count() -> u32 {
    read(|state| {
        state
            .replay_import
            .as_ref()
            .map_or(0, replay::Import::need_count)
    })
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_replay_need_slot(index: u32) -> u32 {
    read(|state| {
        state
            .replay_import
            .as_ref()
            .map_or(0, |import| import.missing_slot(index as usize))
    })
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_replay_need_digest(index: u32, word: u32) -> u32 {
    read(|state| {
        state.replay_import.as_ref().map_or(0, |import| {
            import.digest_word(index as usize, word as usize)
        })
    })
}

/// Verify one acquired archive asset against the prepared replay's digest.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_replay_asset_accept(index: u32, asset: u32) -> u32 {
    deliver(|state| {
        if state.replay_archive.pending() {
            return false;
        }
        let Some(asset) = asset::Asset::from_wire(asset) else {
            return false;
        };
        let Some(bytes) = state.assets.take(asset) else {
            return false;
        };
        state
            .replay_import
            .as_mut()
            .is_some_and(|import| import.accept_asset(index as usize, asset, &bytes))
    })
}

/// Publish the next typed archive read with the generation of its completion.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_replay_archive_read() -> u32 {
    PLAYTEST.with(|playtest| {
        playtest.try_borrow_mut().map_or(0, |mut state| {
            let Playtest {
                replay_import,
                replay_archive,
                ..
            } = &mut *state;
            replay_import
                .as_ref()
                .map_or(0, |import| replay_archive.read(import))
        })
    })
}

/// Deliver bytes for the one currently issued archive read.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_replay_archive_accept(generation: u32) -> u32 {
    deliver(|state| {
        state.replay_import.as_mut().is_some_and(|import| {
            state
                .replay_archive
                .accept(generation, import, &mut state.assets)
        })
    })
}

/// Discard a failed/cancelled import without replacing the previous replay.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_replay_archive_cancel() {
    update_playtest(|state| {
        state.replay_archive.clear();
        state.replay_import = None;
    });
}

/// Consume the input and publish the replay view only if it validates fully.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_replay_finish() -> u32 {
    deliver(Playtest::finish_replay)
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_replay_restart() -> u32 {
    deliver(|state| {
        let Some(view) = &mut state.replay_result else {
            return false;
        };
        view.timeline = replay::Timeline::new();
        view.publish();
        true
    })
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_replay_step(count: u32) -> u32 {
    deliver(|state| {
        let Some(view) = &mut state.replay_result else {
            return false;
        };
        let mut advanced = false;
        for _ in 0..count.min(4) {
            if view.timeline.step(&view.import) != Some(true) {
                break;
            }
            advanced = true;
        }
        if advanced {
            view.publish();
        }
        advanced
    })
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_replay_done() -> u32 {
    read(|state| {
        u32::from(
            state
                .replay_result
                .as_ref()
                .is_some_and(|view| view.timeline.done(&view.import)),
        )
    })
}

/// Latest successful replay result, in the normal FGS1 snapshot format.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_replay_snapshot() -> u32 {
    read(|state| {
        state
            .replay_result
            .as_ref()
            .map_or(0, |view| view.snapshot.as_ptr() as u32)
    })
}

/// Latest successful replay pose, in the normal FGP1 format.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_replay_pose() -> u32 {
    read(|state| {
        state
            .replay_result
            .as_ref()
            .map_or(0, |view| view.pose.as_ptr() as u32)
    })
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
pub extern "C" fn forge_playtest_collision_snapshot() -> u32 {
    PLAYTEST.with(|playtest| {
        let Ok(mut playtest) = playtest.try_borrow_mut() else {
            return 0;
        };
        let Playtest {
            room,
            collision_view,
            ..
        } = &mut *playtest;
        collision::write(room, collision_view);
        collision_view.as_ptr() as u32
    })
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_collision_snapshot_len() -> u32 {
    collision::SIZE as u32
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
pub extern "C" fn forge_playtest_level_request(slot: u32) -> u32 {
    let Some(slot) = asset::Asset::from_wire(slot).and_then(asset::Asset::slot) else {
        return 0;
    };
    PLAYTEST.with(|playtest| {
        playtest
            .try_borrow_mut()
            .map_or(0, |mut state| state.loader.request(slot))
    })
}

/// Publish the next cooked asset requested by a staged level generation.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_level_read(request: u32) -> u32 {
    read(|state| {
        state
            .loader
            .requested_slot(request)
            .and_then(|slot| state.assets.level_read(slot))
            .map_or(0, asset::Asset::read_code)
    })
}

/// Publish the next asset needed for the startup atlas.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_atlas_read() -> u32 {
    read(|state| state.assets.atlas_read().map_or(0, asset::Asset::read_code))
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_level_fail(request: u32) {
    PLAYTEST.with(|playtest| {
        if let Ok(mut state) = playtest.try_borrow_mut() {
            state.loader.fail(request);
        }
    });
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_level_collider(edge: u32) -> i32 {
    read(|state| state.loader.active_collider(edge) as u32) as i32
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_asset_reserve(asset: u32, length: u32) -> u32 {
    let Some(asset) = asset::Asset::from_wire(asset) else {
        return 0;
    };
    PLAYTEST.with(|playtest| {
        playtest
            .try_borrow_mut()
            .map_or(0, |mut state| state.assets.reserve(asset, length as usize))
    })
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_asset_accept(asset: u32, request: u32) -> u32 {
    asset::Asset::from_wire(asset).map_or(0, |asset| {
        deliver(|state| state.accept_asset(asset, request))
    })
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_atlas_view() -> u32 {
    read(|state| state.assets.atlas_view().as_ptr() as u32)
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn forge_playtest_atlas_view_len() -> u32 {
    read(|state| state.assets.atlas_view().len() as u32)
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
