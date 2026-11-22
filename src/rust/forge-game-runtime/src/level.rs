//! Authored level admission. The runtime accepts typed data after a host or
//! player adapter has decoded and verified the cooked artifact.

use crate::{
    entity::{Descriptor, EntityId, Kind, Patrol, Position},
    physics::{Character, Collider, TileGrid, Vector, TILE},
    room::{EXIT, GUARD, HEIGHT, KEY, MAP, PLAYER, WIDTH},
    AnimationTiming,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LevelId {
    RoomOne,
    TraversalYard,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LevelMode {
    Tile,
    Platform,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpawnKind {
    Player,
    Key,
    Exit,
    Guard,
}

#[derive(Clone, Copy, Debug)]
pub struct PatrolSpec {
    pub first_x: u32,
    pub last_x: u32,
    pub period_ticks: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct SpawnSpec {
    pub id: u16,
    pub kind: SpawnKind,
    pub x: u32,
    pub y: u32,
    pub patrol: Option<PatrolSpec>,
}

#[derive(Clone, Debug)]
pub struct SurfaceSpec {
    pub id: String,
    pub friction_q16: u32,
    pub restitution_q16: u32,
}

#[derive(Clone, Debug)]
pub struct LevelSpec {
    pub id: LevelId,
    pub mode: LevelMode,
    pub width: u32,
    pub height: u32,
    pub tiles: Vec<String>,
    pub spawns: Vec<SpawnSpec>,
    pub surfaces: Vec<SurfaceSpec>,
    pub surface: String,
    pub timing: AnimationTiming,
    pub collider: Collider,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LevelError {
    Mode,
    Board,
    Spawn,
    Surface,
    Animation,
    Collider,
}

/// Level data can be reused across independent runs. The step consumes a
/// compact tile grid and validated descriptors with no dependency on a file
/// name, allocator call, or external resource lookup.
#[derive(Clone)]
pub struct Level {
    pub(crate) id: LevelId,
    pub(crate) mode: LevelMode,
    pub(crate) width: u32,
    pub(crate) height: u32,
    tiles: Box<[u8]>,
    pub(crate) player: Descriptor,
    pub(crate) key: Descriptor,
    pub(crate) exit: Descriptor,
    pub(crate) guard: Option<Descriptor>,
    surface: SurfaceSpec,
    pub(crate) timing: AnimationTiming,
    pub(crate) collider: Collider,
}

impl Level {
    pub fn validate(spec: LevelSpec) -> Result<Self, LevelError> {
        if !matches!(
            (spec.id, spec.mode),
            (LevelId::RoomOne, LevelMode::Tile) | (LevelId::TraversalYard, LevelMode::Platform)
        ) {
            return Err(LevelError::Mode);
        }
        if !(3..=64).contains(&spec.width)
            || !(3..=64).contains(&spec.height)
            || spec.tiles.len() != spec.height as usize
        {
            return Err(LevelError::Board);
        }
        if !spec.timing.valid() {
            return Err(LevelError::Animation);
        }
        let [left, top, right, bottom] = spec.collider.edges();
        if left >= 0 || right <= 0 || top >= 0 || bottom != 0 || right - left > TILE || -top > TILE
        {
            return Err(LevelError::Collider);
        }
        let mut tiles = Vec::with_capacity((spec.width * spec.height) as usize);
        for (y, row) in spec.tiles.iter().enumerate() {
            if row.len() != spec.width as usize {
                return Err(LevelError::Board);
            }
            for (x, tile) in row.bytes().enumerate() {
                let boundary = x == 0
                    || y == 0
                    || x + 1 == spec.width as usize
                    || y + 1 == spec.height as usize;
                if !matches!(tile, b'.' | b'#') || (boundary && tile != b'#') {
                    return Err(LevelError::Board);
                }
                tiles.push(tile);
            }
        }
        if spec.surfaces.is_empty()
            || spec.surfaces.len() > 32
            || spec.surfaces.iter().enumerate().any(|(index, item)| {
                item.id.is_empty()
                    || item.id.len() > 64
                    || !item.id.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
                    || item.friction_q16 > TILE as u32
                    || item.restitution_q16 > TILE as u32
                    || spec.surfaces[..index]
                        .iter()
                        .any(|prior| prior.id == item.id)
            })
        {
            return Err(LevelError::Surface);
        }
        let surface = spec
            .surfaces
            .into_iter()
            .find(|surface| surface.id == spec.surface)
            .ok_or(LevelError::Surface)?;

        if !(3..=4).contains(&spec.spawns.len()) {
            return Err(LevelError::Spawn);
        }
        let mut descriptors = [None; 4];
        for (index, row) in spec.spawns.iter().enumerate() {
            if row.id == 0
                || spec.spawns[..index]
                    .iter()
                    .any(|prior| prior.id == row.id || (prior.x, prior.y) == (row.x, row.y))
                || row.x >= spec.width
                || row.y >= spec.height
                || tiles[(row.y * spec.width + row.x) as usize] != b'.'
            {
                return Err(LevelError::Spawn);
            }
            let kind = match row.kind {
                SpawnKind::Player => Kind::Player,
                SpawnKind::Key => Kind::Key,
                SpawnKind::Exit => Kind::Exit,
                SpawnKind::Guard => Kind::Guard,
            };
            let slot = &mut descriptors[kind as usize - 1];
            if slot.is_some() || matches!(kind, Kind::Guard) != row.patrol.is_some() {
                return Err(LevelError::Spawn);
            }
            if let Some(patrol) = row.patrol {
                if spec.mode == LevelMode::Platform
                    || patrol.first_x >= patrol.last_x
                    || patrol.last_x >= spec.width
                    || patrol.period_ticks == 0
                    || !(patrol.first_x..=patrol.last_x).contains(&row.x)
                    || !(patrol.first_x..=patrol.last_x)
                        .all(|x| tiles[(row.y * spec.width + x) as usize] == b'.')
                {
                    return Err(LevelError::Spawn);
                }
            }
            *slot = Some(Descriptor {
                id: EntityId(row.id),
                kind,
                spawn: Position { x: row.x, y: row.y },
                patrol: row.patrol.map(|patrol| Patrol {
                    first_x: patrol.first_x,
                    last_x: patrol.last_x,
                    period_ticks: patrol.period_ticks,
                }),
            });
        }
        let [Some(player), Some(key), Some(exit), guard] = descriptors else {
            return Err(LevelError::Spawn);
        };
        let level = Self {
            id: spec.id,
            mode: spec.mode,
            width: spec.width,
            height: spec.height,
            tiles: tiles.into_boxed_slice(),
            player,
            key,
            exit,
            guard,
            surface,
            timing: spec.timing,
            collider: spec.collider,
        };
        if level.mode == LevelMode::Platform && level.spawn_character().is_none() {
            return Err(LevelError::Spawn);
        }
        Ok(level)
    }

    pub(crate) fn builtin() -> Self {
        Self {
            id: LevelId::RoomOne,
            mode: LevelMode::Tile,
            width: WIDTH,
            height: HEIGHT,
            tiles: MAP
                .into_iter()
                .flat_map(|row| row.iter().copied())
                .collect(),
            player: PLAYER,
            key: KEY,
            exit: EXIT,
            guard: Some(GUARD),
            timing: AnimationTiming::default(),
            collider: Collider::default(),
            surface: SurfaceSpec {
                id: "stone".into(),
                friction_q16: 16_384,
                restitution_q16: 0,
            },
        }
    }

    pub fn id(&self) -> LevelId {
        self.id
    }

    pub fn surface(&self) -> &SurfaceSpec {
        &self.surface
    }

    pub(crate) fn solid(&self, position: Position) -> bool {
        self.is_solid(position.x, position.y)
    }

    pub(crate) fn spawn_character(&self) -> Option<Character> {
        let spawn = self.player.spawn;
        let foot = Vector {
            x: spawn.x as i32 * TILE + TILE / 2,
            y: (spawn.y as i32 + 1) * TILE,
        };
        Character::spawn(self, foot, self.collider)
    }
}

impl TileGrid for Level {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn is_solid(&self, x: u32, y: u32) -> bool {
        x >= self.width || y >= self.height || self.tiles[(y * self.width + x) as usize] == b'#'
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_spec() -> LevelSpec {
        LevelSpec {
            id: LevelId::RoomOne,
            mode: LevelMode::Tile,
            width: 5,
            height: 5,
            tiles: vec![
                "#####".into(),
                "#...#".into(),
                "#...#".into(),
                "#...#".into(),
                "#####".into(),
            ],
            spawns: vec![
                SpawnSpec {
                    id: 1,
                    kind: SpawnKind::Player,
                    x: 1,
                    y: 1,
                    patrol: None,
                },
                SpawnSpec {
                    id: 2,
                    kind: SpawnKind::Key,
                    x: 3,
                    y: 3,
                    patrol: None,
                },
                SpawnSpec {
                    id: 3,
                    kind: SpawnKind::Exit,
                    x: 3,
                    y: 1,
                    patrol: None,
                },
            ],
            surfaces: vec![SurfaceSpec {
                id: "stone".into(),
                friction_q16: 16_384,
                restitution_q16: 0,
            }],
            surface: "stone".into(),
            timing: AnimationTiming::default(),
            collider: Collider::default(),
        }
    }

    #[test]
    fn valid_spec_validates_and_builtin_matches_the_authored_room() {
        let level = Level::validate(valid_spec()).unwrap();
        assert_eq!(
            (level.id(), level.width, level.height),
            (LevelId::RoomOne, 5, 5)
        );
        assert!(level.solid(Position { x: 0, y: 2 }));
        assert!(!level.solid(Position { x: 2, y: 2 }));
        assert!(level.solid(Position { x: 99, y: 99 }));
        assert_eq!(level.surface().id, "stone");
        let builtin = Level::builtin();
        assert_eq!(
            (builtin.id(), builtin.width, builtin.height),
            (LevelId::RoomOne, 13, 9)
        );
        assert_eq!(builtin.player.spawn, Position { x: 1, y: 1 });
        assert_eq!(builtin.key.spawn, Position { x: 1, y: 7 });
        assert_eq!(builtin.exit.spawn, Position { x: 11, y: 4 });
        assert!(builtin.guard.is_some());
    }

    #[test]
    fn validate_rejects_a_mode_mismatch_and_bad_boards() {
        let mut spec = valid_spec();
        spec.mode = LevelMode::Platform;
        assert!(matches!(Level::validate(spec), Err(LevelError::Mode)));
        let mut spec = valid_spec();
        spec.width = 2;
        assert!(matches!(Level::validate(spec), Err(LevelError::Board)));
        let mut spec = valid_spec();
        spec.tiles[0] = "#...#".into();
        assert!(matches!(Level::validate(spec), Err(LevelError::Board)));
        let mut spec = valid_spec();
        spec.tiles[1] = "#.x.#".into();
        assert!(matches!(Level::validate(spec), Err(LevelError::Board)));
        let mut spec = valid_spec();
        spec.tiles[1] = "#..#".into();
        assert!(matches!(Level::validate(spec), Err(LevelError::Board)));
    }

    #[test]
    fn validate_rejects_bad_animation_collider_and_surfaces() {
        let mut spec = valid_spec();
        spec.timing.idle = [0];
        assert!(matches!(Level::validate(spec), Err(LevelError::Animation)));
        let mut spec = valid_spec();
        spec.collider = Collider::from_edges(-100, -100, 100, 100).unwrap();
        assert!(matches!(Level::validate(spec), Err(LevelError::Collider)));
        let mut spec = valid_spec();
        spec.surfaces = Vec::new();
        assert!(matches!(Level::validate(spec), Err(LevelError::Surface)));
        let mut spec = valid_spec();
        spec.surface = "missing".into();
        assert!(matches!(Level::validate(spec), Err(LevelError::Surface)));
        let mut spec = valid_spec();
        spec.surfaces.push(SurfaceSpec {
            id: "stone".into(),
            friction_q16: 0,
            restitution_q16: 0,
        });
        assert!(matches!(Level::validate(spec), Err(LevelError::Surface)));
        let mut spec = valid_spec();
        spec.surfaces[0].friction_q16 = TILE as u32 + 1;
        assert!(matches!(Level::validate(spec), Err(LevelError::Surface)));
    }

    #[test]
    fn validate_rejects_bad_spawns_and_patrols() {
        let mut spec = valid_spec();
        spec.spawns.remove(1);
        assert!(matches!(Level::validate(spec), Err(LevelError::Spawn)));
        let mut spec = valid_spec();
        spec.spawns[1].id = 1;
        assert!(matches!(Level::validate(spec), Err(LevelError::Spawn)));
        let mut spec = valid_spec();
        spec.spawns[1].id = 0;
        assert!(matches!(Level::validate(spec), Err(LevelError::Spawn)));
        let mut spec = valid_spec();
        spec.spawns[1].x = 0;
        assert!(matches!(Level::validate(spec), Err(LevelError::Spawn)));
        let mut spec = valid_spec();
        spec.spawns[1].kind = SpawnKind::Player;
        assert!(matches!(Level::validate(spec), Err(LevelError::Spawn)));
        let mut spec = valid_spec();
        spec.spawns.push(SpawnSpec {
            id: 4,
            kind: SpawnKind::Guard,
            x: 2,
            y: 2,
            patrol: None,
        });
        assert!(matches!(Level::validate(spec), Err(LevelError::Spawn)));
        let mut spec = valid_spec();
        spec.spawns.push(SpawnSpec {
            id: 4,
            kind: SpawnKind::Guard,
            x: 2,
            y: 2,
            patrol: Some(PatrolSpec {
                first_x: 2,
                last_x: 2,
                period_ticks: 12,
            }),
        });
        assert!(matches!(Level::validate(spec), Err(LevelError::Spawn)));
        let mut spec = valid_spec();
        spec.spawns.push(SpawnSpec {
            id: 4,
            kind: SpawnKind::Guard,
            x: 2,
            y: 2,
            patrol: Some(PatrolSpec {
                first_x: 1,
                last_x: 3,
                period_ticks: 12,
            }),
        });
        assert!(Level::validate(spec).is_ok());
    }

    #[test]
    fn validation_is_deterministic_for_the_same_spec() {
        let first = Level::validate(valid_spec()).unwrap();
        let second = Level::validate(valid_spec()).unwrap();
        assert_eq!(
            (first.id(), first.width, first.height),
            (second.id(), second.width, second.height)
        );
        assert_eq!(first.player.spawn, second.player.spawn);
        assert_eq!(first.surface().id, second.surface().id);
    }
}
