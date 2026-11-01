//! Bounded, effect-free conversion from OS arguments to a finite command enum.
//! Only template-file paths may contain non-UTF-8 bytes. No file is opened here.

use super::{
    catalog::{HELP_ROUTE, METHODS_ROUTE, RENDER_ROUTE},
    CliError,
};
use forge_ingress_contract::Payload;
use std::{ffi::OsString, path::PathBuf};

pub(super) const MAX_ARGUMENTS: usize = 256;
pub(super) const MAX_BINDINGS: usize = 128;
pub(super) const MAX_ARGUMENT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, PartialEq)]
pub(super) enum Source {
    Inline(String),
    File(PathBuf),
}

#[derive(Debug, PartialEq)]
pub(super) enum Scalar {
    Text(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
}

#[derive(Debug, PartialEq)]
pub(super) struct Binding {
    pub name: String,
    pub value: Scalar,
}

#[derive(Debug, PartialEq)]
pub(super) struct RenderRequest {
    pub source: Source,
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HelpTarget {
    Render,
    Methods,
}

#[derive(Debug, PartialEq)]
pub(super) enum Command {
    Render(RenderRequest),
    Methods,
    Help(Option<HelpTarget>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Render,
    Methods,
    Help,
}

impl Payload for Command {
    type Kind = Kind;

