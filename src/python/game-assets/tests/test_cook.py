import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
from tempfile import TemporaryDirectory
import unittest


SOURCE = Path(__file__).resolve().parents[1]
LEVELS = SOURCE / "examples/levels"
COOK = (sys.executable, "-B", str(SOURCE / "cook.py"))
RENDERER_AVAILABLE = shutil.which("gdk-pixbuf-thumbnailer") and (
    shutil.which("magick") or shutil.which("convert")
)


class CookTest(unittest.TestCase):
    def setUp(self):
        temporary = TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def _run(self, output, *arguments):
        return subprocess.run(
            [*COOK, "--out", str(output), *map(str, arguments)],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_rejects_unresolved_and_invalid_authored_content(self):
        source = self.root / "levels"
        shutil.copytree(LEVELS, source)
        path = source / "room-one.json"
        room = json.loads(path.read_bytes())
        for field, value, expected in (
            ("surface", "missing", "unknown surface ID"),
            ("atlas", "missing", "unresolved atlas dependency"),
            ("spawns", [room["spawns"][0]], "expected 3..4 array items"),
            ("timing", {}, "unknown fields timing"),
        ):
            with self.subTest(field=field):
                path.write_text(json.dumps({**room, field: value}), encoding="utf-8")
                result = self._run(self.root / "bundle", "--levels", source)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(expected, result.stderr)
                self.assertFalse((self.root / "bundle").exists())

    @unittest.skipUnless(RENDERER_AVAILABLE, "illustrated rendering tools are required")
    def test_cook_is_reproducible_and_publishes_only_complete_generations(self):
        source = self.root / "levels"
        shutil.copytree(LEVELS, source)
        output = self.root / "bundle"
        clean = self.root / "clean"
        output.mkdir()

        first = self._run(output, "--levels", source)
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertTrue(output.is_symlink())
        original_generation = output.resolve()
        self.assertEqual(self._run(output, "--levels", source).returncode, 0)
        self.assertEqual(output.resolve(), original_generation)
        self.assertEqual(self._run(clean, "--clean", "--levels", source).returncode, 0)

        manifest_bytes = (output / "manifest.json").read_bytes()
        self.assertEqual(manifest_bytes, (clean / "manifest.json").read_bytes())
        manifest = json.loads(manifest_bytes)
        self.assertEqual(manifest["version"], 1)
        entries = manifest["entries"]
        for asset in entries.values():
            with self.subTest(asset=asset["path"]):
                data = (output / asset["path"]).read_bytes()
                self.assertEqual(data, (clean / asset["path"]).read_bytes())
                self.assertEqual(hashlib.sha256(data).hexdigest(), asset["sha256"])
        self.assertEqual((output / "cook-state.json").read_bytes(), (clean / "cook-state.json").read_bytes())
        self.assertEqual(entries["room-one"]["optional_dependencies"], ["scout-atlas"])
        self.assertEqual(entries["scout-atlas"]["dependencies"], ["scout-atlas-image"])

        atlas = json.loads((output / entries["scout-atlas"]["path"]).read_bytes())
        room = json.loads((output / entries["room-one"]["path"]).read_bytes())
        yard = json.loads((output / entries["traversal-yard"]["path"]).read_bytes())
        timing = room["timing"]
        self.assertEqual(yard["timing"], timing)
        collider = {"left": -10240, "top": -53248, "right": 18432, "bottom": 0}
        self.assertEqual(room["collider"], collider)
        self.assertEqual(yard["collider"], collider)
        for clip, durations in timing.items():
            self.assertEqual(durations, [frame["duration_ms"] for frame in atlas["clips"][clip]])
        self.assertEqual(atlas["clips"]["jump"][5]["hitboxes"]["body"][1], 15)
        self.assertIsNone(room["spawns"][0]["patrol"])

        level_path = source / "traversal-yard.json"
        authored_yard = json.loads(level_path.read_bytes())
        authored_yard["surface"] = "unknown"
        level_path.write_text(json.dumps(authored_yard), encoding="utf-8")
        self.assertNotEqual(self._run(output, "--levels", source).returncode, 0)
        self.assertEqual(output.resolve(), original_generation)
        self.assertEqual((output / "manifest.json").read_bytes(), manifest_bytes)

        authored_yard["surface"] = "stone"
        authored_yard["surfaces"][0]["friction_q16"] = 32768
        level_path.write_text(json.dumps(authored_yard), encoding="utf-8")
        self.assertEqual(self._run(output, "--levels", source).returncode, 0)
        self.assertNotEqual(output.resolve(), original_generation)
        self.assertEqual((original_generation / "manifest.json").read_bytes(), manifest_bytes)
        updated = json.loads((output / "manifest.json").read_bytes())["entries"]
        self.assertEqual(updated["scout-atlas-image"], entries["scout-atlas-image"])
        self.assertNotEqual(updated["traversal-yard"], entries["traversal-yard"])


if __name__ == "__main__":
    unittest.main()
