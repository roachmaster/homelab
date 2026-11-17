import json
import math
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest

from test_pixel_art import BLUE, CYAN, MAGENTA, SCRIPT, cell_pixel, recipe_fixture


CONVERT = shutil.which("magick") or shutil.which("convert")


def read_illustrated_png(path):
    with path.open("rb") as image:
        width, height = struct.unpack(">II", image.read(24)[16:24])
    rgba = subprocess.run(
        [CONVERT, str(path), "-depth", "8", "rgba:-"],
        check=True,
        capture_output=True,
        timeout=10,
    ).stdout
    stride = width * 4
    if len(rgba) != height * stride:
        raise ValueError("wrong PNG scanline size")
    return [rgba[offset : offset + stride] for offset in range(0, len(rgba), stride)]


@unittest.skipUnless(
    shutil.which("gdk-pixbuf-thumbnailer") and CONVERT,
    "illustrated export requires gdk-pixbuf-thumbnailer and ImageMagick",
)
class IllustratedExportTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.workdir = Path(temp.name)
        self.recipe = recipe_fixture()
        self.skin = {"version": 1, "views": {}}

    def view(self, name, art, definitions=""):
        filename = f"{name}.svg"
        self.skin["views"][name] = filename
        (self.workdir / filename).write_text(
            '<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48"'
            ' viewBox="0 0 12 12">'
            f"{definitions}"
            '<g><g id="hand" transform="translate(9 9)">'
            f"{art}</g></g></svg>",
            encoding="utf-8",
        )

    def export(self, name, *options):
        recipe_path = self.workdir / "recipe.json"
        recipe_path.write_text(json.dumps(self.recipe), encoding="utf-8")
        out = self.workdir / name
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "export", str(recipe_path), "--out", str(out), *options],
            text=True,
            capture_output=True,
            timeout=20,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        result_manifest = json.loads((out / "manifest.json").read_text(encoding="utf-8"))
        return result_manifest, out / result_manifest["image"]

    def illustrated(self):
        skin_path = self.workdir / "skin.json"
        skin_path.write_text(json.dumps(self.skin), encoding="utf-8")
        return self.export("illustrated", "--illustrated", str(skin_path), "--scale", "4")

    def test_scaled_export_keeps_logical_metadata_and_subpixel_artwork(self):
        self.recipe["clips"]["attack"][1]["rotations"] = {"root": 30}
        self.view("side", '<circle r="1" fill="#008848"/>')
        baseline, _ = self.export("greybox", "--stage", "greybox")
        manifest, image = self.illustrated()

        self.assertEqual(manifest["version"], 2)
        self.assertEqual(manifest["atlas_scale"], 4)
        self.assertEqual(manifest["frame_size"], [48, 48])
        self.assertEqual(manifest["logical_frame_size"], baseline["frame_size"])
        self.assertEqual(manifest["pivot"], baseline["pivot"])
        self.assertEqual(manifest["atlas_pivot"], [16, 36])
        self.assertEqual(list(manifest["clips"]), list(baseline["clips"]))
        frames = manifest["clips"]["attack"]
        original_frames = baseline["clips"]["attack"]
        self.assertEqual(len(frames), len(original_frames))
        for frame, original in zip(frames, original_frames):
            self.assertEqual(frame["duration_ms"], original["duration_ms"])
            self.assertEqual(frame["hitboxes"], original["hitboxes"])
            self.assertEqual(frame["rect"][2:], manifest["frame_size"])

        rows = read_illustrated_png(image)
        rect = frames[1]["rect"]
        weights = [
            (x + 0.5, y + 0.5, cell_pixel(rows, rect, x, y)[3])
            for y in range(rect[3])
            for x in range(rect[2])
        ]
        opaque_area = sum(alpha for _, _, alpha in weights)
        self.assertGreater(opaque_area, 0)
        self.assertTrue(any(0 < alpha < 255 for _, _, alpha in weights))
        center_x = sum(x * alpha for x, _, alpha in weights) / opaque_area
        center_y = sum(y * alpha for _, y, alpha in weights) / opaque_area
        self.assertAlmostEqual(center_x, 4 * (3 + math.sqrt(3) - 0.5), delta=0.3)
        self.assertAlmostEqual(center_y, 4 * (4 + 1 + math.sqrt(3) / 2), delta=0.3)

    def test_skins_select_view_grip_mirror_and_hidden_artwork(self):
        self.recipe["layers"].append(
            {
                "name": "front-guide",
                "stage": "greybox",
                "part": "hand",
                "view": "front",
                "draw": [{"op": "pixel", "at": [0, 0], "color": "guide"}],
            }
        )
        neutral = self.recipe["clips"]["attack"][1]
        neutral["hitboxes"]["glove"] = {"part": "hand", "at": [0, 0], "size": [2, 2]}
        self.recipe["clips"]["attack"] = [
            neutral,
            {**neutral, "view": "front"},
            {**neutral, "flip_x": True},
            neutral,
            neutral,
        ]
        rect = '<rect x=".125" y="0" width="2" height="2" fill="url(#ink)"/>'
        side = (
            f'<g data-grip="idle">{rect}</g>'
            '<g data-grip="grab" display="none">'
            '<rect x=".125" y="0" width="2" height="2" fill="#00ffff"/></g>'
        )

        def gradient(color):
            return (
                '<defs><linearGradient id="ink">'
                f'<stop offset="0" stop-color="{color}"/>'
                f'<stop offset="1" stop-color="{color}"/>'
                '</linearGradient></defs>'
            )

        self.view("side", side, gradient("#0000ff"))
        self.view("front", rect, gradient("#ff00ff"))
        self.skin["grips"] = {"attack": {"3": ["hand"]}}
        self.skin["hidden"] = {"attack": {"4": ["hand"]}}

        manifest, image = self.illustrated()
        rows = read_illustrated_png(image)
        frames = manifest["clips"]["attack"]
        side, front, mirrored, grab, hidden = [frame["rect"] for frame in frames]
        self.assertEqual(cell_pixel(rows, side, 24, 24), BLUE)
        self.assertEqual(cell_pixel(rows, front, 24, 24), MAGENTA)
        self.assertEqual(cell_pixel(rows, mirrored, 8, 24), BLUE)
        self.assertEqual(cell_pixel(rows, mirrored, 24, 24)[3], 0)
        self.assertEqual(cell_pixel(rows, grab, 24, 24), CYAN)
        self.assertEqual(cell_pixel(rows, side, 0, 0)[3], 0)
        self.assertEqual(cell_pixel(rows, hidden, 24, 24)[3], 0)
        self.assertEqual(frames[4]["hitboxes"], frames[0]["hitboxes"])
