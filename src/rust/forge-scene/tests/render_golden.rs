//! Schema + template + data must reproduce the checked-in code exactly, and
//! the generated code must follow the construction rules.

use forge_scene::render::{load, render, RenderError};

const DOG_YAML: &str = include_str!("../scenes/dog.yaml");
const DOG_RS: &str = include_str!("../generated/dog.rs");

#[test]
fn dog_scene_renders_byte_for_byte() {
    let rendered = render(DOG_YAML).expect("dog scene renders");
    if rendered != DOG_RS {
        let line = rendered
            .lines()
            .zip(DOG_RS.lines())
            .position(|(a, b)| a != b)
            .map_or_else(|| "length".to_string(), |i| format!("line {}", i + 1));
        panic!(
            "generated/dog.rs is stale (first difference at {line}); run \
             `cargo run -p forge-scene -- render scenes/dog.yaml generated/dog.rs`"
        );
    }
}

#[test]
fn rendering_is_deterministic() {
    assert_eq!(render(DOG_YAML).unwrap(), render(DOG_YAML).unwrap());
}

/// Impl and decorator types may only be built inside a factory or decorator
/// impl. Providers, slots and the container are not instances of a port.
#[test]
fn instances_are_only_constructed_inside_factories() {
    let scene = load(DOG_YAML).unwrap();
    let mut guarded: Vec<String> = scene
        .instances
        .iter()
        .map(|i| forge_scene::scene::pascal(&i.name))
        .collect();
    guarded.extend(
        scene
            .decorators
            .iter()
            .map(|d| forge_scene::scene::pascal(&d.name)),
    );
    let mut inside_factory = false;
    for (index, line) in DOG_RS.lines().enumerate() {
        if line.starts_with("impl ") {
            inside_factory = line.contains("Factory<") || line.contains("Decorator<");
        }
        if line == "}" {
            inside_factory = false;
        }
        for ty in &guarded {
            for construction in [format!("Arc::new({ty})"), format!("Arc::new({ty} {{")] {
                if line.contains(&construction) && !inside_factory {
                    panic!("line {}: {ty} is constructed outside a factory", index + 1);
                }
            }
            let literal = format!(" {ty} {{");
            if line.contains(&literal)
                && !line.contains("struct")
                && !line.contains("impl")
                && !inside_factory
            {
                panic!("line {}: {ty} is constructed outside a factory", index + 1);
            }
        }
    }
}

#[test]
fn business_code_has_no_option_or_string_lookup() {
    let business = DOG_RS
        .split("// ---- Container")
        .next()
        .expect("generated file has a container section");
    assert!(
        !business.contains("Option<"),
        "ports and impls must not expose Option"
    );
    assert!(!business.contains("HashMap") && !business.contains("Any"));
}

fn rejects(yaml: &str) -> String {
    match load(yaml) {
        Err(error) => error.to_string(),
        Ok(_) => panic!("scene should be rejected"),
    }
}

#[test]
fn the_schema_rejects_unknown_fields_and_bad_names() {
    let unknown = DOG_YAML.replace("title: A dog", "colour: brown\ntitle: A dog");
    assert!(
        matches!(load(&unknown), Err(RenderError::Schema(_))),
        "{}",
        rejects(&unknown)
    );
    let lifecycle = DOG_YAML.replace("lifecycle: transient", "lifecycle: pooled");
    assert!(matches!(load(&lifecycle), Err(RenderError::Schema(_))));
    let name = DOG_YAML.replace("name: Kennel", "name: kennel");
    assert!(matches!(load(&name), Err(RenderError::Schema(_))));
}

#[test]
fn semantic_rules_catch_wiring_the_template_cannot_compile() {
    let unknown_port = DOG_YAML.replace("port: Kennel\n", "port: Cattery\n");
    assert!(rejects(&unknown_port).contains("unknown port"));
    let double_bind = DOG_YAML.replace("bind: [rex]", "bind: [rex, pretend_dog]");
    assert!(rejects(&double_bind).contains("twice"));
    let optional_without_default = DOG_YAML.replace("    default: untrained\n", "");
    assert!(rejects(&optional_without_default).contains("needs a port default"));
    let unanswered = DOG_YAML.replace(
        "      - { verb: settle, ask: [training.settle], otherwise: whines }\n",
        "",
    );
    assert!(rejects(&unanswered).contains("every verb"));
    let bound_assisted = DOG_YAML.replace("bind: [rex]", "bind: [rex, engraved_collar]");
    assert!(rejects(&bound_assisted).contains("assisted"));
}

#[test]
fn feature_files_parse_into_the_feature_schema() {
    use forge_scene::bdd::{check_voice, parse, FEATURE_SCHEMA};
    let schema: serde_json::Value = serde_json::from_str(FEATURE_SCHEMA).unwrap();
    for source in [
        include_str!("../features/dog.feature"),
        include_str!("../features/forge_core_providers.feature"),
        include_str!("../features/forge_jobs.feature"),
        include_str!("../features/forge_template_renderers.feature"),
    ] {
        let feature = parse(source).unwrap();
        forge_scene::json_schema::validate(&schema, &feature.to_json()).unwrap();
        assert!(check_voice(&feature).is_empty(), "{}", feature.title);
    }
}
