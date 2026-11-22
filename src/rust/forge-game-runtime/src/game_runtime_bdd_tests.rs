//! BDD contract tier: the room's behavior as public-API scenarios.
//! Given/then form, asserting only what a consumer can observe.

use crate::{Action, Cell, Room, Status};

const TICK: u32 = 16_667;
const TO_KEY: &[u8] = b"RRDDLLDDRRDDLL";
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
fn given_a_fresh_room_when_the_key_is_collected_then_the_exit_opens() {
    let mut room = Room::new();
    assert_eq!(room.cell(11, 4), Cell::LockedExit as u32);
    assert_eq!(room.status(), Status::FindKey as u32);
    walk(&mut room, TO_KEY);
    assert_eq!(room.cell(1, 7), Cell::Player as u32);
    assert_eq!(room.cell(11, 4), Cell::OpenExit as u32);
    assert_eq!(room.status(), Status::ExitUnlocked as u32);
}

#[test]
fn given_an_open_exit_when_the_player_reaches_it_then_the_room_completes() {
    let mut room = Room::new();
    walk(&mut room, TO_KEY);
    walk(&mut room, KEY_TO_EXIT);
    assert_eq!(room.cell(11, 4), Cell::Player as u32);
    assert_eq!(room.status(), Status::Completed as u32);
    assert!(room.trace_replay_complete());
}

#[test]
fn given_a_completed_room_when_input_arrives_then_the_player_does_not_move() {
    let mut room = Room::new();
    walk(&mut room, TO_KEY);
    walk(&mut room, KEY_TO_EXIT);
    walk(&mut room, b"DDD");
    assert_eq!(room.cell(11, 4), Cell::Player as u32);
    assert_eq!(room.status(), Status::Completed as u32);
}

#[test]
fn given_a_completed_room_when_it_restarts_then_the_room_is_fresh() {
    let mut room = Room::new();
    walk(&mut room, TO_KEY);
    walk(&mut room, KEY_TO_EXIT);
    assert_eq!(room.status(), Status::Completed as u32);
    room.restart();
    assert_eq!(room.epoch(), 2);
    assert_eq!(room.tick(), 0);
    assert_eq!(room.cell(1, 1), Cell::Player as u32);
    assert_eq!(room.cell(1, 7), Cell::Key as u32);
    assert_eq!(room.cell(11, 4), Cell::LockedExit as u32);
    assert_eq!(room.status(), Status::FindKey as u32);
}
