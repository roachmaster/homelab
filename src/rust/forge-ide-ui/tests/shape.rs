use forge_ide_ui::{protocol::*, shape::*, UiError};

fn entry(id: u32, parent: Option<u32>, name: &str, kind: EntryKind) -> EntryView {
    EntryView {
        id,
        parent,
        name: NativeName::Utf8(name.into()),
        kind,
        size_bytes: (kind == EntryKind::File).then_some(1),
        lines: (kind == EntryKind::File).then_some(1),
        content_status: if kind == EntryKind::File { "text" } else { "" }.into(),
        diagnostics: vec![],
        declarations: vec![],
    }
}
fn declaration(id: u32, parent: Option<u32>, kind: DeclarationKind, name: &str) -> DeclarationView {
    DeclarationView {
        id,
        parent,
        kind,
        name: name.into(),
        span: SourceSpan {
            start: Position { line: 1, column: 0 },
            end: Position { line: 2, column: 0 },
        },
        module_inline: (kind == DeclarationKind::Module).then_some(true),
        implementation: None,
    }
}
fn implementation(id: u32, parent: Option<u32>, role: Option<&str>, target: &str) -> DeclarationView {
    let mut declaration = declaration(id, parent, DeclarationKind::Impl, target);
    declaration.implementation = Some(Implementation {
        target: target.into(),
        trait_path: role.map(str::to_owned),
        negative: false,
        is_unsafe: false,
    });
    declaration
}
fn report(entries: Vec<EntryView>, complete: bool) -> ReportView {
    ReportView {
        format_version: 1,
        complete,
        files: entries.iter().filter(|e| e.kind == EntryKind::File).count() as u32,
        bytes_read: 0,
        entries,
    }
}
fn simple() -> Vec<DeclarationView> {
    vec![
        declaration(0, None, DeclarationKind::Trait, "Role"),
        declaration(1, None, DeclarationKind::Struct, "First"),
        declaration(2, None, DeclarationKind::Struct, "Second"),
        implementation(3, None, Some("Role"), "First"),
        implementation(4, None, Some("Role"), "Second"),
    ]
}
fn root_file(declarations: Vec<DeclarationView>) -> ReportView {
    let mut source = entry(2, Some(0), "lib.rs", EntryKind::File);
    source.declarations = declarations;
    report(vec![
        entry(0, None, ".", EntryKind::Directory),
        entry(1, Some(0), "Cargo.toml", EntryKind::File),
        source,
    ], true)
}

#[test]
fn one_trait_two_named_header_candidates_and_repeated_blocks_are_distinct_facts() {
    let mut declarations = vec![
        declaration(0, None, DeclarationKind::Trait, "Role"),
        declaration(1, Some(0), DeclarationKind::TraitMethod, "run"),
        declaration(2, None, DeclarationKind::Struct, "First"),
        declaration(3, None, DeclarationKind::Struct, "Second"),
        implementation(4, None, Some("Role"), "First"),
        declaration(5, Some(4), DeclarationKind::ImplMethod, "run"),
        implementation(6, None, Some("Role"), "Second"),
        implementation(7, None, Some("Role"), "First"),
        implementation(8, None, None, "Second"),
        implementation(9, None, Some("Role"), "Second"),
        declaration(10, None, DeclarationKind::Function, "build"),
        declaration(11, None, DeclarationKind::Enum, "State"),
    ];
    declarations[9].implementation.as_mut().unwrap().negative = true;
    let shape = SourceShape::from_report(&root_file(declarations)).unwrap();
    let group = &shape.groups[1];
    assert_eq!(group.manifest, Some(1));
    assert_eq!(shape.entry_groups, vec![1, 1, 1]);
    assert_eq!(group.counts, ShapeCounts {
        rust_files: 1, traits: 1, structs: 2, enums: 1, free_functions: 1,
        methods: 2, trait_impls: 4, inherent_impls: 1, negative_impls: 1,
        candidate_structs: 2,
    });
    assert_eq!(group.traits[0].impl_blocks, 3);
    assert_eq!(group.traits[0].candidate_structs, vec![
        SymbolLocation { entry: 2, declaration: 2 },
        SymbolLocation { entry: 2, declaration: 3 },
    ]);
    assert_eq!(group.impls[4].disposition, ImplDisposition::Negative);
}

