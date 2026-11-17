//! Bounded delivery and tick-boundary activation of cooked level data.

use forge_game_runtime::physics::Collider;
use forge_game_runtime::{
    AnimationTiming, Level, LevelId, LevelMode, LevelSpec, PatrolSpec, Room, SpawnKind, SpawnSpec,
    SurfaceSpec,
};
use serde::Deserialize;

const MAX_LEVEL_BYTES: usize = 16 * 1024;
pub(super) const PUBLICATION_SIZE: usize = 48;

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
    fn decode(bytes: &[u8]) -> Option<Level> {
        let cooked: Self = serde_json::from_slice(bytes).ok()?;
        if cooked.version != 1 || cooked.atlas != "scout-atlas" {
            return None;
        }
        let edges = cooked.collider;
        Level::validate(LevelSpec {
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
            collider: Collider::from_edges(edges.left, edges.top, edges.right, edges.bottom)?,
        })
        .ok()
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
}

#[derive(Default)]
pub(super) struct Loader {
    latest: u32,
    request: Option<Request>,
    prepared: Option<Prepared>,
    active_slot: u32,
    activation_tick: u32,
    active_digest: [u32; 8],
}

impl Loader {
    pub(super) fn begin(&mut self, request: u32, first_word: u32) -> bool {
        if request <= self.latest {
            return false;
        }
        self.latest = request;
        self.prepared = None;
        self.request = Some(Request {
            bytes: Vec::new(),
            digest: [0; 8],
            received: 0,
            first_word,
        });
        true
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
        let Some((level, epoch)) =
            CookedLevel::decode(&input.bytes).zip(active.epoch().checked_add(1))
        else {
            return false;
        };
        let slot = match level.id() {
            LevelId::RoomOne => 1,
            LevelId::TraversalYard => 2,
        };
        self.prepared = Some(Prepared {
            room: Room::from_validated(level, epoch, active.trace_mask()),
            slot,
            digest: input.digest,
        });
        true
    }

    pub(super) fn activate(&mut self, active: &mut Room) -> bool {
        let Some(mut ready) = self.prepared.take() else {
            return false;
        };
        self.activation_tick = active.tick().saturating_add(1);
        self.active_slot = ready.slot;
        self.active_digest = ready.digest;
        ready.room.set_trace_mask(active.trace_mask());
        *active = ready.room;
        true
    }

    pub(super) fn restart(&mut self) {
        self.request = None;
        self.prepared = None;
        self.activation_tick = 0;
    }

    pub(super) fn active_slot(&self) -> u32 {
        self.active_slot
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
