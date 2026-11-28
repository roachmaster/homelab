use super::{Feature, Step};
use std::fmt;

type Action<W> = Box<dyn Fn(&mut W, &[String]) -> Result<(), String>>;
type Bindings<W> = [(Pattern, Action<W>)];

/// A phrase with `{}` placeholders; each placeholder matches one
/// double-quoted value in the step text and is passed to the binding.
struct Pattern {
    phrase: String,
    pieces: Vec<String>,
}

impl Pattern {
    fn new(phrase: &str) -> Self {
        Self {
            phrase: phrase.to_string(),
            pieces: phrase.split("{}").map(str::to_string).collect(),
        }
    }

    fn matches(&self, text: &str) -> Option<Vec<String>> {
        let mut rest = text.strip_prefix(self.pieces[0].as_str())?;
        let mut args = Vec::new();
        for piece in &self.pieces[1..] {
            let quoted = rest.strip_prefix('"')?;
            let end = quoted.find('"')?;
            args.push(quoted[..end].to_string());
            rest = quoted[end + 1..].strip_prefix(piece.as_str())?;
        }
        rest.is_empty().then_some(args)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepError {
    pub scenario: String,
    pub line: usize,
    pub text: String,
    pub reason: String,
}

impl fmt::Display for StepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (line {}): `{}`: {}",
            self.scenario, self.line, self.text, self.reason
        )
    }
}

/// What a successful run exercised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub scenarios: usize,
    pub steps: usize,
}

/// The deterministic backend: every phrase maps to exactly one binding.
///
/// `W` is the scenario's world. A fresh `W::default()` is created for every
/// scenario; capabilities and states arrange it, behaviours observe it.
pub struct Steps<W> {
    capabilities: Vec<(Pattern, Action<W>)>,
    states: Vec<(Pattern, Action<W>)>,
    behaviors: Vec<(Pattern, Action<W>)>,
}

impl<W: Default> Default for Steps<W> {
    fn default() -> Self {
        Self::new()
    }
}

impl<W: Default> Steps<W> {
    pub fn new() -> Self {
        Self {
            capabilities: Vec::new(),
            states: Vec::new(),
            behaviors: Vec::new(),
        }
    }

    /// Bind a `Given` phrase (a capability).
    pub fn given(
        mut self,
        phrase: &str,
        action: impl Fn(&mut W, &[String]) -> Result<(), String> + 'static,
    ) -> Self {
        self.capabilities
            .push((Pattern::new(phrase), Box::new(action)));
        self
    }

    /// Bind a `When` phrase (a state or binding).
    pub fn when(
        mut self,
        phrase: &str,
        action: impl Fn(&mut W, &[String]) -> Result<(), String> + 'static,
    ) -> Self {
        self.states.push((Pattern::new(phrase), Box::new(action)));
        self
    }

    /// Bind a `Then` phrase (an observable behaviour).
    pub fn then(
        mut self,
        phrase: &str,
        action: impl Fn(&mut W, &[String]) -> Result<(), String> + 'static,
    ) -> Self {
        self.behaviors
            .push((Pattern::new(phrase), Box::new(action)));
        self
    }

    /// Every phrase bound under a keyword, for documentation and review.
    pub fn phrases(&self) -> Vec<(&'static str, &str)> {
        let mut all = Vec::new();
        for (kind, list) in [
            ("Given", &self.capabilities),
            ("When", &self.states),
            ("Then", &self.behaviors),
        ] {
            for (pattern, _) in list {
                all.push((kind, pattern.phrase.as_str()));
            }
        }
        all
    }

    /// Run every scenario. Unbound or ambiguous phrases fail before any step
    /// of the feature runs, so a scenario can never pass by skipping a step.
    pub fn run(&self, feature: &Feature) -> Result<Report, Vec<StepError>> {
        let mut plan = Vec::new();
        let mut errors = Vec::new();
        for scenario in &feature.scenarios {
            let mut bound = Vec::new();
            let groups: [(&Bindings<W>, Vec<&Step>); 3] = [
                (
                    &self.capabilities,
                    scenario.capabilities.iter().map(|c| &c.0).collect(),
                ),
                (&self.states, scenario.states.iter().map(|s| &s.0).collect()),
                (
                    &self.behaviors,
                    scenario.behaviors.iter().map(|b| &b.0).collect(),
                ),
            ];
            for (bindings, steps) in groups {
                for step in steps {
                    let matches: Vec<_> = bindings
                        .iter()
                        .filter_map(|(pattern, action)| {
                            pattern.matches(&step.text).map(|args| (action, args))
                        })
                        .collect();
                    match matches.len() {
                        1 => {
                            let (action, args) = matches.into_iter().next().expect("one match");
                            bound.push((step, action, args));
                        }
                        count => errors.push(StepError {
                            scenario: scenario.title.clone(),
                            line: step.line,
                            text: step.text.clone(),
                            reason: if count == 0 {
                                "no step is bound to this phrase".into()
                            } else {
                                format!("{count} bindings match this phrase")
                            },
                        }),
                    }
                }
            }
            plan.push((scenario, bound));
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let mut steps = 0;
        for (scenario, bound) in &plan {
            let mut world = W::default();
            for (step, action, args) in bound {
                steps += 1;
                if let Err(reason) = action(&mut world, args) {
                    errors.push(StepError {
                        scenario: scenario.title.clone(),
                        line: step.line,
                        text: step.text.clone(),
                        reason,
                    });
                    break;
                }
            }
        }
        if errors.is_empty() {
            Ok(Report {
                scenarios: plan.len(),
                steps,
            })
        } else {
            Err(errors)
        }
    }
}

/// Turn a failed check into a step failure message.
pub fn ensure(condition: bool, message: impl Into<String>) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bdd::parse;

    #[derive(Default)]
    struct World {
        name: String,
    }

    fn steps() -> Steps<World> {
        Steps::new()
            .given("a cat can be given a name", |_, _| Ok(()))
            .when("it is called {}", |w: &mut World, args| {
                w.name = args[0].clone();
                Ok(())
            })
            .then("it comes when {} is called", |w, args| {
                ensure(w.name == args[0], format!("it answers to {}", w.name))
            })
    }

    #[test]
    fn quoted_values_flow_from_the_prose_into_the_binding() {
        let feature = parse(
            "Feature: Names\nScenario: Whiskers\nGiven a cat can be given a name\nWhen it is called \"Whiskers\"\nThen it comes when \"Whiskers\" is called\n",
        )
        .unwrap();
        assert_eq!(
            steps().run(&feature).unwrap(),
            Report {
                scenarios: 1,
                steps: 3
            }
        );
    }

    #[test]
    fn an_unbound_phrase_fails_instead_of_inventing_behaviour() {
        let feature = parse(
            "Feature: Names\nScenario: Flying\nGiven a cat can be given a name\nThen it flies\n",
        )
        .unwrap();
        let errors = steps().run(&feature).unwrap_err();
        assert_eq!(errors[0].line, 4);
        assert_eq!(errors[0].reason, "no step is bound to this phrase");
    }

    #[test]
    fn a_wrong_outcome_fails_the_behaviour_step() {
        let feature = parse(
            "Feature: Names\nScenario: Mixup\nGiven a cat can be given a name\nWhen it is called \"Tom\"\nThen it comes when \"Whiskers\" is called\n",
        )
        .unwrap();
        assert_eq!(
            steps().run(&feature).unwrap_err()[0].reason,
            "it answers to Tom"
        );
    }
}
