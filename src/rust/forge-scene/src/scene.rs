//! The scene model: typed data that describes ports, implementations,
//! lifecycles, decorators and environments. Parsed and checked at build time.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    pub scene: String,
    pub title: String,
    pub ports: Vec<Port>,
    pub instances: Vec<Instance>,
    #[serde(default)]
    pub decorators: Vec<DecoratorSpec>,
    pub envs: Vec<Env>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    pub name: String,
    pub verbs: Vec<Verb>,
    #[serde(default)]
    pub default: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verb {
    pub name: String,
    pub request: String,
    pub response: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stereotype {
    Impl,
    Default,
    TestDouble,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Singleton,
    LazySingleton,
    Transient,
    Assisted,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    pub name: String,
    pub port: String,
    #[serde(default = "default_stereotype")]
    pub stereotype: Stereotype,
    pub lifecycle: Lifecycle,
    #[serde(default)]
    pub says: Option<String>,
    #[serde(default)]
    pub assisted_arg: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<Dependency>,
    #[serde(default)]
    pub answers: Vec<Answer>,
}

fn default_stereotype() -> Stereotype {
    Stereotype::Impl
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub port: String,
    pub optional: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    pub verb: String,
    pub ask: Vec<String>,
    pub otherwise: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecoratorSpec {
    pub name: String,
    pub port: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Env {
    pub name: String,
    pub bind: Vec<String>,
    #[serde(default)]
    pub decorate: Vec<String>,
}

/// A scene that does not describe compilable, well-formed wiring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneError(pub String);

impl fmt::Display for SceneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid scene: {}", self.0)
    }
}

impl std::error::Error for SceneError {}

fn fail<T>(message: impl Into<String>) -> Result<T, SceneError> {
    Err(SceneError(message.into()))
}

const RESERVED: &[&str] = &[
    "as", "async", "await", "box", "break", "const", "continue", "crate", "dyn", "else", "enum",
    "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
    "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "yield", "env", "factory", "lines", "scene", "request",
];

pub fn is_snake(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !value.ends_with('_')
        && !value.contains("__")
        && !RESERVED.contains(&value)
}

pub fn is_pascal(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z')) && bytes.all(|b| b.is_ascii_alphanumeric())
}

pub fn pascal(snake: &str) -> String {
    snake
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
                .unwrap_or_default()
        })
        .collect()
}