#[test]
fn module_file_and_ambiguous_names_never_collapse_to_one_symbol() {
    let declarations = vec![
        declaration(0, None, DeclarationKind::Trait, "Role"),
        declaration(1, None, DeclarationKind::Struct, "First"),
        implementation(2, None, Some("Role"), "First"),
        declaration(3, None, DeclarationKind::Module, "left"),
        declaration(4, Some(3), DeclarationKind::Trait, "Role"),
        declaration(5, Some(3), DeclarationKind::Struct, "First"),
        implementation(6, Some(3), Some("Role"), "First"),
        declaration(7, None, DeclarationKind::Module, "right"),
        declaration(8, Some(7), DeclarationKind::Trait, "Role"),
        declaration(9, Some(7), DeclarationKind::Struct, "First"),
        implementation(10, Some(7), Some("Role"), "First"),
        declaration(11, None, DeclarationKind::Trait, "Role"),
    ];
    let mut report = root_file(declarations);
    let mut other = entry(3, Some(0), "other.rs", EntryKind::File);
    other.declarations = simple();
    report.entries.push(other);
    report.files += 1;
    let shape = SourceShape::from_report(&report).unwrap();
    let group = &shape.groups[1];
    assert_eq!(group.impls[0].disposition, ImplDisposition::Unresolved);
    assert!(group.traits[0].candidate_structs.is_empty());
    assert_eq!(group.traits[1].candidate_structs[0], SymbolLocation { entry: 2, declaration: 5 });
    assert_eq!(group.traits[2].candidate_structs[0], SymbolLocation { entry: 2, declaration: 9 });
    assert_eq!(group.traits[4].candidate_structs.len(), 2);
    assert_eq!(group.counts.candidate_structs, 4);
}

#[test]
fn generic_qualified_negative_alias_and_missing_metadata_are_not_proven_relations() {
    let mut declarations = simple();
    declarations.extend([
        implementation(5, None, Some("Role < T >"), "First < T >"),
        implementation(6, None, Some("Role"), "T"),
        implementation(7, None, Some("Role"), "& First"),
        implementation(8, None, Some("crate :: Role"), "First"),
        implementation(9, None, Some("outside :: Role"), "First"),
        declaration(10, None, DeclarationKind::TypeAlias, "Alias"),
        implementation(11, None, Some("Role"), "Alias"),
        declaration(12, None, DeclarationKind::Impl, "missing metadata"),
    ]);
    let shape = SourceShape::from_report(&root_file(declarations)).unwrap();
    let kinds: Vec<_> = shape.groups[1].impls.iter().map(|i| i.disposition).collect();
    assert!(matches!(kinds[0], ImplDisposition::LocalHeaderCandidate { .. }));
    assert_eq!(&kinds[2..], &[
        ImplDisposition::GenericOrComplex, ImplDisposition::Unresolved,
        ImplDisposition::GenericOrComplex, ImplDisposition::QualifiedPath,
        ImplDisposition::QualifiedPath, ImplDisposition::Unresolved,
        ImplDisposition::Unresolved,
    ]);
    assert_eq!(shape.groups[1].counts.inherent_impls, 0);
}

#[test]
fn bare_generic_shadowing_is_only_a_tentative_candidate_because_parameters_are_absent() {
    // Existing wire metadata for `impl<T> Role for T {}` has no generic params.
    // A same-named struct must never turn this into a proven relation.
    let report = root_file(vec![
        declaration(0, None, DeclarationKind::Trait, "Role"),
        declaration(1, None, DeclarationKind::Struct, "T"),
        implementation(2, None, Some("Role"), "T"),
    ]);
    let shape = SourceShape::from_report(&report).unwrap();
    assert!(matches!(shape.groups[1].impls[0].disposition,
        ImplDisposition::LocalHeaderCandidate { .. }));
}

#[test]
fn nested_possible_manifests_partition_sources_even_when_unreadable_or_virtual() {
    let mut outer = entry(2, Some(0), "lib.rs", EntryKind::File);
    outer.declarations = simple();
    let mut inner = entry(5, Some(3), "lib.rs", EntryKind::File);
    inner.declarations = simple();
    let report = report(vec![
        entry(0, None, ".", EntryKind::Directory),
        entry(1, Some(0), "Cargo.toml", EntryKind::File),
        outer,
        entry(3, Some(0), "nested", EntryKind::Directory),
        entry(4, Some(3), "Cargo.toml", EntryKind::Unreadable),
        inner,
    ], false);
    let shape = SourceShape::from_report(&report).unwrap();
    assert!(shape.report_incomplete);
    assert_eq!(shape.entry_groups, vec![1, 1, 1, 2, 2, 2]);
    assert_eq!(shape.groups[1].entries, vec![2]);
    assert_eq!(shape.groups[2].entries, vec![5]);
    assert_eq!(shape.groups[2].opaque_entries, 1);
    // No package or crate name is invented, even for a readable Cargo.toml.
    assert_eq!(shape.groups[1].manifest, Some(1));
}

