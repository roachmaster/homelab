use forge_template::{render, Field, Limits, Schema, Template, TemplateError, Value};
use std::collections::BTreeMap;

fn object(values: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Object(values.into_iter().map(|(k, v)| (k.into(), v)).collect())
}
fn shape(fields: impl IntoIterator<Item = (&'static str, Schema, bool)>) -> Schema {
    Schema::Object {
        fields: fields
            .into_iter()
            .map(|(k, schema, required)| (k.into(), Field { schema, required }))
            .collect(),
        allow_unknown: false,
    }
}
fn text(s: &str) -> Value {
    Value::Text(s.into())
}
fn named() -> Schema {
    shape([("name", Schema::Text, true)])
}

#[test]
fn html_escaping_is_explicit_and_unicode_text_is_preserved() {
    let value = object([("name", text("é & < > \" '"))]);
    assert_eq!(
        render("{{name}}|{{{name}}}|{{& name}}", &value, &named()).unwrap(),
        "é &amp; &lt; &gt; &quot; &#39;|é & < > \" '|é & < > \" '"
    );
}
#[test]
fn parsed_template_reuses_different_contexts_without_retained_state() {
    let template = Template::parse("Hello {{ name }}").unwrap();
    for name in ["Ada", "Grace"] {
        assert_eq!(
            template
                .render(&object([("name", text(name))]), &named())
                .unwrap(),
            format!("Hello {name}")
        );
    }
}
#[test]
fn dotted_paths_lists_current_scope_and_parent_lookup() {
    let schema = shape([
        ("title", Schema::Text, true),
        ("user", shape([("name", Schema::Text, true)]), true),
        (
            "items",
            Schema::List(Box::new(shape([("name", Schema::Text, true)]))),
            true,
        ),
        ("words", Schema::List(Box::new(Schema::Text)), true),
    ]);
    let value = object([
        ("title", text("T")),
        ("user", object([("name", text("Ada"))])),
        (
            "items",
            Value::List(vec![
                object([("name", text("a"))]),
                object([("name", text("b"))]),
            ]),
        ),
        ("words", Value::List(vec![text("x"), text("y")])),
    ]);
    assert_eq!(
        render(
            "{{user.name}} {{#items}}{{title}}:{{name}};{{/items}}{{#words}}{{.}}{{/words}}",
            &value,
            &schema
        )
        .unwrap(),
        "Ada T:a;T:b;xy"
    );
}
#[test]
fn inverted_empty_false_null_and_optional_absent() {
    let schema = shape([
        ("items", Schema::List(Box::new(Schema::Int)), true),
        ("flag", Schema::Bool, true),
        ("nothing", Schema::Null, true),
        ("maybe", Schema::Text, false),
    ]);
    let value = object([
        ("items", Value::List(vec![])),
        ("flag", Value::Bool(false)),
        ("nothing", Value::Null),
    ]);
    assert_eq!(render("{{^items}}empty{{/items}} {{^flag}}off{{/flag}} {{^nothing}}null{{/nothing}} {{^maybe}}absent{{/maybe}}:{{maybe}}", &value, &schema).unwrap(), "empty off null absent:");
}
#[test]
fn zero_and_empty_object_are_truthy_but_empty_text_is_not() {
    let schema = shape([
        ("n", Schema::Int, true),
        ("o", shape([]), true),
        ("s", Schema::Text, true),
    ]);
    let value = object([("n", Value::Int(0)), ("o", object([])), ("s", text(""))]);
    assert_eq!(
        render(
            "{{#n}}{{.}}{{/n}}{{#o}}object{{/o}}{{^s}}empty{{/s}}",
            &value,
            &schema
        )
        .unwrap(),
        "0objectempty"
    );
}
#[test]
fn scalar_root_and_non_object_sections_are_supported() {
    assert_eq!(
        render("{{#.}}{{.}}{{/.}}", &Value::Int(i64::MIN), &Schema::Int).unwrap(),
        i64::MIN.to_string()
    );
    assert_eq!(
        render("{{.}}", &Value::Bool(false), &Schema::Bool).unwrap(),
        "false"
    );
    assert_eq!(render("{{.}}", &Value::Null, &Schema::Null).unwrap(), "");
}
#[test]
fn optional_inner_declaration_shadows_outer_even_when_absent() {
    let schema = shape([
        ("name", Schema::Text, true),
        ("inner", shape([("name", Schema::Text, false)]), true),
    ]);
    let value = object([("name", text("outer")), ("inner", object([]))]);
    assert_eq!(
        render("{{#inner}}[{{name}}]{{/inner}}", &value, &schema).unwrap(),
        "[]"
    );
}
#[test]
fn comments_do_not_trim_surrounding_whitespace() {
    assert_eq!(
        render("a\n  {{! note }}\n雪", &Value::Null, &Schema::Null).unwrap(),
        "a\n  \n雪"
    );
}
#[test]
fn schema_checks_before_even_constant_output_and_reports_path_not_value() {
    let value = object([("name", Value::Int(42))]);
    assert_eq!(
        render("constant", &value, &named()),
        Err(TemplateError::Schema {
            path: "$.name".into(),
            reason: "expected text"
        })
    );
}
#[test]
fn required_unknown_and_wrong_list_items_are_rejected() {
    assert!(matches!(
        render("", &object([]), &named()),
        Err(TemplateError::Schema { .. })
    ));
    assert!(matches!(
        render(
            "",
            &object([("name", text("x")), ("extra", Value::Null)]),
            &named()
        ),
        Err(TemplateError::Schema { .. })
    ));
    assert_eq!(
        render(
            "",
            &Value::List(vec![Value::Int(1), text("wrong")]),
            &Schema::List(Box::new(Schema::Int))
        ),
        Err(TemplateError::Schema {
            path: "$[1]".into(),
            reason: "expected i64"
        })
    );
}
#[test]
fn optional_does_not_mean_nullable() {
    let schema = shape([("maybe", Schema::Text, false)]);
    assert!(matches!(
        render("{{maybe}}", &object([("maybe", Value::Null)]), &schema),
        Err(TemplateError::Schema { .. })
    ));
}
#[test]
fn unknown_allowed_data_still_cannot_be_referenced_without_schema() {
    let schema = Schema::Object {
        fields: BTreeMap::new(),
        allow_unknown: true,
    };
    let value = object([("extra", text("data"))]);
    assert_eq!(render("constant", &value, &schema).unwrap(), "constant");
    assert_eq!(
        render("{{extra}}", &value, &schema),
        Err(TemplateError::UnknownPath("extra".into()))
    );
}
#[test]
fn inactive_branches_still_validate_every_expression() {
    let schema = shape([
        ("show", Schema::Bool, true),
        ("items", Schema::List(Box::new(Schema::Text)), true),
    ]);
    let value = object([("show", Value::Bool(false)), ("items", Value::List(vec![]))]);
    for source in ["{{#show}}{{typo}}{{/show}}", "{{#items}}{{typo}}{{/items}}"] {
        assert_eq!(
            render(source, &value, &schema),
            Err(TemplateError::UnknownPath("typo".into()))
        );
    }
}
#[test]
fn composite_interpolation_is_rejected_even_in_inactive_branch() {
    let schema = shape([
        ("show", Schema::Bool, true),
        ("items", Schema::List(Box::new(Schema::Text)), true),
    ]);
    let value = object([("show", Value::Bool(false)), ("items", Value::List(vec![]))]);
    assert_eq!(
        render("{{#show}}{{items}}{{/show}}", &value, &schema),
        Err(TemplateError::CompositeInterpolation("items".into()))
    );
}
#[test]
fn malformed_syntax_and_unsupported_features_are_explicit_errors() {
    for source in [
        "{{",
        "{{{x}}",
        "{{}}",
        "{{#x}}",
        "{{/x}}",
        "{{#x}}{{/y}}",
        "{{a..b}}",
        "{{a b}}",
        "{{>file}}",
        "{{=x=}}",
        "{{é}}",
        "{{#é}}",
        "{{雪}}",
    ] {
        assert!(
            matches!(Template::parse(source), Err(TemplateError::Syntax { .. })),
            "{source}"
        );
    }
}
#[test]
fn unicode_prefixes_and_multibyte_offsets_do_not_panic() {
    assert_eq!(
        Template::parse("雪{{é}}"),
        Err(TemplateError::Syntax {
            offset: 3,
            reason: "expected ASCII dotted identifiers or '.'"
        })
    );
    for tag in ["é", "☃", "🦀", "\u{2003}", "#", "&", "/", "a.", ".a", "!"] {
        for prefix in ["", "雪", "{{!x}}"] {
            let _ = Template::parse(&format!("{prefix}{{{{{tag}}}}}"));
        }
    }
}
#[test]
fn invalid_schema_identifiers_fail_without_data_key_echo() {
    let schema = shape([("bad-name", Schema::Text, false)]);
    assert!(matches!(
        render("", &object([]), &schema),
        Err(TemplateError::Schema {
            reason: "schema fields must be ASCII identifiers",
            ..
        })
    ));
    let data = object([("\u{1b}[31m", text("secret"))]);
    let error = render("", &data, &shape([])).unwrap_err().to_string();
    assert!(!error.contains("secret") && !error.contains('\u{1b}'));
}
#[test]
fn source_depth_and_node_limits_are_enforced() {
    assert_eq!(
        Template::parse_with_limits(
            "雪",
            Limits {
                max_source_bytes: 2,
                ..Limits::default()
            }
        ),
        Err(TemplateError::Limit("source bytes"))
    );
    assert!(matches!(
        Template::parse_with_limits(
            "{{#a}}{{#a}}x{{/a}}{{/a}}",
            Limits {
                max_depth: 1,
                ..Limits::default()
            }
        ),
        Err(TemplateError::Limit("syntax depth"))
    ));
    assert_eq!(
        Template::parse_with_limits(
            "a{{!x}}b",
            Limits {
                max_nodes: 2,
                ..Limits::default()
            }
        ),
        Err(TemplateError::Limit("syntax nodes"))
    );
}
#[test]
fn data_depth_nodes_and_bytes_are_checked_even_if_unused() {
    let template = Template::parse_with_limits(
        "",
        Limits {
            max_depth: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        template.render(
            &Value::List(vec![Value::List(vec![Value::Null])]),
            &Schema::Null
        ),
        Err(TemplateError::Limit("data/schema depth"))
    );
    let template = Template::parse_with_limits(
        "",
        Limits {
            max_nodes: 2,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        template.render(&Value::List(vec![Value::Null, Value::Null]), &Schema::Null),
        Err(TemplateError::Limit("data/schema nodes"))
    );
    let template = Template::parse_with_limits(
        "",
        Limits {
            max_data_bytes: 3,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        template.render(&text("four"), &Schema::Text),
        Err(TemplateError::Limit("data/schema bytes"))
    );
}
#[test]
fn schema_depth_and_bytes_are_checked_even_for_empty_data() {
    let template = Template::parse_with_limits(
        "",
        Limits {
            max_depth: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        template.render(
            &Value::List(vec![]),
            &Schema::List(Box::new(Schema::List(Box::new(Schema::Null))))
        ),
        Err(TemplateError::Limit("data/schema depth"))
    );
    let template = Template::parse_with_limits(
        "",
        Limits {
            max_data_bytes: 2,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        template.render(&object([]), &shape([("long", Schema::Text, false)])),
        Err(TemplateError::Limit("data/schema bytes"))
    );
}
#[test]
fn escaped_output_budget_counts_expansion_and_utf8_bytes() {
    let template = Template::parse_with_limits(
        "{{.}}",
        Limits {
            max_output_bytes: 4,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        template.render(&text("&"), &Schema::Text),
        Err(TemplateError::Limit("output bytes"))
    );
    assert_eq!(template.render(&text("雪x"), &Schema::Text).unwrap(), "雪x");
    assert_eq!(
        template.render(&text("雪雪"), &Schema::Text),
        Err(TemplateError::Limit("output bytes"))
    );
    assert_eq!(
        Template::parse_with_limits(
            "12345",
            Limits {
                max_output_bytes: 4,
                ..Limits::default()
            }
        )
        .unwrap()
        .render(&Value::Null, &Schema::Null),
        Err(TemplateError::Limit("output bytes"))
    );
}
#[test]
fn work_limits_bound_empty_body_iteration_and_repeated_schema_matching() {
    let template = Template::parse_with_limits(
        "{{#.}}{{/.}}",
        Limits {
            max_steps: 12,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        template.render(
            &Value::List(vec![Value::Null; 10]),
            &Schema::List(Box::new(Schema::Null))
        ),
        Err(TemplateError::Limit("work steps"))
    );
    let schema = Schema::List(Box::new(shape([
        ("a", Schema::Text, false),
        ("b", Schema::Text, false),
        ("c", Schema::Text, false),
    ])));
    let template = Template::parse_with_limits(
        "",
        Limits {
            max_steps: 10,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        template.render(&Value::List(vec![object([]); 10]), &schema),
        Err(TemplateError::Limit("work steps"))
    );
}
#[test]
fn limit_configuration_cannot_disable_bounds_or_overflow() {
    for limits in [
        Limits {
            max_depth: 0,
            ..Limits::default()
        },
        Limits {
            max_depth: 65,
            ..Limits::default()
        },
        Limits {
            max_output_bytes: usize::MAX,
            ..Limits::default()
        },
        Limits {
            max_steps: 0,
            ..Limits::default()
        },
    ] {
        assert!(matches!(
            Template::parse_with_limits("", limits),
            Err(TemplateError::Configuration(_))
        ));
    }
}
#[test]
fn an_error_does_not_poison_or_return_partial_output() {
    let parsed = Template::parse("prefix {{name}}").unwrap();
    assert!(parsed.render(&object([]), &named()).is_err());
    assert_eq!(
        parsed
            .render(&object([("name", text("ok"))]), &named())
            .unwrap(),
        "prefix ok"
    );
}

#[test]
fn an_undeclared_complete_dotted_path_can_resolve_in_parent_schema() {
    let schema = shape([
        ("user", shape([("name", Schema::Text, true)]), true),
        (
            "inner",
            shape([("user", shape([("id", Schema::Int, true)]), true)]),
            true,
        ),
    ]);
    let value = object([
        ("user", object([("name", text("outer"))])),
        ("inner", object([("user", object([("id", Value::Int(7))]))])),
    ]);
    assert_eq!(
        render("{{#inner}}{{user.name}}{{/inner}}", &value, &schema).unwrap(),
        "outer"
    );
}
