use forge_template::{
    render, render_with_methods, Field, Limits, MethodRegistry, Schema, Template, TemplateError,
    Value,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
fn object(items: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Object(items.into_iter().map(|(k, v)| (k.into(), v)).collect())
}
fn shape(items: impl IntoIterator<Item = (&'static str, Schema, bool)>) -> Schema {
    Schema::Object {
        fields: items
            .into_iter()
            .map(|(k, schema, required)| (k.into(), Field { schema, required }))
            .collect(),
        allow_unknown: false,
    }
}
fn text(s: &str) -> Value {
    Value::Text(s.into())
}
fn failure(reason: &str) -> TemplateError {
    TemplateError::Method {
        name: "custom".into(),
        reason: reason.into(),
    }
}

#[test]
fn chains_support_literal_and_path_arguments_then_escape_once() {
    let schema = shape([
        ("name", Schema::Text, true),
        ("replacement", Schema::Text, true),
    ]);
    let value = object([("name", text(" <Ada> ")), ("replacement", text("Grace"))]);
    assert_eq!(
        render(
            "{{name.trim().replace('Ada', replacement).to_uppercase()}}",
            &value,
            &schema
        )
        .unwrap(),
        "&lt;GRACE&gt;"
    );
    assert_eq!(
        render(
            "{{{name.trim().replace('Ada', replacement)}}}",
            &value,
            &schema
        )
        .unwrap(),
        "<Grace>"
    );
}
#[test]
fn quoted_delimiters_parentheses_commas_and_escapes_are_literal_arguments() {
    assert_eq!(
        render(r#"{{.replace('x', 'a,}}(b)')}}"#, &text("x"), &Schema::Text).unwrap(),
        "a,}}(b)"
    );
    assert_eq!(
        render(
            r#"{{{.replace("x", "\"\\\n\t\r'")}}}"#,
            &text("x"),
            &Schema::Text
        )
        .unwrap(),
        "\"\\\n\t\r'"
    );
}
#[test]
fn comments_ignore_unbalanced_quote_characters() {
    for s in [
        "{{! don't panic }}ok",
        "{{! \"unfinished quote }}ok",
        "{{ ! ' }}ok",
    ] {
        assert_eq!(render(s, &Value::Null, &Schema::Null).unwrap(), "ok");
    }
}
#[test]
fn custom_methods_are_explicit_and_receive_evaluated_arguments() {
    let mut registry = MethodRegistry::new();
    registry
        .register("initials", |v| {
            let Value::Text(s) = v else {
                return Err(failure("expected text"));
            };
            Ok(text(
                &s.split_whitespace()
                    .filter_map(|w| w.chars().next())
                    .collect::<String>(),
            ))
        })
        .unwrap();
    registry
        .register_with_args("surround", |v, args| {
            let (Value::Text(s), [Value::Text(left), Value::Text(right)]) = (v, args) else {
                return Err(failure("text plus two text arguments"));
            };
            Ok(text(&format!("{left}{s}{right}")))
        })
        .unwrap();
    let schema = shape([("name", Schema::Text, true), ("left", Schema::Text, true)]);
    let data = object([("name", text("Ada Lovelace")), ("left", text("<"))]);
    assert_eq!(
        render_with_methods(
            "{{name.initials().surround(left, '>')}}",
            &data,
            &schema,
            &registry
        )
        .unwrap(),
        "&lt;AL&gt;"
    );
}
#[test]
fn duplicate_registration_preserves_original_and_default_builtins() {
    let mut registry = MethodRegistry::empty();
    registry.register("answer", |_| Ok(Value::Int(1))).unwrap();
    assert!(registry.register("answer", |_| Ok(Value::Int(2))).is_err());
    assert_eq!(
        render_with_methods("{{.answer()}}", &Value::Null, &Schema::Null, &registry).unwrap(),
        "1"
    );
    let mut registry = MethodRegistry::new();
    assert!(registry.register("trim", |_| Ok(text("override"))).is_err());
    assert_eq!(
        render_with_methods("{{.trim()}}", &text(" x "), &Schema::Text, &registry).unwrap(),
        "x"
    );
    for name in ["", "invalid-name", "é", "a.b"] {
        assert!(registry.register(name, |_| Ok(Value::Null)).is_err());
    }
}
#[test]
fn empty_registry_excludes_builtins_but_custom_same_name_has_custom_shape() {
    let mut registry = MethodRegistry::empty();
    assert!(render_with_methods("{{.trim()}}", &text("x"), &Schema::Text, &registry).is_err());
    registry
        .register("trim", |_| Ok(object([("answer", Value::Int(7))])))
        .unwrap();
    assert_eq!(
        render_with_methods(
            "{{#.trim()}}{{answer}}{{/.trim()}}",
            &Value::Null,
            &Schema::Null,
            &registry
        )
        .unwrap(),
        "7"
    );
}
#[test]
fn missing_optional_receiver_skips_chain_but_null_can_use_default() {
    let schema = shape([
        ("missing", Schema::Text, false),
        ("null", Schema::Null, true),
    ]);
    let data = object([("null", Value::Null)]);
    assert_eq!(
        render(
            "{{missing.default('fallback')}}|{{null.default('fallback')}}",
            &data,
            &schema
        )
        .unwrap(),
        "|fallback"
    );
}
#[test]
fn argument_path_is_checked_and_missing_runtime_argument_errors() {
    let schema = shape([("s", Schema::Text, true), ("maybe", Schema::Text, false)]);
    let data = object([("s", text("x"))]);
    assert!(matches!(
        render("{{s.replace('x', typo)}}", &data, &schema),
        Err(TemplateError::UnknownPath(_))
    ));
    assert!(matches!(
        render("{{s.replace('x', maybe)}}", &data, &schema),
        Err(TemplateError::Method { .. })
    ));
}
#[test]
fn all_known_expression_checks_precede_callbacks_even_in_skipped_branches() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut registry = MethodRegistry::new();
    registry
        .register("touch", move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(text("ran"))
        })
        .unwrap();
    let schema = shape([("name", Schema::Text, true), ("show", Schema::Bool, true)]);
    let data = object([("name", text("x")), ("show", Value::Bool(false))]);
    for source in [
        "{{name.touch()}}{{#show}}{{name.unknown()}}{{/show}}",
        "{{name.touch()}}{{#show}}{{name.add(1)}}{{/show}}",
        "{{name.touch()}}{{#show}}{{name.touch(1)}}{{/show}}",
    ] {
        assert!(render_with_methods(source, &data, &schema, &registry).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
#[test]
fn methods_can_drive_sections_and_parent_lookup_after_dynamic_results() {
    let schema = shape([
        ("name", Schema::Text, true),
        ("items", Schema::List(Box::new(Schema::Text)), true),
    ]);
    let value = object([
        ("name", text("outer")),
        ("items", Value::List(vec![text("a"), text("b")])),
    ]);
    assert_eq!(render("{{#items.first()}}{{name}}:{{.to_uppercase()}}{{/items.first()}}{{^items.is_empty()}}!{{/items.is_empty()}}",&value,&schema).unwrap(),"outer:A!");
    let mut registry = MethodRegistry::new();
    registry
        .register("records", |_| {
            Ok(Value::List(vec![object([("item", text("one"))])]))
        })
        .unwrap();
    assert_eq!(
        render_with_methods(
            "{{#items.records()}}{{name}}:{{item}}{{/items.records()}}",
            &value,
            &schema,
            &registry
        )
        .unwrap(),
        "outer:one"
    );
}
#[test]
fn dynamic_composites_fail_but_missing_dynamic_paths_follow_absent_semantics() {
    let mut registry = MethodRegistry::new();
    registry
        .register("map", |_| Ok(object([("x", Value::Int(1))])))
        .unwrap();
    assert!(matches!(
        render_with_methods("prefix {{.map()}}", &Value::Null, &Schema::Null, &registry),
        Err(TemplateError::CompositeInterpolation(_))
    ));
    assert_eq!(
        render_with_methods(
            "{{#.map()}}{{missing}}{{^missing}}absent{{/missing}}{{/.map()}}",
            &Value::Null,
            &Schema::Null,
            &registry
        )
        .unwrap(),
        "absent"
    );
}
#[test]
fn custom_errors_propagate_without_retry_or_partial_document() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mut registry = MethodRegistry::empty();
    registry
        .register("fail", move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Err(failure("explicit failure"))
        })
        .unwrap();
    assert_eq!(
        render_with_methods("prefix {{.fail()}}", &Value::Null, &Schema::Null, &registry),
        Err(failure("explicit failure"))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
#[test]
fn malformed_calls_are_typed_syntax_errors() {
    for s in [
        "{{trim()}}",
        "{{.trim(}}",
        "{{.trim().}}",
        "{{.trim()x}}",
        "{{.replace('x',)}}",
        "{{.replace(,'x')}}",
        "{{.replace(other.trim(),'x')}}",
        "{{.replace('x','\\q')}}",
        "{{.replace('x','unfinished)}}",
        "{{.add(NaN)}}",
        "{{.add(1e999)}}",
    ] {
        assert!(
            matches!(Template::parse(s), Err(TemplateError::Syntax { .. })),
            "{s}"
        );
    }
}
#[test]
fn method_calls_and_arguments_consume_syntax_budget() {
    assert!(matches!(
        Template::parse_with_limits(
            "{{.replace('x','y')}}",
            Limits {
                max_nodes: 3,
                ..Limits::default()
            }
        ),
        Err(TemplateError::Limit("syntax nodes"))
    ));
}
#[test]
fn nonfinite_input_and_method_results_are_rejected() {
    for n in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(render("{{.}}", &Value::Float(n), &Schema::Float).is_err());
        let mut registry = MethodRegistry::empty();
        registry
            .register("invalid", move |_| Ok(Value::Float(n)))
            .unwrap();
        assert!(
            render_with_methods("{{.invalid()}}", &Value::Null, &Schema::Null, &registry).is_err()
        );
    }
}
#[test]
fn callback_limits_validate_after_return_and_do_not_claim_preemption() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut registry = MethodRegistry::empty();
    registry
        .register("large", move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(text("123456789"))
        })
        .unwrap();
    let template = Template::parse_with_limits(
        "{{.large()}}",
        Limits {
            max_data_bytes: 8,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        template.render_with_methods(&Value::Null, &Schema::Null, &registry),
        Err(TemplateError::Limit(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
#[test]
fn builtin_expansion_and_chains_exhaust_budgets_without_partial_result() {
    let template = Template::parse_with_limits(
        "{{.replace('a','123456789')}}",
        Limits {
            max_data_bytes: 10,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        template.render(&text("aaa"), &Schema::Text),
        Err(TemplateError::Limit(_))
    ));
    let template = Template::parse_with_limits(
        "{{.trim().trim().trim()}}",
        Limits {
            max_steps: 20,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        template.render(&text("abcdefghijk"), &Schema::Text),
        Err(TemplateError::Limit("work steps"))
    ));
}
#[test]
fn callback_panics_propagate_not_translate_to_success() {
    let mut registry = MethodRegistry::empty();
    registry
        .register("panic", |_| panic!("trusted callback"))
        .unwrap();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render_with_methods(
            "{{.panic()}}",
            &Value::Null,
            &Schema::Null,
            &registry
        )))
        .is_err()
    );
}
