//! Fixed-step simulation for the first playable Forge room.
//!
//! An adapter supplies elapsed microseconds and directional input edges, then
//! reads the complete room after advancing it. The simulation owns no clock,
//! display, input device, or platform resources.

mod animation;
mod entity;
mod input;
mod level;
pub mod physics;
mod queries;
mod ring;
mod room;

#[cfg(test)]
mod game_runtime_bdd_tests;
#[cfg(test)]
mod game_runtime_component_tests;

pub use animation::{AnimationTiming, PlayerPose};
pub use entity::EntityView;
pub use level::{
    Level, LevelError, LevelId, LevelMode, LevelSpec, PatrolSpec, SpawnKind, SpawnSpec, SurfaceSpec,
};
pub use queries::{CollisionFilter, CollisionTargets, PhantomHits};
pub use room::{Action, Cell, EffectiveInput, Event, Room, Status, TraceRecord};
