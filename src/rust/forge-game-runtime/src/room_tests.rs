use super::*;

const TICK: u32 = 16_667;
const TO_KEY: &[u8] = b"RRDDLLDDRRDDLL";
const TO_LOCKED_EXIT: &[u8] = b"RRDDLLDDRRRRDDRRRRRRUUU";
const KEY_TO_EXIT: &[u8] = b"RRUURRDDRRRRRRUUU";

fn tap(room: &mut Room, letter: u8) {
    let action = match letter {
        b'U' => Action::Up,
        b'D' => Action::Down,
        b'L' => Action::Left,
        b'R' => Action::Right,
        _ => panic!("invalid direction"),
    };
    room.input(action, true);
    room.input(action, false);
}

fn walk(room: &mut Room, path: &[u8]) {
    for &letter in path {
        tap(room, letter);
        room.advance(TICK);
    }
}

#[test]
fn walls_and_locked_exit_block_movement_until_the_key_is_collected() {
    let mut room = Room::new();
    assert_eq!((room.width(), room.height()), (13, 9));
    assert_eq!(room.cell(0, 1), Cell::Wall as u32);
    assert_eq!(room.cell(u32::MAX, 1), Cell::Wall as u32);
    walk(&mut room, b"ULD");
    assert_eq!(room.cell(1, 1), Cell::Player as u32);
    walk(&mut room, TO_LOCKED_EXIT);
    assert_eq!(room.cell(11, 5), Cell::Player as u32);
    assert_eq!(room.cell(11, 4), Cell::LockedExit as u32);
    assert_eq!(room.status(), Status::FindKey as u32);

    room.restart();
    walk(&mut room, TO_KEY);
    assert_eq!(room.cell(1, 7), Cell::Player as u32);
    assert_eq!(room.cell(11, 4), Cell::OpenExit as u32);
    assert_eq!(room.status(), Status::ExitUnlocked as u32);
    walk(&mut room, KEY_TO_EXIT);
    assert_eq!(room.cell(11, 4), Cell::Player as u32);
    assert_eq!(room.cell(1, 7), Cell::Floor as u32);
    assert_eq!(room.status(), Status::Completed as u32);
    walk(&mut room, b"DDD");
    assert_eq!(room.cell(11, 4), Cell::Player as u32);

    room.restart();
    assert_eq!(room.cell(1, 1), Cell::Player as u32);
    assert_eq!(room.cell(1, 7), Cell::Key as u32);
    assert_eq!(room.cell(11, 4), Cell::LockedExit as u32);
    assert_eq!(room.status(), Status::FindKey as u32);
}

#[test]
fn short_taps_survive_sub_tick_frames_and_play_in_order_once() {
    let mut room = Room::new();
    for &letter in b"RRD" {
        tap(&mut room, letter);
    }
    room.advance(16_666);
    assert_eq!(room.cell(1, 1), Cell::Player as u32);
    room.advance(1);
    assert_eq!(room.cell(2, 1), Cell::Player as u32);
    room.advance(TICK);
    assert_eq!(room.cell(3, 1), Cell::Player as u32);
    room.advance(TICK);
    assert_eq!(room.cell(3, 2), Cell::Player as u32);
    room.advance(100_000);
    assert_eq!(room.cell(3, 2), Cell::Player as u32);
}

#[test]
fn hitch_runs_at_most_four_ticks_and_removes_whole_tick_debt() {
    let mut room = Room::new();
    for &letter in b"RRDDLL" {
        tap(&mut room, letter);
    }
    room.advance(1_000_000);
    assert_eq!(room.cell(3, 3), Cell::Player as u32);
    room.advance(0);
    assert_eq!(room.cell(3, 3), Cell::Player as u32);
    room.advance(TICK);
    assert_eq!(room.cell(2, 3), Cell::Player as u32);
}

#[test]
fn single_step_ignores_accumulated_time_and_hitch_counter_keeps_full_range() {
    let mut room = Room::new();
    room.advance(33_333);
    assert_eq!((room.tick(), room.interpolation()), (1, 999_980));
    room.step_one_tick();
    assert_eq!(
        (room.tick(), room.ticks_run(), room.ticks_dropped()),
        (2, 1, 0)
    );
    assert_eq!(room.interpolation(), 999_980);

    room.advance(u32::MAX);
    assert_eq!(room.ticks_run(), 4);
    assert!(room.ticks_dropped() > u8::MAX.into());
    let hitch = room.trace().last().unwrap();
    assert_eq!(hitch.channel, HITCH_CHANNEL);
    assert_eq!(hitch.data, room.ticks_dropped());
}

#[test]
fn player_collides_with_the_guards_new_tick_position() {
    let mut room = Room::new();
    room.table.get_mut(room.player).unwrap().position = Position { x: 8, y: 1 };
    for _ in 1..GUARD_PATROL.period_ticks {
        room.step_one_tick();
    }
    assert_eq!(room.cell(6, 1), Cell::Guard as u32);
    tap(&mut room, b'L');
    room.step_one_tick();
    assert_eq!(room.cell(7, 1), Cell::Guard as u32);
    assert_eq!(room.cell(8, 1), Cell::Player as u32);
    assert_eq!(
        room.events().collect::<Vec<_>>(),
        [Event {
            sequence: 1,
            tick: 12,
            target: 4,
            kind: 4,
            x: 7,
            y: 7
        }]
    );
    let guard = room
        .entities()
        .find(|entity| entity.id == GUARD.id.0)
        .unwrap();
    assert_eq!(
        (guard.previous_x, guard.x, guard.y),
        (6 * 256, 7 * 256, 7 * 256)
    );
}

