//! Bounded, read-only collision queries against completed room state.

use crate::entity::{Entity, Kind, Position};
use crate::physics::{cast_room_ray, Collider, RoomRayHit, Vector, TILE};
use crate::room::RoomCollision;
use crate::{Room, Status};

const HIT_LIMIT: usize = 8;

/// Select the entity groups a room collision query can report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollisionTargets {
    Characters,
    Triggers,
    AllEntities,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CollisionFilter {
    pub targets: CollisionTargets,
    /// Omit the stable ID of the body issuing the query when applicable.
    pub exclude: Option<u16>,
}

impl CollisionFilter {
    fn admits(self, entity: &Entity) -> bool {
        self.exclude != Some(entity.id.0)
            && match self.targets {
                CollisionTargets::Characters => matches!(entity.kind, Kind::Player | Kind::Guard),
                CollisionTargets::Triggers => matches!(entity.kind, Kind::Key | Kind::Exit),
                CollisionTargets::AllEntities => true,
            }
    }
}

/// The nearest eight matching entities in distance and then stable ID order.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PhantomHits {
    ranks: [(i64, u16); HIT_LIMIT],
    len: usize,
    dropped: u32,
}

impl PhantomHits {
    pub fn ids(&self) -> impl ExactSizeIterator<Item = u16> + '_ {
        self.ranks[..self.len].iter().map(|&(_, id)| id)
    }

    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    fn insert(&mut self, rank: (i64, u16)) {
        let index = self.ranks[..self.len].partition_point(|&hit| hit < rank);
        if self.len == HIT_LIMIT {
            self.dropped += 1;
        } else {
            self.len += 1;
        }
        if index < HIT_LIMIT {
            self.ranks.copy_within(index..self.len - 1, index + 1);
            self.ranks[index] = rank;
        }
    }
}

impl Room {
    /// Return overlapping live entities from the last completed tick. The
    /// query uses continuous, y-down Q16 tile coordinates and ranks by
    /// squared distance between collider centers. Terrain has no entity ID.
    pub fn phantom_overlap(
        &self,
        query: Collider,
        anchor: Vector,
        filter: CollisionFilter,
    ) -> PhantomHits {
        let center = doubled_center(query, anchor);
        let mut hits = PhantomHits::default();
        for (id, shape, root) in self.collision_bodies(filter) {
            if query.overlaps(anchor, shape, root) {
                let target = doubled_center(shape, root);
                // Overlapping shapes bound the center deltas by their dimensions.
                let dx = center.0 - target.0;
                let dy = center.1 - target.1;
                hits.insert((dx * dx + dy * dy, id));
            }
        }
        hits
    }

    /// Return the nearest solid tile or filtered entity hit by this segment in
    /// continuous, y-down Q16 coordinates. Terrain, including a closed exit,
    /// occludes entities at equal or greater distance.
    pub fn cast_ray(
        &self,
        origin: Vector,
        delta: Vector,
        filter: CollisionFilter,
    ) -> Option<RoomRayHit> {
        let grid = RoomCollision {
            level: &self.level,
            closed_exit: (self.status == Status::FindKey).then(|| self.entity(self.exit).position),
        };
        cast_room_ray(&grid, origin, delta, self.collision_bodies(filter))
    }

    /// Read the IDs, colliders and origins used by this room's collision queries.
    pub fn collision_bodies(
        &self,
        filter: CollisionFilter,
    ) -> impl Iterator<Item = (u16, Collider, Vector)> + '_ {
        self.table
            .live()
            .filter(move |entity| filter.admits(entity))
            .map(|entity| {
                let (shape, root) = match (&self.character, entity.kind) {
                    (Some(character), Kind::Player) => {
                        (character.collider(), character.frame().position)
                    }
                    _ => tile_collider(entity.position),
                };
                (entity.id.0, shape, root)
            })
    }
}

pub(super) fn tile_collider(position: Position) -> (Collider, Vector) {
    (
        Collider::new(TILE / 2, TILE).expect("tile-sized collider"),
        Vector {
            x: position.x as i32 * TILE + TILE / 2,
            y: (position.y as i32 + 1) * TILE,
        },
    )
}

