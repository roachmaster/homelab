use forge_bootstrap::{PreparedGroup, RegistrationError, Registry, ValidationError};
use std::{cell::Cell, error::Error, rc::Rc};

#[test]
fn heterogeneous_groups_remain_typed_owned_and_unconstructed_until_extraction() {
    let calls = Rc::new(Cell::new(0));
    let counted = calls.clone();
    let first = move || {
        counted.set(counted.get() + 1);
        42
    };
    let text = String::from("borrowed config");
    let second = || text.as_str(); // Non-static factories are supported.
    let registry = Registry::new()
        .register(PreparedGroup::new("first", &["answer", "version"], &[], first).unwrap())
        .unwrap()
        .register(PreparedGroup::new("second", &["message"], &["answer"], second).unwrap())
        .unwrap();
    assert_eq!(registry.group_count(), 2);
    assert_eq!(registry.binding_count(), 3);
    assert_eq!(registry.groups()[1].requires(), &["answer"]);
    assert_eq!(calls.get(), 0);
    let frozen = registry.freeze().unwrap();
    assert_eq!(frozen.group_count(), 2);
    assert_eq!(frozen.binding_count(), 3);
    assert_eq!(frozen.groups()[0].provides(), &["answer", "version"]);
    assert_eq!(calls.get(), 0);
    let (((), first), second) = frozen.into_factories();
    assert_eq!(first(), 42);
    assert_eq!(second(), "borrowed config");
    assert_eq!(calls.get(), 1);
}

#[test]
fn late_conflict_rejects_the_entire_multi_factory_group_without_overwrite() {
    let calls = Cell::new(0);
    let registry = Registry::new()
        .register(
            PreparedGroup::new("original", &["taken"], &[], || {
                calls.set(calls.get() + 1);
                "original"
            })
            .unwrap(),
        )
        .unwrap();
    let before = registry.groups().to_vec();
    // The first two claims would succeed individually; the final one conflicts.
    let incoming = PreparedGroup::new(
        "new",
        &["new.a", "new.b", "taken"],
        &[],
        (|| "a", || "b", || "replacement"),
    )
    .unwrap();
    let rejected = registry.register(incoming).unwrap_err();
    assert_eq!(rejected.original().groups(), before.as_slice());
    assert_eq!(rejected.original().binding_count(), 1);
    assert!(!rejected.original().contains_binding("new.a"));
    assert!(!rejected.original().contains_binding("new.b"));
    assert_eq!(
        rejected.reason(),
        &RegistrationError::BindingConflict {
            binding: "taken".into(),
            owner: "original".into(),
        }
    );
    assert_eq!(calls.get(), 0);
    let (original, incoming, reason) = rejected.into_parts();
    assert!(reason.to_string().contains("original"));
    let ((), factory) = original.freeze().unwrap().into_factories();
    assert_eq!(factory(), "original");
    // The rejected typed group is retained, and may be registered elsewhere.
    let ((), (a, b, c)) = Registry::new()
        .register(incoming)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    assert_eq!((a(), b(), c()), ("a", "b", "replacement"));
}

#[test]
fn duplicate_group_id_preserves_the_first_group_even_with_disjoint_bindings() {
    let original = Registry::new()
        .register(PreparedGroup::new("same", &["a"], &[], 7).unwrap())
        .unwrap();
    let error = original
        .register(PreparedGroup::new("same", &["b"], &[], 99).unwrap())
        .unwrap_err();
    assert_eq!(
        error.reason(),
        &RegistrationError::DuplicateGroup("same".into())
    );
    assert!(!error.original().contains_binding("b"));
    let (original, _, _) = error.into_parts();
    assert_eq!(original.freeze().unwrap().into_factories(), ((), 7));
}

#[test]
fn a_failed_attempt_can_be_followed_by_a_valid_group_without_stale_claims() {
    let registry = Registry::new()
        .register(PreparedGroup::new("one", &["a"], &[], ()).unwrap())
        .unwrap();
    let rejected = registry
        .register(PreparedGroup::new("bad", &["free", "a"], &[], ()).unwrap())
        .unwrap_err();
    let (registry, _, _) = rejected.into_parts();
    let registry = registry
        .register(PreparedGroup::new("good", &["free"], &["a"], ()).unwrap())
        .unwrap();
    assert_eq!(registry.freeze().unwrap().group_count(), 2);
}

#[test]
fn group_local_validation_rejects_duplicates_without_calling_recipes() {
    let calls = Cell::new(0);
    let error = PreparedGroup::new("bad", &["same", "same"], &[], || calls.set(1)).unwrap_err();
    assert_eq!(error, RegistrationError::DuplicateProvided("same".into()));
    let error =
        PreparedGroup::new("bad", &["one"], &["needed", "needed"], || calls.set(1)).unwrap_err();
    assert_eq!(
        error,
        RegistrationError::DuplicateRequirement("needed".into())
    );
    assert_eq!(calls.get(), 0);
}

