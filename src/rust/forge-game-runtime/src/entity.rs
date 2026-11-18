use crate::{Action, Level};

const CAPACITY: usize = 256;
const TILE_SCALE: i32 = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Position {
    pub(super) x: u32,
    pub(super) y: u32,
}

impl Position {
    pub(super) fn next(self, action: Action) -> Option<Self> {
        let (dx, dy) = match action {
            Action::Up => (0, -1),
            Action::Down => (0, 1),
            Action::Left => (-1, 0),
            Action::Right => (1, 0),
            Action::Jump => return None,
        };
        Some(Self {
            x: self.x.checked_add_signed(dx)?,
            y: self.y.checked_add_signed(dy)?,
        })
    }

    pub(super) fn world_y(self, height: u32) -> u32 {
        height - 1 - self.y
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct EntityId(pub(super) u16);

#[repr(u8)]
#[derive(Clone, Copy)]
pub(super) enum Kind {
    Player = 1,
    Key = 2,
    Exit = 3,
    Guard = 4,
}

#[derive(Clone, Copy)]
pub(super) struct Descriptor {
    pub(super) id: EntityId,
    pub(super) kind: Kind,
    pub(super) spawn: Position,
    pub(super) patrol: Option<Patrol>,
}

#[derive(Clone, Copy)]
pub(super) struct Patrol {
    pub(super) first_x: u32,
    pub(super) last_x: u32,
    pub(super) period_ticks: u32,
}

impl Patrol {
    fn is_valid_for(self, level: &Level, spawn: Position) -> bool {
        self.first_x < self.last_x
            && self.last_x < level.width
            && (self.first_x..=self.last_x).contains(&spawn.x)
            && self.period_ticks != 0
            && (self.first_x..=self.last_x).all(|x| !level.solid(Position { x, y: spawn.y }))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Handle {
    epoch: u32,
    generation: u32,
    slot: u8,
}

#[derive(Clone, Copy)]
pub(super) struct Entity {
    pub(super) id: EntityId,
    pub(super) kind: Kind,
    pub(super) position: Position,
    previous: Position,
    pub(super) facing: Action,
    pub(super) state: u8,
    pub(super) patrol: Option<Patrol>,
}

impl Entity {
    fn new(descriptor: Descriptor) -> Self {
        Self {
            id: descriptor.id,
            kind: descriptor.kind,
            position: descriptor.spawn,
            previous: descriptor.spawn,
            facing: Action::Right,
            state: 0,
            patrol: descriptor.patrol,
        }
    }

    fn view(self, height: u32) -> EntityView {
        let facing = match self.kind {
            Kind::Player | Kind::Guard => match self.facing {
                Action::Up => 0,
                Action::Right => 1,
                Action::Down => 2,
                Action::Left => 3,
                Action::Jump => 1,
            },
            Kind::Key | Kind::Exit => 0,
        };
        EntityView {
            id: self.id.0,
            kind: self.kind as u8,
            state: self.state | (facing << 6),
            x: self.position.x as i32 * TILE_SCALE,
            y: self.position.world_y(height) as i32 * TILE_SCALE,
            previous_x: self.previous.x as i32 * TILE_SCALE,
            previous_y: self.previous.world_y(height) as i32 * TILE_SCALE,
        }
    }
}

/// Published position is y-up and fixed-point with 256 units per tile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntityView {
    pub id: u16,
    pub kind: u8,
    pub state: u8,
    pub x: i32,
    pub y: i32,
    pub previous_x: i32,
    pub previous_y: i32,
}

#[derive(Clone, Copy)]
struct Slot {
    generation: u32,
    entity: Option<Entity>,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum SpawnError {
    InvalidPosition,
    InvalidId,
    InvalidPatrol,
    DuplicateId,
    Capacity,
}

pub(super) struct Table {
    epoch: u32,
    slots: [Slot; CAPACITY],
}

impl Table {
    pub(super) fn new(epoch: u32) -> Self {
        Self {
            epoch,
            slots: [Slot {
                generation: 1,
                entity: None,
            }; CAPACITY],
        }
    }

    pub(super) fn spawn(
        &mut self,
        level: &Level,
        descriptor: Descriptor,
    ) -> Result<Handle, SpawnError> {
        if level.solid(descriptor.spawn) {
            return Err(SpawnError::InvalidPosition);
        }
        if descriptor.id.0 == 0 {
            return Err(SpawnError::InvalidId);
        }
        if matches!(descriptor.kind, Kind::Guard) != descriptor.patrol.is_some()
            || descriptor
                .patrol
                .is_some_and(|patrol| !patrol.is_valid_for(level, descriptor.spawn))
        {
            return Err(SpawnError::InvalidPatrol);
        }
        if self
            .slots
            .iter()
            .filter_map(|slot| slot.entity)
            .any(|entity| entity.id == descriptor.id)
        {
            return Err(SpawnError::DuplicateId);
        }
        let (index, slot) = self
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.entity.is_none() && slot.generation != 0)
            .ok_or(SpawnError::Capacity)?;
        slot.entity = Some(Entity::new(descriptor));
        Ok(Handle {
            epoch: self.epoch,
            generation: slot.generation,
            slot: index as u8,
        })
    }

    pub(super) fn get(&self, handle: Handle) -> Option<&Entity> {
        let slot = &self.slots[usize::from(handle.slot)];
        if self.epoch == handle.epoch && slot.generation == handle.generation {
            slot.entity.as_ref()
        } else {
            None
        }
    }

    pub(super) fn get_mut(&mut self, handle: Handle) -> Option<&mut Entity> {
        let slot = &mut self.slots[usize::from(handle.slot)];
        if self.epoch == handle.epoch && slot.generation == handle.generation {
            slot.entity.as_mut()
        } else {
            None
        }
    }

    pub(super) fn despawn(&mut self, handle: Handle) -> bool {
        if self.get(handle).is_none() {
            return false;
        }
        let slot = &mut self.slots[usize::from(handle.slot)];
        slot.entity = None;
        // Retire exhausted slots for the remainder of the epoch.
        slot.generation = slot.generation.checked_add(1).unwrap_or(0);
        true
    }

    pub(super) fn begin_tick(&mut self) {
        for entity in self
            .slots
            .iter_mut()
            .filter_map(|slot| slot.entity.as_mut())
        {
            entity.previous = entity.position;
        }
    }

    pub(super) fn live(&self) -> impl Iterator<Item = &Entity> {
        self.slots.iter().filter_map(|slot| slot.entity.as_ref())
    }

    pub(super) fn views(&self, height: u32) -> impl Iterator<Item = EntityView> + '_ {
        self.live().map(move |entity| entity.view(height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(id: u16) -> Descriptor {
        Descriptor {
            id: EntityId(id),
            kind: Kind::Guard,
            spawn: Position { x: 5, y: 1 },
            patrol: Some(Patrol {
                first_x: 5,
                last_x: 10,
                period_ticks: 12,
            }),
        }
    }

    #[test]
    fn capacity_and_stable_id_validation_leave_live_entities_intact() {
        let mut table = Table::new(1);
        let level = Level::builtin();
        assert_eq!(
            table.spawn(&level, descriptor(0)),
            Err(SpawnError::InvalidId)
        );
        assert_eq!(
            table.spawn(
                &level,
                Descriptor {
                    spawn: Position { x: u32::MAX, y: 0 },
                    ..descriptor(1)
                }
            ),
            Err(SpawnError::InvalidPosition)
        );
        for id in 1..=256 {
            let handle = table.spawn(&level, descriptor(id)).unwrap();
            assert_eq!(table.get(handle).unwrap().id, EntityId(id));
        }
        assert_eq!(
            table.spawn(&level, descriptor(1)),
            Err(SpawnError::DuplicateId)
        );
        assert_eq!(
            table.spawn(&level, descriptor(257)),
            Err(SpawnError::Capacity)
        );
        assert_eq!(table.views(level.height).count(), 256);
    }

    #[test]
    fn rejects_patrols_that_cross_walls_or_cannot_move() {
        let mut table = Table::new(1);
        let level = Level::builtin();
        let guard = descriptor(1);
        for patrol in [
            Patrol {
                first_x: 4,
                last_x: 10,
                period_ticks: 12,
            },
            Patrol {
                first_x: 5,
                last_x: 10,
                period_ticks: 0,
            },
        ] {
            assert_eq!(
                table.spawn(
                    &level,
                    Descriptor {
                        patrol: Some(patrol),
                        ..guard
                    }
                ),
                Err(SpawnError::InvalidPatrol)
            );
        }
        assert_eq!(table.views(level.height).count(), 0);
    }

    #[test]
    fn reused_slot_and_retired_generation_never_revalidate_old_handles() {
        let mut table = Table::new(1);
        let level = Level::builtin();
        let first = table.spawn(&level, descriptor(1)).unwrap();
        assert!(table.despawn(first));
        let next = table.spawn(&level, descriptor(2)).unwrap();
        assert_eq!(first.slot, next.slot);
        assert!(table.get(first).is_none());
        assert!(table.despawn(next));
        table.slots[usize::from(first.slot)].generation = u32::MAX;
        let exhausted = table.spawn(&level, descriptor(3)).unwrap();
        assert!(table.despawn(exhausted));
        assert_ne!(
            table.spawn(&level, descriptor(4)).unwrap().slot,
            exhausted.slot
        );
        assert!(table.get(exhausted).is_none());
    }
}
