"""Unit tests for rust_ast.py: Rust tokenizer, use trees, items, modules,
and the workspace dependency graph builder."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
RUST_ROOT = SCRIPTS.parent


def _load(name: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


RUST_AST = _load("rust_ast")


def _parse(source: str):
    return RUST_AST.parse_source(source, "test.rs")


class TokenizerTests(unittest.TestCase):
    def _texts(self, source):
        return [(t.kind, t.text) for t in RUST_AST.tokenize(source)
                if t.text != "<eof>"]

    def test_nested_block_comments_are_skipped(self):
        toks = self._texts("a /* outer /* inner */ still */ b")
        self.assertEqual([t for _, t in toks], ["a", "b"])

    def test_line_and_doc_comments_are_skipped(self):
        toks = self._texts("/// doc\n//! inner\n// plain\na")
        self.assertEqual([t for _, t in toks], ["a"])

    def test_raw_strings_with_hashes(self):
        toks = self._texts('let s = r#"a "b" c"#; let t = r##"x "# y"##;')
        strings = [t for k, t in toks if k == "STRING"]
        self.assertEqual(strings, ['r#"a "b" c"#', 'r##"x "# y"##'])

    def test_byte_char_literals_do_not_open_strings(self):
        # b'"' and b'\'' contain quotes that must not desync the lexer:
        # each byte char is one STRING token and the code around it survives.
        toks = self._texts("match c { b'\"' | b'\\'' | b'}' => 1, _ => 0 }")
        self.assertEqual(
            [t for _, t in toks],
            ["match", "c", "{", "b'\"'", "|", "b'\\''", "|", "b'}'",
             "=>", "1", ",", "_", "=>", "0", "}"])
        strings = [t for k, t in toks if k == "STRING"]
        self.assertEqual(strings, ["b'\"'", "b'\\''", "b'}'"])

    def test_char_vs_lifetime(self):
        toks = self._texts("fn f<'a>(x: &'a str, c: char) { let q = 'a'; }")
        kinds = {t: k for k, t in toks}
        self.assertEqual(kinds["'a"], "LIFETIME")
        self.assertEqual(kinds["'a'"], "STRING")

    def test_raw_identifiers(self):
        toks = self._texts("let r#type = 1;")
        self.assertIn(("IDENT", "type"), toks)


class UseTreeTests(unittest.TestCase):
    def test_simple_and_nested_groups(self):
        pf = _parse("""
            use std::collections::BTreeMap;
            use crate::ingress::{Header, Response, RouteKey};
            use forge_core::command::{ArgumentDescriptor, DescriptorError as E};
        """)
        paths = [tuple(i.path) for i in pf.imports]
        self.assertIn(("std", "collections", "BTreeMap"), paths)
        self.assertIn(("crate", "ingress", "Header"), paths)
        self.assertIn(("crate", "ingress", "Response"), paths)
        self.assertIn(("crate", "ingress", "RouteKey"), paths)
        self.assertIn(("forge_core", "command", "ArgumentDescriptor"), paths)
        aliased = [i for i in pf.imports if i.alias == "E"]
        self.assertEqual(len(aliased), 1)
        self.assertEqual(aliased[0].path, ["forge_core", "command", "DescriptorError"])

    def test_deeply_nested_group(self):
        pf = _parse("use a::{b::{c, d}, e};")
        self.assertEqual(sorted(tuple(i.path) for i in pf.imports),
                         [("a", "b", "c"), ("a", "b", "d"), ("a", "e")])

    def test_glob_and_self(self):
        pf = _parse("use super::*; use crate::foo::{self, Bar};")
        globs = [i for i in pf.imports if i.glob]
        self.assertEqual(len(globs), 1)
        self.assertEqual(globs[0].path, ["super"])
        selves = [i for i in pf.imports if i.is_self]
        self.assertEqual(len(selves), 1)
        self.assertEqual(selves[0].path, ["crate", "foo", "self"])

    def test_visibility_and_cfg(self):
        pf = _parse("""
            pub use crate::task::Task;
            pub(crate) use crate::io_client::IoClient;
            #[cfg(feature = "messages")]
            pub use crate::messages::Messages;
        """)
        self.assertEqual(pf.imports[0].visibility, "pub")
        self.assertEqual(pf.imports[1].visibility, "pub(crate)")
        self.assertEqual(pf.imports[2].cfg, 'feature = "messages"')
        self.assertEqual(pf.imports[2].visibility, "pub")


class ItemTests(unittest.TestCase):
    def test_items_and_visibility(self):
        pf = _parse("""
            pub struct Registry<G> { groups: G }
            pub(crate) enum Error { Missing }
            trait Task { fn run(&self); }
            pub type Handler = fn();
            const MAX: usize = 4;
            static mut COUNT: u32 = 0;
        """)
        got = {(i.name, i.kind, i.visibility) for i in pf.items}
        self.assertIn(("Registry", "struct", "pub"), got)
        self.assertIn(("Error", "enum", "pub(crate)"), got)
        self.assertIn(("Task", "trait", ""), got)
        self.assertIn(("Handler", "type", "pub"), got)
        self.assertIn(("MAX", "const", ""), got)
        self.assertIn(("COUNT", "static", ""), got)

    def test_const_with_struct_literal_initializer(self):
        pf = _parse("""
            pub(crate) const KEY: Descriptor = Descriptor {
                id: EntityId(2), kind: Kind::Key,
            };
            const NEXT: u8 = 1;
        """)
        self.assertEqual([i.name for i in pf.items], ["KEY", "NEXT"])
        self.assertEqual(pf.notes, [])

    def test_macro_rules_is_an_item_not_a_call(self):
        pf = _parse("macro_rules! convert { ($a:expr) => { $a }; }")
        self.assertEqual([(i.name, i.kind) for i in pf.items],
                         [("convert", "macro")])
        self.assertEqual(pf.notes, [])

    def test_macro_invocation_at_item_position_is_skipped(self):
        pf = _parse("register!(foo, bar); pub struct After;")
        self.assertEqual([i.name for i in pf.items], ["After"])
        self.assertEqual(pf.notes, [])

    def test_extern_crate(self):
        pf = _parse("extern crate serde; extern crate self as foo;")
        self.assertEqual([(i.name, i.kind) for i in pf.items],
                         [("serde", "extern_crate"), ("foo", "extern_crate")])

    def test_const_fn_is_not_a_const_item(self):
        pf = _parse("pub const fn make() -> u32 { 1 }")
        self.assertEqual([(i.name, i.kind) for i in pf.items],
                         [("make", "fn")])


class ModuleTests(unittest.TestCase):
    def test_mod_declarations_and_path_attr(self):
        pf = _parse("""
            pub mod bridge;
            mod internal;
            #[path = "room_tests.rs"]
            mod tests;
            mod inline { pub struct Inner; }
        """)
        mods = {(m.name, m.inline, m.path_attr) for m in pf.mods}
        self.assertIn(("bridge", False, None), mods)
        self.assertIn(("internal", False, None), mods)
        self.assertIn(("tests", False, "room_tests.rs"), mods)
        self.assertIn(("inline", True, None), mods)
        inner = [i for i in pf.items if i.name == "Inner"]
        self.assertEqual(inner[0].module, ["inline"])

    def test_imports_inside_inline_mod_track_their_module(self):
        pf = _parse("""
            use crate::top::Top;
            #[cfg(test)]
            mod tests {
                use super::*;
                use crate::other::Other;
            }
        """)
        top = [i for i in pf.imports if i.module == []]
        inner = [i for i in pf.imports if i.module == ["tests"]]
        self.assertEqual(len(top), 1)
        self.assertEqual(len(inner), 2)
        self.assertEqual(inner[0].path, ["super"])

    def test_scoped_use_inside_fn_body_is_local(self):
        pf = _parse("""
            pub fn run() {
                use std::collections::HashMap;
                let _map: HashMap<u32, u32> = HashMap::new();
            }
        """)
        self.assertEqual(len(pf.imports), 1)
        self.assertTrue(pf.imports[0].local)
        self.assertEqual(pf.imports[0].scope, "run")
        self.assertEqual(pf.imports[0].path, ["std", "collections", "HashMap"])


class GraphTests(unittest.TestCase):
    def _fixture_crate(self, root: Path) -> None:
        (root / "core" / "src").mkdir(parents=True)
        (root / "core" / "Cargo.toml").write_text(
            '[package]\nname = "mini-core"\nversion = "0.1.0"\nedition = "2021"\n')
        (root / "core" / "src" / "lib.rs").write_text(
            "pub mod ingress;\npub mod bridge;\n")
        (root / "core" / "src" / "ingress.rs").write_text(
            "pub struct Envelope;\n")
        (root / "core" / "src" / "bridge.rs").write_text(
            "use crate::ingress::Envelope;\nuse std::collections::BTreeMap;\n"
            "pub struct Bridge;\n")
        (root / "core" / "src" / "bridge_tests.rs").write_text(
            "use super::Bridge;\n")
        (root / "core" / "src" / "bridge.rs").write_text(
            (root / "core" / "src" / "bridge.rs").read_text()
            + '#[path = "bridge_tests.rs"]\nmod tests;\n')

    def test_fixture_graph(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._fixture_crate(root)
            graph = RUST_AST.build_graph(root)
        self.assertEqual(graph["errors"], [])
        modules = graph["modules"]
        self.assertIn("mini_core::ingress", modules)
        self.assertIn("mini_core::bridge", modules)
        # #[path] override: bridge_tests.rs is bridge::tests, not a top module.
        self.assertIn("mini_core::bridge::tests", modules)
        self.assertNotIn("mini_core::bridge_tests", modules)
        bridge = modules["mini_core::bridge"]
        local = [e for e in bridge["edges"] if e["kind"] == "local"]
        self.assertEqual([(e["to"], e["names"]) for e in local],
                         [("mini_core::ingress", ["Envelope"])])
        std = [e for e in bridge["edges"] if e["kind"] == "std"]
        self.assertEqual(len(std), 1)
        # The tests module imports its parent via super::.
        tests = modules["mini_core::bridge::tests"]
        self.assertEqual([(e["to"], e["names"]) for e in tests["edges"]
                          if e["kind"] == "local"],
                         [("mini_core::bridge", ["Bridge"])])

    def test_real_workspace_parses_clean(self):
        graph = RUST_AST.build_graph(RUST_ROOT)
        self.assertEqual(graph["errors"], [])
        self.assertGreater(len(graph["modules"]), 300)
        # The layering the forge-core test guards, reproduced from the AST:
        # bridge -> ingress and access -> ingress, nothing else local.
        core = {k: v for k, v in graph["modules"].items()
                if k.startswith("forge_core::")
                and "::tests::" not in k and "::examples::" not in k}
        local = {k: sorted(e["to"] for e in v["edges"] if e["kind"] == "local")
                 for k, v in core.items()}
        self.assertEqual(local.get("forge_core::bridge"), ["forge_core::ingress"])
        self.assertEqual(local.get("forge_core::access"), ["forge_core::ingress"])
        self.assertEqual(local.get("forge_core::task"), [])
        self.assertEqual(local.get("forge_core::provider"), [])
        # Every local/crate edge target is a known module.
        known = set(graph["modules"])
        for name, mod in graph["modules"].items():
            for edge in mod["edges"]:
                if edge["kind"] in ("local", "crate"):
                    self.assertIn(edge["to"], known, f"{name} -> {edge['to']}")


if __name__ == "__main__":
    unittest.main()
