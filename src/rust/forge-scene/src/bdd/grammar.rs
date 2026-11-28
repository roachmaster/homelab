use super::{Behavior, Capability, Feature, Scenario, State, Step};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub reason: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.reason)
    }
}

impl std::error::Error for ParseError {}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Given,
    When,
    Then,
}

/// Parse one feature file.
///
/// Grammar: `Feature: <title>`, optional narrative lines, then one or more
/// `Scenario: <title>` blocks. Each block is one or more `Given`, zero or more
/// `When`, one or more `Then`, in that order; `And`/`But` repeat the previous
/// keyword. `#` starts a comment line. Steps are lower-case prose, so the
/// keyword's capital letter is the only one most lines need.
pub fn parse(source: &str) -> Result<Feature, ParseError> {
    let mut feature: Option<Feature> = None;
    let mut current: Option<(Scenario, Option<Kind>)> = None;
    let err = |line: usize, reason: &str| ParseError {
        line,
        reason: reason.to_string(),
    };

    let finish = |current: &mut Option<(Scenario, Option<Kind>)>,
                  feature: &mut Feature|
     -> Result<(), ParseError> {
        if let Some((scenario, _)) = current.take() {
            if scenario.capabilities.is_empty() || scenario.behaviors.is_empty() {
                return Err(ParseError {
                    line: scenario.line,
                    reason: "a scenario needs at least one Given and one Then".into(),
                });
            }
            feature.scenarios.push(scenario);
        }
        Ok(())
    };

    for (index, raw) in source.lines().enumerate() {
        let line = index + 1;
        let text = raw.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        if let Some(title) = text.strip_prefix("Feature:") {
            if feature.is_some() {
                return Err(err(line, "only one Feature per file"));
            }
            feature = Some(Feature {
                title: title.trim().to_string(),
                narrative: Vec::new(),
                scenarios: Vec::new(),
            });
            continue;
        }
        let Some(feature) = feature.as_mut() else {
            return Err(err(line, "a file starts with `Feature:`"));
        };
        if let Some(title) = text.strip_prefix("Scenario:") {
            finish(&mut current, feature)?;
            let title = title.trim();
            if title.is_empty() {
                return Err(err(line, "a scenario needs a title"));
            }
            current = Some((
                Scenario {
                    title: title.to_string(),
                    line,
                    capabilities: Vec::new(),
                    states: Vec::new(),
                    behaviors: Vec::new(),
                },
                None,
            ));
            continue;
        }
        let (keyword, rest) = text.split_once(' ').unwrap_or((text, ""));
        let kind = match keyword {
            "Given" => Some(Kind::Given),
            "When" => Some(Kind::When),
            "Then" => Some(Kind::Then),
            "And" | "But" => None,
            _ => {
                if current.is_none() && feature.scenarios.is_empty() {
                    feature.narrative.push(text.to_string());
                    continue;
                }
                return Err(err(
                    line,
                    "expected Given, When, Then, And, But or Scenario:",
                ));
            }
        };
        let Some((scenario, last)) = current.as_mut() else {
            return Err(err(line, "a step belongs inside a Scenario"));
        };
        let kind = match (kind, *last) {
            (Some(kind), _) => kind,
            (None, Some(previous)) => previous,
            (None, None) => return Err(err(line, "`And`/`But` must follow a step")),
        };
        if last.is_some_and(|previous| kind < previous) {
            return Err(err(line, "steps go Given, then When, then Then"));
        }
        let rest = rest.trim();
        if rest.is_empty() {
            return Err(err(line, "a step needs words after its keyword"));
        }
        let step = Step {
            line,
            text: rest.to_string(),
        };
        match kind {
            Kind::Given => scenario.capabilities.push(Capability(step)),
            Kind::When => scenario.states.push(State(step)),
            Kind::Then => scenario.behaviors.push(Behavior(step)),
        }
        *last = Some(kind);
    }
    let Some(mut feature) = feature else {
        return Err(err(1, "a file starts with `Feature:`"));
    };
    finish(&mut current, &mut feature)?;
    if feature.scenarios.is_empty() {
        return Err(err(1, "a feature needs at least one scenario"));
    }
    Ok(feature)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_parse_into_capability_state_and_behavior_nodes() {
        let feature = parse(
            "Feature: Crates\n  Dogs and crates.\n\n  Scenario: Trained\n    Given a dog can be crate trained\n    When it is crate trained\n    Then it is calm in a crate\n    And it sleeps\n",
        )
        .unwrap();
        assert_eq!(feature.narrative, ["Dogs and crates."]);
        let scenario = &feature.scenarios[0];
        assert_eq!(
            scenario.capabilities[0].0.text,
            "a dog can be crate trained"
        );
        assert_eq!(scenario.states[0].0.line, 6);
        assert_eq!(scenario.behaviors.len(), 2);
    }

    #[test]
    fn out_of_order_and_incomplete_scenarios_are_rejected() {
        let backwards =
            "Feature: x\nScenario: y\nGiven a dog can bark\nThen it barks\nWhen it is awake\n";
        assert_eq!(parse(backwards).unwrap_err().line, 5);
        let no_then = "Feature: x\nScenario: y\nGiven a dog can bark\nWhen it is awake\n";
        assert!(parse(no_then).is_err());
        assert!(parse("Scenario: y\n").is_err());
    }
}
