//! Presentation metadata for help is separate from business task input.
use crate::parser::safe_text;
#[cfg(feature = "disk")]
use forge_local::CommandKind;
use forge_local::{ArgumentDescriptor, CommandDescriptor, CommandInfo, DataType, Presence};
use std::ffi::OsStr;

/// Reusable terminal convention, not a field added to every typed command.
pub struct HelpOption {
    pub short: &'static str,
    pub long: &'static str,
    pub argument: ArgumentDescriptor,
}

pub const HELP_OPTION: HelpOption = HelpOption {
    short: "-h",
    long: "--help",
    argument: ArgumentDescriptor {
        name: "help",
        help: "Show command metadata without running business work.",
        data_type: DataType::Bool,
        presence: Presence::Optional {
            default: Some("false"),
        },
    },
};
impl HelpOption {
    pub fn matches(&self, argument: &OsStr) -> bool {
        argument == self.short || argument == self.long
    }
}

fn type_name(kind: &DataType) -> String {
    match kind {
        DataType::Bool => "Bool".into(),
        DataType::Text => "Text".into(),
        DataType::Path => "Path".into(),
        DataType::I64 => "I64".into(),
        DataType::U64 => "U64".into(),
        DataType::F64 => "F64".into(),
        DataType::Custom(name) => format!("Custom({})", safe_text(name)),
    }
}
fn presence_text(presence: &Presence) -> String {
    match presence {
        Presence::Required => "required".into(),
        Presence::Optional {
            default: Some(value),
        } => format!("optional, default={}", safe_text(value)),
        Presence::Optional { default: None } => "optional, no default".into(),
    }
}
fn argument_text(argument: &ArgumentDescriptor) -> String {
    format!(
        "{}: {} ({}) — {}",
        safe_text(argument.name),
        type_name(&argument.data_type),
        presence_text(&argument.presence),
        safe_text(argument.help)
    )
}
fn path_or_value_syntax(argument: &ArgumentDescriptor) -> String {
    let value = format!(
        "{}:{}",
        safe_text(argument.name),
        type_name(&argument.data_type)
    );
    match &argument.presence {
        Presence::Required => format!("<{value}>"),
        Presence::Optional {
            default: Some(default),
        } => format!("[{value}={}]", safe_text(default)),
        Presence::Optional { default: None } => format!("[{value}]"),
    }
}
fn is_no_follow(info: &CommandInfo, argument: &ArgumentDescriptor) -> bool {
    #[cfg(feature = "disk")]
    {
        info.kind == CommandKind::DiskPermissions
            && argument.name == "follow_symlinks"
            && argument.data_type == DataType::Bool
    }
    #[cfg(not(feature = "disk"))]
    {
        let _ = (info, argument);
        false
    }
}

/// Semantic names/types/defaults come from the descriptor; only the selected
/// terminal codec's `--no-follow` spelling is adapter-owned.
pub fn usage(info: &CommandInfo) -> String {
    let mut result = safe_text(info.descriptor.name);
    for _ in info
        .descriptor
        .arguments
        .iter()
        .filter(|arg| is_no_follow(info, arg))
    {
        result.push_str(" [--no-follow]");
    }
    for argument in info
        .descriptor
        .arguments
        .iter()
        .filter(|arg| !is_no_follow(info, arg))
    {
        result.push(' ');
        result.push_str(&path_or_value_syntax(argument));
    }
    result
}

pub fn catalog(commands: &[CommandInfo]) -> String {
    let mut output = String::from("Registered commands:\n");
    for command in commands {
        output.push_str(&format!(
            "  {}  {}\n",
            usage(command),
            safe_text(command.descriptor.help)
        ));
    }
    output
}

pub fn command_help(info: &CommandInfo) -> String {
    let CommandDescriptor {
        name,
        help,
        arguments,
    } = info.descriptor;
    let mut output = format!(
        "Command: {}\n{}\nUsage: {}\nHelp: {} [{}|{}] or help {}\nArguments:\n",
        safe_text(name),
        safe_text(help),
        usage(info),
        safe_text(name),
        HELP_OPTION.short,
        HELP_OPTION.long,
        safe_text(name)
    );
    if arguments.is_empty() {
        output.push_str("  (none)\n");
    }
    for argument in *arguments {
        output.push_str(&format!("  {}\n", argument_text(argument)));
        if is_no_follow(info, argument) {
            output.push_str(
                "    CLI --no-follow supplies false; omission uses the declared default.\n",
            );
        }
    }
    output.push_str(&format!(
        "Terminal option (not task input):\n  {}, {} — {}\n",
        HELP_OPTION.short,
        HELP_OPTION.long,
        argument_text(&HELP_OPTION.argument)
    ));
    output
}