#[test]
fn restart_publishes_new_epoch_and_drops_delivery_to_the_old_guard() {
    let mut room = Room::new();
    let prior = room.guard;
    room.restart();
    assert_ne!(room.guard, prior);
    assert_eq!((room.epoch(), room.tick()), (2, 0));
    assert_eq!(room.cell(6, 1), Cell::Guard as u32);
    assert_eq!(room.trace().next().unwrap().data, 1);
    room.deliver(PendingEvent {
        kind: EventKind::GuardBlocked,
        target: prior.unwrap(),
        position: GUARD.spawn,
    });
    assert_eq!(room.events().count(), 0);
    assert_eq!(room.events_dropped(), 1);
    assert_eq!(room.trace().last().unwrap().channel, ERROR_CHANNEL);
}

#[test]
fn trace_replays_canonical_room_and_ordered_effects() {
    let mut room = Room::new();
    walk(&mut room, TO_KEY);
    walk(&mut room, KEY_TO_EXIT);
    assert_eq!(room.status(), Status::Completed as u32);
    assert!(room.trace_replay_complete());
    assert_eq!(
        room.events()
            .map(|event| (event.sequence, event.kind, event.target))
            .collect::<Vec<_>>(),
        [(1, 1, 2), (2, 2, 3), (3, 3, 3)]
    );
    let mut actions = room
        .trace()
        .filter(|record| record.channel == INPUT_CHANNEL)
        .peekable();
    let mut replay = Room::new();
    for tick in 1..=room.tick() {
        let action = if actions.peek().is_some_and(|record| record.tick == tick) {
            let record = actions.next().unwrap();
            Some([Action::Up, Action::Down, Action::Left, Action::Right][record.data as usize])
        } else {
            None
        };
        assert!(replay.replay_tick(EffectiveInput::Tile(action)));
    }
    assert_eq!(replay.status(), room.status());
    assert_eq!(
        replay.entities().collect::<Vec<_>>(),
        room.entities().collect::<Vec<_>>()
    );
    assert_eq!(
        replay.events().collect::<Vec<_>>(),
        room.events().collect::<Vec<_>>()
    );
}

#[test]
fn masked_and_overflowed_traces_are_explicitly_incomplete() {
    let mut room = Room::new();
    room.set_trace_mask(room.trace_mask() & !(1 << INPUT_CHANNEL));
    tap(&mut room, b'U');
    room.step_one_tick();
    assert!(!room.trace_replay_complete());
    room.set_trace_mask(DEFAULT_TRACE_MASK);
    assert!(!room.trace_replay_complete());
    room.restart();
    assert!(room.trace_replay_complete());
    for _ in 0..600 {
        tap(&mut room, b'U');
        room.step_one_tick();
    }
    assert_eq!(room.trace().count(), 512);
    assert_eq!(room.trace_dropped(), 89);
    assert_eq!(room.trace().next().unwrap().sequence, 90);
    assert_eq!(room.trace_latest_sequence(), 601);
    assert!(!room.trace_replay_complete());
}

#[test]
fn event_overflow_keeps_latest_sequences_in_delivery_order() {
    let mut room = Room::new();
    for _ in 0..70 {
        room.deliver(PendingEvent {
            kind: EventKind::GuardBlocked,
            target: room.guard.unwrap(),
            position: GUARD.spawn,
        });
    }
    assert_eq!(room.events_dropped(), 6);
    assert!(room.events().map(|event| event.sequence).eq(7..=70));
}

#[test]
fn clearing_focus_drops_held_input_pending_input_and_fractional_time() {
    let mut room = Room::new();
    room.input(Action::Right, true);
    room.advance(TICK);
    assert_eq!(room.cell(2, 1), Cell::Player as u32);
    tap(&mut room, b'L');
    room.advance(16_665);
    room.clear_input();
    room.advance(1_000_000);
    assert_eq!(room.cell(2, 1), Cell::Player as u32);
    room.input(Action::Right, true);
    room.advance(1);
    assert_eq!(room.cell(2, 1), Cell::Player as u32);
    room.advance(TICK);
    assert_eq!(room.cell(3, 1), Cell::Player as u32);
}

#[test]
fn same_input_produces_same_state_at_different_frame_partitions() {
    let mut normal = Room::new();
    let mut uneven = Room::new();
    for segment in [TO_KEY, KEY_TO_EXIT] {
        for group in segment.chunks(2) {
            for &letter in group {
                tap(&mut normal, letter);
                tap(&mut uneven, letter);
            }
            for _ in group {
                normal.advance(TICK);
            }
            uneven.advance(TICK * group.len() as u32 / 3);
            uneven.advance(TICK * group.len() as u32 - TICK * group.len() as u32 / 3);
            assert_eq!(normal.status(), uneven.status());
            for y in 0..normal.height() {
                for x in 0..normal.width() {
                    assert_eq!(normal.cell(x, y), uneven.cell(x, y));
                }
            }
        }
    }
    assert_eq!(uneven.status(), Status::Completed as u32);
}