#[test]
fn failed_rust_and_opaque_scope_are_unknown_not_successful_empty_files() {
    let mut failed = entry(2, Some(0), "broken.rs", EntryKind::File);
    failed.content_status = "rust_unavailable".into();
    let mut binary = entry(3, Some(0), "binary.rs", EntryKind::File);
    binary.content_status = "binary".into();
    binary.lines = None;
    let mut large = entry(4, Some(0), "large.rs", EntryKind::File);
    large.content_status = "too_large".into();
    large.lines = None;
    let report = report(vec![
        entry(0, None, ".", EntryKind::Directory),
        entry(1, Some(0), "Cargo.toml", EntryKind::File),
        failed, binary, large,
        entry(5, Some(0), "vendor", EntryKind::Excluded),
        entry(6, Some(0), "link", EntryKind::Symlink),
        entry(7, Some(0), "hidden", EntryKind::Unreadable),
    ], false);
    let shape = SourceShape::from_report(&report).unwrap();
    assert_eq!(shape.groups[1].counts.rust_files, 3);
    assert_eq!(shape.groups[1].unavailable_rust, 3);
    assert_eq!(shape.groups[1].opaque_entries, 3);
    assert!(shape.groups[1].traits.is_empty());
    assert!(shape.report_incomplete);
}

#[test]
fn no_manifest_directory_named_manifest_and_encoded_names_are_handled_honestly() {
    let mut source = entry(2, Some(0), "lib.rs", EntryKind::File);
    source.declarations = simple();
    let unassigned = report(vec![
        entry(0, None, ".", EntryKind::Directory),
        entry(1, Some(0), "Cargo.toml", EntryKind::Directory),
        source,
    ], true);
    let shape = SourceShape::from_report(&unassigned).unwrap();
    assert_eq!(shape.groups.len(), 1);
    assert_eq!(shape.groups[0].entries, vec![2]);
    for name in [
        NativeName::UnixBytes(b"Cargo.toml".to_vec()),
        NativeName::WindowsWide("Cargo.toml".encode_utf16().collect()),
    ] {
        let mut report = root_file(simple());
        report.entries[1].name = name;
        assert_eq!(SourceShape::from_report(&report).unwrap().groups.len(), 2);
    }
    let mut opaque = root_file(simple());
    opaque.entries[1].name = NativeName::EncodedBytes(b"Cargo.toml".to_vec());
    let shape = SourceShape::from_report(&opaque).unwrap();
    assert_eq!(shape.groups.len(), 1);
    assert_eq!(shape.groups[0].opaque_entries, 1);
}

#[test]
fn invalid_report_is_rejected_before_indexing_and_limit_checks_are_preserved() {
    let mut invalid = root_file(simple());
    invalid.entries[2].parent = Some(99);
    assert_eq!(SourceShape::from_report(&invalid), Err(UiError::InvalidReport));
    let mut invalid = root_file(simple());
    invalid.entries[2].declarations[0].name = "x".repeat(MAX_TEXT_BYTES + 1);
    assert_eq!(SourceShape::from_report(&invalid), Err(UiError::Limit));
}

#[test]
fn trait_only_source_is_visible_without_an_architectural_verdict() {
    let shape = SourceShape::from_report(&root_file(vec![
        declaration(0, None, DeclarationKind::Trait, "Role"),
    ])).unwrap();
    let group = &shape.groups[1];
    assert_eq!(group.counts.traits, 1);
    assert_eq!(group.counts.structs, 0);
    assert_eq!(group.counts.trait_impls, 0);
    assert!(group.traits[0].candidate_structs.is_empty());
}

#[test]
fn ambiguous_structs_and_a_dot_rs_filename_do_not_invent_relations() {
    let mut declarations = simple();
    declarations.push(declaration(5, None, DeclarationKind::Struct, "First"));
    let mut report = root_file(declarations);
    let shape = SourceShape::from_report(&report).unwrap();
    assert_eq!(shape.groups[1].impls[0].disposition, ImplDisposition::Unresolved);
    assert_eq!(shape.groups[1].traits[0].candidate_structs.len(), 1);
    report.entries[2].name = NativeName::Utf8(".rs".into());
    assert_eq!(SourceShape::from_report(&report).unwrap().groups[1].counts.rust_files, 0);
}
