//! Flattens a validated scene into the template context.
//!
//! The template language has sections but no equality operator, so every
//! decision the template needs (which lifecycle, which slot is defaulted, which
//! environment is complete) is computed here as a boolean or a list.

use crate::scene::{pascal, snake, Instance, Lifecycle, Scene, SceneError, Stereotype};
use forge_template::Value;
use std::collections::{BTreeMap, BTreeSet};

struct Obj(BTreeMap<String, Value>);

impl Obj {
    fn new() -> Self {
        Self(BTreeMap::new())
    }
    fn text(mut self, key: &str, value: impl Into<String>) -> Self {
        self.0.insert(key.into(), Value::Text(value.into()));
        self
    }
    fn flag(mut self, key: &str, value: bool) -> Self {
        self.0.insert(key.into(), Value::Bool(value));
        self
    }
    fn list(mut self, key: &str, values: Vec<Value>) -> Self {
        self.0.insert(key.into(), Value::List(values));
        self
    }
    fn int(mut self, key: &str, value: usize) -> Self {
        self.0.insert(key.into(), Value::Int(value as i64));
        self
    }
    fn done(self) -> Value {
        Value::Object(self.0)
    }
}

/// A Rust string literal for arbitrary scene text.
fn literal(text: &str) -> String {
    format!("{text:?}")
}

fn lifecycle_flags(obj: Obj, lifecycle: Lifecycle) -> Obj {
    obj.flag("singleton", lifecycle == Lifecycle::Singleton)
        .flag("lazy", lifecycle == Lifecycle::LazySingleton)
        .flag("transient", lifecycle == Lifecycle::Transient)
}

