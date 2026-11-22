//! Local presentation derived from this tick's resolved motion.

use crate::physics::{MotionFrame, MotionPhase};

#[derive(Clone, Copy, Debug)]
pub struct AnimationTiming {
    pub idle: [u32; 1],
    pub walk: [u32; 4],
    pub jump: [u32; 12],
    pub crouch: [u32; 4],
}

impl Default for AnimationTiming {
    fn default() -> Self {
        Self {
            idle: [240],
            walk: [105, 85, 105, 85],
            jump: [105, 50, 50, 50, 50, 50, 50, 50, 50, 80, 80, 100],
            crouch: [80, 300, 80, 85],
        }
    }
}

impl AnimationTiming {
    pub(crate) fn valid(self) -> bool {
        self.idle
            .into_iter()
            .chain(self.walk)
            .chain(self.jump)
            .chain(self.crouch)
            .all(|duration| (1..=60_000).contains(&duration))
    }
}

/// Pose to join with the completed tick and the stable player ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlayerPose {
    pub entity_id: u16,
    pub phase: MotionPhase,
    pub frame: u32,
    pub left: bool,
    pub grounded: bool,
    pub contacts: u8,
}

pub(crate) struct Animator {
    phase: MotionPhase,
    frame: usize,
    elapsed: u32,
}

impl Animator {
    pub(crate) fn new() -> Self {
        Self {
            phase: MotionPhase::Idle,
            frame: 0,
            elapsed: 0,
        }
    }

    pub(crate) fn advance(&mut self, motion: MotionFrame, timing: AnimationTiming) {
        let phase = if self.phase == MotionPhase::Land
            && self.frame < 3
            && motion.phase == MotionPhase::Idle
        {
            MotionPhase::Land
        } else {
            motion.phase
        };
        self.update(phase, timing);
    }

    pub(crate) fn advance_tile(&mut self, walking: bool, timing: AnimationTiming) {
        self.update(
            if walking {
                MotionPhase::Walk
            } else {
                MotionPhase::Idle
            },
            timing,
        );
    }

    fn update(&mut self, phase: MotionPhase, timing: AnimationTiming) {
        let (start, end, durations, looping): (usize, usize, &[u32], bool) = match phase {
            MotionPhase::Idle => (0, 0, &timing.idle, true),
            MotionPhase::Walk => (0, 3, &timing.walk, true),
            MotionPhase::Rise => (1, 5, &timing.jump, false),
            MotionPhase::Fall => (5, 8, &timing.jump, false),
            MotionPhase::Land => (1, 3, &timing.crouch, false),
        };
        if self.phase != phase {
            self.phase = phase;
            self.frame = start;
            self.elapsed = 0;
            return;
        }
        // Store milliseconds in sixtieths. One tick adds exactly 1000 units.
        self.elapsed += 1000;
        while self.elapsed >= durations[self.frame] * 60 {
            self.elapsed -= durations[self.frame] * 60;
            if self.frame < end {
                self.frame += 1;
            } else if looping {
                self.frame = start;
            } else {
                self.elapsed = 0;
                break;
            }
        }
    }

    pub(crate) fn view(
        &self,
        entity_id: u16,
        left: bool,
        motion: Option<MotionFrame>,
    ) -> PlayerPose {
        PlayerPose {
            entity_id,
            phase: self.phase,
            frame: self.frame as u32,
            left,
            grounded: motion.is_none_or(|state| state.grounded),
            contacts: motion.map_or(0, |state| {
                u8::from(state.landed.is_some())
                    | (u8::from(state.head_bump.is_some()) << 1)
                    | (u8::from(state.wall_contact.is_some()) << 2)
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_timing_is_valid_and_bounds_are_exact() {
        assert!(AnimationTiming::default().valid());
        assert!(!AnimationTiming {
            idle: [0],
            ..AnimationTiming::default()
        }
        .valid());
        assert!(!AnimationTiming {
            walk: [105, 85, 105, 60_001],
            ..AnimationTiming::default()
        }
        .valid());
        assert!(AnimationTiming {
            jump: [1, 60_000, 50, 50, 50, 50, 50, 50, 50, 80, 80, 100],
            ..AnimationTiming::default()
        }
        .valid());
    }

    #[test]
    fn animator_starts_idle_and_walk_advances_frames_on_schedule() {
        let mut animator = Animator::new();
        let timing = AnimationTiming::default();
        let pose = animator.view(1, false, None);
        assert_eq!((pose.phase, pose.frame), (MotionPhase::Idle, 0));
        assert!(pose.grounded);
        assert_eq!(pose.contacts, 0);
        animator.advance_tile(false, timing);
        assert_eq!(animator.view(1, false, None).phase, MotionPhase::Idle);
        animator.advance_tile(true, timing);
        let walking = animator.view(1, false, None);
        assert_eq!((walking.phase, walking.frame), (MotionPhase::Walk, 0));
        // Walk frame 0 lasts 105 ms = 6300 sixtieths-ms units; each tick adds 1000.
        for _ in 0..6 {
            animator.advance_tile(true, timing);
        }
        assert_eq!(animator.view(1, false, None).frame, 0);
        animator.advance_tile(true, timing);
        assert_eq!(animator.view(1, false, None).frame, 1);
        animator.advance_tile(false, timing);
        let idle = animator.view(1, false, None);
        assert_eq!((idle.phase, idle.frame), (MotionPhase::Idle, 0));
    }

    #[test]
    fn view_reports_entity_facing_and_grounding_exactly() {
        let animator = Animator::new();
        let pose = animator.view(7, true, None);
        assert_eq!(pose.entity_id, 7);
        assert!(pose.left);
        assert!(pose.grounded);
    }
}
