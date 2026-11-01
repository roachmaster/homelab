use forge_ide::syntax::{
    analyze, Counts, Declaration, DeclarationKind as Kind, Position, SyntaxError,
    MAX_DECLARATION_DEPTH, MAX_SOURCE_TOKENS,
};

fn outline(source: &str) -> forge_ide::syntax::RustSummary {
    analyze(source, 10_000, 32).unwrap()
}

fn child<'a>(parent: &'a Declaration, name: &str) -> &'a Declaration {
    parent
        .children
        .iter()
        .find(|node| node.name == name)
        .unwrap()
}

#[test]
fn comments_strings_and_function_local_declarations_are_not_keyword_counts() {
    let source = r####"
// trait Ghost { fn fake(); } impl Ghost for Phantom {}
/* struct Wrong; /* fn nested_comment() {} */ enum AlsoWrong {} */
fn actual() {
    let _ = "trait Fake {} impl Hidden {} fn text() {}";
    let _ = r###"{{{))) trait Raw {} fn raw() {}"###;
    fn local_function() {}
    struct Local;
    macro_rules! local_macro { () => { trait Expanded {} } }
    local_macro!();
}
"####;
    let summary = analyze(source, 1, 16).unwrap();
    assert_eq!(summary.counts.total, 1);
    assert_eq!(summary.counts.free_functions, 1);
    assert_eq!(summary.counts.structs, 0);
    assert_eq!(summary.counts.traits, 0);
    assert_eq!(summary.counts.macros, 0);
    assert_eq!(summary.declarations[0].name, "actual");
    assert!(summary.declarations[0].children.is_empty());
}

#[test]
fn declarations_are_hierarchical_and_method_counts_keep_their_owners() {
    let summary = outline(
        r#"
pub mod outer {
    pub trait Described {
        type Output;
        const CODE: u8 = 1;
        fn required(&self) -> Self::Output;
        fn defaulted(&self) { fn hidden() {} }
    }
    pub struct Thing { pub value: u8 }
    impl Thing {
        pub fn new() -> Self { Self { value: 0 } }
        const LIMIT: u8 = 4;
    }
    impl Described for Thing {
        type Output = u8;
        const CODE: u8 = 2;
        fn required(&self) -> Self::Output { self.value }
    }
    pub enum Choice { Unit, Tuple(u8, String), Named { x: u8 } }
    pub union Raw { value: u32, bytes: [u8; 4] }
    pub type Alias = Thing;
    pub const GLOBAL: u32 = 4;
    pub static STORE: u32 = 4;
    pub fn free() {}
    mod external;
    mod inner { fn nested() {} }
    macro_rules! make { () => { fn generated() {} } }
    make!();
}
"#,
    );
    assert_eq!(
        summary.counts,
        Counts {
            total: 34,
            modules: 3,
            free_functions: 2,
            traits: 1,
            impl_blocks: 2,
            trait_impls: 1,
            inherent_impls: 1,
            trait_methods: 2,
            impl_methods: 2,
            structs: 1,
            enums: 1,
            unions: 1,
            type_aliases: 1,
            associated_types: 2,
            constants: 4,
            statics: 1,
            macros: 2,
            variants: 3,
            fields: 6,
        }
    );
    assert_eq!(summary.declarations.len(), 1);
    let outer = &summary.declarations[0];
    assert_eq!(outer.kind, Kind::Module);
    let described = child(outer, "Described");
    assert_eq!(described.kind, Kind::Trait);
    assert_eq!(child(described, "required").kind, Kind::TraitMethod);
    assert_eq!(child(described, "defaulted").kind, Kind::TraitMethod);
    assert!(child(described, "defaulted").children.is_empty());
    let inherent = outer
        .children
        .iter()
        .find(|node| node.kind == Kind::Impl && node.name == "Thing")
        .unwrap();
    assert_eq!(child(inherent, "new").kind, Kind::ImplMethod);
    let implemented = child(outer, "Described for Thing");
    assert_eq!(child(implemented, "required").kind, Kind::ImplMethod);
    assert_eq!(child(implemented, "Output").kind, Kind::AssociatedType);
    assert_eq!(child(outer, "free").kind, Kind::Function);
    let choice = child(outer, "Choice");
    assert_eq!(
        choice
            .children
            .iter()
            .map(|node| node.name.as_str())
            .collect::<Vec<_>>(),
        ["Unit", "Tuple", "Named"]
    );
    assert_eq!(
        child(choice, "Tuple")
            .children
            .iter()
            .map(|node| node.name.as_str())
            .collect::<Vec<_>>(),
        ["0", "1"]
    );
    assert_eq!(child(child(choice, "Named"), "x").kind, Kind::Field);
    assert_eq!(child(child(outer, "inner"), "nested").kind, Kind::Function);
}

