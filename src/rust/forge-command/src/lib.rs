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

mod descriptor;

pub use descriptor::{
    validate_descriptors, ArgumentDescriptor, CommandDescriptor, DataType, DescribeCommand,
    DescriptorError, Presence,
};
