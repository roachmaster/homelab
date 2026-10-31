//! Typed, effect-free registration followed by consuming validation/freeze.
//!
//! A registry owns both metadata and a nested tuple of complete factory groups.
//! It never constructs a client, resolves a service or executes a dependency graph.
//! Configured registrars are trusted code: their lack of I/O must be tested, not
//! assumed to be enforced by Rust. Panics/allocation failure are not rolled back.
use std::{collections::BTreeMap, error::Error, fmt};

/// Controlled metadata rejection, independent of application construction errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationError {
    InvalidIdentifier(&'static str),
    EmptyGroup,
    DuplicateProvided(String),
    DuplicateRequirement(String),
    DuplicateGroup(String),
    BindingConflict { binding: String, owner: String },
}
impl fmt::Display for RegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentifier(field) => write!(f, "invalid {field} identifier"),
            Self::EmptyGroup => write!(f, "a factory group must provide at least one binding"),
            Self::DuplicateProvided(id) => write!(f, "duplicate provided binding {id}"),
            Self::DuplicateRequirement(id) => write!(f, "duplicate requirement {id}"),
            Self::DuplicateGroup(id) => write!(f, "factory group {id} is already registered"),
            Self::BindingConflict { binding, owner } => {
                write!(f, "binding {binding} already belongs to group {owner}")
            }
        }
    }
}
impl Error for RegistrationError {}

/// Validation checks the order the application declares; it does not reorder it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    MissingRequirement {
        group: String,
        binding: String,
    },
    SelfDependency {
        group: String,
        binding: String,
    },
    DependencyOrder {
        group: String,
        binding: String,
        provider: String,
    },
}
impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRequirement { group, binding } => {
                write!(f, "group {group} requires missing binding {binding}")
            }
            Self::SelfDependency { group, binding } => {
                write!(
                    f,
                    "group {group} declares its own binding {binding} as an external dependency"
                )
            }
            Self::DependencyOrder {
                group,
                binding,
                provider,
            } => {
                write!(
                    f,
                    "group {provider} providing {binding} must precede group {group}"
                )
            }
        }
    }
}
impl Error for ValidationError {}

/// Immutable diagnostics supplied with the same typed group as its recipes.
///
/// Binding identifiers are assembly metadata, not ingress destinations or type
/// lookup keys. Requirements name bindings from *earlier groups*; dependencies
/// between factories inside one cohesive group remain that group's typed recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupInfo {
    id: String,
    provides: Vec<String>,
    requires: Vec<String>,
}
impl GroupInfo {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn provides(&self) -> &[String] {
        &self.provides
    }
    pub fn requires(&self) -> &[String] {
        &self.requires
    }
}

fn valid_identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn identifiers(
    values: &[&str],
    field: &'static str,
    provided: bool,
) -> Result<Vec<String>, RegistrationError> {
    let mut result = Vec::with_capacity(values.len());
    for value in values {
        if !valid_identifier(value) {
            return Err(RegistrationError::InvalidIdentifier(field));
        }
        if result.iter().any(|old| old == value) {
            return Err(if provided {
                RegistrationError::DuplicateProvided((*value).into())
            } else {
                RegistrationError::DuplicateRequirement((*value).into())
            });
        }
        result.push((*value).into());
    }
    Ok(result)
}

/// One complete cohesive set of configured factories/recipes. No factory call
/// occurs here. The registry imposes no erased factory or provider interface.
///
/// Values are deliberately inaccessible through this wrapper before freeze.
/// A caller can still retain its own handle/clone before staging: this API is a
/// construction protocol for trusted code, not a security or effect sandbox.
pub struct PreparedGroup<F> {
    info: GroupInfo,
    factories: F,
}
impl<F> PreparedGroup<F> {
    pub fn new(
        id: impl Into<String>,
        provides: &[&str],
        requires: &[&str],
        factories: F,
    ) -> Result<Self, RegistrationError> {
        let id = id.into();
        if !valid_identifier(&id) {
            return Err(RegistrationError::InvalidIdentifier("group"));
        }
        if provides.is_empty() {
            return Err(RegistrationError::EmptyGroup);
        }
        Ok(Self {
            info: GroupInfo {
                id,
                provides: identifiers(provides, "provided binding", true)?,
                requires: identifiers(requires, "required binding", false)?,
            },
            factories,
        })
    }
    pub fn info(&self) -> &GroupInfo {
        &self.info
    }
}
impl<F> fmt::Debug for PreparedGroup<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedGroup")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

