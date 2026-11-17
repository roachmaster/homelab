import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
import zlib


SCRIPT = Path(__file__).resolve().parents[1] / "pixel_art.py"
TRANSPARENT = (0, 0, 0, 0)
MAGENTA = (255, 0, 255, 255)
BLUE = (0, 0, 255, 255)
CYAN = (0, 255, 255, 255)
NAVY = (0, 0, 64, 255)
GOLD = (255, 192, 0, 255)


def recipe_fixture():
    return {
        "version": 1,
        "canvas": {"width": 12, "height": 12, "pivot": [4, 9]},
        "palette": {
            "guide": "#FF00FF",
            "base": "#0000FF",
            "accent": "#00FFFF",
            "shadow": "#000040",
            "trim": "#FFC000",
        },
        "parts": {
            "root": {"offset": [3, 4]},
            "hand": {"parent": "root", "offset": [2, 1]},
        },
        "layers": [
            {
                "name": "joint-guide",
                "stage": "greybox",
                "part": "hand",
                "draw": [
                    {"op": "line", "from": [0, 0], "to": [1, 0], "color": "guide"}
                ],
            },
            {
                "name": "glove-base",
                "stage": "flat",
                "part": "hand",
                "draw": [
                    {"op": "rect", "at": [0, 0], "size": [2, 2], "color": "base"},
                    {"op": "pixel", "at": [2, 0], "color": "accent"},
                ],
            },
            {
                "name": "glove-shadow",
                "stage": "finish",
                "part": "hand",
                "draw": [{"op": "pixel", "at": [0, 0], "color": "shadow"}],
            },
            {
                "name": "glove-trim",
                "stage": "finish",
                "part": "hand",
                "skin": "gold",
                "draw": [{"op": "pixel", "at": [1, 1], "color": "trim"}],
            },
        ],
        "clips": {
            "attack": [
                {
                    "duration_ms": 90,
                    "offsets": {"root": [-1, 0], "hand": [1, -1]},
                    "hitboxes": {
                        "body": {"part": "root", "at": [0, 0], "size": [2, 3]},
                        "attack": {"part": "hand", "at": [2, 0], "size": [2, 1]},
                    },
                },
                {
                    "duration_ms": 120,
                    "offsets": {},
                    "hitboxes": {
                        "body": {"part": "root", "at": [0, 0], "size": [2, 3]},
                    },
                },
            ]
        },
    }


