//! One description source for registered routes and CLI help. The help option is
//! an adapter convention, not a field inserted into rendering business input.

use super::input::{HelpTarget, MAX_ARGUMENTS, MAX_ARGUMENT_BYTES, MAX_BINDINGS};
use forge_command::{ArgumentDescriptor, CommandDescriptor, DataType, DescribeCommand, Presence};
use std::fmt::Write;

pub(super) const RENDER_ROUTE: &str = "template.render";
pub(super) const METHODS_ROUTE: &str = "template.methods";
pub(super) const HELP_ROUTE: &str = "template.help";

const OPTIONAL: Presence = Presence::Optional { default: None };

pub(super) const HELP_OPTION: ArgumentDescriptor = ArgumentDescriptor {
    name: "help",
    help: "Show help without rendering or reading a template file.",
    data_type: DataType::Bool,
    presence: Presence::Optional {
        default: Some("false"),
    },
};

const RENDER: CommandDescriptor = CommandDescriptor {
    name: RENDER_ROUTE,
    help: "Render a template with explicitly typed, flat named scalar bindings.",
    arguments: &[
        ArgumentDescriptor {
            name: "template",
            help: "Inline template text; choose exactly one source option.",
            data_type: DataType::Text,
            presence: OPTIONAL,
        },
        ArgumentDescriptor {
            name: "template-file",
            help: "Native template path; choose exactly one source option.",
            data_type: DataType::Path,
            presence: OPTIONAL,
        },
        ArgumentDescriptor {
            name: "text",
            help: "Bind NAME=VALUE as text; an empty VALUE is allowed.",
            data_type: DataType::Text,
            presence: OPTIONAL,
        },
        ArgumentDescriptor {
            name: "int",
            help: "Bind NAME=VALUE as a signed 64-bit integer.",
            data_type: DataType::I64,
            presence: OPTIONAL,
        },
        ArgumentDescriptor {
            name: "float",
            help: "Bind NAME=VALUE as a finite floating-point number.",
            data_type: DataType::F64,
            presence: OPTIONAL,
        },
        ArgumentDescriptor {
            name: "bool",
            help: "Bind NAME=true or NAME=false; spelling is case-sensitive.",
            data_type: DataType::Bool,
            presence: OPTIONAL,
        },
        ArgumentDescriptor {
            name: "null",
            help: "Bind NAME as null, without =VALUE.",
            data_type: DataType::Custom("null"),
            presence: OPTIONAL,
        },
    ],
};

const METHODS: CommandDescriptor = CommandDescriptor {
    name: METHODS_ROUTE,
    help: "List the installed built-in template method names without rendering.",
    arguments: &[],
};

const HELP: CommandDescriptor = CommandDescriptor {
    name: HELP_ROUTE,
    help: "Show general help or help for render or methods.",
    arguments: &[ArgumentDescriptor {
        name: "command",
        help: "Optional help target: render or methods.",
        data_type: DataType::Custom("command"),
        presence: OPTIONAL,
    }],
};

struct TemplateCommands;

impl DescribeCommand for TemplateCommands {
    fn descriptors(&self) -> &'static [CommandDescriptor] {
        &[RENDER, METHODS, HELP]
    }
}

pub(super) fn descriptors() -> &'static [CommandDescriptor] {
    TemplateCommands.descriptors()
}

fn presence(presence: Presence) -> String {
    match presence {
        Presence::Required => "required".into(),
        Presence::Optional { default: None } => "optional".into(),
        Presence::Optional {
            default: Some(value),
        } => format!("optional, default={value}"),
    }
}

fn argument(output: &mut String, argument: &ArgumentDescriptor, flag: bool) {
    let prefix = if flag { "--" } else { "" };
    writeln!(
        output,
        "  {prefix}{}: {} ({})\n      {}",
        argument.name,
        argument.data_type,
        presence(argument.presence),
        argument.help
    )
    .unwrap();
}

fn render_usage(output: &mut String) {
    // The adapter defines flag grammar; names and semantic categories stay in
    // the descriptors. The contract has no mutually-exclusive/repeated marker.
    let arguments = RENDER.arguments;
    write!(
        output,
        "Usage: forge-template render (--{} TEXT | --{} PATH)",
        arguments[0].name, arguments[1].name
    )
    .unwrap();
    for field in &arguments[2..] {
        let operand = if field.data_type == DataType::Custom("null") {
            "NAME"
        } else {
            "NAME=VALUE"
        };
        write!(output, " [--{} {operand}]...", field.name).unwrap();
    }
    output.push('\n');
}