/// An owned pending registry. Each registration changes its tuple type; ordinary
/// Rust composition selects which registrars are invoked. No `Any` or global
/// business enum is needed for unrelated factory-group types.
///
/// ```
/// use forge_bootstrap::{PreparedGroup, Registry};
/// let registry = Registry::new()
///     .register(PreparedGroup::new("numbers", &["number"], &[], || 7).unwrap()).unwrap()
///     .register(PreparedGroup::new("text", &["text"], &["number"], || "ready").unwrap()).unwrap();
/// let (((), number_factory), text_factory) = registry.freeze().unwrap().into_factories();
/// assert_eq!(number_factory(), 7);
/// assert_eq!(text_factory(), "ready");
/// ```
///
/// Factories cannot be extracted from a pending registry:
/// ```compile_fail
/// use forge_bootstrap::Registry;
/// let pending = Registry::new();
/// pending.into_factories();
/// ```
pub struct Registry<G = ()> {
    groups: Vec<GroupInfo>,
    bindings: BTreeMap<String, usize>,
    factories: G,
}
impl Registry<()> {
    pub fn new() -> Self {
        Self {
            groups: Vec::new(),
            bindings: BTreeMap::new(),
            factories: (),
        }
    }
}
impl Default for Registry<()> {
    fn default() -> Self {
        Self::new()
    }
}
impl<G> Registry<G> {
    pub fn groups(&self) -> &[GroupInfo] {
        &self.groups
    }
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }
    pub fn binding_count(&self) -> usize {
        self.bindings.len()
    }
    pub fn contains_binding(&self, id: &str) -> bool {
        self.bindings.contains_key(id)
    }

    /// Check *all* claims before committing metadata or the typed group.
    /// Rejection owns the unchanged original registry and the rejected group.
    pub fn register<F>(
        mut self,
        group: PreparedGroup<F>,
    ) -> Result<Registry<(G, F)>, Rejected<G, F>> {
        let error = if self.groups.iter().any(|old| old.id == group.info.id) {
            Some(RegistrationError::DuplicateGroup(group.info.id.clone()))
        } else {
            group.info.provides.iter().find_map(|binding| {
                self.bindings
                    .get(binding)
                    .map(|index| RegistrationError::BindingConflict {
                        binding: binding.clone(),
                        owner: self.groups[*index].id.clone(),
                    })
            })
        };
        if let Some(reason) = error {
            return Err(Rejected {
                state: Box::new(RejectedState {
                    original: self,
                    group,
                    reason,
                }),
            });
        }
        let PreparedGroup { info, factories } = group;
        let index = self.groups.len();
        for binding in &info.provides {
            self.bindings.insert(binding.clone(), index);
        }
        self.groups.push(info);
        Ok(Registry {
            groups: self.groups,
            bindings: self.bindings,
            factories: (self.factories, factories),
        })
    }

    /// Consume the pending plan only after its selected requirements and explicit
    /// order validate. No resources are created. An empty assembly is permitted.
    pub fn freeze(self) -> Result<Frozen<G>, FreezeRejected<G>> {
        if let Some(reason) = self.validation_error() {
            return Err(FreezeRejected {
                state: Box::new(FreezeRejectedState {
                    original: self,
                    reason,
                }),
            });
        }
        Ok(Frozen {
            groups: self.groups,
            factories: self.factories,
        })
    }

    fn validation_error(&self) -> Option<ValidationError> {
        for (index, group) in self.groups.iter().enumerate() {
            for binding in &group.requires {
                match self.bindings.get(binding) {
                    None => {
                        return Some(ValidationError::MissingRequirement {
                            group: group.id.clone(),
                            binding: binding.clone(),
                        })
                    }
                    Some(owner) if *owner == index => {
                        return Some(ValidationError::SelfDependency {
                            group: group.id.clone(),
                            binding: binding.clone(),
                        })
                    }
                    Some(owner) if *owner > index => {
                        return Some(ValidationError::DependencyOrder {
                            group: group.id.clone(),
                            binding: binding.clone(),
                            provider: self.groups[*owner].id.clone(),
                        })
                    }
                    Some(_) => {}
                }
            }
        }
        None
    }
}
impl<G> fmt::Debug for Registry<G> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registry")
            .field("groups", &self.groups)
            .finish_non_exhaustive()
    }
}

