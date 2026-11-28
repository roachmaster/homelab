//! Voice rules for hand-written scenarios.
//!
//! - A `Given` states a capability: the subject *can*, *may* or *needs* something.
//! - A `Then` is an observable behaviour. It must not restate the `When` state
//!   ("when it is crate trained, then it is crate trained") and must not describe
//!   how the code routes a call ("then it answers through its owner").

use super::Feature;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceError {
    pub line: usize,
    pub reason: String,
}

impl fmt::Display for VoiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.reason)
    }
}

const CAPABILITY_WORDS: &[&str] = &[" can ", " may ", " needs ", " need "];
const ROUTING_WORDS: &[&str] = &[
    "answers through",
    "through its",
    "delegates",
    "routes",
    "calls into",
    "forwards",
];

fn tail(text: &str) -> Vec<String> {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|word| !word.is_empty())
        .collect();
    words[words.len().saturating_sub(2)..].to_vec()
}

pub fn check_voice(feature: &Feature) -> Vec<VoiceError> {
    let mut errors = Vec::new();
    for scenario in &feature.scenarios {
        for capability in &scenario.capabilities {
            let padded = format!(" {} ", capability.0.text);
            if !CAPABILITY_WORDS.iter().any(|word| padded.contains(word)) {
                errors.push(VoiceError {
                    line: capability.0.line,
                    reason: "a Given states a capability: what the subject can, may or needs"
                        .into(),
                });
            }
        }
        for behavior in &scenario.behaviors {
            let text = behavior.0.text.to_lowercase();
            if let Some(word) = ROUTING_WORDS.iter().find(|word| text.contains(*word)) {
                errors.push(VoiceError {
                    line: behavior.0.line,
                    reason: format!("a Then describes what you observe, not routing (`{word}`)"),
                });
            }
            for state in &scenario.states {
                let state_tail = tail(&state.0.text);
                if state_tail.len() == 2 && tail(&behavior.0.text) == state_tail {
                    errors.push(VoiceError {
                        line: behavior.0.line,
                        reason: format!(
                            "a Then must not restate the When (`{}`)",
                            state_tail.join(" ")
                        ),
                    });
                }
            }
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bdd::parse;

    fn voice(steps: &str) -> Vec<VoiceError> {
        check_voice(&parse(&format!("Feature: f\nScenario: s\n{steps}")).unwrap())
    }

    #[test]
    fn observable_behaviour_passes() {
        assert!(voice("Given a dog can be crate trained\nWhen it is crate trained\nThen it is calm in a crate\n").is_empty());
    }

    #[test]
    fn restating_the_state_fails() {
        let errors = voice("Given a dog can be crate trained\nWhen it is crate trained\nThen it is crate trained\n");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, 5);
    }

    #[test]
    fn routing_language_fails() {
        assert_eq!(voice("Given a dog can have an owner\nWhen it is owned\nThen it answers through its owner\n").len(), 1);
    }

    #[test]
    fn a_given_without_a_capability_fails() {
        assert_eq!(voice("Given a dog\nThen it barks\n").len(), 1);
    }
}
