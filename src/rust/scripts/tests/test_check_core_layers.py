"""Architectural regressions for grouped/aliased and nested core dependencies."""
from pathlib import Path
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
import check_core_layers as CHECK


class CoreLayerTests(unittest.TestCase):
    def test_layer_contract_names_only_known_distinct_layers(self):
        for source, destinations in CHECK.LAYERS.items():
            with self.subTest(layer=source):
                self.assertLessEqual(destinations, set(CHECK.LAYERS))
                self.assertNotIn(source, destinations)

    def fixture(self, files=None):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        crate = Path(temporary.name) / "forge-core"
        (crate / "src").mkdir(parents=True)
        (crate / "Cargo.toml").write_text(
            '[package]\nname="forge-core"\nversion="0.1.0"\nedition="2021"\n')
        sources = {f"src/{name}.rs": "pub struct Marker;\n" for name in CHECK.LAYERS}
        sources["src/lib.rs"] = "".join(f"pub mod {name};\n" for name in CHECK.LAYERS)
        sources.update(files or {})
        for name, source in sources.items():
            path = crate / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(source)
        return crate

    def rejected(self, source, **files):
        crate = self.fixture({"src/provider.rs": source, **files})
        findings = CHECK.check_core_layers(crate)
        self.assertTrue(any(f.message == "forbidden core layer edge: provider -> task"
                            for f in findings), findings)
        return findings

    def test_current_core_has_no_layer_errors(self):
        self.assertEqual(CHECK.check_core_layers(SCRIPTS.parent / "forge-core"), [])

    def test_grouped_import_bypasses_old_literal_scan_but_is_rejected(self):
        source = "use crate::{task::Marker};\n"
        self.assertNotIn("crate::task", source)
        self.rejected(source)

    def test_relative_import_bypasses_old_literal_scan_but_is_rejected(self):
        for source in ("use super::task::Marker;", "use super::{task::Marker};"):
            with self.subTest(source=source):
                self.assertNotIn("crate::", source)
                self.rejected(source)

    def test_module_alias_and_direct_qualified_type_are_rejected(self):
        for source in (
            "use crate as root; fn f(_: root::task::Marker) {}",
            "use crate::task as hidden; fn f(_: hidden::Marker) {}",
            "fn f(_: crate::task::Marker) {}",
            "fn f() { let _: Option<super::task::Marker> = None; }",
            "extern crate self as core_alias; fn f(_: core_alias::task::Marker) {}",
        ):
            with self.subTest(source=source):
                self.rejected(source)

    def test_root_self_crate_alias_is_resolved_from_other_modules(self):
        root = "".join(f"pub mod {name};\n" for name in CHECK.LAYERS)
        self.rejected("fn f(_: own_core::task::Marker) {}",
                      **{"src/lib.rs": "extern crate self as own_core;\n" + root})
        self.rejected("fn f(_: ::own_core::task::Marker) {}",
                      **{"src/lib.rs": "extern crate self as own_core;\n" + root})

    def test_root_reexport_cannot_hide_the_original_layer(self):
        root = "".join(f"pub mod {name};\n" for name in CHECK.LAYERS)
        self.rejected("use crate::Hidden;",
                      **{"src/lib.rs": root + "pub use task::Marker as Hidden;\n"})

    def test_inline_nested_self_alias_is_rejected(self):
        self.rejected("mod inner { use self::super_alias::task::Marker; "
                      "use super::super as super_alias; }")

    def test_nested_files_and_path_attributes_keep_their_declaring_layer(self):
        for declaration, file in (
            ("mod inner;", "src/provider/inner.rs"),
            ('#[path="provider/hidden.rs"] mod inner;', "src/provider/hidden.rs"),
        ):
            with self.subTest(declaration=declaration):
                findings = self.rejected(declaration,
                    **{file: "fn f(_: super::super::task::Marker) {}"})
                self.assertTrue(any(f.file == file for f in findings))

    def test_local_aliases_are_hoisted_but_do_not_leak_between_functions(self):
        self.rejected("fn f(_: ()) { let _: Option<alias::task::Marker> = None; "
                      "use crate as alias; }")
        crate = self.fixture({"src/task.rs":
            "fn first() { use crate as env; let _: Option<env::provider::Marker> = None; }\n"
            "fn second() { use std as env; let _: Option<env::time::Duration> = None; }\n"})
        self.assertEqual(CHECK.check_core_layers(crate), [])

    def test_globs_have_known_provenance_and_root_globs_fail_closed(self):
        self.rejected("pub use crate::task::*;")
        crate = self.fixture({"src/provider.rs": "use crate::*;"})
        self.assertTrue(any("root glob" in f.message for f in CHECK.check_core_layers(crate)))

    def test_all_allowed_edges_work_with_grouped_aliases_and_reexports(self):
        files = {}
        for source, targets in CHECK.LAYERS.items():
            files[f"src/{source}.rs"] = "pub struct Marker;\n" + "".join(
                f"pub use crate::{{{target}::Marker as {target}_marker}};\n"
                for target in targets)
        self.assertEqual(CHECK.check_core_layers(self.fixture(files)), [])

    def test_allowed_self_paths_and_external_globs_are_not_false_positives(self):
        crate = self.fixture({"src/provider.rs":
            "mod inner { pub struct Marker; }\nuse self::inner as own;\n"
            "use std::collections::*; fn f(_: own::Marker) {}\n"})
        self.assertEqual(CHECK.check_core_layers(crate), [])

    def test_comments_and_strings_do_not_create_edges(self):
        crate = self.fixture({"src/provider.rs":
            '/* crate::task::Marker /* nested */ */\n'
            '// use crate::{task::Marker};\n'
            'const TEXT: &str = r###"use crate::task::Marker;"###;\n'})
        self.assertEqual(CHECK.check_core_layers(crate), [])

    def test_literal_macro_paths_are_checked_without_expanding_macros(self):
        self.rejected("macro_rules! hidden { () => { $crate::task::Marker }; }")

    def test_unknown_or_missing_and_orphan_modules_fail_closed(self):
        crate = self.fixture({"src/provider.rs": "mod missing;"})
        self.assertTrue(any("missing core module" in f.message for f in CHECK.check_core_layers(crate)))
        crate = self.fixture({"src/provider/hidden.rs": "use crate::task::Marker;"})
        self.assertTrue(any("outside the module graph" in f.message for f in CHECK.check_core_layers(crate)))
        crate = self.fixture({"src/lib.rs": "pub mod surprise;", "src/surprise.rs": ""})
        self.assertTrue(any("inventory changed" in f.message for f in CHECK.check_core_layers(crate)))

    def test_cyclic_alias_is_rejected_without_recursion_failure(self):
        root = "".join(f"pub mod {name};\n" for name in CHECK.LAYERS)
        crate = self.fixture({"src/lib.rs": root + "pub use task::Alias;\n",
                              "src/task.rs": "pub use crate::Alias;"})
        self.assertTrue(any("cyclic" in f.message for f in CHECK.check_core_layers(crate)))

    def test_incomplete_parses_and_unowned_textual_includes_fail_closed(self):
        crate = self.fixture({"src/provider.rs": "unsupported source item;"})
        self.assertTrue(any("incomplete Rust source graph" in f.message
                            for f in CHECK.check_core_layers(crate)))
        crate = self.fixture({"src/provider.rs": 'include!("provider/hidden.rs");',
                              "src/provider/hidden.rs": "use crate::task::Marker;"})
        self.assertTrue(any("include! source has no module owner" in f.message
                            for f in CHECK.check_core_layers(crate)))

    def test_alternative_files_for_one_module_fail_closed_instead_of_missing_a_branch(self):
        crate = self.fixture({
            "src/provider.rs": '#[cfg(unix)] #[path="provider/one.rs"] mod inner;\n'
                               '#[cfg(not(unix))] #[path="provider/two.rs"] mod inner;\n',
            "src/provider/one.rs": "pub struct Marker;",
            "src/provider/two.rs": "use crate::task::Marker;",
        })
        self.assertTrue(any("ambiguous repeated module declaration" in f.message
                            for f in CHECK.check_core_layers(crate)))


if __name__ == "__main__":
    unittest.main()
