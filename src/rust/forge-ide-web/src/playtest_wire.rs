//! Fixed-layout little-endian records for the playtest browser adapter.
//!
//! Board rows are y-down for tile painting. Entity positions and event tiles
//! are y-up. Entity positions carry eight fractional bits.

use forge_game_runtime::Room;

const VERSION: u16 = 1;
const SNAPSHOT_HEADER: usize = 32;
const BOARD_CAPACITY: usize = 64 * 64;
const ENTITY_SIZE: usize = 20;
const ENTITY_CAPACITY: usize = 256;
const EVENT_SIZE: usize = 16;
const EVENT_CAPACITY: usize = 64;
const STATS_SIZE: usize = 12;
const ENTITY_OFFSET: usize = SNAPSHOT_HEADER + BOARD_CAPACITY;
const EVENT_OFFSET: usize = ENTITY_OFFSET + ENTITY_SIZE * ENTITY_CAPACITY;
const STATS_OFFSET: usize = EVENT_OFFSET + EVENT_SIZE * EVENT_CAPACITY;
pub(super) const SNAPSHOT_SIZE: usize = STATS_OFFSET + STATS_SIZE;

const TRACE_HEADER: usize = 24;
const TRACE_RECORD_SIZE: usize = 16;
const TRACE_CAPACITY: usize = 512;
pub(super) const TRACE_SIZE: usize = TRACE_HEADER + TRACE_RECORD_SIZE * TRACE_CAPACITY;

pub(super) fn snapshot(room: &Room, buffer: &mut [u8; SNAPSHOT_SIZE]) {
    buffer.fill(0);
    let width = room.width();
    let height = room.height();
    put(buffer, 0, *b"FGS1");
    put(buffer, 4, VERSION.to_le_bytes());
    put(buffer, 6, (width as u16).to_le_bytes());
    put(buffer, 8, (height as u16).to_le_bytes());
    put(buffer, 14, (room.input_depth() as u16).to_le_bytes());
    put(buffer, 16, room.epoch().to_le_bytes());
    put(buffer, 20, room.tick().to_le_bytes());
    put(buffer, 24, room.interpolation().to_le_bytes());
    buffer[28] = room.status() as u8;
    buffer[29] = room.ticks_run() as u8;

    if (1..=64).contains(&width) && (1..=64).contains(&height) {
        for y in 0..height {
            for x in 0..width {
                let index = (y * width + x) as usize;
                buffer[SNAPSHOT_HEADER + index] = room.cell(x, y) as u8;
            }
        }
    }

    let mut entity_count = 0u16;
    for (record, entity) in buffer[ENTITY_OFFSET..EVENT_OFFSET]
        .chunks_exact_mut(ENTITY_SIZE)
        .zip(room.entities())
    {
        put(record, 0, entity.id.to_le_bytes());
        record[2] = entity.kind;
        record[3] = entity.state;
        put(record, 4, entity.x.to_le_bytes());
        put(record, 8, entity.y.to_le_bytes());
        put(record, 12, entity.previous_x.to_le_bytes());
        put(record, 16, entity.previous_y.to_le_bytes());
        entity_count += 1;
    }
    put(buffer, 10, entity_count.to_le_bytes());

    let mut event_count = 0u16;
    for (record, event) in buffer[EVENT_OFFSET..STATS_OFFSET]
        .chunks_exact_mut(EVENT_SIZE)
        .zip(room.events())
    {
        put(record, 0, event.sequence.to_le_bytes());
        put(record, 4, event.tick.to_le_bytes());
        put(record, 8, event.target.to_le_bytes());
        record[10] = event.kind;
        put(record, 12, event.x.to_le_bytes());
        put(record, 14, event.y.to_le_bytes());
        event_count += 1;
    }
    put(buffer, 12, event_count.to_le_bytes());
    put(buffer, STATS_OFFSET, room.ticks_dropped().to_le_bytes());
    put(buffer, STATS_OFFSET + 4, room.input_dropped().to_le_bytes());
    put(
        buffer,
        STATS_OFFSET + 8,
        room.events_dropped().to_le_bytes(),
    );
}

pub(super) fn trace(room: &Room, buffer: &mut [u8; TRACE_SIZE]) {
    buffer.fill(0);
    put(buffer, 0, *b"FGT1");
    put(buffer, 4, VERSION.to_le_bytes());
    put(buffer, 8, room.epoch().to_le_bytes());
    put(buffer, 12, room.trace_latest_sequence().to_le_bytes());
    put(buffer, 16, room.trace_dropped().to_le_bytes());
    put(buffer, 20, room.trace_mask().to_le_bytes());
    // The mask occupies three bytes. The high byte carries replay flags.
    buffer[23] = u8::from(room.trace_replay_complete());

    let mut count = 0u16;
    for (record, entry) in buffer[TRACE_HEADER..]
        .chunks_exact_mut(TRACE_RECORD_SIZE)
        .zip(room.trace())
    {
        put(record, 0, entry.sequence.to_le_bytes());
        put(record, 4, entry.tick.to_le_bytes());
        record[8] = entry.phase;
        record[9] = entry.channel;
        put(record, 10, entry.target.to_le_bytes());
        put(record, 12, entry.data.to_le_bytes());
        count += 1;
    }
    put(buffer, 6, count.to_le_bytes());
}

fn put<const N: usize>(bytes: &mut [u8], offset: usize, value: [u8; N]) {
    bytes[offset..offset + N].copy_from_slice(&value);
}
