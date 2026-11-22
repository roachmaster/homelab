"""Tests for the pre-compile imports / pub use check (check_imports.py)."""

import importlib.util
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent
RUST_ROOT = SCRIPTS.parent


def _load(name: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CHECK = _load("check_imports")


def _crate(root: Path, files: dict[str, str], name: str = "mini") -> Path:
    """Write a one-crate fixture: {relative path: content} under root/name."""
    crate = root / name
    (crate / "Cargo.toml").parent.mkdir(parents=True, exist_ok=True)
    (crate / "Cargo.toml").write_text(
        f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2021"\n')
    for rel, content in files.items():
        path = crate / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
    return crate


def _findings(root: Path, rule: str | None = None) -> list:
    found = CHECK.check_workspace(root)
    if rule is not None:
        return [f for f in found if f.rule == rule]
    return found


class CheckTests(unittest.TestCase):
    def test_clean_crate_has_no_findings(self):
        with tempfile.TemporaryDirectory() as tmp:
            _crate(Path(tmp), {
                "src/lib.rs": "pub mod inner;\n",
                "src/inner.rs": "pub struct Thing;\n",
            })
            self.assertEqual(_findings(Path(tmp)), [])

    def test_r1_unresolved_import(self):
        with tempfile.TemporaryDirectory() as tmp:
            _crate(Path(tmp), {
                "src/lib.rs": "pub mod inner;\n",
                "src/inner.rs": "use crate::missing::Nope;\n",
            })
            found = _findings(Path(tmp), "R1")
            self.assertEqual(len(found), 1)
            self.assertIn("missing::Nope", found[0].message)

    def test_r1_resolves_through_reexport(self):
        with tempfile.TemporaryDirectory() as tmp:
            _crate(Path(tmp), {
                "src/lib.rs": (
                    "mod inner;\npub mod user;\n"
                    "pub use inner::Thing;\n"),
                "src/inner.rs": "pub struct Thing;\n",
                "src/user.rs": "use crate::Thing;\n",
            })
            self.assertEqual(_findings(Path(tmp)), [])

    def test_r1_macro_generated_item_resolves(self):
        with tempfile.TemporaryDirectory() as tmp:
            _crate(Path(tmp), {
                "src/lib.rs": "pub mod defs;\npub mod user;\n",
                "src/defs.rs": (
                    "macro_rules! define {\n"
                    "    ($kind:ident { $($v:ident),+ }) => {\n"
                    "        pub enum $kind { $($v),+ }\n"
                    "    };\n"
                    "}\n"
                    "define!(Widget { A, B });\n"),
                "src/user.rs": "use crate::defs::Widget;\n",
            })
            self.assertEqual(_findings(Path(tmp), "R1"), [])

    def test_r1_macro_export_resolves_at_root(self):
        with tempfile.TemporaryDirectory() as tmp:
            _crate(Path(tmp), {
                "src/lib.rs": "pub mod gen;\npub mod user;\n",
                "src/gen.rs": (
                    "#[macro_export]\n"
                    "macro_rules! comp { () => {}; }\n"),
                "src/user.rs": "use crate::comp;\n",
            })
            self.assertEqual(_findings(Path(tmp), "R1"), [])

    def test_r2_private_import_flagged(self):
        with tempfile.TemporaryDirectory() as tmp:
            _crate(Path(tmp), {
                "src/lib.rs": "pub mod a;\npub mod b;\n",
                "src/a.rs": "struct Secret;\n",
                "src/b.rs": "use crate::a::Secret;\n",
            })
            found = _findings(Path(tmp), "R2")
            self.assertEqual(len(found), 1)
            self.assertIn("Secret", found[0].message)

    def test_r2_same_module_private_import_ok(self):
        with tempfile.TemporaryDirectory() as tmp:
            _crate(Path(tmp), {
                "src/lib.rs": "pub mod a;\n",
                "src/a.rs": (
                    "struct Secret;\nmod inner {\n"
                    "    use super::Secret;\n}\n"),
            })
            self.assertEqual(_findings(Path(tmp), "R2"), [])

    def test_r2_pub_crate_across_crates_flagged(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _crate(root, {
                "src/lib.rs": "pub mod a;\n",
                "src/a.rs": "pub(crate) struct Shared;\n",
            }, name="one")
            _crate(root, {
                "src/lib.rs": "pub mod b;\n",
                "src/b.rs": "use one::a::Shared;\n",
            }, name="two")
            found = _findings(root, "R2")
            self.assertEqual(len(found), 1)
            self.assertIn("Shared", found[0].message)

    def test_r3_lib_shape_flagged(self):
        with tempfile.TemporaryDirectory() as tmp:
            _crate(Path(tmp), {
                "src/lib.rs": "pub struct Loose;\n",
            })
            found = _findings(Path(tmp), "R3")
            self.assertEqual(len(found), 1)
            self.assertIn("Loose", found[0].message)

    def test_r4_unreachable_pub_warned_and_reexport_clears_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _crate(root, {
                "src/lib.rs": "mod inner;\n",
                "src/inner.rs": "pub struct Hidden;\n",
            })
            found = _findings(root, "R4")
            self.assertEqual(len(found), 1)
            self.assertEqual(found[0].level, "warn")
            self.assertIn("Hidden", found[0].message)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _crate(root, {
                "src/lib.rs": "mod inner;\npub use inner::Hidden;\n",
                "src/inner.rs": "pub struct Hidden;\n",
            })
            self.assertEqual(_findings(root, "R4"), [])

    def test_r5_duplicate_import_flagged(self):
        with tempfile.TemporaryDirectory() as tmp:
            _crate(Path(tmp), {
                "src/lib.rs": "pub mod a;\npub mod b;\n",
                "src/a.rs": "pub struct Thing;\n",
                "src/b.rs": (
                    "use crate::a::Thing;\nuse crate::a::Thing;\n"),
            })
            found = _findings(Path(tmp), "R5")
            self.assertEqual(len(found), 1)
            self.assertIn("Thing", found[0].message)

    def test_r5_different_namespaces_ok(self):
        with tempfile.TemporaryDirectory() as tmp:
            _crate(Path(tmp), {
                "src/lib.rs": "pub mod a;\n",
                "src/a.rs": "pub struct Bar;\npub fn Bar() {}\n",
            })
            self.assertEqual(_findings(Path(tmp), "R5"), [])

    def test_r5_cfg_alternatives_ok(self):
        with tempfile.TemporaryDirectory() as tmp:
            _crate(Path(tmp), {
                "src/lib.rs": "pub mod a;\n",
                "src/a.rs": (
                    '#[cfg(feature = "x")]\n'
                    "pub const VALUE: u8 = 1;\n"
                    '#[cfg(not(feature = "x"))]\n'
                    "pub const VALUE: u8 = 2;\n"),
            })
            self.assertEqual(_findings(Path(tmp), "R5"), [])

    def test_r6_pub_use_widening_flagged(self):
        with tempfile.TemporaryDirectory() as tmp:
            _crate(Path(tmp), {
                "src/lib.rs": "mod inner;\npub use inner::Secret;\n",
                "src/inner.rs": "pub(crate) struct Secret;\n",
            })
            found = _findings(Path(tmp), "R6")
            self.assertEqual(len(found), 1)
            self.assertIn("Secret", found[0].message)

    def test_r7_missing_module_flagged(self):
        with tempfile.TemporaryDirectory() as tmp:
            _crate(Path(tmp), {
                "src/lib.rs": "pub mod gone;\n",
            })
            found = _findings(Path(tmp), "R7")
            self.assertEqual(len(found), 1)
            self.assertEqual(found[0].level, "error")
            self.assertIn("gone", found[0].message)

    def test_r7_cfg_gated_missing_module_is_warn(self):
        with tempfile.TemporaryDirectory() as tmp:
            _crate(Path(tmp), {
                "src/lib.rs": '#[cfg(feature = "x")]\npub mod gone;\n',
            })
            found = _findings(Path(tmp), "R7")
            self.assertEqual(len(found), 1)
            self.assertEqual(found[0].level, "warn")

    def test_r8_orphan_file_warned(self):
        with tempfile.TemporaryDirectory() as tmp:
            _crate(Path(tmp), {
                "src/lib.rs": "pub mod inner;\n",
                "src/inner.rs": "pub struct Thing;\n",
                "src/dead.rs": "pub struct Dead;\n",
            })
            found = _findings(Path(tmp), "R8")
            self.assertEqual(len(found), 1)
            self.assertIn("dead.rs", found[0].file)

    def test_real_workspace_is_clean(self):
        found = CHECK.check_workspace(RUST_ROOT)
        errors = [f for f in found if f.level == "error"]
        warns = [f for f in found if f.level == "warn"]
        self.assertEqual(errors, [])
        self.assertEqual(warns, [])


if __name__ == "__main__":
    unittest.main()