    fn kind(&self) -> Kind {
        match self {
            Self::Render(_) => Kind::Render,
            Self::Methods => Kind::Methods,
            Self::Help(_) => Kind::Help,
        }
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct Invocation {
    pub route: &'static str,
    pub command: Command,
}

fn usage(message: &str) -> CliError {
    CliError::Usage(message.into())
}

fn utf8(argument: OsString) -> Result<String, CliError> {
    argument
        .into_string()
        .map_err(|_| usage("arguments other than --template-file paths must be UTF-8"))
}

fn is_help(argument: &str) -> bool {
    matches!(argument, "-h" | "--help")
}

fn help_invocation(target: Option<HelpTarget>) -> Invocation {
    Invocation {
        route: HELP_ROUTE,
        command: Command::Help(target),
    }
}

/// Arguments exclude the executable name. Check the entire input's count/byte
/// bounds before cloning names or values, even when a help token is present.
/// Option operands are literal data: a value equal to `--help` is not a flag.
pub(super) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Invocation, CliError> {
    let mut bounded = Vec::new();
    let mut bytes = 0usize;
    for argument in args {
        if bounded.len() == MAX_ARGUMENTS {
            return Err(usage("at most 256 arguments are accepted"));
        }
        bytes = bytes
            .checked_add(argument.as_encoded_bytes().len())
            .ok_or_else(|| usage("total argument bytes exceed 2 MiB"))?;
        if bytes > MAX_ARGUMENT_BYTES {
            return Err(usage("total argument bytes exceed 2 MiB"));
        }
        bounded.push(argument);
    }
    let mut args = bounded.into_iter();
    let Some(verb) = args.next() else {
        return Ok(help_invocation(None));
    };
    match utf8(verb)?.as_str() {
        "render" => parse_render(args),
        "methods" => {
            if let Some(argument) = args.next() {
                if !is_help(&utf8(argument)?) || args.next().is_some() {
                    return Err(usage("methods accepts only an optional --help or -h"));
                }
                Ok(help_invocation(Some(HelpTarget::Methods)))
            } else {
                Ok(Invocation {
                    route: METHODS_ROUTE,
                    command: Command::Methods,
                })
            }
        }
        "help" => {
            let target = match args.next().map(utf8).transpose()?.as_deref() {
                None => None,
                Some("render") => Some(HelpTarget::Render),
                Some("methods") => Some(HelpTarget::Methods),
                _ => {
                    return Err(usage(
                        "help accepts only the optional target render or methods",
                    ))
                }
            };
            if args.next().is_some() {
                return Err(usage("help accepts at most one target"));
            }
            Ok(help_invocation(target))
        }
        "--help" | "-h" => {
            if args.next().is_some() {
                return Err(usage("global --help or -h accepts no additional arguments"));
            }
            Ok(help_invocation(None))
        }
        _ => Err(usage("expected render, methods or help")),
    }
}

fn operand(args: &mut std::vec::IntoIter<OsString>) -> Result<OsString, CliError> {
    args.next()
        .ok_or_else(|| usage("option is missing its value"))
}

fn parse_render(mut args: std::vec::IntoIter<OsString>) -> Result<Invocation, CliError> {
    let mut source = None;
    let mut bindings = Vec::new();
    let mut help = false;
    while let Some(argument) = args.next() {
        let option = utf8(argument)?;
        match option.as_str() {
            "--help" | "-h" => {
                if help {
                    return Err(usage("help may be requested only once"));
                }
                help = true;
            }
            "--template" | "--template-file" => {
                if source.is_some() {
                    return Err(usage(
                        "choose exactly one --template or --template-file source",
                    ));
                }
                let value = operand(&mut args)?;
                source = Some(if option == "--template" {
                    Source::Inline(utf8(value)?)
                } else {
                    if value.is_empty() {
                        return Err(usage("--template-file requires a nonempty path"));
                    }
                    Source::File(PathBuf::from(value))
                });
            }
            "--text" | "--int" | "--float" | "--bool" | "--null" => {
                let value = utf8(operand(&mut args)?)?;
                append_binding(&mut bindings, &option, &value)?;
            }
            _ => {
                return Err(usage(
                    "render received an unknown option or extra positional argument",
                ))
            }
        }
    }
    // Help does not suppress any earlier malformed/unknown option or value.
    // Only render's required source is waived for this separate help command.
    if help {
        return Ok(help_invocation(Some(HelpTarget::Render)));
    }
    let source =
        source.ok_or_else(|| usage("render requires exactly one --template or --template-file"))?;
    Ok(Invocation {
        route: RENDER_ROUTE,
        command: Command::Render(RenderRequest { source, bindings }),
    })
}

fn valid_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z' | b'A'..=b'Z' | b'_'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn append_binding(bindings: &mut Vec<Binding>, option: &str, input: &str) -> Result<(), CliError> {
    if bindings.len() == MAX_BINDINGS {
        return Err(usage("at most 128 named bindings are accepted"));
    }
    let (name, value) = if option == "--null" {
        (input, "")
    } else {
        input
            .split_once('=')
            .ok_or_else(|| usage("typed bindings require NAME=VALUE"))?
    };
    if !valid_name(name) {
        return Err(usage("binding names must match [A-Za-z_][A-Za-z0-9_]*"));
    }
    if bindings.iter().any(|binding| binding.name == name) {
        return Err(usage(
            "binding names must be unique across all scalar types",
        ));
    }
    let value = match option {
        "--text" => Scalar::Text(value.into()),
        "--int" => Scalar::Int(
            value
                .parse()
                .map_err(|_| usage("--int requires a signed 64-bit integer"))?,
        ),
        "--float" => {
            let number: f64 = value
                .parse()
                .map_err(|_| usage("--float requires a finite floating-point number"))?;
            if !number.is_finite() {
                return Err(usage("--float requires a finite floating-point number"));
            }
            Scalar::Float(number)
        }
        "--bool" => Scalar::Bool(match value {
            "true" => true,
            "false" => false,
            _ => return Err(usage("--bool requires exactly true or false")),
        }),
        "--null" => Scalar::Null,
        _ => return Err(usage("unknown binding type")),
    };
    bindings.push(Binding {
        name: name.into(),
        value,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Invocation, CliError> {
        parse(args.iter().map(OsString::from))
    }

    fn rejected(args: &[&str]) {
        assert!(matches!(parsed(args), Err(CliError::Usage(_))), "{args:?}");
    }

    #[test]
    fn each_verb_selects_its_explicit_route_and_faithful_payload_kind() {
        let rendered = parsed(&["render", "--template", "Hello"]).unwrap();
        assert_eq!(rendered.route, RENDER_ROUTE);
        assert_eq!(rendered.command.kind(), Kind::Render);
        let methods = parsed(&["methods"]).unwrap();
        assert_eq!(methods.route, METHODS_ROUTE);
        assert_eq!(methods.command.kind(), Kind::Methods);
        let help = parsed(&[]).unwrap();
        assert_eq!(help.route, HELP_ROUTE);
        assert_eq!(help.command.kind(), Kind::Help);
    }

    #[test]
    fn all_scalar_inputs_and_first_equals_splitting_are_preserved() {
        let invocation = parsed(&[
            "render",
            "--text",
            "name=Ada=Byron",
            "--template",
            "{{ name }}",
            "--int",
            "count=-42",
            "--float",
            "ratio=1.25e2",
            "--bool",
            "ready=true",
            "--null",
            "missing",
            "--text",
            "empty=",
        ])
        .unwrap();
        let Command::Render(request) = invocation.command else {
            panic!("render expected")
        };
        assert_eq!(request.source, Source::Inline("{{ name }}".into()));
        assert_eq!(
            request.bindings,
            vec![
                Binding {
                    name: "name".into(),
                    value: Scalar::Text("Ada=Byron".into())
                },
                Binding {
                    name: "count".into(),
                    value: Scalar::Int(-42)
                },
                Binding {
                    name: "ratio".into(),
                    value: Scalar::Float(125.0)
                },
                Binding {
                    name: "ready".into(),
                    value: Scalar::Bool(true)
                },
                Binding {
                    name: "missing".into(),
                    value: Scalar::Null
                },
                Binding {
                    name: "empty".into(),
                    value: Scalar::Text(String::new())
                },
            ]
        );
    }

    #[test]
    fn every_supported_help_spelling_has_no_source_requirement() {
        for args in [vec![], vec!["help"], vec!["-h"], vec!["--help"]] {
            assert_eq!(parsed(&args).unwrap(), help_invocation(None));
        }
        for (verb, target) in [
            ("render", HelpTarget::Render),
            ("methods", HelpTarget::Methods),
        ] {
            for args in [vec!["help", verb], vec![verb, "--help"], vec![verb, "-h"]] {
                assert_eq!(parsed(&args).unwrap(), help_invocation(Some(target)));
            }
        }
    }

    #[test]
    fn help_validates_other_tokens_before_it_can_suppress_rendering() {
        for args in [
            vec!["render", "--help", "--unknown"],
            vec!["render", "--int", "n=broken", "--help"],
            vec!["render", "--help", "--bool", "n=yes"],
            vec!["render", "--help", "--template"],
            vec![
                "render",
                "--help",
                "--template",
                "x",
                "--template-file",
                "file",
            ],
            vec!["render", "--text", "n=x", "--null", "n", "--help"],
            vec!["render", "--help", "trailing"],
            vec!["render", "--help", "-h"],
            vec!["methods", "--help", "trailing"],
            vec!["--help", "render"],
            vec!["help", "unknown"],
            vec!["help", "render", "trailing"],
        ] {
            rejected(&args);
        }
        assert_eq!(
            parsed(&["render", "--text", "n=valid", "--help"]).unwrap(),
            help_invocation(Some(HelpTarget::Render))
        );
    }

    #[test]
    fn option_operands_are_literal_and_empty_inline_templates_are_valid() {
        for value in ["", "--help", "--bogus", "é🙂"] {
            let invocation = parsed(&["render", "--template", value]).unwrap();
            let Command::Render(request) = invocation.command else {
                panic!("render expected")
            };
            assert_eq!(request.source, Source::Inline(value.into()));
        }
        rejected(&["render", "--template-file", ""]);
    }

    #[test]
    fn source_selection_is_exactly_one_and_unknown_extra_or_missing_args_fail() {
        for args in [
            vec!["render"],
            vec!["render", "--template"],
            vec!["render", "--template-file"],
            vec!["render", "--template", "a", "--template", "b"],
            vec!["render", "--template-file", "a", "--template-file", "b"],
            vec!["render", "--template-file", "a", "--template", "b"],
            vec!["render", "--template", "a", "--unknown"],
            vec!["render", "--template", "a", "extra"],
            vec!["render", "--template", "a", "--text"],
            vec!["render", "--template=x"],
            vec!["render", "--", "file"],
            vec!["methods", "extra"],
            vec!["methods", "--template", "a"],
            vec!["unknown"],
        ] {
            rejected(&args);
        }
    }

    #[test]
    fn binding_names_are_ascii_identifiers_unique_across_types() {
        for name in [
            "", "1first", "has.dot", "has-dash", "a b", "é", "a\n", "n=value",
        ] {
            rejected(&["render", "--template", "", "--null", name]);
        }
        for name in ["_", "_n2", "NAME", "a1"] {
            assert!(parsed(&["render", "--template", "", "--null", name]).is_ok());
        }
        for flag in ["--text", "--int", "--float", "--bool"] {
            rejected(&["render", "--template", "", flag, "missing_equals"]);
        }
        rejected(&[
            "render",
            "--template",
            "",
            "--int",
            "n=1",
            "--text",
            "n=one",
        ]);
        rejected(&["render", "--template", "", "--null", "n", "--null", "n"]);
    }

    #[test]
    fn numbers_and_booleans_are_strict_and_do_not_overflow_or_accept_nonfinite_values() {
        for bad in [
            "9223372036854775808",
            "-9223372036854775809",
            "1.0",
            " 1",
            "",
        ] {
            rejected(&["render", "--template", "", "--int", &format!("n={bad}")]);
        }
        for good in [i64::MIN.to_string(), i64::MAX.to_string(), "+1".into()] {
            assert!(parsed(&["render", "--template", "", "--int", &format!("n={good}")]).is_ok());
        }
        for bad in ["NaN", "inf", "-inf", "1e9999", "broken", ""] {
            rejected(&["render", "--template", "", "--float", &format!("n={bad}")]);
        }
        for good in ["-0.0", "1e-10", "1.7976931348623157e308"] {
            assert!(parsed(&["render", "--template", "", "--float", &format!("n={good}")]).is_ok());
        }
        for bad in ["TRUE", "False", "1", "0", "yes", "", " true"] {
            rejected(&["render", "--template", "", "--bool", &format!("n={bad}")]);
        }
        assert!(parsed(&["render", "--template", "", "--bool", "n=false"]).is_ok());
    }

    #[test]
    fn all_argument_bounds_apply_before_help_and_stop_an_unbounded_iterator() {
        let mut args = vec![OsString::from("render"), "--template".into(), "".into()];
        for index in 0..126 {
            args.extend(["--null".into(), format!("n{index}").into()]);
        }
        args.push("--help".into());
        assert_eq!(args.len(), MAX_ARGUMENTS);
        assert_eq!(
            parse(args.clone()).unwrap(),
            help_invocation(Some(HelpTarget::Render))
        );
        args.push("--help".into());
        assert!(matches!(parse(args), Err(CliError::Usage(_))));
        assert!(matches!(
            parse(std::iter::repeat(OsString::from("--help"))),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn exact_total_byte_limit_is_accepted_and_one_more_byte_is_rejected() {
        let overhead = "render".len() + "--template".len();
        let value = "a".repeat(MAX_ARGUMENT_BYTES - overhead);
        assert!(parse(["render".into(), "--template".into(), value.clone().into()]).is_ok());
        assert!(matches!(
            parse([
                "render".into(),
                "--template".into(),
                format!("{value}a").into()
            ]),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            parse(["--help".into(), "a".repeat(MAX_ARGUMENT_BYTES).into()]),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn binding_limit_is_independent_of_the_stricter_current_argument_grammar() {
        let mut bindings = Vec::new();
        for index in 0..MAX_BINDINGS {
            append_binding(&mut bindings, "--null", &format!("n{index}")).unwrap();
        }
        assert!(matches!(
            append_binding(&mut bindings, "--null", "extra"),
            Err(CliError::Usage(_))
        ));
        assert_eq!(bindings.len(), MAX_BINDINGS);
    }

    #[cfg(unix)]
    #[test]
    fn file_paths_preserve_non_utf8_but_all_other_operands_reject_it() {
        use std::os::unix::ffi::OsStringExt;
        let native = OsString::from_vec(vec![b'p', 0xff]);
        let invocation =
            parse(["render".into(), "--template-file".into(), native.clone()]).unwrap();
        let Command::Render(request) = invocation.command else {
            panic!("render expected")
        };
        assert_eq!(request.source, Source::File(PathBuf::from(native.clone())));
        for args in [
            vec![native.clone()],
            vec!["render".into(), "--template".into(), native.clone()],
            vec!["render".into(), "--help".into(), native.clone()],
            vec![
                "render".into(),
                "--template".into(),
                "".into(),
                "--text".into(),
                native.clone(),
            ],
            vec!["methods".into(), native],
        ] {
            assert!(matches!(parse(args), Err(CliError::Usage(_))));
        }
    }
}
