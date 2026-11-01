//! Public reusable registration. Configuration is checked without native I/O;
//! the selected factory opens the root only after the host freezes registration.
use crate::{
    filesystem::{FsError, NativeFiles},
    task::{InspectError, InspectTask, Limits},
};
use forge_bootstrap::{PreparedGroup, RegistrationError};
use forge_provider::Factory;
use std::{error::Error, ffi::OsString, fmt, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdeConfig {
    Native {
        root: PathBuf,
        limits: Limits,
        excluded_names: Vec<OsString>,
    },
}
impl IdeConfig {
    pub fn project(root: PathBuf) -> Self {
        Self::Native {
            root,
            limits: Limits::default(),
            excluded_names: vec![".git".into(), "target".into()],
        }
    }
}
#[derive(Debug)]
pub enum AssemblyError {
    Configuration(InspectError),
    Registration(RegistrationError),
    Filesystem(FsError),
}
impl fmt::Display for AssemblyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(e) => e.fmt(f),
            Self::Registration(e) => e.fmt(f),
            Self::Filesystem(e) => e.fmt(f),
        }
    }
}
impl Error for AssemblyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Configuration(e) => Some(e),
            Self::Registration(e) => Some(e),
            Self::Filesystem(e) => Some(e),
        }
    }
}
#[derive(Clone, Debug)]
pub struct InspectFactory {
    root: PathBuf,
    limits: Limits,
    excluded_names: Vec<OsString>,
}
impl Factory<InspectTask<NativeFiles>> for InspectFactory {
    type Error = AssemblyError;
    fn create(&self) -> Result<InspectTask<NativeFiles>, AssemblyError> {
        let files = NativeFiles::open(&self.root).map_err(AssemblyError::Filesystem)?;
        InspectTask::new(files, self.limits.clone(), self.excluded_names.clone())
            .map_err(AssemblyError::Configuration)
    }
}
/// The application names this group/binding, chooses singleton or transient,
/// and may construct an InspectTask with its own typed client instead.
pub fn register_ide(
    id: impl Into<String>,
    config: IdeConfig,
) -> Result<PreparedGroup<InspectFactory>, AssemblyError> {
    let IdeConfig::Native {
        root,
        limits,
        excluded_names,
    } = config;
    if !root.is_absolute()
        || root
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(AssemblyError::Configuration(InspectError::Configuration(
            "root must be absolute without parent traversal",
        )));
    }
    // Validate the same task policy without constructing a filesystem client.
    InspectTask::new((), limits.clone(), excluded_names.clone())
        .map_err(AssemblyError::Configuration)?;
    let id = id.into();
    PreparedGroup::new(
        id.clone(),
        &[&id],
        &[],
        InspectFactory {
            root,
            limits,
            excluded_names,
        },
    )
    .map_err(AssemblyError::Registration)
}