def read_rgba_png(path):
    """Return rows from the tool's unfiltered RGBA8 sheet."""
    png = path.read_bytes()
    if png[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("invalid PNG signature")
    cursor = 8
    compressed = bytearray()
    while cursor < len(png):
        size, kind = struct.unpack_from(">I4s", png, cursor)
        data = png[cursor + 8 : cursor + 8 + size]
        if kind == b"IHDR":
            width, height, depth, color_type, _, _, interlace = struct.unpack(
                ">IIBBBBB", data
            )
            if (depth, color_type, interlace) != (8, 6, 0):
                raise ValueError("expected a non-interlaced RGBA8 PNG")
        elif kind == b"IDAT":
            compressed.extend(data)
        cursor += 12 + size
    scanlines = zlib.decompress(compressed)
    stride = 1 + width * 4
    if len(scanlines) != stride * height:
        raise ValueError("wrong PNG scanline size")
    rows = []
    for offset in range(0, len(scanlines), stride):
        if scanlines[offset] != 0:
            raise ValueError("expected unfiltered scanlines")
        rows.append(scanlines[offset + 1 : offset + stride])
    return rows


def cell_pixel(rows, rect, x, y):
    left, top, width, height = rect
    if not (0 <= x < width and 0 <= y < height):
        raise ValueError("pixel is outside the atlas cell")
    start = (left + x) * 4
    return tuple(rows[top + y][start : start + 4])


class PixelArtTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.workdir = Path(temp.name)
        self.source = self.workdir / "recipe.json"
        self.recipe = recipe_fixture()
        self.save_recipe()

    def save_recipe(self):
        self.source.write_text(json.dumps(self.recipe), encoding="utf-8")

    def run_cli(self, command, *args, success=True):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), command, str(self.source), *args],
            capture_output=True,
            text=True,
            timeout=10,
        )
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0)
        return result

    def export(self, stage, skin=None):
        out = self.workdir / (skin or stage)
        args = ["--stage", stage, "--out", str(out)]
        if skin is not None:
            args.extend(["--skin", skin])
        self.run_cli("export", *args)
        manifest = json.loads((out / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest["version"], 1)
        self.assertEqual(manifest["image"], "sheet.png")
        self.assertEqual(manifest["stage"], stage)
        self.assertEqual(manifest["skin"], skin)
        return manifest, read_rgba_png(out / manifest["image"])

    def test_exports_share_motion_and_isolate_layer_stages(self):
        self.run_cli("check")
        greybox, grey_pixels = self.export("greybox")
        flat, flat_pixels = self.export("flat")
        final, final_pixels = self.export("final")
        skinned, skin_pixels = self.export("final", "gold")

        for manifest in (greybox, flat, final, skinned):
            self.assertEqual(manifest["frame_size"], [12, 12])
            self.assertEqual(manifest["pivot"], [4, 9])
            self.assertEqual(manifest["clips"], greybox["clips"])
        active, recovery = greybox["clips"]["attack"]
        self.assertEqual(active["duration_ms"], 90)
        self.assertEqual(
            active["hitboxes"], {"body": [2, 4, 2, 3], "attack": [7, 4, 2, 1]}
        )
        self.assertEqual(recovery["duration_ms"], 120)
        self.assertEqual(recovery["hitboxes"], {"body": [3, 4, 2, 3]})
        for frame in (active, recovery):
            self.assertEqual(frame["rect"][2:], [12, 12])

        rect = active["rect"]
        self.assertEqual(cell_pixel(grey_pixels, rect, 5, 4), MAGENTA)
        self.assertEqual(cell_pixel(grey_pixels, rect, 6, 4), MAGENTA)
        self.assertEqual(cell_pixel(grey_pixels, rect, 6, 5), TRANSPARENT)
        self.assertEqual(cell_pixel(flat_pixels, rect, 5, 4), BLUE)
        self.assertEqual(cell_pixel(final_pixels, rect, 5, 4), NAVY)
        self.assertEqual(cell_pixel(final_pixels, rect, 6, 5), BLUE)
        self.assertEqual(cell_pixel(skin_pixels, rect, 6, 5), GOLD)
        self.assertEqual(cell_pixel(final_pixels, rect, 7, 4), CYAN)
        self.assertEqual(cell_pixel(final_pixels, recovery["rect"], 5, 5), NAVY)

    def test_steps_preserve_draw_order_and_accumulate_pixels(self):
        out = self.workdir / "trace"
        self.run_cli(
            "steps", "--clip", "attack", "--frame", "0", "--stage", "final",
            "--skin", "gold", "--out", str(out),
        )
        details = json.loads((out / "steps.json").read_text(encoding="utf-8"))
        self.assertEqual(details["version"], 1)
        self.assertEqual(details["image"], "steps.png")
        self.assertEqual(details["clip"], "attack")
        self.assertEqual(details["frame"], 0)
        self.assertEqual(details["stage"], "final")
        self.assertEqual(details["skin"], "gold")
        steps = details["steps"]
        layers = self.recipe["layers"][1:]
        self.assertEqual(
            [(step["layer"], step["part"], step["draw"]) for step in steps],
            [
                (layer["name"], layer["part"], draw)
                for layer in layers
                for draw in layer["draw"]
            ],
        )
        rows = read_rgba_png(out / details["image"])
        first, second, third, fourth = [step["rect"] for step in steps]
        self.assertEqual(cell_pixel(rows, first, 5, 4), BLUE)
        self.assertEqual(cell_pixel(rows, first, 7, 4), TRANSPARENT)
        self.assertEqual(cell_pixel(rows, second, 7, 4), CYAN)
        self.assertEqual(cell_pixel(rows, third, 5, 4), NAVY)
        self.assertEqual(cell_pixel(rows, fourth, 5, 4), NAVY)
        self.assertEqual(cell_pixel(rows, fourth, 7, 4), CYAN)
        self.assertEqual(cell_pixel(rows, fourth, 6, 5), GOLD)

    def test_polygon_fills_the_part_in_each_pose(self):
        self.recipe["layers"][1]["draw"] = [
            {
                "op": "polygon",
                "points": [[0, 0], [4, 0], [0, 4]],
                "color": "base",
            }
        ]
        self.save_recipe()
        flat, rows = self.export("flat")
        active, recovery = flat["clips"]["attack"]
        self.assertEqual(cell_pixel(rows, active["rect"], 6, 5), BLUE)
        self.assertEqual(cell_pixel(rows, active["rect"], 8, 7), TRANSPARENT)
        self.assertEqual(cell_pixel(rows, recovery["rect"], 6, 6), BLUE)
        self.assertEqual(cell_pixel(rows, recovery["rect"], 6, 4), TRANSPARENT)

    def test_joint_rotations_move_descendants_hitboxes_and_step_previews(self):
        self.recipe["parts"]["probe"] = {"parent": "hand", "offset": [1, 0]}
        self.recipe["layers"].append({
            "name": "probe-tip",
            "stage": "flat",
            "part": "probe",
            "draw": [{"op": "pixel", "at": [0, 1], "color": "trim"}],
        })
        frame = self.recipe["clips"]["attack"][0]
        frame["offsets"]["root"] = [1, 0]
        frame["rotations"] = {"root": 90, "hand": 90}
        self.save_recipe()

        manifest, rows = self.export("flat")
        active = manifest["clips"]["attack"][0]
        self.assertEqual(
            active["hitboxes"], {"body": [2, 4, 3, 2], "attack": [1, 7, 2, 1]}
        )
        self.assertEqual(cell_pixel(rows, active["rect"], 4, 7), BLUE)
        self.assertEqual(cell_pixel(rows, active["rect"], 2, 7), CYAN)
        self.assertEqual(cell_pixel(rows, active["rect"], 3, 6), GOLD)
        self.assertEqual(cell_pixel(rows, active["rect"], 5, 4), TRANSPARENT)

        out = self.workdir / "rotated-steps"
        self.run_cli("steps", "--clip", "attack", "--stage", "flat", "--out", str(out))
        details = json.loads((out / "steps.json").read_text(encoding="utf-8"))
        rows = read_rgba_png(out / details["image"])
        first, second, third = [step["rect"] for step in details["steps"]]
        self.assertEqual(cell_pixel(rows, first, 3, 6), BLUE)
        self.assertEqual(cell_pixel(rows, first, 2, 7), TRANSPARENT)
        self.assertEqual(cell_pixel(rows, second, 2, 7), CYAN)
        self.assertEqual(cell_pixel(rows, third, 3, 6), GOLD)

    def test_oblique_rotation_samples_filled_pixels_inside_hitbox_bounds(self):
        frame = self.recipe["clips"]["attack"][1]
        frame["rotations"] = {"hand": 45}
        frame["hitboxes"]["glove"] = {"part": "hand", "at": [0, 0], "size": [2, 2]}
        self.save_recipe()

        manifest, rows = self.export("flat")
        rotated = manifest["clips"]["attack"][1]
        self.assertEqual(rotated["hitboxes"]["glove"], [4, 5, 3, 3])
        self.assertEqual(cell_pixel(rows, rotated["rect"], 6, 5), TRANSPARENT)
        for x, y in ((5, 5), (4, 6), (5, 6), (6, 6), (5, 7)):
            self.assertEqual(cell_pixel(rows, rotated["rect"], x, y), BLUE)

    def test_spin_selects_drawn_views_and_mirrors_profile_about_pivot(self):
        views = (("front", [1, 1], "trim"), ("back", [2, 2], "shadow"))
        for view, at, color in views:
            self.recipe["layers"].append({
                "name": f"helmet-{view}",
                "stage": "flat",
                "part": "root",
                "view": view,
                "draw": [{"op": "pixel", "at": at, "color": color}],
            })
        profile = self.recipe["clips"]["attack"][0]
        self.recipe["clips"]["spin"] = [
            profile,
            {**profile, "view": "front"},
            {**profile, "flip_x": True},
            {**profile, "view": "back"},
        ]
        self.save_recipe()

        manifest, rows = self.export("flat")
        right, front, left, back = manifest["clips"]["spin"]
        self.assertEqual(cell_pixel(rows, right["rect"], 7, 4), CYAN)
        self.assertEqual(cell_pixel(rows, right["rect"], 3, 5), TRANSPARENT)
        self.assertEqual(cell_pixel(rows, left["rect"], 1, 4), CYAN)
        self.assertEqual(cell_pixel(rows, left["rect"], 7, 4), TRANSPARENT)
        self.assertEqual(
            left["hitboxes"], {"body": [5, 4, 2, 3], "attack": [0, 4, 2, 1]}
        )
        self.assertEqual(cell_pixel(rows, front["rect"], 3, 5), GOLD)
        self.assertEqual(cell_pixel(rows, front["rect"], 4, 6), TRANSPARENT)
        self.assertEqual(cell_pixel(rows, back["rect"], 3, 5), TRANSPARENT)
        self.assertEqual(cell_pixel(rows, back["rect"], 4, 6), NAVY)
        for frame in (front, back):
            self.assertEqual(cell_pixel(rows, frame["rect"], 7, 4), TRANSPARENT)

    def test_check_rejects_a_part_cycle(self):
        self.recipe["parts"]["root"]["parent"] = "hand"
        self.save_recipe()
        result = self.run_cli("check", success=False)
        self.assertIn("cycle", (result.stdout + result.stderr).lower())
        self.assertNotIn("Traceback", result.stderr)


if __name__ == "__main__":
    unittest.main()
