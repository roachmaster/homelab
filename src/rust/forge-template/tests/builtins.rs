use forge_template::{
    render, render_with_methods, Field, Limits, MethodRegistry, Schema, Template, TemplateError,
    Value,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

fn text(value: &str) -> Value {
    Value::Text(value.into())
}

fn object(entries: &[(&str, Value)]) -> Value {
    Value::Object(
        entries
            .iter()
            .map(|(name, value)| ((*name).into(), value.clone()))
            .collect(),
    )
}

// A custom method supplies a typed, structurally checked intermediate value. This
// lets the tests observe heterogeneous builtin results without inventing an Any
// schema or adding a public evaluation API just for tests.
fn evaluate(input: Value, expression: &str) -> Result<Value, TemplateError> {
    let captured = Arc::new(Mutex::new(None));
    let capture = captured.clone();
    let mut methods = MethodRegistry::new();
    methods.register("fixture", move |_| Ok(input.clone()))?;
    methods.register("capture", move |value| {
        *capture.lock().unwrap() = Some(value.clone());
        Ok(Value::Null)
    })?;
    let source = format!("{{{{.fixture().{expression}.capture()}}}}");
    Template::parse(&source)?.render_with_methods(&Value::Null, &Schema::Null, &methods)?;
    let result = captured.lock().unwrap().take().unwrap();
    Ok(result)
}

struct Case {
    method: &'static str,
    arguments: &'static str,
    arity: usize,
    input: Value,
    expected: Value,
}

fn cases() -> Vec<Case> {
    let list = Value::List(vec![Value::Int(4), Value::Int(9)]);
    let map = object(&[("b", Value::Int(2)), ("a", Value::Int(1))]);
    vec![
        Case {
            method: "snake_case",
            arguments: "",
            arity: 0,
            input: text("HTTPServer xmlReader"),
            expected: text("http_server_xml_reader"),
        },
        Case {
            method: "pascal_case",
            arguments: "",
            arity: 0,
            input: text("hello_world"),
            expected: text("HelloWorld"),
        },
        Case {
            method: "camel_case",
            arguments: "",
            arity: 0,
            input: text("HELLO_world"),
            expected: text("helloWorld"),
        },
        Case {
            method: "kebab_case",
            arguments: "",
            arity: 0,
            input: text("HTTPServer"),
            expected: text("http-server"),
        },
        Case {
            method: "upper_snake_case",
            arguments: "",
            arity: 0,
            input: text("helloHTTPServer"),
            expected: text("HELLO_HTTP_SERVER"),
        },
        Case {
            method: "to_SCREAMING_SNAKE_CASE",
            arguments: "",
            arity: 0,
            input: text("helloHTTPServer"),
            expected: text("HELLO_HTTP_SERVER"),
        },
        Case {
            method: "to_uppercase",
            arguments: "",
            arity: 0,
            input: text("Straße"),
            expected: text("STRASSE"),
        },
        Case {
            method: "to_lowercase",
            arguments: "",
            arity: 0,
            input: text("ΟΣ"),
            expected: text("ος"),
        },
        Case {
            method: "trim",
            arguments: "",
            arity: 0,
            input: text("\u{2003}\tAda \n"),
            expected: text("Ada"),
        },
        Case {
            method: "length",
            arguments: "",
            arity: 0,
            input: text("é🙂"),
            expected: Value::Int(2),
        },
        Case {
            method: "is_empty",
            arguments: "",
            arity: 0,
            input: Value::Null,
            expected: Value::Bool(true),
        },
        Case {
            method: "is_null",
            arguments: "",
            arity: 0,
            input: Value::Null,
            expected: Value::Bool(true),
        },
        Case {
            method: "to_string",
            arguments: "",
            arity: 0,
            input: Value::List(vec![text("x"), Value::Null]),
            expected: text("[x, null]"),
        },
        Case {
            method: "type",
            arguments: "",
            arity: 0,
            input: Value::Float(2.5),
            expected: text("number"),
        },
        Case {
            method: "default",
            arguments: "\"fallback\"",
            arity: 1,
            input: Value::Null,
            expected: text("fallback"),
        },
        Case {
            method: "contains",
            arguments: "\"red\"",
            arity: 1,
            input: text("infrared"),
            expected: Value::Bool(true),
        },
        Case {
            method: "starts_with",
            arguments: "\"in\"",
            arity: 1,
            input: text("infrared"),
            expected: Value::Bool(true),
        },
        Case {
            method: "ends_with",
            arguments: "\"red\"",
            arity: 1,
            input: text("infrared"),
            expected: Value::Bool(true),
        },
        Case {
            method: "replace",
            arguments: "\"aa\", \"X\"",
            arity: 2,
            input: text("baaa"),
            expected: text("bXa"),
        },
        Case {
            method: "join",
            arguments: "\"|\"",
            arity: 1,
            input: Value::List(vec![text("a"), Value::Int(3), Value::Null]),
            expected: text("a|3|null"),
        },
        Case {
            method: "first",
            arguments: "",
            arity: 0,
            input: list.clone(),
            expected: Value::Int(4),
        },
        Case {
            method: "last",
            arguments: "",
            arity: 0,
            input: list.clone(),
            expected: Value::Int(9),
        },
        Case {
            method: "at",
            arguments: "1",
            arity: 1,
            input: list,
            expected: Value::Int(9),
        },
        Case {
            method: "get",
            arguments: "\"a\"",
            arity: 1,
            input: map.clone(),
            expected: Value::Int(1),
        },
        Case {
            method: "has",
            arguments: "\"a\"",
            arity: 1,
            input: map.clone(),
            expected: Value::Bool(true),
        },
        Case {
            method: "keys",
            arguments: "",
            arity: 0,
            input: map.clone(),
            expected: Value::List(vec![text("a"), text("b")]),
        },
        Case {
            method: "values",
            arguments: "",
            arity: 0,
            input: map,
            expected: Value::List(vec![Value::Int(1), Value::Int(2)]),
        },
        Case {
            method: "not",
            arguments: "",
            arity: 0,
            input: Value::Bool(true),
            expected: Value::Bool(false),
        },
        Case {
            method: "abs",
            arguments: "",
            arity: 0,
            input: Value::Int(-4),
            expected: Value::Int(4),
        },
        Case {
            method: "round",
            arguments: "",
            arity: 0,
            input: Value::Float(-1.5),
            expected: Value::Float(-2.0),
        },
        Case {
            method: "floor",
            arguments: "",
            arity: 0,
            input: Value::Float(1.9),
            expected: Value::Float(1.0),
        },
        Case {
            method: "ceil",
            arguments: "",
            arity: 0,
            input: Value::Float(1.1),
            expected: Value::Float(2.0),
        },
        Case {
            method: "add",
            arguments: "2",
            arity: 1,
            input: Value::Int(5),
            expected: Value::Int(7),
        },
        Case {
            method: "subtract",
            arguments: "2",
            arity: 1,
            input: Value::Int(5),
            expected: Value::Int(3),
        },
        Case {
            method: "multiply",
            arguments: "2",
            arity: 1,
            input: Value::Int(5),
            expected: Value::Int(10),
        },
        Case {
            method: "divide",
            arguments: "2",
            arity: 1,
            input: Value::Int(5),
            expected: Value::Float(2.5),
        },
        Case {
            method: "to_int",
            arguments: "",
            arity: 0,
            input: text("42"),
            expected: Value::Int(42),
        },
        Case {
            method: "to_float",
            arguments: "",
            arity: 0,
            input: text("2.5"),
            expected: Value::Float(2.5),
        },
    ]
}

fn assert_method_error(error: TemplateError, method: &str) {
    assert!(
        matches!(error, TemplateError::Method { ref name, .. } if name == method),
        "expected {method} method error, got {error:?}"
    );
}

#[test]
fn all_38_registered_builtins_have_typed_happy_path_coverage() {
    let methods = MethodRegistry::new();
    let registered: BTreeSet<_> = methods.names().collect();
    let cases = cases();
    let exercised: BTreeSet<_> = cases.iter().map(|case| case.method).collect();
    assert_eq!(registered.len(), 38);
    assert_eq!(exercised, registered);
    for case in cases {
        assert_eq!(
            evaluate(case.input, &format!("{}({})", case.method, case.arguments)).unwrap(),
            case.expected,
            "{}",
            case.method
        );
    }
}

#[test]
fn every_builtin_rejects_too_many_and_where_applicable_too_few_arguments() {
    for case in cases() {
        let too_many = std::iter::repeat_n("null", case.arity + 1)
            .collect::<Vec<_>>()
            .join(",");
        assert_method_error(
            evaluate(case.input.clone(), &format!("{}({too_many})", case.method)).unwrap_err(),
            case.method,
        );
        if case.arity != 0 {
            assert_method_error(
                evaluate(case.input, &format!("{}()", case.method)).unwrap_err(),
                case.method,
            );
        }
    }
}

#[test]
fn every_receiver_restricted_builtin_rejects_incompatible_values() {
    let unrestricted = ["is_empty", "is_null", "to_string", "type", "default"];
    for case in cases() {
        if unrestricted.contains(&case.method) {
            continue;
        }
        let input = if case.method == "not" {
            Value::Int(1)
        } else {
            Value::Bool(true)
        };
        assert_method_error(
            evaluate(input, &format!("{}({})", case.method, case.arguments)).unwrap_err(),
            case.method,
        );
    }
}

#[test]
fn arguments_are_checked_without_coercing_text_indexes_or_numbers() {
    let cases = [
        (text("abc"), "contains", "3"),
        (object(&[]), "contains", "true"),
        (text("abc"), "starts_with", "3"),
        (text("abc"), "ends_with", "false"),
        (text("abc"), "replace", "false, \"x\""),
        (text("abc"), "replace", "\"a\", 3"),
        (Value::List(vec![]), "join", "3"),
        (Value::List(vec![]), "at", "1.0"),
        (Value::List(vec![]), "at", "\"1\""),
        (Value::List(vec![]), "at", "-1"),
        (object(&[]), "get", "1"),
        (object(&[]), "has", "null"),
        (Value::Int(2), "add", "true"),
        (Value::Int(2), "subtract", "\"1\""),
        (Value::Int(2), "multiply", "null"),
        (Value::Int(2), "divide", "\"2\""),
    ];
    for (input, method, arguments) in cases {
        assert_method_error(
            evaluate(input, &format!("{method}({arguments})")).unwrap_err(),
            method,
        );
    }
}

#[test]
fn unicode_words_acronyms_digits_and_case_aliases_follow_the_source_contract() {
    let input = text("XMLHttpRequest foo2BAR déjàVu Straße");
    for (method, expected) in [
        ("snake_case", "xml_http_request_foo2bar_déjà_vu_straße"),
        ("kebab_case", "xml-http-request-foo2bar-déjà-vu-straße"),
        ("pascal_case", "XmlHttpRequestFoo2barDéjàVuStraße"),
        ("camel_case", "xmlHttpRequestFoo2barDéjàVuStraße"),
        (
            "upper_snake_case",
            "XML_HTTP_REQUEST_FOO2BAR_DÉJÀ_VU_STRASSE",
        ),
        (
            "to_SCREAMING_SNAKE_CASE",
            "XML_HTTP_REQUEST_FOO2BAR_DÉJÀ_VU_STRASSE",
        ),
    ] {
        assert_eq!(
            evaluate(input.clone(), &format!("{method}()")).unwrap(),
            text(expected)
        );
        assert_eq!(
            evaluate(text(" _-! "), &format!("{method}()")).unwrap(),
            text("")
        );
    }
    assert_eq!(
        evaluate(text("İ"), "snake_case()").unwrap(),
        text("i\u{307}")
    );
    assert_eq!(
        evaluate(text("İ"), "pascal_case()").unwrap(),
        text("I\u{307}")
    );
    assert_eq!(evaluate(text("ΟΣ"), "to_lowercase()").unwrap(), text("ος"));
    assert_eq!(
        evaluate(text("e\u{301}"), "length()").unwrap(),
        Value::Int(2)
    );
}

#[test]
fn emptiness_null_and_default_distinguish_zero_false_and_empty_objects() {
    for falsey in [
        Value::Null,
        Value::Bool(false),
        text(""),
        Value::List(vec![]),
    ] {
        assert_eq!(
            evaluate(falsey, "default(\"fallback\")").unwrap(),
            text("fallback")
        );
    }
    for truthy in [
        Value::Bool(true),
        Value::Int(0),
        Value::Float(0.0),
        object(&[]),
        text(" "),
    ] {
        assert_eq!(
            evaluate(truthy.clone(), "default(\"fallback\")").unwrap(),
            truthy
        );
    }
    for empty in [Value::Null, text(""), Value::List(vec![]), object(&[])] {
        assert_eq!(evaluate(empty, "is_empty()").unwrap(), Value::Bool(true));
    }
    for nonempty in [
        Value::Bool(false),
        Value::Int(0),
        Value::Float(0.0),
        text(" "),
    ] {
        assert_eq!(
            evaluate(nonempty.clone(), "is_empty()").unwrap(),
            Value::Bool(false)
        );
        assert_eq!(evaluate(nonempty, "is_null()").unwrap(), Value::Bool(false));
    }
    assert_eq!(evaluate(Value::Null, "length()").unwrap(), Value::Int(0));
    assert_eq!(
        evaluate(object(&[("x", Value::Null)]), "length()").unwrap(),
        Value::Int(1)
    );
    assert_eq!(
        evaluate(Value::List(vec![Value::Null; 2]), "length()").unwrap(),
        Value::Int(2)
    );
}

#[test]
fn type_names_and_collection_text_are_not_json_serialization() {
    for (value, expected) in [
        (Value::Null, "null"),
        (Value::Bool(true), "boolean"),
        (Value::Int(1), "integer"),
        (Value::Float(1.0), "number"),
        (text("a"), "string"),
        (Value::List(vec![]), "array"),
        (object(&[]), "object"),
    ] {
        assert_eq!(evaluate(value, "type()").unwrap(), text(expected));
    }
    let nested = object(&[
        (
            "z",
            Value::List(vec![text("unquoted"), Value::Null, Value::Bool(true)]),
        ),
        ("a", object(&[("n", Value::Float(1.5))])),
    ]);
    assert_eq!(
        evaluate(nested, "to_string()").unwrap(),
        text("{a: {n: 1.5}, z: [unquoted, null, true]}")
    );
    assert_eq!(evaluate(Value::Null, "to_string()").unwrap(), text("null"));
    assert_eq!(
        render("{{.to_string()}}", &text("<&>"), &Schema::Text).unwrap(),
        "&lt;&amp;&gt;"
    );
    assert_eq!(
        render("{{{.to_string()}}}", &text("<&>"), &Schema::Text).unwrap(),
        "<&>"
    );
}

#[test]
fn absent_collection_entries_are_null_and_object_enumeration_is_sorted() {
    for expression in ["first()", "last()", "at(0)"] {
        assert_eq!(
            evaluate(Value::List(vec![]), expression).unwrap(),
            Value::Null
        );
    }
    let largest_index = evaluate(Value::List(vec![]), "at(9223372036854775807)");
    if usize::try_from(i64::MAX).is_ok() {
        assert_eq!(largest_index.unwrap(), Value::Null);
    } else {
        assert_method_error(largest_index.unwrap_err(), "at");
    }
    let data = object(&[("z", Value::Int(2)), ("a", Value::Int(1))]);
    assert_eq!(
        evaluate(data.clone(), "get(\"missing\")").unwrap(),
        Value::Null
    );
    assert_eq!(
        evaluate(data.clone(), "has(\"missing\")").unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        evaluate(data.clone(), "contains(\"a\")").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        evaluate(data.clone(), "contains(\"b\")").unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        evaluate(data.clone(), "keys().join(\",\")").unwrap(),
        text("a,z")
    );
    assert_eq!(evaluate(data, "values().join(\",\")").unwrap(), text("1,2"));
    assert_eq!(
        evaluate(object(&[]), "keys()").unwrap(),
        Value::List(vec![])
    );
    assert_eq!(
        evaluate(object(&[]), "values()").unwrap(),
        Value::List(vec![])
    );
}

#[test]
fn containment_uses_typed_structural_equality_and_accepts_path_arguments() {
    assert_eq!(
        evaluate(Value::List(vec![Value::Int(1)]), "contains(1)").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        evaluate(Value::List(vec![Value::Int(1)]), "contains(1.0)").unwrap(),
        Value::Bool(false)
    );
    let nested = object(&[
        ("label", text("x")),
        ("numbers", Value::List(vec![Value::Int(1), Value::Int(2)])),
    ]);
    let shape = Schema::Object {
        fields: BTreeMap::from([
            (
                "label".into(),
                Field {
                    schema: Schema::Text,
                    required: true,
                },
            ),
            (
                "numbers".into(),
                Field {
                    schema: Schema::List(Box::new(Schema::Int)),
                    required: true,
                },
            ),
        ]),
        allow_unknown: false,
    };
    let context = object(&[
        ("items", Value::List(vec![nested.clone()])),
        ("needle", nested),
    ]);
    let schema = Schema::Object {
        fields: BTreeMap::from([
            (
                "items".into(),
                Field {
                    schema: Schema::List(Box::new(shape.clone())),
                    required: true,
                },
            ),
            (
                "needle".into(),
                Field {
                    schema: shape,
                    required: true,
                },
            ),
        ]),
        allow_unknown: false,
    };
    assert_eq!(
        render("{{items.contains(needle)}}", &context, &schema).unwrap(),
        "true"
    );
}

#[test]
fn replace_and_join_handle_empty_unicode_and_nested_values() {
    assert_eq!(
        evaluate(text("é🙂"), "replace(\"\", \"-\")").unwrap(),
        text("-é-🙂-")
    );
    assert_eq!(
        evaluate(text(""), "replace(\"\", \"x\")").unwrap(),
        text("x")
    );
    assert_eq!(
        evaluate(text("aaaa"), "replace(\"aa\", \"x\")").unwrap(),
        text("xx")
    );
    assert_eq!(
        evaluate(text("abc"), "replace(\"none\", \"x\")").unwrap(),
        text("abc")
    );
    assert_eq!(
        evaluate(text("abc"), "contains(\"\")").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        evaluate(text("abc"), "starts_with(\"abcd\")").unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        evaluate(text("abc"), "ends_with(\"z\")").unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        evaluate(Value::List(vec![]), "join(\",\")").unwrap(),
        text("")
    );
    assert_eq!(
        evaluate(
            Value::List(vec![
                Value::List(vec![Value::Int(1)]),
                object(&[("a", Value::Bool(false))])
            ]),
            "join(\"|\")"
        )
        .unwrap(),
        text("[1]|{a: false}")
    );
}

#[test]
fn integer_overflows_and_division_by_positive_or_negative_zero_are_errors() {
    for (input, expression, method) in [
        (Value::Int(i64::MIN), "abs()", "abs"),
        (Value::Int(i64::MAX), "add(1)", "add"),
        (Value::Int(i64::MIN), "subtract(1)", "subtract"),
        (Value::Int(i64::MAX), "multiply(2)", "multiply"),
        (Value::Int(5), "divide(0)", "divide"),
        (Value::Float(5.0), "divide(-0.0)", "divide"),
        (Value::Float(f64::MAX), "multiply(2.0)", "multiply"),
        (Value::Float(f64::MAX), "add(1.7976931348623157e308)", "add"),
    ] {
        assert_method_error(evaluate(input, expression).unwrap_err(), method);
    }
    assert_eq!(
        evaluate(Value::Int(i64::MAX), "add(0)").unwrap(),
        Value::Int(i64::MAX)
    );
    assert_eq!(
        evaluate(Value::Int(i64::MIN), "subtract(0)").unwrap(),
        Value::Int(i64::MIN)
    );
    assert_eq!(
        evaluate(Value::Int(-3), "multiply(-2)").unwrap(),
        Value::Int(6)
    );
}

#[test]
fn numeric_results_preserve_integer_vs_float_semantics() {
    for method in ["round", "floor", "ceil"] {
        assert_eq!(
            evaluate(Value::Int(i64::MAX), &format!("{method}()")).unwrap(),
            Value::Int(i64::MAX)
        );
    }
    assert_eq!(
        evaluate(Value::Float(-1.2), "floor()").unwrap(),
        Value::Float(-2.0)
    );
    assert_eq!(
        evaluate(Value::Float(-1.2), "ceil()").unwrap(),
        Value::Float(-1.0)
    );
    assert_eq!(
        evaluate(Value::Float(1.5), "round()").unwrap(),
        Value::Float(2.0)
    );
    assert_eq!(
        evaluate(Value::Float(-2.5), "abs()").unwrap(),
        Value::Float(2.5)
    );
    assert_eq!(
        evaluate(Value::Int(4), "divide(2)").unwrap(),
        Value::Float(2.0)
    );
    assert_eq!(
        evaluate(Value::Int(4), "add(0.5)").unwrap(),
        Value::Float(4.5)
    );
    assert_eq!(
        evaluate(Value::Float(4.5), "subtract(2)").unwrap(),
        Value::Float(2.5)
    );
    assert_eq!(
        evaluate(Value::Float(1.5), "multiply(2)").unwrap(),
        Value::Float(3.0)
    );
    assert_eq!(
        evaluate(Value::Bool(false), "not()").unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn numeric_conversions_truncate_saturate_and_reject_invalid_or_nonfinite_text() {
    for (input, expected) in [
        (Value::Float(-3.9), Value::Int(-3)),
        (Value::Float(3.9), Value::Int(3)),
        (Value::Float(f64::MAX), Value::Int(i64::MAX)),
        (Value::Float(-f64::MAX), Value::Int(i64::MIN)),
        (text("+12"), Value::Int(12)),
        (text("-9223372036854775808"), Value::Int(i64::MIN)),
        (Value::Int(12), Value::Int(12)),
    ] {
        assert_eq!(evaluate(input, "to_int()").unwrap(), expected);
    }
    for input in [" 12", "12 ", "1.5", "9223372036854775808", "NaN"] {
        assert_method_error(evaluate(text(input), "to_int()").unwrap_err(), "to_int");
    }
    for input in ["NaN", "inf", "-inf", "1e9999", "not a number"] {
        assert_method_error(evaluate(text(input), "to_float()").unwrap_err(), "to_float");
    }
    assert_eq!(
        evaluate(text("1.25e2"), "to_float()").unwrap(),
        Value::Float(125.0)
    );
    assert_eq!(
        evaluate(Value::Int(7), "to_float()").unwrap(),
        Value::Float(7.0)
    );
    assert_eq!(
        evaluate(Value::Float(7.5), "to_float()").unwrap(),
        Value::Float(7.5)
    );
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(render("{{.abs()}}", &Value::Float(value), &Schema::Float).is_err());
        assert!(evaluate(Value::Float(value), "to_float()").is_err());
    }
}

#[test]
fn chained_builtins_use_both_default_and_explicit_registry_entry_points() {
    let methods = MethodRegistry::new();
    assert_eq!(
        render_with_methods(
            "{{.trim().snake_case().to_uppercase()}}",
            &text(" HTTPServer "),
            &Schema::Text,
            &methods
        )
        .unwrap(),
        "HTTP_SERVER"
    );
    let parsed = Template::parse("{{.trim().to_uppercase()}}").unwrap();
    assert_eq!(
        parsed
            .render_with_methods(&text(" Ada "), &Schema::Text, &methods)
            .unwrap(),
        "ADA"
    );
    assert_eq!(
        parsed.render(&text(" Grace "), &Schema::Text).unwrap(),
        "GRACE"
    );
    assert_method_error(
        parsed
            .render_with_methods(&text(" Ada "), &Schema::Text, &MethodRegistry::empty())
            .unwrap_err(),
        "trim",
    );
}

#[test]
fn expansion_limits_are_checked_for_replace_join_formatting_and_unicode_case() {
    for (source, input, schema, max_data_bytes) in [
        (
            "{{.replace(\"\", \"0123456789\")}}",
            text("abcdefghij"),
            Schema::Text,
            80,
        ),
        (
            "{{.join(\"0123456789\")}}",
            Value::List(vec![text(""); 10]),
            Schema::List(Box::new(Schema::Text)),
            40,
        ),
        (
            "{{.to_string()}}",
            Value::List(vec![text(""); 5]),
            Schema::List(Box::new(Schema::Text)),
            4,
        ),
        ("{{.to_lowercase()}}", text("İ"), Schema::Text, 2),
        ("{{.pascal_case()}}", text("İ"), Schema::Text, 2),
    ] {
        let template = Template::parse_with_limits(
            source,
            Limits {
                max_data_bytes,
                ..Limits::default()
            },
        )
        .unwrap();
        assert!(
            matches!(
                template.render(&input, &schema),
                Err(TemplateError::Limit(_))
            ),
            "{source}"
        );
    }
}

#[test]
fn repeated_structural_comparisons_consume_the_shared_work_budget() {
    let values = Value::List(vec![text(&"a".repeat(80)); 32]);
    let needle = format!("{}b", "a".repeat(79));
    let source = format!("{{{{.contains(\"{needle}\")}}}}");
    let schema = Schema::List(Box::new(Schema::Text));
    assert_eq!(render(&source, &values, &schema).unwrap(), "false");
    let limited = Template::parse_with_limits(
        &source,
        Limits {
            max_steps: 1000,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        limited.render(&values, &schema),
        Err(TemplateError::Limit("work steps"))
    ));
}

#[test]
fn explicit_argument_extensions_chain_with_builtins_and_empty_names_are_not_builtin_hints() {
    let mut methods = MethodRegistry::new();
    methods
        .register_with_args("prefix", |value, args| match (value, args) {
            (Value::Text(value), [Value::Text(prefix)]) => {
                Ok(Value::Text(format!("{prefix}{value}")))
            }
            _ => Err(TemplateError::Method {
                name: "prefix".into(),
                reason: "expected text and one text prefix".into(),
            }),
        })
        .unwrap();
    assert_eq!(
        render_with_methods(
            r#"{{.prefix("Hello ").snake_case()}}"#,
            &text("Forge"),
            &Schema::Text,
            &methods,
        )
        .unwrap(),
        "hello_forge"
    );
    let mut custom_only = MethodRegistry::empty();
    custom_only.register("trim", |_| Ok(Value::Int(7))).unwrap();
    assert_eq!(
        render_with_methods(
            "{{.trim()}}",
            &Value::Bool(true),
            &Schema::Bool,
            &custom_only
        )
        .unwrap(),
        "7"
    );
}

#[test]
fn list_containment_compares_scalar_types_and_does_not_coerce() {
    for (receiver, expression, expected) in [
        (Value::List(vec![Value::Null]), "contains(null)", true),
        (
            Value::List(vec![Value::Bool(false)]),
            "contains(false)",
            true,
        ),
        (Value::List(vec![Value::Bool(false)]), "contains(0)", false),
        (Value::List(vec![Value::Float(1.5)]), "contains(1.5)", true),
        (Value::List(vec![text("one")]), "contains(\"one\")", true),
        (Value::List(vec![text("one")]), "contains(\"two\")", false),
        (
            Value::List(vec![text("one")]),
            "contains(\"longer\")",
            false,
        ),
    ] {
        assert_eq!(
            evaluate(receiver, expression).unwrap(),
            Value::Bool(expected)
        );
    }
}
