//! Bounded delivery and tick-boundary activation of cooked level data.

use forge_game_runtime::physics::Collider;
use forge_game_runtime::{
    AnimationTiming, Level, LevelId, LevelMode, LevelSpec, PatrolSpec, Room, SpawnKind, SpawnSpec,
    SurfaceSpec,
};
use serde::Deserialize;

const MAX_LEVEL_BYTES: usize = 16 * 1024;
pub(super) const PUBLICATION_SIZE: usize = 48;

fn level_slot(level: &Level) -> u32 {
    match level.id() {
        LevelId::RoomOne => 1,
        LevelId::TraversalYard => 2,
    }
}

pub(super) fn decode_replay_level(bytes: &[u8], slot: u32) -> Option<Level> {
    let (level, _) = CookedLevel::decode(bytes)?;
    (level_slot(&level) == slot).then_some(level)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CookedLevel {
    version: u16,
    id: CookedId,
    mode: CookedMode,
    width: u32,
    height: u32,
    tiles: Vec<String>,
    spawns: Vec<CookedSpawn>,
    atlas: String,
    surfaces: Vec<CookedSurface>,
    surface: String,
    timing: CookedTiming,
    collider: CookedCollider,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CookedId {
    RoomOne,
    TraversalYard,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum CookedMode {
    Tile,
    Platform,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CookedSpawn {
    id: u16,
    kind: CookedKind,
    x: u32,
    y: u32,
    patrol: Option<CookedPatrol>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum CookedKind {
    Player,
    Key,
    Exit,
    Guard,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CookedPatrol {
    first_x: u32,
    last_x: u32,
    period_ticks: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CookedSurface {
    id: String,
    friction_q16: u32,
    restitution_q16: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CookedTiming {
    idle: [u32; 1],
    walk: [u32; 4],
    jump: [u32; 12],
    crouch: [u32; 4],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CookedCollider {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl CookedLevel {
    fn decode(bytes: &[u8]) -> Option<(Level, [i32; 4])> {
        let cooked: Self = serde_json::from_slice(bytes).ok()?;
        if cooked.version != 1 || cooked.atlas != "scout-atlas" {
            return None;
        }
        let edges = cooked.collider;
        let collider = Collider::from_edges(edges.left, edges.top, edges.right, edges.bottom)?;
        let level = Level::validate(LevelSpec {
            id: match cooked.id {
                CookedId::RoomOne => LevelId::RoomOne,
                CookedId::TraversalYard => LevelId::TraversalYard,
            },
            mode: match cooked.mode {
                CookedMode::Tile => LevelMode::Tile,
                CookedMode::Platform => LevelMode::Platform,
            },
            width: cooked.width,
            height: cooked.height,
            tiles: cooked.tiles,
            spawns: cooked
                .spawns
                .into_iter()
                .map(|spawn| SpawnSpec {
                    id: spawn.id,
                    kind: match spawn.kind {
                        CookedKind::Player => SpawnKind::Player,
                        CookedKind::Key => SpawnKind::Key,
                        CookedKind::Exit => SpawnKind::Exit,
                        CookedKind::Guard => SpawnKind::Guard,
                    },
                    x: spawn.x,
                    y: spawn.y,
                    patrol: spawn.patrol.map(|patrol| PatrolSpec {
                        first_x: patrol.first_x,
                        last_x: patrol.last_x,
                        period_ticks: patrol.period_ticks,
                    }),
                })
                .collect(),
            surfaces: cooked
                .surfaces
                .into_iter()
                .map(|surface| SurfaceSpec {
                    id: surface.id,
                    friction_q16: surface.friction_q16,
                    restitution_q16: surface.restitution_q16,
                })
                .collect(),
            surface: cooked.surface,
            timing: AnimationTiming {
                idle: cooked.timing.idle,
                walk: cooked.timing.walk,
                jump: cooked.timing.jump,
                crouch: cooked.timing.crouch,
            },
            collider,
        })
        .ok()?;
        Some((level, collider.edges()))
    }
}

struct Request {
    bytes: Vec<u8>,
    digest: [u32; 8],
    received: u8,
    first_word: u32,
}

struct Prepared {
    room: Room,
    slot: u32,
    digest: [u32; 8],
    collider: [i32; 4],
    verified: bool,
}

struct RetainedLevel {
    level: Level,
    digest: [u32; 8],
}

#[derive(Default)]
pub(super) struct Loader {
    latest: u32,
    request: Option<Request>,
    requested_slot: Option<u32>,
    prepared: Option<Prepared>,
    replay_levels: [Option<RetainedLevel>; 2],
    active_slot: u32,
    activation_tick: u32,
    active_digest: [u32; 8],
    active_collider: [i32; 4],
}

impl Loader {
    pub(super) fn begin(&mut self, request: u32, first_word: u32) -> bool {
        if request <= self.latest {
            return false;
        }
        self.latest = request;
        self.prepared = None;
        self.requested_slot = None;
        self.request = Some(Request {
            bytes: Vec::new(),
            digest: [0; 8],
            received: 0,
            first_word,
        });
        true
    }

    pub(super) fn request(&mut self, slot: u32) -> u32 {
        let Some(request) = self
            .latest
            .checked_add(1)
            .filter(|_| slot != self.active_slot)
        else {
            return 0;
        };
        self.latest = request;
        self.request = None;
        self.requested_slot = Some(slot);
        self.prepared = None;
        request
    }

    pub(super) fn awaiting(&self, request: u32, slot: u32) -> bool {
        self.latest == request && self.requested_slot == Some(slot)
    }

    pub(super) fn fail(&mut self, request: u32) {
        if self.latest == request {
            self.requested_slot = None;
        }
    }

    pub(super) fn accept(
        &mut self,
        request: u32,
        slot: u32,
        bytes: &[u8],
        digest: [u32; 8],
        active: &Room,
    ) -> bool {
        if !self.awaiting(request, slot) {
            return false;
        }
        self.requested_slot = None;
        self.stage(bytes, digest, Some(slot), active)
    }

    pub(super) fn byte(&mut self, request: u32, byte: u32) -> bool {
        let Some(input) = self.request.as_mut().filter(|_| request == self.latest) else {
            return false;
        };
        if input.bytes.len() == MAX_LEVEL_BYTES || byte > u8::MAX as u32 {
            self.request = None;
            return false;
        }
        input.bytes.push(byte as u8);
        true
    }

    pub(super) fn manifest_word(&mut self, request: u32, index: u32, word: u32) -> bool {
        let Some(input) = self.request.as_mut().filter(|_| request == self.latest) else {
            return false;
        };
        if index >= 8
            || (index == 0 && word != input.first_word)
            || input.received & (1 << index) != 0
        {
            self.request = None;
            return false;
        }
        input.received |= 1 << index;
        input.digest[index as usize] = word;
        true
    }

    pub(super) fn prepare(&mut self, request: u32, active: &Room) -> bool {
        if request != self.latest {
            return false;
        }
        let Some(input) = self
            .request
            .take()
            .filter(|input| input.received == u8::MAX)
        else {
            return false;
        };
        self.stage(&input.bytes, input.digest, None, active)
    }

    fn stage(
        &mut self,
        bytes: &[u8],
        digest: [u32; 8],
        expected: Option<u32>,
        active: &Room,
    ) -> bool {
        let Some(((level, collider), epoch)) =
            CookedLevel::decode(bytes).zip(active.epoch().checked_add(1))
        else {
            return false;
        };
        let slot = level_slot(&level);
        if expected.is_some_and(|expected| expected != slot) {
            return false;
        }
        if expected.is_some() {
            self.replay_levels[slot as usize - 1] = Some(RetainedLevel {
                level: level.clone(),
                digest,
            });
        }
        self.prepared = Some(Prepared {
            room: Room::from_validated(level, epoch, active.trace_mask()),
            slot,
            digest,
            collider,
            verified: expected.is_some(),
        });
        true
    }

    pub(super) fn activate(
        &mut self,
        active: &mut Room,
        before_replace: impl FnOnce(u32, Option<[u32; 8]>, u32),
    ) -> bool {
        let Some(mut ready) = self.prepared.take() else {
            return false;
        };
        before_replace(
            ready.slot,
            ready.verified.then_some(ready.digest),
            ready.room.epoch(),
        );
        self.activation_tick = active.tick().saturating_add(1);
        self.active_slot = ready.slot;
        self.active_digest = ready.digest;
        self.active_collider = ready.collider;
        ready.room.set_trace_mask(active.trace_mask());
        *active = ready.room;
        true
    }

    pub(super) fn restart(&mut self) {
        self.request = None;
        self.requested_slot = None;
        self.prepared = None;
        self.activation_tick = 0;
    }

    pub(super) fn active_slot(&self) -> u32 {
        self.active_slot
    }

    pub(super) fn replay_level(&self, slot: u32, digest: [u32; 8]) -> Option<Level> {
        let retained = self
            .replay_levels
            .get(slot.checked_sub(1)? as usize)?
            .as_ref()?;
        (retained.digest == digest).then(|| retained.level.clone())
    }

    pub(super) fn active_collider(&self, edge: u32) -> i32 {
        self.active_collider
            .get(edge as usize)
            .copied()
            .unwrap_or(0)
    }

    pub(super) fn publication(&self, epoch: u32, bytes: &mut [u8; PUBLICATION_SIZE]) {
        bytes[..4].copy_from_slice(b"FGL1");
        bytes[4..8].copy_from_slice(&epoch.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.activation_tick.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.active_slot.to_le_bytes());
        for (part, word) in bytes[16..].chunks_exact_mut(4).zip(self.active_digest) {
            part.copy_from_slice(&word.to_le_bytes());
        }
    }
}
