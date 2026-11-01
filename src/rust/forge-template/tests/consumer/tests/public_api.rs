#![cfg(feature = "rendering")]
// Deliberately import only the dependency's public API. No example/private source inclusion.
use forge_bootstrap::{RegistrationError, Registry};
use forge_provider::{Factory, InstanceProvider, Singleton, Transient};
use forge_template::{
    assembly::{register_renderer, AssemblyError, RenderConfig},
    Field, Limits, MethodRegistry, Schema, TemplateError, Value,
};
use std::{
    collections::BTreeMap,
    error::Error,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

fn config(source: &str) -> RenderConfig {
    RenderConfig::Inline {
        source: source.into(),
        schema: Schema::Text,
        limits: Limits::default(),
    }
}
fn methods(calls: Arc<AtomicUsize>) -> MethodRegistry {
    let mut registry = MethodRegistry::new();
    registry
        .register("observed", move |value| {
            calls.fetch_add(1, Ordering::SeqCst);
            let Value::Text(text) = value else {
                return Err(TemplateError::Method {
                    name: "observed".into(),
                    reason: "text required".into(),
                });
            };
            Ok(Value::Text(format!("[{text}]")))
        })
        .unwrap();
    registry
}

#[test]
fn public_registrar_preserves_custom_methods_and_defers_callbacks() {
    let calls = Arc::new(AtomicUsize::new(0));
    let group = register_renderer(
        "templates.title",
        config("{{ .trim().observed().to_uppercase() }}"),
        methods(calls.clone()),
    )
    .unwrap();
    assert_eq!(group.info().id(), "templates.title");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let frozen = Registry::new().register(group).unwrap().freeze().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let ((), factory) = frozen.into_factories();
    let renderer = factory.create().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let provider = Singleton::new(renderer);
    let first = provider.acquire().unwrap();
    let second = provider.acquire().unwrap();
    assert!(std::ptr::eq(first, second));
    assert_eq!(
        first.render(&Value::Text(" forge ".into())).unwrap(),
        "[FORGE]"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn syntax_failure_waits_for_create_and_transient_acquisition() {
    let group =
        register_renderer("templates.bad", config("{{#open}}"), MethodRegistry::new()).unwrap();
    let frozen = Registry::new().register(group).unwrap().freeze().unwrap();
    let ((), factory) = frozen.into_factories();
    let provider = Transient::new(factory);
    assert!(matches!(
        provider.acquire(),
        Err(TemplateError::Syntax { .. })
    ));
    assert!(matches!(
        provider.acquire(),
        Err(TemplateError::Syntax { .. })
    ));
}

#[test]
fn transient_creations_are_distinct_but_capture_the_same_callback_state() {
    let calls = Arc::new(AtomicUsize::new(0));
    let group = register_renderer(
        "templates.shared-methods",
        config("{{ .observed() }}"),
        methods(calls.clone()),
    )
    .unwrap();
    let ((), factory) = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let provider = Transient::new(factory);
    let first = provider.acquire().unwrap();
    let second = provider.acquire().unwrap();
    assert!(!std::ptr::eq(&*first, &*second));
    assert_eq!(first.render(&Value::Text("a".into())).unwrap(), "[a]");
    assert_eq!(second.render(&Value::Text("b".into())).unwrap(), "[b]");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn duplicates_preserve_original_factory_and_return_incoming_group() {
    let first =
        register_renderer("templates.same", config("first"), MethodRegistry::new()).unwrap();
    let incoming =
        register_renderer("templates.same", config("second"), MethodRegistry::new()).unwrap();
    let original = Registry::new().register(first).unwrap();
    let rejected = original.register(incoming).unwrap_err();
    let (original, incoming, _) = rejected.into_parts();
    let ((), first_factory) = original.freeze().unwrap().into_factories();
    let ((), second_factory) = Registry::new()
        .register(incoming)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    assert_eq!(
        first_factory
            .create()
            .unwrap()
            .render(&Value::Text(String::new()))
            .unwrap(),
        "first"
    );
    assert_eq!(
        second_factory
            .create()
            .unwrap()
            .render(&Value::Text(String::new()))
            .unwrap(),
        "second"
    );
}

#[test]
fn distinct_ids_allow_multiple_configured_templates() {
    let a = register_renderer("templates.a", config("A"), MethodRegistry::new()).unwrap();
    let b = register_renderer("templates.b", config("B"), MethodRegistry::new()).unwrap();
    let (((), a), b) = Registry::new()
        .register(a)
        .unwrap()
        .register(b)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    assert_eq!(
        a.create()
            .unwrap()
            .render(&Value::Text(String::new()))
            .unwrap(),
        "A"
    );
    assert_eq!(
        b.create()
            .unwrap()
            .render(&Value::Text(String::new()))
            .unwrap(),
        "B"
    );
}

#[test]
fn startup_errors_are_typed_and_happen_without_callbacks() {
    let calls = Arc::new(AtomicUsize::new(0));
    let bad = RenderConfig::Embedded {
        source: "",
        schema: Schema::Text,
        limits: Limits {
            max_depth: 0,
            ..Limits::default()
        },
    };
    let error = register_renderer("templates.invalid", bad, methods(calls.clone())).unwrap_err();
    assert!(matches!(
        error,
        AssemblyError::Configuration(TemplateError::Configuration(_))
    ));
    assert!(error
        .source()
        .unwrap()
        .downcast_ref::<TemplateError>()
        .is_some());
    let bad = RenderConfig::Embedded {
        source: "too long",
        schema: Schema::Text,
        limits: Limits {
            max_source_bytes: 1,
            ..Limits::default()
        },
    };
    assert!(matches!(
        register_renderer("templates.invalid", bad, MethodRegistry::new()),
        Err(AssemblyError::Configuration(TemplateError::Limit(
            "source bytes"
        )))
    ));
    let bad = RenderConfig::Embedded {
        source: "",
        schema: Schema::Object {
            fields: BTreeMap::from([(
                "bad-key".into(),
                Field {
                    schema: Schema::Text,
                    required: true,
                },
            )]),
            allow_unknown: false,
        },
        limits: Limits::default(),
    };
    assert!(matches!(
        register_renderer("templates.invalid", bad, MethodRegistry::new()),
        Err(AssemblyError::Configuration(TemplateError::Schema { .. }))
    ));
    assert!(matches!(
        register_renderer("bad id", config("ok"), MethodRegistry::new()),
        Err(AssemblyError::Registration(
            RegistrationError::InvalidIdentifier("group")
        ))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn runtime_schema_and_method_errors_propagate_without_hidden_retry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = methods(calls.clone());
    registry
        .register("fail", |_| {
            Err(TemplateError::Method {
                name: "fail".into(),
                reason: "original cause".into(),
            })
        })
        .unwrap();
    let group = register_renderer(
        "templates.runtime",
        config("{{ .observed().fail() }}"),
        registry,
    )
    .unwrap();
    let ((), factory) = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let renderer = factory.create().unwrap();
    assert!(matches!(
        renderer.render(&Value::Int(1)),
        Err(TemplateError::Schema { .. })
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        renderer.render(&Value::Text("hello".into())),
        Err(TemplateError::Method {
            name: "fail".into(),
            reason: "original cause".into()
        })
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn configured_limits_and_empty_registry_are_not_replaced_by_defaults() {
    let mut bounded = config("{{ .to_uppercase() }}");
    if let RenderConfig::Inline { limits, .. } = &mut bounded {
        limits.max_output_bytes = 1;
    }
    let group = register_renderer("templates.limited", bounded, MethodRegistry::new()).unwrap();
    let ((), factory) = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    assert!(matches!(
        factory
            .create()
            .unwrap()
            .render(&Value::Text("long".into())),
        Err(TemplateError::Limit("output bytes"))
    ));
    let group = register_renderer(
        "templates.empty",
        config("{{ .to_uppercase() }}"),
        MethodRegistry::empty(),
    )
    .unwrap();
    let ((), factory) = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    assert!(matches!(
        factory.create().unwrap().render(&Value::Text("x".into())),
        Err(TemplateError::Method { .. })
    ));
}

#[test]
fn created_renderer_retains_owned_configuration_after_factory_drop() {
    let calls = Arc::new(AtomicUsize::new(0));
    let renderer = {
        let source = String::from("{{ .observed() }}");
        let group =
            register_renderer("templates.owned", config(&source), methods(calls.clone())).unwrap();
        let ((), factory) = Registry::new()
            .register(group)
            .unwrap()
            .freeze()
            .unwrap()
            .into_factories();
        factory.create().unwrap()
    };
    assert_eq!(
        renderer.render(&Value::Text("retained".into())).unwrap(),
        "[retained]"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
