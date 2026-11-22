//! Public reusable registration. Configuration is checked without native I/O;
//! the selected factory opens the root only after the host freezes registration.
use crate::{
    ast_graph::{AstGraphLimits, AstGraphTask},
    filesystem::{FsError, NativeFiles},
    task::{InspectError, InspectTask, Limits},
};
use forge_core::bootstrap::{PreparedGroup, RegistrationError};
use forge_core::provider::Factory;
use std::{error::Error, ffi::OsString, fmt, path::PathBuf};

#[cfg(all(unix, feature = "managed-writes"))]
use crate::filesystem::managed_write::{ManagedWriter, WriteError};
#[cfg(all(unix, feature = "managed-writes"))]
use std::collections::BTreeMap;

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
            excluded_names: vec![
                ".git".into(),
                "target".into(),
                crate::filesystem::WRITE_JOURNAL.into(),
            ],
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
impl InspectFactory {
    /// Create the bounded source task with the same root/exclusion authority.
    /// Called explicitly after registration is frozen, just like inspection.
    pub fn create_source(&self) -> Result<crate::source::SourceTask<NativeFiles>, AssemblyError> {
        let files = NativeFiles::open(&self.root).map_err(AssemblyError::Filesystem)?;
        Ok(crate::source::SourceTask::new(
            files,
            self.excluded_names.clone(),
        ))
    }
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
    let factory = configured_factory(config)?;
    let id = id.into();
    PreparedGroup::new(id.clone(), &[&id], &[], factory).map_err(AssemblyError::Registration)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AstGraphConfig {
    Native {
        root: PathBuf,
        limits: AstGraphLimits,
        excluded_names: Vec<OsString>,
    },
}

impl AstGraphConfig {
    pub fn project(root: PathBuf) -> Self {
        Self::Native {
            root,
            limits: AstGraphLimits::default(),
            excluded_names: vec![
                ".git".into(),
                "target".into(),
                "node_modules".into(),
                crate::filesystem::WRITE_JOURNAL.into(),
            ],
        }
    }
}

#[derive(Clone, Debug)]
pub struct AstGraphFactory {
    root: PathBuf,
    limits: AstGraphLimits,
    excluded_names: Vec<OsString>,
}

impl Factory<AstGraphTask<NativeFiles>> for AstGraphFactory {
    type Error = AssemblyError;

    fn create(&self) -> Result<AstGraphTask<NativeFiles>, AssemblyError> {
        let files = NativeFiles::open(&self.root).map_err(AssemblyError::Filesystem)?;
        AstGraphTask::new(files, self.limits.clone(), self.excluded_names.clone())
            .map_err(AssemblyError::Configuration)
    }
}

/// Stage the AST graph task without opening the root directory.
/// The host freezes registration before calling `AstGraphFactory::create`.
pub fn register_ast_graph(
    id: impl Into<String>,
    config: AstGraphConfig,
) -> Result<PreparedGroup<AstGraphFactory>, AssemblyError> {
    let AstGraphConfig::Native {
        root,
        limits,
        excluded_names,
    } = config;
    validate_root(&root)?;
    AstGraphTask::new((), limits.clone(), excluded_names.clone())
        .map_err(AssemblyError::Configuration)?;
    let factory = AstGraphFactory {
        root,
        limits,
        excluded_names,
    };
    let id = id.into();
    PreparedGroup::new(id.clone(), &[&id], &[], factory).map_err(AssemblyError::Registration)
}

fn configured_factory(config: IdeConfig) -> Result<InspectFactory, AssemblyError> {
    let IdeConfig::Native {
        root,
        limits,
        excluded_names,
    } = config;
    validate_root(&root)?;
    // Validate the same task policy without constructing a filesystem client.
    InspectTask::new((), limits.clone(), excluded_names.clone())
        .map_err(AssemblyError::Configuration)?;
    Ok(InspectFactory {
        root,
        limits,
        excluded_names,
    })
}

fn validate_root(root: &std::path::Path) -> Result<(), AssemblyError> {
    if !root.is_absolute()
        || root
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(AssemblyError::Configuration(InspectError::Configuration(
            "root must be absolute without parent traversal",
        )));
    }
    Ok(())
}

