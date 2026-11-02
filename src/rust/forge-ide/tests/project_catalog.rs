use forge_bootstrap::Registry;
use forge_ide::assembly::{register_projects, IdeConfig, ProjectAlias};
use forge_ide::task::InspectRequest;
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_task::Task;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "forge-project-catalog-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn alias(s: &str) -> ProjectAlias {
    ProjectAlias::new(s).unwrap()
}

#[test]
fn runtime_length_collection_is_one_typed_group_then_explicit_construction() {
    let t = Temp::new();
    let mut configs = vec![];
    for i in 0..3 {
        let path = t.0.join(format!("p{i}"));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("lib.rs"), "struct S;").unwrap();
        configs.push((alias(&format!("project-{i}")), IdeConfig::project(path)));
    }
    let group = register_projects("projects", configs).unwrap();
    let registry = Registry::new().register(group).unwrap();
    assert_eq!(registry.group_count(), 1);
    assert_eq!(registry.binding_count(), 1);
    let ((), projects) = registry.freeze().unwrap().into_factories();
    assert_eq!(projects.factories().len(), 3);
    for (name, factory) in projects.into_factories() {
        assert!(name.as_str().starts_with("project-"));
        let provider = Singleton::new(factory.create().unwrap());
        let report = provider
            .acquire()
            .unwrap()
            .run(InspectRequest::Project)
            .unwrap();
        assert_eq!(report.files, 1);
    }
}
#[test]
fn catalog_registration_is_effect_free_and_config_failure_does_not_return_partial_group() {
    let t = Temp::new();
    let missing = t.0.join("missing");
    let group = register_projects(
        "projects",
        vec![(alias("missing"), IdeConfig::project(missing.clone()))],
    )
    .unwrap();
    assert!(!missing.exists());
    let ((), projects) = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    assert!(projects.factories()[0].1.create().is_err());
    assert!(register_projects(
        "invalid",
        vec![
            (alias("valid"), IdeConfig::project(missing.clone())),
            (alias("bad"), IdeConfig::project(PathBuf::from("relative"))),
        ]
    )
    .is_err());
    assert!(!missing.exists());
}
#[test]
fn duplicates_fail_without_replacing_registered_catalog() {
    let t = Temp::new();
    assert!(register_projects(
        "duplicate",
        vec![
            (alias("same"), IdeConfig::project(t.0.clone())),
            (alias("same"), IdeConfig::project(t.0.join("missing"))),
        ]
    )
    .is_err());
    let registry = Registry::new()
        .register(
            register_projects(
                "projects",
                vec![(alias("original"), IdeConfig::project(t.0.clone()))],
            )
            .unwrap(),
        )
        .unwrap();
    let rejected = registry
        .register(
            register_projects(
                "projects",
                vec![(
                    alias("replacement"),
                    IdeConfig::project(t.0.join("missing")),
                )],
            )
            .unwrap(),
        )
        .unwrap_err();
    let (original, _, _) = rejected.into_parts();
    let ((), projects) = original.freeze().unwrap().into_factories();
    assert_eq!(projects.factories()[0].0.as_str(), "original");
    assert!(projects.factories()[0].1.create().is_ok());
}
#[test]
fn alias_catalog_limits_and_explicit_empty_catalog() {
    for name in [
        "",
        "..",
        ".",
        "../project",
        "/etc",
        "a/b",
        "x\\y",
        "with space",
        "é",
    ] {
        assert!(ProjectAlias::new(name).is_err(), "{name}");
    }
    assert!(ProjectAlias::new("a".repeat(129)).is_err());
    assert!(ProjectAlias::new("project_a.1-x").is_ok());
    let projects = (0..129)
        .map(|i| {
            (
                alias(&format!("p{i}")),
                IdeConfig::project(PathBuf::from("/not-opened")),
            )
        })
        .collect();
    assert!(register_projects("too-many", projects).is_err());
    let ((), empty) = Registry::new()
        .register(register_projects("empty", vec![]).unwrap())
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    assert!(empty.factories().is_empty());
}
