//! Static, transport-neutral descriptions of exposed command operations.
//!
//! Description is independent of execution: a task may opt into this contract,
//! while non-task command providers may implement it too. This crate has no task,
//! I/O-client, ingress, parser, serialization or reflection dependency.
//!
//! ```
//! use forge_command::{
//!     validate_descriptors, ArgumentDescriptor, CommandDescriptor, DataType,
//!     DescribeCommand, Presence,
//! };
//!
//! struct Catalog; // No execution trait is required.
//!
//! impl DescribeCommand for Catalog {
//!     fn descriptors(&self) -> &'static [CommandDescriptor] {
//!         &[CommandDescriptor {
//!             name: "files.inspect",
//!             help: "Inspect a file without changing it.",
//!             arguments: &[ArgumentDescriptor {
//!                 name: "path",
//!                 help: "The file to inspect.",
//!                 data_type: DataType::Path,
//!                 presence: Presence::Required,
//!             }],
//!         }]
//!     }
//! }
//!
//! let catalog: &dyn DescribeCommand = &Catalog;
//! assert_eq!(catalog.descriptors()[0].arguments[0].data_type, DataType::Path);
//! validate_descriptors(catalog.descriptors()).unwrap();
//! ```

use std::fmt;

/// Describes one or more exposed operations without invoking them.
///
/// Implementations must return static descriptor values without effects: no task
/// execution, client calls, resource acquisition or discovery through live I/O.
/// This is a behavioral contract, not a compiler-enforced purity guarantee;
/// implementations are trusted Rust code and their panics propagate.
///
/// A single operation returns a one-element slice. A capability-owned input enum
/// multiplexing several operations returns one descriptor per exposed operation,
/// each with its own arguments. No framework-global business enum is required.
/// Composition selects the descriptions belonging to registered bindings; merely
/// implementing this trait does not register, authorize or execute anything.
///
/// The descriptions are explicit declarations, not reflection of a Rust input
/// type. Consumers must verify their agreement with typed inputs and adapters.
/// Use [`validate_descriptors`] to check a complete returned set before exposing
/// it; calling this method does not perform validation automatically.
pub trait DescribeCommand {
    fn descriptors(&self) -> &'static [CommandDescriptor];
}

/// Presentation metadata for one exposed operation, not an executable binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandDescriptor {
    /// Stable operation name, with protocol-specific spelling left to adapters.
    pub name: &'static str,
    /// Human-readable explanation of what the operation does.
    pub help: &'static str,
    /// Business arguments in presentation order; an empty slice is valid.
    pub arguments: &'static [ArgumentDescriptor],
}

/// A named business input, independent of positional/flag/wire representation.
///
/// A CLI's `--help` or `-h` definition belongs to that adapter, not to every
/// business input. Its help argument can use this same metadata type separately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArgumentDescriptor {
    pub name: &'static str,
    pub help: &'static str,
    pub data_type: DataType,
    pub presence: Presence,
}

/// Semantic input categories, not Rust reflection, codecs or schema validators.
///
/// `Path` does not require UTF-8 and does not promise existence or permissions.
/// `Custom` names a capability-owned concept without registering a runtime type.
/// An adapter owns its accepted textual/wire representation and conversions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataType {
    Bool,
    Text,
    Path,
    I64,
    U64,
    F64,
    Custom(&'static str),
}

impl fmt::Display for DataType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Bool => "bool",
            Self::Text => "text",
            Self::Path => "path",
            Self::I64 => "i64",
            Self::U64 => "u64",
            Self::F64 => "f64",
            Self::Custom(name) => name,
        })
    }
}

/// Whether a caller must supply an argument.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Presence {
    Required,
    Optional {
        /// Human-readable default only: not parsed, applied or type-checked.
        /// `None` means no default is advertised; an empty text default is valid.
        default: Option<&'static str>,
    },
}