/// Successful metadata/order validation, not a running application. Assembly is
/// explicit typed code after extracting the groups; it must preserve the declared
/// order and publish nothing on failure. Constructor types enforce actual types;
/// metadata cannot prove arbitrary recipe code follows its declarations.
///
/// A frozen plan has no registration method:
/// ```compile_fail
/// use forge_bootstrap::{PreparedGroup, Registry};
/// let frozen = Registry::new().freeze().unwrap();
/// frozen.register(PreparedGroup::new("new", &["new"], &[], ()).unwrap());
/// ```
///
/// Freeze consumes the pending builder:
/// ```compile_fail
/// use forge_bootstrap::Registry;
/// let registry = Registry::new();
/// let frozen = registry.freeze().unwrap();
/// let _ = registry.group_count();
/// ```
pub struct Frozen<G> {
    groups: Vec<GroupInfo>,
    factories: G,
}
impl<G> Frozen<G> {
    pub fn groups(&self) -> &[GroupInfo] {
        &self.groups
    }
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }
    pub fn binding_count(&self) -> usize {
        self.groups.iter().map(|g| g.provides.len()).sum()
    }
    pub fn into_factories(self) -> G {
        self.factories
    }
}
impl<G> fmt::Debug for Frozen<G> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Frozen")
            .field("groups", &self.groups)
            .finish_non_exhaustive()
    }
}

/// A non-mutating failed group commit. No factory/debug bound is required.
pub struct Rejected<G, F> {
    // Allocation happens only on failure; potentially large typed factory tuples
    // do not inflate every successful Result's stack representation.
    state: Box<RejectedState<G, F>>,
}
struct RejectedState<G, F> {
    original: Registry<G>,
    group: PreparedGroup<F>,
    reason: RegistrationError,
}
impl<G, F> Rejected<G, F> {
    pub fn reason(&self) -> &RegistrationError {
        &self.state.reason
    }
    pub fn original(&self) -> &Registry<G> {
        &self.state.original
    }
    pub fn into_parts(self) -> (Registry<G>, PreparedGroup<F>, RegistrationError) {
        let state = *self.state;
        (state.original, state.group, state.reason)
    }
}
impl<G, F> fmt::Debug for Rejected<G, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.state.reason.fmt(f)
    }
}
impl<G, F> fmt::Display for Rejected<G, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.state.reason.fmt(f)
    }
}
impl<G, F> Error for Rejected<G, F> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.state.reason)
    }
}

/// Validation failure retains the complete unmodified pending registry.
pub struct FreezeRejected<G> {
    state: Box<FreezeRejectedState<G>>,
}
struct FreezeRejectedState<G> {
    original: Registry<G>,
    reason: ValidationError,
}
impl<G> FreezeRejected<G> {
    pub fn reason(&self) -> &ValidationError {
        &self.state.reason
    }
    pub fn original(&self) -> &Registry<G> {
        &self.state.original
    }
    pub fn into_parts(self) -> (Registry<G>, ValidationError) {
        let state = *self.state;
        (state.original, state.reason)
    }
}
impl<G> fmt::Debug for FreezeRejected<G> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.state.reason.fmt(f)
    }
}
impl<G> fmt::Display for FreezeRejected<G> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.state.reason.fmt(f)
    }
}
impl<G> Error for FreezeRejected<G> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.state.reason)
    }
}
