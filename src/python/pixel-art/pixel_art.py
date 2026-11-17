"""Render editable pixel recipes to transparent sprite sheets and drawing steps."""

import argparse
from fractions import Fraction
import json
import math
from pathlib import Path
import struct
from tempfile import TemporaryDirectory
import zlib

from recipe import RecipeError, STAGES, frame_hitboxes, frame_poses, read_recipe, visible_layers


class Raster:
    def __init__(self, width, height):
        self.width = width
        self.height = height
        self.pixels = bytearray(width * height * 4)

    def rect(self, x, y, width, height, color):
        left = max(0, x)
        right = min(self.width, x + width)
        if left >= right:
            return
        row = color * (right - left)
        for py in range(max(0, y), min(self.height, y + height)):
            start = (py * self.width + left) * 4
            self.pixels[start:start + len(row)] = row

    def line(self, a, b, color):
        x, y = a
        end_x, end_y = b
        dx = abs(end_x - x)
        dy = -abs(end_y - y)
        step_x = 1 if x < end_x else -1
        step_y = 1 if y < end_y else -1
        error = dx + dy
        while True:
            self.rect(x, y, 1, 1, color)
            if (x, y) == b:
                break
            twice_error = 2 * error
            if twice_error >= dy:
                error += dy
                x += step_x
            if twice_error <= dx:
                error += dx
                y += step_y

    def polygon(self, points, color):
        edges = list(zip(points, points[1:] + points[:1]))
        min_y = max(0, min(y for _, y in points))
        max_y = min(self.height - 1, max(y for _, y in points))
        # Half-open scanline crossings count a shared vertex exactly once.
        for y in range(min_y, max_y + 1):
            crossings = []
            for (x1, y1), (x2, y2) in edges:
                if min(y1, y2) <= y < max(y1, y2):
                    crossings.append(x1 + Fraction((y - y1) * (x2 - x1), y2 - y1))
            crossings.sort()
            for left, right in zip(crossings[::2], crossings[1::2]):
                first, last = math.ceil(left), math.floor(right)
                self.rect(first, y, last - first + 1, 1, color)
        for a, b in edges:
            self.line(a, b, color)

    def draw(self, operation, pose, palette):
        color = palette[operation["color"]]
        if pose.angle == 0 and not pose.flip_x:
            self.draw_untransformed(operation, (pose.x, pose.y), color)
            return

        x, y, width, height = operation_bounds(operation)
        left, top, span_x, span_y = pose.bounds(x, y, width, height)
        right, bottom = min(self.width, left + span_x), min(self.height, top + span_y)
        left, top = max(0, left), max(0, top)
        if left >= right or top >= bottom:
            return

        # Sample the part-local stencil from the final grid. Forward scattering
        # rotated pixels can leave holes in otherwise solid limbs.
        mask = Raster(width, height)
        mask.draw_untransformed(operation, (-x, -y), color)
        for py in range(top, bottom):
            for px in range(left, right):
                local_x, local_y = pose.local_point(px, py)
                sx, sy = math.floor(local_x + 0.5) - x, math.floor(local_y + 0.5) - y
                if 0 <= sx < width and 0 <= sy < height and mask.pixels[(sy * width + sx) * 4 + 3]:
                    self.rect(px, py, 1, 1, color)

    def draw_untransformed(self, operation, origin, color):
        kind = operation["op"]
        if kind in ("pixel", "rect"):
            x, y = translated(operation["at"], origin)
            self.rect(x, y, *operation.get("size", (1, 1)), color)
        elif kind == "line":
            self.line(translated(operation["from"], origin), translated(operation["to"], origin), color)
        else:
            self.polygon([translated(point, origin) for point in operation["points"]], color)

    def copy_into(self, destination, x, y):
        row_bytes = self.width * 4
        for row in range(self.height):
            start = row * row_bytes
            target = ((y + row) * destination.width + x) * 4
            destination.pixels[target:target + row_bytes] = self.pixels[start:start + row_bytes]

    def write_png(self, path):
        def chunk(kind, data):
            return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

        row_bytes = self.width * 4
        rows = b"".join(
            b"\0" + self.pixels[start:start + row_bytes]
            for start in range(0, len(self.pixels), row_bytes)
        )
        header = struct.pack(">IIBBBBB", self.width, self.height, 8, 6, 0, 0, 0)
        with path.open("wb") as sink:
            sink.write(b"\x89PNG\r\n\x1a\n")
            sink.write(chunk(b"IHDR", header))
            sink.write(chunk(b"IDAT", zlib.compress(rows)))
            sink.write(chunk(b"IEND", b""))


class Atlas:
    def __init__(self, width, height, count):
        self.width = width
        self.height = height
        self.columns = min(8, count)
        self.index = 0
        rows = (count + self.columns - 1) // self.columns
        self.sheet = Raster(width * self.columns, height * rows)

    def add(self, image):
        row, column = divmod(self.index, self.columns)
        x, y = column * self.width, row * self.height
        image.copy_into(self.sheet, x, y)
        self.index += 1
        return [x, y, self.width, self.height]


def translated(point, origin):
    return point[0] + origin[0], point[1] + origin[1]


def operation_bounds(operation):
    if "at" in operation:
        return *operation["at"], *operation.get("size", (1, 1))
    points = operation["points"] if operation["op"] == "polygon" else (operation["from"], operation["to"])
    xs, ys = zip(*points)
    return min(xs), min(ys), max(xs) - min(xs) + 1, max(ys) - min(ys) + 1