fn verbs(scene: &Scene, port: &str) -> Vec<Value> {
    scene
        .port(port)
        .map(|port| {
            port.verbs
                .iter()
                .map(|verb| {
                    Obj::new()
                        .text("verb", &verb.name)
                        .text("request", &verb.request)
                        .text("response", &verb.response)
                        .done()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn probe(scene: &Scene, port: &str) -> (String, String) {
    let verb = &scene.port(port).expect("validated port").verbs[0];
    (verb.name.clone(), verb.request.clone())
}

fn deps(instance: &Instance) -> Vec<Value> {
    instance
        .depends_on
        .iter()
        .map(|dep| {
            Obj::new()
                .text("field", snake(&dep.port))
                .text("dep_port", &dep.port)
                .done()
        })
        .collect()
}

/// Build the complete context for `templates/scene.rs.forge`.
pub fn build(scene: &Scene) -> Result<Value, SceneError> {
    scene.validate()?;
    let scene_type = pascal(&scene.scene);
    let order = scene.dependency_order()?;

    let mut newtypes = Vec::new();
    let mut seen = BTreeSet::new();
    for port in &scene.ports {
        for verb in &port.verbs {
            for ty in [&verb.request, &verb.response] {
                if seen.insert(ty.clone()) {
                    newtypes.push(Obj::new().text("newtype", ty).done());
                }
            }
        }
    }

    let ports = scene
        .ports
        .iter()
        .map(|port| {
            Obj::new()
                .text("port_name", &port.name)
                .text("port_snake", snake(&port.name))
                .list("verbs", verbs(scene, &port.name))
                .done()
        })
        .collect();

    let mut leaves = Vec::new();
    let mut assisted = Vec::new();
    let mut composites = Vec::new();
    for instance in &scene.instances {
        let type_name = pascal(&instance.name);
        let (probe_verb, probe_request) = probe(scene, &instance.port);
        let base = Obj::new()
            .text("inst_name", &instance.name)
            .text("type_name", &type_name)
            .text("port", &instance.port)
            .text("probe_verb", probe_verb)
            .text("probe_request", probe_request)
            .list("verbs", verbs(scene, &instance.port));
        if let Some(arg) = &instance.assisted_arg {
            assisted.push(base.text("arg", arg).done());
        } else if instance.depends_on.is_empty() {
            let says = instance.says.clone().unwrap_or_default();
            leaves.push(
                lifecycle_flags(base, instance.lifecycle)
                    .text("says_lit", literal(&says))
                    .done(),
            );
        } else {
            let port = scene.port(&instance.port).expect("validated port");
            let answers = instance
                .answers
                .iter()
                .map(|answer| {
                    let verb = port
                        .verbs
                        .iter()
                        .find(|v| v.name == answer.verb)
                        .expect("validated");
                    let asks = answer
                        .ask
                        .iter()
                        .map(|ask| {
                            let (field, target_verb) = ask.split_once('.').expect("validated");
                            let dep = instance
                                .depends_on
                                .iter()
                                .find(|dep| snake(&dep.port) == field)
                                .expect("validated");
                            let target = scene.port(&dep.port).expect("validated");
                            let request = &target
                                .verbs
                                .iter()
                                .find(|v| v.name == target_verb)
                                .expect("validated")
                                .request;
                            Obj::new()
                                .text("ask_field", field)
                                .text("ask_verb", target_verb)
                                .text("ask_request", request)
                                .done()
                        })
                        .collect();
                    Obj::new()
                        .text("verb", &verb.name)
                        .text("request", &verb.request)
                        .text("response", &verb.response)
                        .list("asks", asks)
                        .text("otherwise_lit", literal(&answer.otherwise))
                        .done()
                })
                .collect();
            composites.push(
                base.list("deps", deps(instance))
                    .list("answers", answers)
                    .done(),
            );
        }
    }

    let decorators: Vec<Value> = scene
        .decorators
        .iter()
        .map(|decorator| {
            Obj::new()
                .text("deco_name", &decorator.name)
                .text("deco_type", pascal(&decorator.name))
                .text("deco_port", &decorator.port)
                .list("verbs", verbs(scene, &decorator.port))
                .done()
        })
        .collect();

    let mut envs = Vec::new();
    let mut first_complete = None;
    for env in &scene.envs {
        let bound: BTreeMap<&str, &Instance> = env
            .bind
            .iter()
            .map(|name| {
                let instance = scene.instance(name).expect("validated");
                (instance.port.as_str(), instance)
            })
            .collect();
        let mut stages = Vec::new();
        let mut slots = Vec::new();
        let mut provided = BTreeSet::new();
        let mut complete = true;
        for port in &order {
            let chosen = bound
                .get(port.name.as_str())
                .copied()
                .map(|i| (i, false))
                .or_else(|| {
                    port.default
                        .as_ref()
                        .map(|name| (scene.instance(name).expect("validated"), true))
                });
            let port_snake = snake(&port.name);
            let slot = Obj::new()
                .text("slot", &port_snake)
                .text("slot_port", &port.name)
                .list("verbs", verbs(scene, &port.name));
            let Some((instance, defaulted)) = chosen else {
                slots.push(
                    lifecycle_flags(slot, Lifecycle::Assisted)
                        .flag("missing", true)
                        .flag("bound", false)
                        .flag("defaulted", false)
                        .flag("leaf", false)
                        .flag("shared", false)
                        .flag("fresh", false)
                        .text("type_name", "")
                        .list("deps", Vec::new())
                        .list("decorations", Vec::new())
                        .done(),
                );
                continue;
            };
            let required: Vec<String> = instance
                .depends_on
                .iter()
                .filter(|dep| !dep.optional)
                .map(|dep| snake(&dep.port))
                .collect();
            for requirement in &required {
                if !provided.contains(requirement) {
                    complete = false;
                }
            }
            provided.insert(port_snake.clone());
            let requires_lit = format!(
                "&[{}]",
                required
                    .iter()
                    .map(|r| literal(r))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            stages.push(
                Obj::new()
                    .text("binding", &port_snake)
                    .text("requires_lit", requires_lit)
                    .done(),
            );
            let decorations: Vec<Value> = env
                .decorate
                .iter()
                .filter_map(|name| scene.decorators.iter().find(|d| &d.name == name))
                .filter(|decorator| decorator.port == port.name)
                .map(|decorator| {
                    Obj::new()
                        .text("deco_name", &decorator.name)
                        .text("deco_type", pascal(&decorator.name))
                        .done()
                })
                .collect();
            let (probe_verb, probe_request) = probe(scene, &port.name);
            slots.push(
                lifecycle_flags(slot, instance.lifecycle)
                    .flag("missing", false)
                    .flag("bound", !defaulted)
                    .flag("defaulted", defaulted)
                    .flag("leaf", instance.depends_on.is_empty())
                    .flag("shared", instance.lifecycle != Lifecycle::Transient)
                    .flag("fresh", instance.lifecycle == Lifecycle::Transient)
                    .flag("double", instance.stereotype == Stereotype::TestDouble)
                    .text("type_name", pascal(&instance.name))
                    .text("probe_verb", probe_verb)
                    .text("probe_request", probe_request)
                    .list("deps", deps(instance))
                    .list("decorations", decorations)
                    .done(),
            );
        }
        if complete && first_complete.is_none() {
            first_complete = Some(pascal(&env.name));
        }
        envs.push(
            Obj::new()
                .text("env_name", &env.name)
                .text("variant", pascal(&env.name))
                .flag("complete", complete)
                .list("stages", stages)
                .list("slots", slots)
                .done(),
        );
    }
    let Some(first_complete) = first_complete else {
        return Err(SceneError(
            "at least one environment must be complete".into(),
        ));
    };

    Ok(Obj::new()
        .text("scene", &scene.scene)
        .text("title", &scene.title)
        .text("scene_type", &scene_type)
        .text("first_complete", first_complete)
        .int("env_count", scene.envs.len())
        .list("newtypes", newtypes)
        .list("ports", ports)
        .list("leaves", leaves)
        .list("assisted", assisted)
        .list("composites", composites)
        .list("decorators", decorators)
        .list("envs", envs)
        .done())
}