pub(super) fn help(target: Option<HelpTarget>) -> String {
    let mut output = String::new();
    match target {
        None => {
            output.push_str("forge-template — schema-checked template rendering\n\nUsage: forge-template <command> [options]\n\nCommands:\n");
            for (verb, descriptor) in ["render", "methods", "help"].into_iter().zip(descriptors()) {
                writeln!(
                    output,
                    "  {verb} ({})\n      {}",
                    descriptor.name, descriptor.help
                )
                .unwrap();
            }
            output.push_str("\nUse 'forge-template help render' or 'forge-template render --help' for render options.\nUse 'forge-template help methods' for method listing help.\n");
        }
        Some(HelpTarget::Render) => {
            writeln!(output, "render ({})\n{}\n", RENDER.name, RENDER.help).unwrap();
            render_usage(&mut output);
            output.push_str("\nRequired source group: exactly one of the two source options above; they are mutually exclusive.\nBindings may repeat by type, but each NAME must be unique across all types.\nNAME must match [A-Za-z_][A-Za-z0-9_]*; only flat scalar bindings are accepted.\n\nOptions:\n");
            for field in RENDER.arguments {
                argument(&mut output, field, true);
            }
            writeln!(output, "\nLimits: {MAX_ARGUMENTS} arguments, {MAX_BINDINGS} bindings, {} MiB total OS-argument bytes.\nThe argument-count bound can limit bindings before their own bound.", MAX_ARGUMENT_BYTES / (1024 * 1024)).unwrap();
            output.push_str("Only file paths may be non-UTF-8. Option values are consumed literally; quote spaces for your shell.\n");
        }
        Some(HelpTarget::Methods) => {
            writeln!(
                output,
                "methods ({})\n{}\n\nUsage: forge-template methods\nArguments: none.",
                METHODS.name, METHODS.help
            )
            .unwrap();
        }
    }
    output.push_str("\nHelp option (-h alias):\n");
    argument(&mut output, &HELP_OPTION, true);
    output.push_str("Unknown or malformed options still fail when help is requested.\n");
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_command::validate_descriptors;

    #[test]
    fn descriptors_are_valid_and_are_the_exact_binding_catalog() {
        validate_descriptors(descriptors()).unwrap();
        assert_eq!(
            descriptors()
                .iter()
                .map(|descriptor| descriptor.name)
                .collect::<Vec<_>>(),
            [RENDER_ROUTE, METHODS_ROUTE, HELP_ROUTE]
        );
        assert!(descriptors()
            .iter()
            .flat_map(|descriptor| descriptor.arguments)
            .all(|argument| argument.name != HELP_OPTION.name));
        assert_eq!(HELP_OPTION.data_type, DataType::Bool);
    }

    #[test]
    fn global_help_uses_every_registered_description() {
        let output = help(None);
        for descriptor in descriptors() {
            assert!(output.contains(descriptor.name));
            assert!(output.contains(descriptor.help));
        }
        assert!(output.contains(HELP_OPTION.help));
        assert!(output.contains("--help"));
        assert!(output.contains("-h"));
    }

    #[test]
    fn render_help_uses_argument_names_types_presence_and_group_caveats() {
        let output = help(Some(HelpTarget::Render));
        for field in RENDER.arguments {
            assert!(output.contains(&format!(
                "--{}: {} ({})",
                field.name,
                field.data_type,
                presence(field.presence)
            )));
            assert!(output.contains(field.help));
        }
        assert!(output.contains("Required source group"));
        assert!(output.contains("mutually exclusive"));
        assert!(output.contains("unique across all types"));
        assert!(output.contains("flat scalar"));
        assert!(output.contains("--null NAME"));
        assert!(!output.contains("--null NAME=VALUE"));
        assert!(output.contains("256 arguments"));
        assert!(output.contains("128 bindings"));
    }

    #[test]
    fn method_help_describes_no_rendering_and_no_business_arguments() {
        let output = help(Some(HelpTarget::Methods));
        assert!(output.contains(METHODS.help));
        assert!(output.contains("Arguments: none"));
        assert!(!output.contains("--template"));
        assert!(output.contains(HELP_OPTION.help));
    }
}