def rgba_palette(recipe):
    return {name: bytes.fromhex(value[1:]) + b"\xff" for name, value in recipe["palette"].items()}


def write_output(directory, atlas, index_file, metadata):
    directory.mkdir(parents=True, exist_ok=True)
    image_file = metadata["image"]
    for name in (image_file, index_file):
        if (directory / name).is_dir():
            raise RecipeError(f"output path is a directory: {directory / name}")
    with TemporaryDirectory(dir=directory) as scratch:
        temporary = Path(scratch)
        atlas.sheet.write_png(temporary / image_file)
        (temporary / index_file).write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
        (temporary / image_file).replace(directory / image_file)
        (temporary / index_file).replace(directory / index_file)


def export_sprites(recipe, args):
    views = dict.fromkeys(frame.get("view") for frames in recipe["clips"].values() for frame in frames)
    layers_by_view = {view: visible_layers(recipe, args.stage, args.skin, view) for view in views}
    canvas = recipe["canvas"]
    width, height = canvas["width"], canvas["height"]
    palette = rgba_palette(recipe)
    total_frames = sum(map(len, recipe["clips"].values()))
    atlas = Atlas(width, height, total_frames)
    clips = {}
    for clip, frames in recipe["clips"].items():
        clips[clip] = []
        for frame in frames:
            poses = frame_poses(recipe, frame)
            image = Raster(width, height)
            for layer in layers_by_view[frame.get("view")]:
                for operation in layer["draw"]:
                    image.draw(operation, poses[layer["part"]], palette)
            clips[clip].append({
                "rect": atlas.add(image),
                "duration_ms": frame["duration_ms"],
                "hitboxes": frame_hitboxes(frame, poses),
            })
    write_output(args.out, atlas, "manifest.json", {
        "version": 1,
        "image": "sheet.png",
        "stage": args.stage,
        "skin": args.skin,
        "frame_size": [width, height],
        "pivot": canvas["pivot"],
        "clips": clips,
    })


def export_steps(recipe, args):
    frames = recipe["clips"].get(args.clip)
    if frames is None:
        raise RecipeError(f"unknown clip {args.clip!r}")
    if not 0 <= args.frame < len(frames):
        raise RecipeError(f"frame for {args.clip!r} must be between 0 and {len(frames) - 1}")
    frame = frames[args.frame]
    layers = visible_layers(recipe, args.stage, args.skin, frame.get("view"))
    if args.layer is not None and not any(layer["name"] == args.layer for layer in layers):
        raise RecipeError(f"layer {args.layer!r} is unavailable in the selected stage and skin")

    count = sum(len(layer["draw"]) for layer in layers if args.layer in (None, layer["name"]))
    if count > 512:
        raise RecipeError("a step sheet can contain up to 512 operations; use --layer for a smaller view")
    width, height = recipe["canvas"]["width"], recipe["canvas"]["height"]
    palette = rgba_palette(recipe)
    poses = frame_poses(recipe, frame)
    atlas = Atlas(width, height, count)
    image = Raster(width, height)
    steps = []
    for layer in layers:
        for operation in layer["draw"]:
            image.draw(operation, poses[layer["part"]], palette)
            if args.layer in (None, layer["name"]):
                steps.append({
                    "rect": atlas.add(image),
                    "layer": layer["name"],
                    "part": layer["part"],
                    "draw": operation,
                })
        if args.layer == layer["name"]:
            break
    write_output(args.out, atlas, "steps.json", {
        "version": 1,
        "image": "steps.png",
        "clip": args.clip,
        "frame": args.frame,
        "stage": args.stage,
        "skin": args.skin,
        "steps": steps,
    })


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    check = commands.add_parser("check", help="validate the recipe and pose references")
    check.add_argument("recipe", type=Path)
    export = commands.add_parser("export", help="write a sprite atlas and animation manifest")
    steps = commands.add_parser("steps", help="show cumulative drawing operations for one pose")
    for command in (export, steps):
        command.add_argument("recipe", type=Path)
        command.add_argument("--stage", choices=STAGES)
        command.add_argument("--skin", help="include layers tagged with this skin")
        command.add_argument("--out", type=Path, required=True, help="output directory")
    export.add_argument("--illustrated", type=Path, metavar="SKIN_JSON", help="use editable SVG part art")
    export.add_argument("--scale", type=int, help="pixels per logical unit for illustrated art (default 4)")
    steps.add_argument("--clip", required=True)
    steps.add_argument("--frame", type=int, default=0, help="zero-based frame index")
    steps.add_argument("--layer", help="record only this visible layer over its underlying art")
    args = parser.parse_args()

    try:
        recipe = read_recipe(args.recipe)
        if args.command == "check":
            print(f"Valid recipe: {len(recipe['parts'])} parts, {len(recipe['clips'])} clips")
            return
        if args.command == "export" and args.illustrated is not None:
            from illustrated import export_illustrated

            if args.skin is not None or args.stage not in (None, "final"):
                raise RecipeError("illustrated export takes its finished art from the illustrated skin")
            export_illustrated(recipe, args.illustrated, args.scale, args.out)
        else:
            args.stage = args.stage or "greybox"
            if args.command == "export":
                if args.scale is not None:
                    raise RecipeError("--scale requires --illustrated")
                export_sprites(recipe, args)
            else:
                export_steps(recipe, args)
    except (OSError, ValueError, RecursionError) as error:
        parser.exit(2, f"{parser.prog}: {error}\n")
    print(args.out)


if __name__ == "__main__":
    main()
