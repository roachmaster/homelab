use std::collections::VecDeque;

use crate::Action;

const MAX_EDGES: usize = 32;
const REPEAT_DELAY: u8 = 10;
const REPEAT_INTERVAL: u8 = 6;

#[derive(Clone, Copy)]
struct Edge {
    action: Action,
    pressed: bool,
}

pub(super) struct InputBuffer {
    edges: VecDeque<Edge>,
    observed: [bool; 4],
    held: Vec<Action>,
    repeat_ticks: u8,
}

impl InputBuffer {
    pub(super) fn new() -> Self {
        Self {
            edges: VecDeque::with_capacity(MAX_EDGES),
            observed: [false; 4],
            held: Vec::with_capacity(4),
            repeat_ticks: 0,
        }
    }

    pub(super) fn clear(&mut self) {
        self.edges.clear();
        self.observed.fill(false);
        self.held.clear();
        self.repeat_ticks = 0;
    }

    pub(super) fn enqueue(&mut self, action: Action, pressed: bool) {
        if self.observed[action as usize] == pressed {
            return;
        }
        if self.edges.len() == MAX_EDGES {
            // Reset held state along with lost edges so an omitted release
            // cannot leave a direction active.
            self.clear();
        }
        self.observed[action as usize] = pressed;
        self.edges.push_back(Edge { action, pressed });
    }

    pub(super) fn next_step(&mut self) -> Option<Action> {
        let mut pressed = None;
        while let Some(&edge) = self.edges.front() {
            if edge.pressed && pressed.is_some() {
                break;
            }
            self.edges.pop_front();
            if edge.pressed {
                self.held.push(edge.action);
                pressed = Some(edge.action);
            } else {
                self.held.retain(|&action| action != edge.action);
            }
        }
        if pressed.is_some() {
            self.repeat_ticks = REPEAT_DELAY;
            return pressed;
        }
        let &direction = self.held.last()?;
        self.repeat_ticks = self.repeat_ticks.saturating_sub(1);
        if self.repeat_ticks == 0 {
            self.repeat_ticks = REPEAT_INTERVAL;
            return Some(direction);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_press_does_not_repeat_before_the_initial_delay() {
        let mut input = InputBuffer::new();
        input.enqueue(Action::Right, true);
        input.enqueue(Action::Right, true);
        assert_eq!(input.next_step(), Some(Action::Right));
        for _ in 1..REPEAT_DELAY {
            assert_eq!(input.next_step(), None);
        }
        assert_eq!(input.next_step(), Some(Action::Right));
        input.enqueue(Action::Right, false);
        for _ in 0..REPEAT_INTERVAL {
            assert_eq!(input.next_step(), None);
        }
    }

    #[test]
    fn overflow_drops_pending_and_held_movement() {
        let mut input = InputBuffer::new();
        input.enqueue(Action::Right, true);
        assert_eq!(input.next_step(), Some(Action::Right));
        for _ in 0..MAX_EDGES / 2 {
            input.enqueue(Action::Left, true);
            input.enqueue(Action::Left, false);
        }
        input.enqueue(Action::Down, true);
        assert_eq!(input.next_step(), Some(Action::Down));
        input.enqueue(Action::Down, false);
        for _ in 0..=REPEAT_DELAY {
            assert_eq!(input.next_step(), None);
        }
    }
}