/// Invalid descriptor declarations, with the affected names or argument index.
///
/// Blank means empty or entirely Unicode whitespace. Name identity is exact and
/// case-sensitive; validation does not trim, normalize or impose a CLI grammar.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DescriptorError {
    EmptyDescriptorSet,
    EmptyCommandName,
    EmptyCommandHelp {
        command: &'static str,
    },
    DuplicateCommandName {
        command: &'static str,
    },
    EmptyArgumentName {
        command: &'static str,
        index: usize,
    },
    EmptyArgumentHelp {
        command: &'static str,
        argument: &'static str,
    },
    EmptyCustomDataType {
        command: &'static str,
        argument: &'static str,
    },
    DuplicateArgumentName {
        command: &'static str,
        argument: &'static str,
    },
}

impl fmt::Display for DescriptorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyDescriptorSet => formatter.write_str("command descriptor set is empty"),
            Self::EmptyCommandName => formatter.write_str("command name is blank"),
            Self::EmptyCommandHelp { command } => {
                write!(formatter, "command {command:?} help is blank")
            }
            Self::DuplicateCommandName { command } => {
                write!(formatter, "duplicate command name {command:?}")
            }
            Self::EmptyArgumentName { command, index } => {
                write!(
                    formatter,
                    "command {command:?} argument {index} name is blank"
                )
            }
            Self::EmptyArgumentHelp { command, argument } => {
                write!(
                    formatter,
                    "command {command:?} argument {argument:?} help is blank"
                )
            }
            Self::EmptyCustomDataType { command, argument } => {
                write!(
                    formatter,
                    "command {command:?} argument {argument:?} custom type name is blank"
                )
            }
            Self::DuplicateArgumentName { command, argument } => {
                write!(
                    formatter,
                    "command {command:?} has duplicate argument name {argument:?}"
                )
            }
        }
    }
}

impl std::error::Error for DescriptorError {}

impl CommandDescriptor {
    /// Checks nonblank names/help/custom-type labels and unique argument names.
    ///
    /// Reports the first invalid declaration in presentation order. No input is
    /// parsed, default applied, runtime type reflected or operation executed.
    /// This does not establish agreement with a typed input, route or adapter.
    pub fn validate(&self) -> Result<(), DescriptorError> {
        if self.name.trim().is_empty() {
            return Err(DescriptorError::EmptyCommandName);
        }
        if self.help.trim().is_empty() {
            return Err(DescriptorError::EmptyCommandHelp { command: self.name });
        }
        for (index, argument) in self.arguments.iter().enumerate() {
            if argument.name.trim().is_empty() {
                return Err(DescriptorError::EmptyArgumentName {
                    command: self.name,
                    index,
                });
            }
            if argument.help.trim().is_empty() {
                return Err(DescriptorError::EmptyArgumentHelp {
                    command: self.name,
                    argument: argument.name,
                });
            }
            if matches!(argument.data_type, DataType::Custom(name) if name.trim().is_empty()) {
                return Err(DescriptorError::EmptyCustomDataType {
                    command: self.name,
                    argument: argument.name,
                });
            }
            if self.arguments[..index]
                .iter()
                .any(|previous| previous.name == argument.name)
            {
                return Err(DescriptorError::DuplicateArgumentName {
                    command: self.name,
                    argument: argument.name,
                });
            }
        }
        Ok(())
    }
}

/// Validates a nonempty operation set and rejects duplicate operation names.
///
/// Each operation is checked with [`CommandDescriptor::validate`], then compared
/// with earlier operation names. The first error in slice order is returned.
/// Argument names may repeat across different operations. This is a declaration
/// check, not a route registry, authorization policy or complete input schema.
/// Validation is allocation-free and quadratic in operation/argument counts,
/// intended for small static catalogs at assembly time, not per-request parsing.
pub fn validate_descriptors(descriptors: &[CommandDescriptor]) -> Result<(), DescriptorError> {
    if descriptors.is_empty() {
        return Err(DescriptorError::EmptyDescriptorSet);
    }
    for (index, descriptor) in descriptors.iter().enumerate() {
        descriptor.validate()?;
        if descriptors[..index]
            .iter()
            .any(|previous| previous.name == descriptor.name)
        {
            return Err(DescriptorError::DuplicateCommandName {
                command: descriptor.name,
            });
        }
    }
    Ok(())
}