pub fn snake(pascal: &str) -> String {
    let mut out = String::new();
    for (index, ch) in pascal.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

impl Scene {
    pub fn port(&self, name: &str) -> Option<&Port> {
        self.ports.iter().find(|port| port.name == name)
    }

    pub fn instance(&self, name: &str) -> Option<&Instance> {
        self.instances.iter().find(|instance| instance.name == name)
    }

    /// Check everything the template relies on, so the rendered code compiles
    /// and every environment either freezes or is rejected for a stated reason.
    pub fn validate(&self) -> Result<(), SceneError> {
        if !is_snake(&self.scene) {
            return fail(format!("scene name {:?} must be snake_case", self.scene));
        }
        let mut types = BTreeMap::new();
        let mut names = BTreeSet::new();
        for port in &self.ports {
            if !is_pascal(&port.name) || !names.insert(port.name.clone()) {
                return fail(format!("port {:?} must be unique PascalCase", port.name));
            }
            if !is_snake(&snake(&port.name)) {
                return fail(format!(
                    "port {} would generate a reserved field name",
                    port.name
                ));
            }
            if port.verbs.is_empty() {
                return fail(format!("port {} needs at least one verb", port.name));
            }
            let mut verbs = BTreeSet::new();
            for verb in &port.verbs {
                if !is_snake(&verb.name) || !verbs.insert(&verb.name) {
                    return fail(format!(
                        "port {} verb {:?} must be unique snake_case",
                        port.name, verb.name
                    ));
                }
                for ty in [&verb.request, &verb.response] {
                    if !is_pascal(ty) {
                        return fail(format!("type {ty:?} must be PascalCase"));
                    }
                    types.insert(ty.clone(), ());
                }
            }
        }
        for port in &self.ports {
            if types.contains_key(&port.name) {
                return fail(format!(
                    "port {} reuses a request/response type name",
                    port.name
                ));
            }
        }
        let mut instance_names = BTreeSet::new();
        let mut type_names: BTreeSet<String> = names.iter().cloned().collect();
        type_names.extend(types.keys().cloned());
        for instance in &self.instances {
            if !is_snake(&instance.name) || !instance_names.insert(instance.name.clone()) {
                return fail(format!(
                    "instance {:?} must be unique snake_case",
                    instance.name
                ));
            }
            for generated in [pascal(&instance.name), pascal(&instance.name) + "Factory"] {
                if !type_names.insert(generated.clone()) {
                    return fail(format!(
                        "instance {} would generate a duplicate type {generated}",
                        instance.name
                    ));
                }
            }
            let Some(port) = self.port(&instance.port) else {
                return fail(format!(
                    "instance {} names unknown port {}",
                    instance.name, instance.port
                ));
            };
            self.validate_instance(instance, port)?;
        }
        for port in &self.ports {
            if let Some(default) = &port.default {
                match self.instance(default) {
                    Some(instance)
                        if instance.port == port.name
                            && instance.stereotype == Stereotype::Default
                            && instance.depends_on.is_empty()
                            && instance.lifecycle != Lifecycle::Assisted => {}
                    _ => {
                        return fail(format!(
                            "port {} default {default} must be a leaf default of that port",
                            port.name
                        ))
                    }
                }
            }
        }
        let mut decorators = BTreeSet::new();
        for decorator in &self.decorators {
            if !is_snake(&decorator.name) || !decorators.insert(decorator.name.clone()) {
                return fail(format!(
                    "decorator {:?} must be unique snake_case",
                    decorator.name
                ));
            }
            if self.port(&decorator.port).is_none() {
                return fail(format!("decorator {} names unknown port", decorator.name));
            }
            for generated in [
                pascal(&decorator.name),
                pascal(&decorator.name) + "Decorator",
            ] {
                if !type_names.insert(generated.clone()) {
                    return fail(format!(
                        "decorator {} would generate a duplicate type {generated}",
                        decorator.name
                    ));
                }
            }
        }
        self.validate_envs(&decorators)?;
        self.dependency_order().map(|_| ())
    }

    fn validate_instance(&self, instance: &Instance, port: &Port) -> Result<(), SceneError> {
        let name = &instance.name;
        let assisted = instance.lifecycle == Lifecycle::Assisted;
        if assisted != instance.assisted_arg.is_some() {
            return fail(format!(
                "instance {name}: `assisted_arg` goes with the assisted lifecycle only"
            ));
        }
        if let Some(arg) = &instance.assisted_arg {
            if !is_snake(arg) {
                return fail(format!("instance {name}: assisted_arg must be snake_case"));
            }
        }
        let composite = !instance.depends_on.is_empty();
        if composite {
            if assisted || instance.says.is_some() {
                return fail(format!("instance {name}: a composite answers through `answers`, not `says` or assisted args"));
            }
            let mut deps = BTreeSet::new();
            for dep in &instance.depends_on {
                if self.port(&dep.port).is_none() || !deps.insert(dep.port.clone()) {
                    return fail(format!(
                        "instance {name}: dependency {} is unknown or repeated",
                        dep.port
                    ));
                }
                if dep.port == instance.port {
                    return fail(format!("instance {name} cannot depend on its own port"));
                }
                if dep.optional
                    && self
                        .port(&dep.port)
                        .and_then(|p| p.default.as_ref())
                        .is_none()
                {
                    return fail(format!(
                        "instance {name}: optional dependency {} needs a port default",
                        dep.port
                    ));
                }
            }
            let answered: BTreeSet<&str> =
                instance.answers.iter().map(|a| a.verb.as_str()).collect();
            let declared: BTreeSet<&str> = port.verbs.iter().map(|v| v.name.as_str()).collect();
            if answered != declared || answered.len() != instance.answers.len() {
                return fail(format!(
                    "instance {name} must answer every verb of {} exactly once",
                    port.name
                ));
            }
            for answer in &instance.answers {
                for ask in &answer.ask {
                    let (field, verb) = ask.split_once('.').unwrap_or_default();
                    let target = instance
                        .depends_on
                        .iter()
                        .find(|dep| snake(&dep.port) == field)
                        .and_then(|dep| self.port(&dep.port));
                    let Some(target) = target else {
                        return fail(format!(
                            "instance {name}: `{ask}` does not name a dependency"
                        ));
                    };
                    if !target.verbs.iter().any(|v| v.name == verb) {
                        return fail(format!("instance {name}: `{ask}` names an unknown verb"));
                    }
                }
            }
        } else {
            if !instance.answers.is_empty() {
                return fail(format!(
                    "instance {name}: `answers` needs collaborators in `depends_on`"
                ));
            }
            if assisted == instance.says.is_some() {
                return fail(format!("instance {name}: a leaf needs `says`, an assisted leaf needs only `assisted_arg`"));
            }
        }
        Ok(())
    }

    fn validate_envs(&self, decorators: &BTreeSet<String>) -> Result<(), SceneError> {
        let mut envs = BTreeSet::new();
        for env in &self.envs {
            if !is_snake(&env.name) || !envs.insert(env.name.clone()) {
                return fail(format!("env {:?} must be unique snake_case", env.name));
            }
            let mut ports = BTreeSet::new();
            for bound in &env.bind {
                let Some(instance) = self.instance(bound) else {
                    return fail(format!("env {} binds unknown instance {bound}", env.name));
                };
                if instance.lifecycle == Lifecycle::Assisted {
                    return fail(format!(
                        "env {}: assisted {bound} is reached through its provider, not bound",
                        env.name
                    ));
                }
                if !ports.insert(instance.port.clone()) {
                    return fail(format!(
                        "env {} binds port {} twice",
                        env.name, instance.port
                    ));
                }
            }
            for decorator in &env.decorate {
                if !decorators.contains(decorator) {
                    return fail(format!(
                        "env {} uses unknown decorator {decorator}",
                        env.name
                    ));
                }
            }
        }
        Ok(())
    }

    /// Ports ordered so every port comes after the ports its implementations
    /// depend on. This is the order groups are staged in every environment.
    pub fn dependency_order(&self) -> Result<Vec<&Port>, SceneError> {
        let mut edges: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for port in &self.ports {
            edges.entry(&port.name).or_default();
        }
        for instance in &self.instances {
            for dep in &instance.depends_on {
                edges.entry(&instance.port).or_default().insert(&dep.port);
            }
        }
        let mut ordered: Vec<&Port> = Vec::new();
        let mut placed = BTreeSet::new();
        while ordered.len() < self.ports.len() {
            let next = self.ports.iter().find(|port| {
                !placed.contains(port.name.as_str())
                    && edges[port.name.as_str()]
                        .iter()
                        .all(|dep| placed.contains(dep))
            });
            let Some(port) = next else {
                return fail("ports depend on each other in a cycle");
            };
            placed.insert(port.name.as_str());
            ordered.push(port);
        }
        Ok(ordered)
    }
}