fn doubled_center(collider: Collider, anchor: Vector) -> (i64, i64) {
    let [left, top, right, bottom] = collider.edges();
    (
        2 * i64::from(anchor.x) + i64::from(left + right),
        2 * i64::from(anchor.y) + i64::from(top + bottom),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{Descriptor, EntityId};
    use crate::physics::{Normal, RayTarget};
    use crate::{
        AnimationTiming, Level, LevelId, LevelMode, LevelSpec, SpawnKind, SpawnSpec, SurfaceSpec,
    };

    #[test]
    fn phantom_filters_and_keeps_the_nearest_eight_with_stable_ties() {
        let mut room = Room::new();
        for id in (10..20).rev() {
            room.table
                .spawn(
                    &room.level,
                    Descriptor {
                        id: EntityId(id),
                        kind: Kind::Key,
                        spawn: Position {
                            x: if id < 16 { 1 } else { 2 },
                            y: 1,
                        },
                        patrol: None,
                    },
                )
                .unwrap();
        }

        let query = Collider::new(2 * TILE, TILE).unwrap();
        let (_, anchor) = tile_collider(Position { x: 2, y: 1 });
        let hits = room.phantom_overlap(
            query,
            anchor,
            CollisionFilter {
                targets: CollisionTargets::Triggers,
                exclude: None,
            },
        );
        assert_eq!(
            hits.ids().collect::<Vec<_>>(),
            [16, 17, 18, 19, 10, 11, 12, 13]
        );
        assert_eq!(hits.dropped(), 2);

        let hits = room.phantom_overlap(
            query,
            anchor,
            CollisionFilter {
                targets: CollisionTargets::Characters,
                exclude: Some(1),
            },
        );
        assert_eq!(hits.ids().len(), 0);
        assert_eq!(hits.dropped(), 0);
    }

    #[test]
    fn phantom_uses_the_platform_collider_and_leaves_state_intact() {
        let level = Level::validate(LevelSpec {
            id: LevelId::TraversalYard,
            mode: LevelMode::Platform,
            width: 5,
            height: 3,
            tiles: ["#####", "#...#", "#####"].map(str::to_owned).into(),
            spawns: [SpawnKind::Player, SpawnKind::Key, SpawnKind::Exit]
                .into_iter()
                .enumerate()
                .map(|(index, kind)| SpawnSpec {
                    id: index as u16 + 1,
                    kind,
                    x: index as u32 + 1,
                    y: 1,
                    patrol: None,
                })
                .collect(),
            surfaces: vec![SurfaceSpec {
                id: "floor".into(),
                friction_q16: 0,
                restitution_q16: 0,
            }],
            surface: "floor".into(),
            timing: AnimationTiming::default(),
            collider: Collider::default(),
        })
        .unwrap();
        let room = Room::from_validated(level, 1, 0);
        let before = room.motion().unwrap();
        let filter = CollisionFilter {
            targets: CollisionTargets::Characters,
            exclude: None,
        };
        let query = Collider::new(TILE / 32, TILE / 2).unwrap();
        let beside = Vector {
            x: before.position.x - 3 * TILE / 8,
            y: before.position.y,
        };

        assert_eq!(room.phantom_overlap(query, beside, filter).ids().len(), 0);
        assert_eq!(
            room.phantom_overlap(query, before.position, filter)
                .ids()
                .next(),
            Some(1)
        );
        assert_eq!(room.tick(), 0);
        assert_eq!(room.motion().unwrap(), before);
        assert_eq!(room.events().count(), 0);
    }

    #[test]
    fn room_ray_selects_filtered_entities_or_terrain_in_exact_hit_order() {
        let mut room = Room::new();
        for id in [11, 10] {
            room.table
                .spawn(
                    &room.level,
                    Descriptor {
                        id: EntityId(id),
                        kind: Kind::Key,
                        spawn: Position { x: 2, y: 1 },
                        patrol: None,
                    },
                )
                .unwrap();
        }
        let origin = Vector {
            x: 3 * TILE / 2,
            y: 3 * TILE / 2,
        };
        let delta = Vector { x: 4 * TILE, y: 0 };
        let mut filter = CollisionFilter {
            targets: CollisionTargets::AllEntities,
            exclude: None,
        };
        let initial = room.cast_ray(origin, delta, filter).unwrap();
        assert_eq!(initial.target, RayTarget::Entity(1));
        assert_eq!(initial.point, origin);
        assert_eq!(initial.distance, 0);

        filter.exclude = Some(1);
        let nearby = room.cast_ray(origin, delta, filter).unwrap();
        assert_eq!(nearby.target, RayTarget::Entity(10));
        assert_eq!(
            nearby.point,
            Vector {
                x: 2 * TILE,
                y: origin.y
            }
        );
        assert_eq!(nearby.distance, TILE / 2);
        assert_eq!(nearby.normal, Normal::Left);

        filter.targets = CollisionTargets::Characters;
        let wall = room.cast_ray(origin, delta, filter).unwrap();
        assert_eq!(wall.target, RayTarget::Tile { x: 4, y: 1 });
        assert_eq!(wall.distance, 5 * TILE / 2);
        assert_eq!(wall.normal, Normal::Left);
        assert_eq!(room.tick(), 0);
        assert_eq!(room.events().count(), 0);
    }

    #[test]
    fn room_ray_reports_floor_occludes_locked_exit_and_misses_clear_space() {
        let room = Room::new();
        let filter = CollisionFilter {
            targets: CollisionTargets::AllEntities,
            exclude: None,
        };
        let origin = Vector {
            x: 5 * TILE / 2,
            y: 3 * TILE / 2,
        };
        let floor = room
            .cast_ray(origin, Vector { x: 0, y: TILE }, filter)
            .unwrap();
        assert_eq!(floor.target, RayTarget::Tile { x: 2, y: 2 });
        assert_eq!(
            floor.point,
            Vector {
                x: origin.x,
                y: 2 * TILE
            }
        );
        assert_eq!(floor.distance, TILE / 2);
        assert_eq!(floor.normal, Normal::Up);
        assert!(room
            .cast_ray(origin, Vector { x: TILE / 4, y: 0 }, filter)
            .is_none());

        let origin = Vector {
            x: 23 * TILE / 2,
            y: 11 * TILE / 2,
        };
        let exit = room
            .cast_ray(origin, Vector { x: 0, y: -TILE }, filter)
            .unwrap();
        assert_eq!(exit.target, RayTarget::Tile { x: 11, y: 4 });
        assert_eq!(exit.normal, Normal::Down);
    }
}
