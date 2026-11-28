//! Per-environment containers: stage, freeze, then construct.

use crate::FoundationError;
use forge_core::bootstrap::{GroupInfo, PreparedGroup, Registry};

/// A container type that every environment builds through the same step.
///
/// Implementations stage one `forge-core` group per bound port with
/// [`EnvBuilder`], freeze it, and only then construct providers. A frozen plan
/// is proof that every required collaborator was bound before anything ran.
pub trait Assemble: Sized {
    type Env: Copy;
    fn assemble(env: Self::Env) -> Result<Self, FoundationError>;
}

/// Typed staging over `forge_core::bootstrap::Registry`.
///
/// Each staged group provides one binding and names the bindings it requires.
/// The registry's validation is reused unchanged: a requirement nobody provides
/// fails as `MissingRequirement`, and groups must be staged dependencies first.
pub struct EnvBuilder<G> {
    name: &'static str,
    registry: Registry<G>,
}

impl EnvBuilder<()> {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            registry: Registry::new(),
        }
    }
}

impl<G> EnvBuilder<G> {
    /// Stage the group that provides `binding` and requires `requires`.
    pub fn stage(
        self,
        binding: &str,
        requires: &[&str],
    ) -> Result<EnvBuilder<(G, ())>, FoundationError> {
        let group = PreparedGroup::new(binding, &[binding], requires, ())?;
        Ok(EnvBuilder {
            name: self.name,
            registry: self.registry.register(group)?,
        })
    }

    /// Validate the plan. Nothing has been constructed yet.
    pub fn freeze(self) -> Result<FrozenEnv, FoundationError> {
        let frozen = self.registry.freeze()?;
        Ok(FrozenEnv {
            name: self.name,
            groups: frozen.groups().to_vec(),
        })
    }
}

/// A validated environment plan, ready for construction.
#[derive(Debug, Clone)]
pub struct FrozenEnv {
    name: &'static str,
    groups: Vec<GroupInfo>,
}

impl FrozenEnv {
    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn bindings(&self) -> impl Iterator<Item = &str> {
        self.groups.iter().map(|group| group.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::bootstrap::{RegistrationError, ValidationError};

    #[test]
    fn a_complete_plan_freezes_in_dependency_order() {
        let frozen = EnvBuilder::new("prod")
            .stage("collar", &[])
            .unwrap()
            .stage("dog", &["collar"])
            .unwrap()
            .freeze()
            .unwrap();
        assert_eq!(frozen.name(), "prod");
        assert_eq!(frozen.bindings().collect::<Vec<_>>(), ["collar", "dog"]);
    }

    #[test]
    fn a_missing_requirement_fails_at_freeze() {
        let error = EnvBuilder::new("lonely")
            .stage("kennel", &["dog"])
            .unwrap()
            .freeze()
            .unwrap_err();
        assert_eq!(error.missing_binding(), Some("dog"));
        assert!(matches!(
            error,
            FoundationError::Freeze(ValidationError::MissingRequirement { .. })
        ));
    }

    #[test]
    fn a_port_cannot_be_bound_twice() {
        let error = EnvBuilder::new("prod")
            .stage("dog", &[])
            .unwrap()
            .stage("dog", &[])
            .err()
            .unwrap();
        assert!(matches!(
            error,
            FoundationError::Registration(RegistrationError::DuplicateGroup(_))
        ));
    }
}