/// Logical catalog key, never a path or an authenticated identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectAlias(String);
impl ProjectAlias {
    pub fn new(value: impl Into<String>) -> Result<Self, AssemblyError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || value == "."
            || value == ".."
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(AssemblyError::Configuration(InspectError::Configuration(
                "project alias must be a bounded logical token, not a path",
            )));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for ProjectAlias {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A runtime-length homogeneous collection staged as one cohesive catalog group.
/// The host constructs each factory only after outer registration is frozen.
#[derive(Clone, Debug)]
pub struct ProjectFactories {
    factories: Vec<(ProjectAlias, InspectFactory)>,
}
impl ProjectFactories {
    pub fn factories(&self) -> &[(ProjectAlias, InspectFactory)] {
        &self.factories
    }
    pub fn into_factories(self) -> Vec<(ProjectAlias, InspectFactory)> {
        self.factories
    }
}

/// Validate a whole project catalog before returning its factory group. Empty
/// catalogs are supported (node-only host); at most 128 distinct aliases. The
/// group provides its catalog binding id, not dynamically looked-up DI services.
/// Neither configuration validation nor registration opens a root. If a later
/// create fails, the host must drop created instances and publish no partial host.
pub fn register_projects(
    id: impl Into<String>,
    configs: Vec<(ProjectAlias, IdeConfig)>,
) -> Result<PreparedGroup<ProjectFactories>, AssemblyError> {
    if configs.len() > 128 {
        return Err(AssemblyError::Configuration(InspectError::Configuration(
            "project catalog supports at most 128 aliases",
        )));
    }
    let mut factories = Vec::with_capacity(configs.len());
    let mut aliases = std::collections::BTreeSet::new();
    for (alias, config) in configs {
        if !aliases.insert(alias.clone()) {
            return Err(AssemblyError::Configuration(InspectError::Configuration(
                "duplicate project alias",
            )));
        }
        factories.push((alias, configured_factory(config)?));
    }
    let id = id.into();
    PreparedGroup::new(id.clone(), &[&id], &[], ProjectFactories { factories })
        .map_err(AssemblyError::Registration)
}

#[cfg(all(unix, feature = "managed-writes"))]
#[derive(Clone, Debug)]
pub struct ManagedWriterConfig {
    pub root: PathBuf,
}

#[cfg(all(unix, feature = "managed-writes"))]
#[derive(Clone, Debug)]
pub struct ManagedWriterFactory(ManagedWriterConfig);

#[cfg(all(unix, feature = "managed-writes"))]
impl Factory<ManagedWriter> for ManagedWriterFactory {
    type Error = WriteError;

    fn create(&self) -> Result<ManagedWriter, Self::Error> {
        ManagedWriter::open(&self.0.root)
    }
}

/// Stage the selected writable projects without opening roots or journals.
/// The host creates and retains one writer per configured project after freeze.
#[cfg(all(unix, feature = "managed-writes"))]
pub fn register_managed_writers(
    id: impl Into<String>,
    configs: BTreeMap<ProjectAlias, ManagedWriterConfig>,
) -> Result<PreparedGroup<BTreeMap<ProjectAlias, ManagedWriterFactory>>, AssemblyError> {
    if configs.len() > 128 {
        return Err(AssemblyError::Configuration(InspectError::Configuration(
            "managed writer catalog supports at most 128 aliases",
        )));
    }
    for config in configs.values() {
        validate_root(&config.root)?;
    }
    let id = id.into();
    let factories = configs
        .into_iter()
        .map(|(alias, config)| (alias, ManagedWriterFactory(config)))
        .collect();
    PreparedGroup::new(id.clone(), &[&id], &[], factories).map_err(AssemblyError::Registration)
}
