//! A port's binding inside one environment.

use crate::{FoundationError, ProviderRef};
use std::sync::Arc;

/// What an environment bound to a port.
///
/// `Defaulted` is the null-object rule: an optional collaborator nobody bound
/// is still a real, empty implementation, so dependents never see `Option`.
/// `Unbound` only exists for ports with no default; asking it for a provider is
/// an error, and freezing the environment already rejects any dependent that
/// required it.
pub enum Slot<T: ?Sized> {
    Bound(ProviderRef<T>),
    Defaulted(ProviderRef<T>),
    Unbound(&'static str),
}

impl<T: ?Sized> Slot<T> {
    pub fn bound(provider: ProviderRef<T>) -> Self {
        Self::Bound(provider)
    }

    pub fn defaulted(provider: ProviderRef<T>) -> Self {
        Self::Defaulted(provider)
    }

    pub fn unbound(port: &'static str) -> Self {
        Self::Unbound(port)
    }

    /// The provider a dependent should hold.
    pub fn provider(&self) -> Result<ProviderRef<T>, FoundationError> {
        match self {
            Self::Bound(provider) | Self::Defaulted(provider) => Ok(Arc::clone(provider)),
            Self::Unbound(port) => Err(FoundationError::Unbound { port }),
        }
    }

    pub fn is_default(&self) -> bool {
        matches!(self, Self::Defaulted(_))
    }

    pub fn is_bound(&self) -> bool {
        matches!(self, Self::Bound(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SingletonProvider;

    trait Tag: Send + Sync {
        fn read(&self) -> String;
    }
    struct Empty;
    impl Tag for Empty {
        fn read(&self) -> String {
            String::new()
        }
    }
    fn empty() -> Result<Arc<dyn Tag>, FoundationError> {
        Ok(Arc::new(Empty))
    }

    #[test]
    fn a_defaulted_slot_still_provides_the_empty_implementation() {
        let slot: Slot<dyn Tag> =
            Slot::defaulted(Arc::new(SingletonProvider::from_factory(&empty).unwrap()));
        assert!(slot.is_default() && !slot.is_bound());
        assert_eq!(slot.provider().unwrap().provide().unwrap().read(), "");
    }

    #[test]
    fn an_unbound_slot_names_its_port() {
        let slot: Slot<dyn Tag> = Slot::unbound("Tag");
        assert_eq!(
            slot.provider().err(),
            Some(FoundationError::Unbound { port: "Tag" })
        );
    }
}