#[test]
fn inline_and_external_modules_are_distinguished_without_loading_files() {
    let summary = outline("mod absent_file; mod empty {} mod outer { mod child; }");
    let roots = &summary.declarations;
    assert_eq!(roots[0].module_inline, Some(false));
    assert_eq!(roots[1].module_inline, Some(true));
    assert!(roots[0].children.is_empty());
    assert!(roots[1].children.is_empty());
    assert_eq!(roots[2].module_inline, Some(true));
    assert_eq!(roots[2].children[0].module_inline, Some(false));
    assert_eq!(summary.counts.modules, 4);
}

#[test]
fn implementation_text_preserves_inherent_trait_negative_and_unsafe_distinctions() {
    let summary = outline(
        r#"
impl Marker {}
unsafe impl<T> std::marker::Send for Box<T> {}
impl !Send for Marker {}
"#,
    );
    let inherent = summary.declarations[0].implementation.as_ref().unwrap();
    assert_eq!(inherent.target, "Marker");
    assert_eq!(inherent.trait_path, None);
    assert!(!inherent.negative);
    assert!(!inherent.is_unsafe);
    let generic = summary.declarations[1].implementation.as_ref().unwrap();
    assert_eq!(generic.target, "Box < T >");
    assert_eq!(generic.trait_path.as_deref(), Some("std :: marker :: Send"));
    assert!(generic.is_unsafe);
    let negative = summary.declarations[2].implementation.as_ref().unwrap();
    assert_eq!(negative.trait_path.as_deref(), Some("Send"));
    assert!(negative.negative);
    assert_eq!(summary.counts.impl_blocks, 3);
    assert_eq!(summary.counts.inherent_impls, 1);
    assert_eq!(summary.counts.trait_impls, 2);
}

#[test]
fn macro_definitions_and_invocations_stay_unexpanded_in_each_scope() {
    let summary = outline(
        r#"
macro_rules! generate { () => { fn invisible() {} } }
tools::generate!();
trait T { declarations!(); }
impl Holder { members!(); }
"#,
    );
    assert_eq!(summary.counts.macros, 4);
    assert_eq!(summary.counts.free_functions, 0);
    assert_eq!(summary.counts.trait_methods, 0);
    assert_eq!(summary.counts.impl_methods, 0);
    assert_eq!(summary.declarations[0].name, "generate");
    assert_eq!(summary.declarations[1].name, "tools :: generate");
    assert_eq!(summary.declarations[2].children[0].name, "declarations");
    assert_eq!(summary.declarations[3].children[0].name, "members");
    assert!(summary.declarations[0].children.is_empty());
}

