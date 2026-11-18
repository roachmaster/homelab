//! On-demand collision data from the completed playtest tick.

use forge_game_runtime::{
    physics::{RayTarget, Vector, TILE},
    CollisionFilter, CollisionTargets, Room,
};

const BODY_OFFSET: usize = 96;
const BODY_BYTES: usize = 20;
pub(super) const SIZE: usize = BODY_OFFSET + 256 * BODY_BYTES;

fn write_i32(bytes: &mut [u8], offset: usize, value: i32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub(super) fn write(room: &Room, bytes: &mut [u8; SIZE]) {
    bytes.fill(0);
    bytes[..4].copy_from_slice(b"FGC1");
    bytes[4..8].copy_from_slice(&room.epoch().to_le_bytes());
    bytes[8..12].copy_from_slice(&room.tick().to_le_bytes());
    let filter = CollisionFilter {
        targets: CollisionTargets::AllEntities,
        exclude: None,
    };
    let mut bodies = 0u16;
    for (record, (id, collider, anchor)) in bytes[BODY_OFFSET..]
        .chunks_exact_mut(BODY_BYTES)
        .zip(room.collision_bodies(filter))
    {
        record[..2].copy_from_slice(&id.to_le_bytes());
        let [left, top, right, bottom] = collider.edges();
        write_i32(record, 4, anchor.x + left);
        write_i32(record, 8, anchor.y + top);
        write_i32(record, 12, anchor.x + right);
        write_i32(record, 16, anchor.y + bottom);
        bodies += 1;
    }
    bytes[12..14].copy_from_slice(&bodies.to_le_bytes());
    let Some(motion) = room.motion() else {
        return;
    };
    bytes[14] = u8::from(motion.grounded);
    for (index, contact) in [
        motion.contacts.ground,
        motion.contacts.head,
        motion.contacts.wall,
    ]
    .into_iter()
    .flatten()
    .enumerate()
    {
        let offset = 48 + index * 16;
        write_i32(bytes, offset, contact.tile_x);
        write_i32(bytes, offset + 4, contact.tile_y);
        write_i32(bytes, offset + 8, contact.penetration);
        bytes[offset + 12] = contact.normal as u8;
        bytes[15] += 1;
    }
    let origin = Vector {
        x: motion.position.x,
        y: motion.position.y - TILE / 4,
    };
    let probe = CollisionFilter {
        exclude: Some(room.pose().entity_id),
        ..filter
    };
    if let Some(hit) = room.cast_ray(origin, Vector { x: 0, y: TILE }, probe) {
        write_i32(bytes, 16, origin.x);
        write_i32(bytes, 20, origin.y);
        write_i32(bytes, 24, hit.point.x);
        write_i32(bytes, 28, hit.point.y);
        write_i32(bytes, 32, hit.distance);
        bytes[36] = hit.normal as u8;
        match hit.target {
            RayTarget::Tile { x, y } => {
                bytes[37] = 1;
                write_i32(bytes, 40, x);
                write_i32(bytes, 44, y);
            }
            RayTarget::Entity(id) => {
                bytes[37] = 2;
                bytes[38..40].copy_from_slice(&id.to_le_bytes());
            }
        }
    }
}
