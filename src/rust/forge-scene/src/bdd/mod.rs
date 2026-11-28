//! Natural-language scenarios over a deterministic backend.
//!
//! Prose is written by hand in `.feature` files and parsed into a typed AST:
//!
//! | Keyword | AST node | Meaning |
//! | --- | --- | --- |
//! | `Given` | [`Capability`] | what the subject *can* have or needs |
//! | `When` | [`State`] | the state or binding it is in |
//! | `Then` | [`Behavior`] | what you can observe as a result |
//!
//! `And`/`But` continue the previous keyword. The AST's JSON shape is
//! `schemas/feature.schema.json`. A [`Steps`] registry binds each phrase to one
//! fixed function; outcomes come from the code under test, so a scenario can
//! observe behaviour but never invent it. An unbound phrase is an error.

mod grammar;
mod steps;
mod voice;

pub use grammar::{parse, ParseError};
pub use steps::{ensure, Report, StepError, Steps};
pub use voice::{check_voice, VoiceError};

/// A step as written, with its 1-based line number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub line: usize,
    pub text: String,
}

/// `Given`: a capability of the subject, e.g. "a dog can have an owner".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability(pub Step);

/// `When`: a state or binding, e.g. "it has no collar but is chipped".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State(pub Step);

/// `Then`: an observable behaviour, e.g. "a shelter scan finds its owner".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Behavior(pub Step);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scenario {
    pub title: String,
    pub line: usize,
    pub capabilities: Vec<Capability>,
    pub states: Vec<State>,
    pub behaviors: Vec<Behavior>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feature {
    pub title: String,
    pub narrative: Vec<String>,
    pub scenarios: Vec<Scenario>,
}

impl Feature {
    /// The AST in the JSON shape described by `schemas/feature.schema.json`.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::json;
        let step = |s: &Step| json!({ "line": s.line, "text": s.text });
        json!({
            "feature": self.title,
            "narrative": self.narrative,
            "scenarios": self.scenarios.iter().map(|scenario| json!({
                "title": scenario.title,
                "line": scenario.line,
                "given": scenario.capabilities.iter().map(|c| json!({"capability": step(&c.0)})).collect::<Vec<_>>(),
                "when": scenario.states.iter().map(|s| json!({"state": step(&s.0)})).collect::<Vec<_>>(),
                "then": scenario.behaviors.iter().map(|b| json!({"behavior": step(&b.0)})).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }
}

/// The JSON Schema for parsed features.
pub const FEATURE_SCHEMA: &str = include_str!("../../schemas/feature.schema.json");

/// Parse a feature, check its AST against the feature schema, check the voice
/// rules, then run it against `steps`. Every failure is reported with its line.
pub fn verify<W: Default>(source: &str, steps: &Steps<W>) -> Result<Report, String> {
    let feature = parse(source).map_err(|e| e.to_string())?;
    let schema: serde_json::Value =
        serde_json::from_str(FEATURE_SCHEMA).map_err(|e| e.to_string())?;
    crate::json_schema::validate(&schema, &feature.to_json())
        .map_err(|e| format!("feature AST: {e}"))?;
    let voice = check_voice(&feature);
    if !voice.is_empty() {
        return Err(voice
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"));
    }
    steps.run(&feature).map_err(|errors| {
        errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    })
}