#[test]
fn spans_are_one_based_lines_zero_based_character_columns_and_exclusive_ends() {
    let source = "\n// prefix\npub fn café() {}\nmod nest {\n    pub struct Niño { x: u8 }\n}\n";
    let summary = outline(source);
    let function = &summary.declarations[0];
    assert_eq!(function.name, "café");
    assert_eq!(function.span.start, Position { line: 3, column: 0 });
    assert_eq!(
        function.span.end,
        Position {
            line: 3,
            column: "pub fn café() {}".chars().count()
        }
    );
    let module = &summary.declarations[1];
    assert_eq!(module.span.end, Position { line: 6, column: 1 });
    let structure = &module.children[0];
    assert_eq!(structure.name, "Niño");
    assert_eq!(structure.span.start, Position { line: 5, column: 4 });
    let line = source.lines().nth(4).unwrap();
    assert_eq!(
        structure.span.end,
        Position {
            line: 5,
            column: line.chars().count()
        }
    );
    let field = &structure.children[0];
    let field_column = line[..line.find("x:").unwrap()].chars().count();
    assert_eq!(
        field.span.start,
        Position {
            line: 5,
            column: field_column
        }
    );
    assert_eq!(
        field.span.end,
        Position {
            line: 5,
            column: field_column + "x: u8".chars().count()
        }
    );
}

#[test]
fn raw_names_and_unicode_before_a_declaration_do_not_turn_columns_into_bytes() {
    let source = "struct Café; fn r#type() {}";
    let summary = outline(source);
    assert_eq!(summary.declarations[0].name, "Café");
    let function = &summary.declarations[1];
    assert_eq!(function.name, "r#type");
    assert_eq!(function.span.start.column, "struct Café; ".chars().count());
    assert_eq!(function.span.end.column, source.chars().count());
    assert_ne!(function.span.end.column, source.len());
}

#[test]
fn attributes_remain_in_spans_but_cfg_is_not_evaluated() {
    let source = "#[cfg(nonexistent)]\npub struct Hidden;";
    let summary = outline(source);
    assert_eq!(summary.counts.structs, 1);
    assert_eq!(
        summary.declarations[0].span.start,
        Position { line: 1, column: 0 }
    );
    assert_eq!(
        summary.declarations[0].span.end,
        Position {
            line: 2,
            column: "pub struct Hidden;".len()
        }
    );
}

#[test]
fn parse_failures_preserve_a_message_and_real_source_location() {
    let error = analyze("// header\nfn () {}", 100, 16).unwrap_err();
    let SyntaxError::Parse { message, span } = &error else {
        panic!("parse error expected: {error}")
    };
    assert!(!message.is_empty());
    assert_eq!(span.start, Position { line: 2, column: 3 });
    assert!(error.to_string().contains("2:3"));
    let error: &dyn std::error::Error = &error;
    assert!(error.source().is_none());
    assert!(matches!(
        analyze("fn unfinished( {", 100, 16),
        Err(SyntaxError::Parse { .. })
    ));
}

#[test]
fn node_and_outline_depth_budgets_have_exact_boundaries_and_no_partial_result() {
    let source = "struct S { a: u8, b: u8 }";
    assert_eq!(analyze(source, 3, 2).unwrap().counts.total, 3);
    let error = analyze(source, 2, 2).unwrap_err();
    let SyntaxError::NodeLimit { limit, span } = error else {
        panic!("node limit expected")
    };
    assert_eq!(limit, 2);
    assert_eq!(span.start.column, source.find("b:").unwrap());
    let error = analyze(source, 3, 1).unwrap_err();
    let SyntaxError::DepthLimit { limit, span } = error else {
        panic!("outline depth expected")
    };
    assert_eq!(limit, 1);
    assert_eq!(span.start.column, source.find("a:").unwrap());
    assert_eq!(
        analyze("fn one() {}", 1, 1).unwrap().counts.free_functions,
        1
    );
}

#[test]
fn invalid_budgets_are_rejected_before_source_parsing_and_empty_files_are_valid() {
    for (nodes, depth) in [(0, 1), (1, 0), (1, MAX_DECLARATION_DEPTH + 1)] {
        assert_eq!(
            analyze("not Rust", nodes, depth),
            Err(SyntaxError::InvalidLimits {
                max_nodes: nodes,
                max_depth: depth
            })
        );
    }
    let summary = analyze("// empty source\n", 1, 1).unwrap();
    assert!(summary.declarations.is_empty());
    assert_eq!(summary.counts, Counts::default());
}

