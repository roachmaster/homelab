//! Observed source shape, not Cargo target discovery or Rust name resolution.
//!
//! Cargo.toml names delimit directory inventories only: the report does not carry
//! manifest contents, so package names, virtual workspaces, targets, cfg, imports,
//! impl generic parameters and macro expansion remain unknown. Header candidates
//! deliberately retain that uncertainty, including `impl<T> Role for T` when a
//! struct named T happens to be present. No candidate is a proven implementation.
use crate::{protocol::*, validate_report, UiError};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SymbolLocation {
    pub entry: u32,
    pub declaration: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShapeCounts {
    pub rust_files: usize,
    pub traits: usize,
    pub structs: usize,
    pub enums: usize,
    pub free_functions: usize,
    pub methods: usize,
    /// Includes negative impl blocks; these are separately counted below.
    pub trait_impls: usize,
    pub inherent_impls: usize,
    pub negative_impls: usize,
    /// Distinct struct declarations with at least one tentative header match.
    pub candidate_structs: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraitShape {
    pub location: SymbolLocation,
    pub candidate_structs: Vec<SymbolLocation>,
    /// Tentatively matched positive blocks, not distinct implementing types.
    pub impl_blocks: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImplDisposition {
    Inherent,
    Negative,
    /// Generic arguments, references, tuples or other non-bare type spellings.
    GenericOrComplex,
    /// Qualified paths may be local or external; the report cannot decide.
    QualifiedPath,
    /// Missing metadata, ambiguous names, aliases, imports or unknown types.
    Unresolved,
    /// Exact bare spellings and unique declarations in one lexical file scope.
    /// This is still tentative: impl parameters and imports are not reported.
    LocalHeaderCandidate {
        trait_location: SymbolLocation,
        struct_location: SymbolLocation,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImplShape {
    pub location: SymbolLocation,
    pub disposition: ImplDisposition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceGroup {
    /// A possible manifest marker, not a parsed Cargo package declaration.
    pub manifest: Option<u32>,
    pub directory: u32,
    /// Rust file entries, including unavailable file contents.
    pub entries: Vec<u32>,
    pub counts: ShapeCounts,
    pub traits: Vec<TraitShape>,
    pub structs: Vec<SymbolLocation>,
    pub impls: Vec<ImplShape>,
    pub unavailable_rust: usize,
    /// Excluded, symlink, unreadable, special or unknown-encoding entries can
    /// conceal more Rust sources or manifest boundaries. Not a full census.
    pub opaque_entries: usize,
}
impl SourceGroup {
    fn new(manifest: Option<u32>, directory: u32) -> Self {
        Self {
            manifest,
            directory,
            entries: vec![],
            counts: ShapeCounts::default(),
            traits: vec![],
            structs: vec![],
            impls: vec![],
            unavailable_rust: 0,
            opaque_entries: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceShape {
    /// Index zero always represents sources without a known manifest boundary.
    pub groups: Vec<SourceGroup>,
    pub entry_groups: Vec<usize>,
    pub report_incomplete: bool,
}

impl SourceShape {
    /// Derive bounded facts from a structurally validated, accepted report.
    /// Counts include authored test/example sources and unevaluated cfg branches.
    /// Uninspected content contributes unknown coverage, never invented zeroes.
    pub fn from_report(report: &ReportView) -> Result<Self, UiError> {
        validate_report(report)?;
        let mut shape = Self {
            groups: vec![SourceGroup::new(None, 0)],
            entry_groups: vec![0; report.entries.len()],
            report_incomplete: !report.complete,
        };
        let mut boundaries = BTreeMap::new();
        for entry in &report.entries {
            if entry.kind != EntryKind::Directory && is_manifest(&entry.name) {
                if let Some(parent) = entry.parent {
                    // Duplicate authored markers in an alternative report are
                    // not resolved into multiple packages or donated outward.
                    boundaries.entry(parent).or_insert_with(|| {
                        let group = shape.groups.len();
                        shape.groups.push(SourceGroup::new(Some(entry.id), parent));
                        group
                    });
                }
            }
        }
        for entry in &report.entries {
            let group = if entry.kind == EntryKind::Directory {
                boundaries.get(&entry.id).copied()
            } else {
                None
            }
            .unwrap_or_else(|| {
                entry
                    .parent
                    .map(|parent| shape.entry_groups[parent as usize])
                    .unwrap_or(0)
            });
            shape.entry_groups[entry.id as usize] = group;
            let group = &mut shape.groups[group];
            if matches!(
                entry.kind,
                EntryKind::Symlink | EntryKind::Excluded | EntryKind::Unreadable | EntryKind::Other
            ) || matches!(&entry.name, NativeName::EncodedBytes(_))
            {
                group.opaque_entries += 1;
            }
            if entry.kind != EntryKind::File || !is_rust(&entry.name) {
                continue;
            }
            group.entries.push(entry.id);
            group.counts.rust_files += 1;
            if entry.content_status != "text" {
                group.unavailable_rust += 1;
                continue;
            }
            for declaration in &entry.declarations {
                let location = SymbolLocation {
                    entry: entry.id,
                    declaration: declaration.id,
                };
                match declaration.kind {
                    DeclarationKind::Trait => {
                        group.counts.traits += 1;
                        group.traits.push(TraitShape {
                            location,
                            candidate_structs: vec![],
                            impl_blocks: 0,
                        });
                    }
                    DeclarationKind::Struct => {
                        group.counts.structs += 1;
                        group.structs.push(location);
                    }
                    DeclarationKind::Enum => group.counts.enums += 1,
                    DeclarationKind::Function => group.counts.free_functions += 1,
                    DeclarationKind::TraitMethod | DeclarationKind::ImplMethod => {
                        group.counts.methods += 1;
                    }
                    DeclarationKind::Impl => {
                        if let Some(implementation) = &declaration.implementation {
                            if implementation.trait_path.is_some() {
                                group.counts.trait_impls += 1;
                            } else {
                                group.counts.inherent_impls += 1;
                            }
                            if implementation.negative {
                                group.counts.negative_impls += 1;
                            }
                        }
                        group.impls.push(ImplShape {
                            location,
                            disposition: ImplDisposition::Unresolved,
                        });
                    }
                    _ => {}
                }
            }
        }
        for group in &mut shape.groups {
            match_headers(report, group);
        }
        Ok(shape)
    }
}

// Compare actual native names without lossy display spelling. EncodedBytes has
// no promised encoding, so it cannot safely establish a manifest or Rust suffix.
fn name_matches(name: &NativeName, ascii: &[u8], suffix: bool) -> bool {
    match name {
        NativeName::Utf8(text) => {
            if suffix { text.as_bytes().ends_with(ascii) } else { text.as_bytes() == ascii }
        }
        NativeName::UnixBytes(bytes) => {
            if suffix { bytes.ends_with(ascii) } else { bytes.as_slice() == ascii }
        }
        NativeName::WindowsWide(units) => {
            let start = if suffix { units.len().saturating_sub(ascii.len()) } else { 0 };
            let units = &units[start..];
            units.len() == ascii.len()
                && units.iter().zip(ascii).all(|(unit, byte)| *unit == u16::from(*byte))
        }
        NativeName::EncodedBytes(_) => false,
    }
}
fn is_manifest(name: &NativeName) -> bool {
    name_matches(name, b"Cargo.toml", false)
}
fn is_rust(name: &NativeName) -> bool {
    let length = match name {
        NativeName::Utf8(text) => text.len(),
        NativeName::UnixBytes(bytes) | NativeName::EncodedBytes(bytes) => bytes.len(),
        NativeName::WindowsWide(units) => units.len(),
    };
    length > 3 && name_matches(name, b".rs", true)
}
fn bare_identifier(text: &str) -> bool {
    let text = text.strip_prefix("r#").unwrap_or(text);
    let mut chars = text.chars();
    // Conservative ASCII subset: unsupported Unicode remains unresolved rather
    // than reimplementing Rust's Unicode identifier grammar in presentation.
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
fn lexical_scope(entry: &EntryView, declaration: &DeclarationView) -> bool {
    declaration.parent.is_none_or(|parent| {
        let parent = &entry.declarations[parent as usize];
        parent.kind == DeclarationKind::Module && parent.module_inline == Some(true)
    })
}

type ScopeName<'a> = (u32, Option<u32>, &'a str);
fn match_headers(report: &ReportView, group: &mut SourceGroup) {
    let mut traits: BTreeMap<ScopeName<'_>, Vec<usize>> = BTreeMap::new();
    let mut structs: BTreeMap<ScopeName<'_>, Vec<SymbolLocation>> = BTreeMap::new();
    for (index, item) in group.traits.iter().enumerate() {
        let entry = &report.entries[item.location.entry as usize];
        let declaration = &entry.declarations[item.location.declaration as usize];
        if lexical_scope(entry, declaration) {
            traits.entry((entry.id, declaration.parent, &declaration.name)).or_default().push(index);
        }
    }
    for location in &group.structs {
        let entry = &report.entries[location.entry as usize];
        let declaration = &entry.declarations[location.declaration as usize];
        if lexical_scope(entry, declaration) {
            structs.entry((entry.id, declaration.parent, &declaration.name)).or_default().push(*location);
        }
    }
    let mut candidates = BTreeSet::new();
    let mut trait_candidates = vec![BTreeSet::new(); group.traits.len()];
    for item in &mut group.impls {
        let entry = &report.entries[item.location.entry as usize];
        let declaration = &entry.declarations[item.location.declaration as usize];
        let Some(implementation) = &declaration.implementation else { continue };
        let Some(trait_path) = &implementation.trait_path else {
            item.disposition = ImplDisposition::Inherent;
            continue;
        };
        if implementation.negative {
            item.disposition = ImplDisposition::Negative;
        } else if trait_path.contains(['<', '>']) || implementation.target.contains(['<', '>']) {
            item.disposition = ImplDisposition::GenericOrComplex;
        } else if trait_path.contains("::") || implementation.target.contains("::") {
            item.disposition = ImplDisposition::QualifiedPath;
        } else if !bare_identifier(trait_path) || !bare_identifier(&implementation.target) {
            item.disposition = ImplDisposition::GenericOrComplex;
        } else if lexical_scope(entry, declaration) {
            let trait_indices = traits.get(&(entry.id, declaration.parent, trait_path.as_str()));
            let locations = structs.get(&(entry.id, declaration.parent, implementation.target.as_str()));
            if let (Some(indices), Some(locations)) = (trait_indices, locations) {
                if let ([index], [location]) = (indices.as_slice(), locations.as_slice()) {
                    let trait_shape = &mut group.traits[*index];
                    trait_shape.impl_blocks += 1;
                    // Source order is stable, but repeated impl blocks do not
                    // pretend to be additional distinct candidate structs.
                    trait_candidates[*index].insert(*location);
                    candidates.insert(*location);
                    item.disposition = ImplDisposition::LocalHeaderCandidate {
                        trait_location: trait_shape.location,
                        struct_location: *location,
                    };
                }
            }
        }
    }
    for (item, candidates) in group.traits.iter_mut().zip(trait_candidates) {
        item.candidate_structs = candidates.into_iter().collect();
    }
    group.counts.candidate_structs = candidates.len();
}