#[test]
fn identifiers_are_bounded_and_nonempty_without_exposing_invalid_values() {
    for id in ["", "with spaces", "path/file", "unicode-λ", "line\nfeed"] {
        let error = PreparedGroup::new(id, &["valid"], &[], ()).unwrap_err();
        assert_eq!(error, RegistrationError::InvalidIdentifier("group"));
        assert_eq!(error.to_string(), "invalid group identifier");
    }
    assert!(PreparedGroup::new("a".repeat(256), &["a.b_c-9"], &[], ()).is_ok());
    assert!(PreparedGroup::new("a".repeat(257), &["valid"], &[], ()).is_err());
    assert_eq!(
        PreparedGroup::new("valid", &[], &[], ()).unwrap_err(),
        RegistrationError::EmptyGroup
    );
    assert_eq!(
        PreparedGroup::new("valid", &["bad value"], &[], ()).unwrap_err(),
        RegistrationError::InvalidIdentifier("provided binding")
    );
    assert_eq!(
        PreparedGroup::new("valid", &["good"], &[""], ()).unwrap_err(),
        RegistrationError::InvalidIdentifier("required binding")
    );
}

#[test]
fn missing_requirement_fails_freeze_and_retains_pending_metadata() {
    let calls = Cell::new(0);
    let registry = Registry::new()
        .register(PreparedGroup::new("consumer", &["task"], &["client"], || calls.set(1)).unwrap())
        .unwrap();
    let rejected = registry.freeze().unwrap_err();
    assert_eq!(
        rejected.reason(),
        &ValidationError::MissingRequirement {
            group: "consumer".into(),
            binding: "client".into(),
        }
    );
    assert_eq!(rejected.original().group_count(), 1);
    let (registry, _) = rejected.into_parts();
    assert!(registry.contains_binding("task"));
    assert_eq!(calls.get(), 0);
}

#[test]
fn forward_dependency_is_rejected_instead_of_silently_reordering() {
    let registry = Registry::new()
        .register(PreparedGroup::new("consumer", &["task"], &["client"], ()).unwrap())
        .unwrap()
        .register(PreparedGroup::new("provider", &["client"], &[], ()).unwrap())
        .unwrap();
    assert_eq!(
        registry.freeze().unwrap_err().reason(),
        &ValidationError::DependencyOrder {
            group: "consumer".into(),
            binding: "client".into(),
            provider: "provider".into(),
        }
    );
}

#[test]
fn self_and_cyclic_external_dependencies_cannot_freeze() {
    let self_dependent = Registry::new()
        .register(PreparedGroup::new("self", &["a"], &["a"], ()).unwrap())
        .unwrap();
    assert_eq!(
        self_dependent.freeze().unwrap_err().reason(),
        &ValidationError::SelfDependency {
            group: "self".into(),
            binding: "a".into(),
        }
    );
    let cyclic = Registry::new()
        .register(PreparedGroup::new("a", &["a"], &["b"], ()).unwrap())
        .unwrap()
        .register(PreparedGroup::new("b", &["b"], &["a"], ()).unwrap())
        .unwrap();
    assert!(matches!(
        cyclic.freeze().unwrap_err().reason(),
        ValidationError::DependencyOrder { .. }
    ));
}

#[test]
fn freeze_validates_every_group_before_any_factory_can_be_extracted() {
    let calls = Cell::new(0);
    let registry = Registry::new()
        .register(PreparedGroup::new("valid", &["one"], &[], || calls.set(1)).unwrap())
        .unwrap()
        .register(PreparedGroup::new("invalid", &["two"], &["missing"], || calls.set(2)).unwrap())
        .unwrap();
    assert!(registry.freeze().is_err());
    assert_eq!(calls.get(), 0);
}

#[test]
fn errors_preserve_causes_without_debugging_private_factory_state() {
    let registry = Registry::new()
        .register(PreparedGroup::new("one", &["one"], &[], "not in diagnostics").unwrap())
        .unwrap();
    let error = registry
        .register(PreparedGroup::new("one", &["two"], &[], || 1).unwrap())
        .unwrap_err();
    assert!(error.source().unwrap().is::<RegistrationError>());
    assert!(!format!("{error:?}").contains("not in diagnostics"));
    let invalid = Registry::new()
        .register(PreparedGroup::new("bad", &["bad"], &["absent"], ()).unwrap())
        .unwrap()
        .freeze()
        .unwrap_err();
    assert!(invalid.source().unwrap().is::<ValidationError>());
}

#[test]
fn empty_registry_can_freeze_for_an_application_with_no_groups() {
    let frozen = Registry::default().freeze().unwrap();
    assert_eq!(frozen.group_count(), 0);
    assert_eq!(frozen.binding_count(), 0);
    let () = frozen.into_factories();
}