#[test]
fn parser_is_not_a_compiler_and_omitted_item_categories_are_explicit() {
    let summary = outline(
        r#"
use std::fmt;
extern crate dependency;
extern "C" { fn foreign(); }
trait Alias = Send + Sync;
fn repeated() { missing_name(); }
fn repeated() {}
"#,
    );
    assert_eq!(summary.counts.total, 2);
    assert_eq!(summary.counts.free_functions, 2);
    assert_eq!(summary.counts.traits, 0);
}

#[test]
fn lexical_preflight_rejects_very_deep_groups_before_recursive_ast_parsing() {
    let source = format!(
        "fn deep() {{ let _ = {}0{}; }}",
        "(".repeat(20_000),
        ")".repeat(20_000)
    );
    let error = analyze(&source, 100, 16).unwrap_err();
    let SyntaxError::TokenDepthLimit { limit, span } = error else {
        panic!("token group guard expected")
    };
    assert_eq!(limit, 16);
    assert_eq!(span.start.line, 1);
}

#[test]
fn lexical_preflight_counts_real_tokens_not_delimiters_in_raw_strings_or_comments() {
    let source = format!(
        "fn shallow() {{ let _ = r###\"{}\"###; /* {} */ }}",
        "[(}".repeat(500),
        "{[(".repeat(500)
    );
    assert_eq!(analyze(&source, 1, 1).unwrap().counts.free_functions, 1);
    let source = format!(
        "{}\nfn final_item() {{}}",
        "name ".repeat(MAX_SOURCE_TOKENS + 1)
    );
    assert!(matches!(
        analyze(&source, 100, 16),
        Err(SyntaxError::TokenLimit {
            limit: MAX_SOURCE_TOKENS,
            ..
        })
    ));
}

#[test]
fn file_headers_preserve_lines_and_do_not_hide_inner_attribute_groups() {
    for source in [
        "#!/usr/bin/env rust-script\nfn main() {}",
        "\u{feff}#!/bin/anything 'arbitrary header\nfn main() {}",
    ] {
        let summary = outline(source);
        assert_eq!(
            summary.declarations[0].span.start,
            Position { line: 2, column: 0 }
        );
        assert_eq!(summary.declarations[0].name, "main");
    }
    for source in [
        "#![allow(dead_code)]\nfn main() {}",
        "#! /* nested /* comment */ okay */ [allow(dead_code)]\nfn main() {}",
        "#! // comment\n[allow(dead_code)]\nfn main() {}",
    ] {
        assert_eq!(outline(source).counts.free_functions, 1);
    }
    let source = "#! /* comment */ [allow((((((((dead_code))))))))]\nfn main() {}";
    assert!(matches!(
        analyze(source, 100, 4),
        Err(SyntaxError::TokenDepthLimit { .. })
    ));
}

#[cfg(feature = "yaml")]
#[test]
fn report_types_serialize_at_the_optional_yaml_boundary() {
    let summary =
        outline("trait Named { fn name(&self); } impl Named for Thing { fn name(&self) {} }");
    let output = serde_yaml_ng::to_string(&summary).unwrap();
    assert!(output.contains("kind: trait_method"));
    assert!(output.contains("kind: impl_method"));
    assert!(output.contains("trait_path: Named"));
    assert!(output.contains("trait_impls: 1"));
    assert!(output.contains("free_functions: 0"));
    assert!(output.contains("line: 1"));
    assert!(output.contains("column: 0"));
}

#[test]
fn implementation_type_constants_are_reported_but_function_body_literals_are_not() {
    let summary = outline(
        r#"
impl Marker for [u8; 7] {
    fn work(&self) { let _ = "body literal deliberately not reported"; }
}
"#,
    );
    let implementation = summary.declarations[0].implementation.as_ref().unwrap();
    assert_eq!(implementation.target, "[u8 ; 7]");
    assert_eq!(implementation.trait_path.as_deref(), Some("Marker"));
    assert_eq!(summary.counts.impl_methods, 1);
    assert!(!format!("{summary:?}").contains("body literal deliberately not reported"));
}
