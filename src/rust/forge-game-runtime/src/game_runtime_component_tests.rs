//! Component tier: the room driven through its public input/tick/replay
//! interface, asserting exact observed inputs, exact traces and events, and
//! rejection without mutation. Public-API behavior scenarios live in
//! `game_runtime_bdd_tests`; per-module unit tests live inline in each module.

use super::*;
use crate::physics::{MotionInput, MotionPhase};

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

fn complete_room() -> Room {
    let mut room = Room::new();
    walk(&mut room, TO_KEY);
    walk(&mut room, KEY_TO_EXIT);
    assert_eq!(room.status(), Status::Completed as u32);
    room
}

#[test]
fn advance_observed_reports_the_exact_effective_input_sequence() {
    let mut room = Room::new();
    for &letter in b"RRD" {
        tap(&mut room, letter);
    }
    let mut observed = Vec::new();
    room.advance_observed(TICK * 3, |tick, input| observed.push((tick, input)));
    assert_eq!(
        observed,
        vec![
            (1, EffectiveInput::Tile(Some(Action::Right))),
            (2, EffectiveInput::Tile(Some(Action::Right))),
            (3, EffectiveInput::Tile(Some(Action::Down))),
        ]
    );
    assert_eq!(room.ticks_run(), 3);
    assert_eq!(room.cell(3, 2), Cell::Player as u32);
}

#[test]
fn step_observed_reports_one_tick_and_its_effective_input() {
    let mut room = Room::new();
    tap(&mut room, b'R');
    let mut seen = None;
    room.step_observed(|tick, input| seen = Some((tick, input)));
    assert_eq!(seen, Some((1, EffectiveInput::Tile(Some(Action::Right)))));
    assert_eq!(room.cell(2, 1), Cell::Player as u32);
    let mut idle = None;
    room.step_observed(|tick, input| idle = Some((tick, input)));
    assert_eq!(idle, Some((2, EffectiveInput::Tile(None))));
}

#[test]
fn replay_tick_rejects_invalid_inputs_without_advancing() {
    let mut room = Room::new();
    assert!(!room.replay_tick(EffectiveInput::Tile(Some(Action::Jump))));
    assert!(!room.replay_tick(EffectiveInput::Platform(MotionInput::default())));
    assert!(!room.replay_tick(EffectiveInput::Completed));
    assert_eq!(room.tick(), 0);
    assert_eq!(room.status(), Status::FindKey as u32);
    assert_eq!(room.events().count(), 0);
    assert_eq!(room.trace().count(), 1);
    assert!(room.replay_tick(EffectiveInput::Tile(Some(Action::Right))));
    assert_eq!(room.tick(), 1);
    assert_eq!(room.cell(2, 1), Cell::Player as u32);

    let mut completed = complete_room();
    let tick = completed.tick();
    let events: Vec<Event> = completed.events().collect();
    assert!(!completed.replay_tick(EffectiveInput::Tile(Some(Action::Right))));
    assert!(!completed.replay_tick(EffectiveInput::Tile(None)));
    assert_eq!(completed.tick(), tick);
    assert_eq!(completed.events().collect::<Vec<_>>(), events);
    assert!(completed.replay_tick(EffectiveInput::Completed));
}

#[test]
fn identical_inputs_produce_identical_traces_events_and_entities() {
    fn run() -> (Vec<TraceRecord>, Vec<Event>, Vec<EntityView>) {
        let mut room = Room::new();
        walk(&mut room, TO_KEY);
        walk(&mut room, KEY_TO_EXIT);
        (
            room.trace().collect(),
            room.events().collect(),
            room.entities().collect(),
        )
    }
    let first = run();
    let second = run();
    assert_eq!(first, second);
    assert_eq!(
        first
            .1
            .iter()
            .map(|event| (event.sequence, event.kind, event.target))
            .collect::<Vec<_>>(),
        vec![(1, 1, 2), (2, 2, 3), (3, 3, 3)]
    );
    assert!(first.0.iter().any(|record| record.channel == INPUT_CHANNEL));
}

#[test]
fn pose_reports_walk_phase_and_facing_after_movement() {
    let mut room = Room::new();
    let idle = room.pose();
    assert_eq!((idle.phase, idle.frame), (MotionPhase::Idle, 0));
    assert_eq!(idle.entity_id, 1);
    tap(&mut room, b'R');
    room.step_one_tick();
    let walking = room.pose();
    assert_eq!(walking.phase, MotionPhase::Walk);
    assert!(!walking.left);
    tap(&mut room, b'L');
    room.step_one_tick();
    assert!(room.pose().left);
    assert_eq!(room.cell(1, 1), Cell::Player as u32);
}
