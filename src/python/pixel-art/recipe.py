"""Read and validate the editable sprite recipe."""

from dataclasses import dataclass
from functools import lru_cache
import json
import math
import re


class RecipeError(ValueError):
    """A recipe or requested view cannot be rendered."""


STAGES = ("greybox", "flat", "final")
DRAW_FIELDS = {
    "pixel": ("at",),
    "rect": ("at", "size"),
    "line": ("from", "to"),
    "polygon": ("points",),
}


@lru_cache(maxsize=360)
def rotation(angle):
    if angle % 90 == 0:
        return ((1, 0), (0, 1), (-1, 0), (0, -1))[angle // 90]
    radians = math.radians(angle)
    return math.cos(radians), math.sin(radians)


@dataclass(frozen=True)
class PartPose:
    x: float
    y: float
    angle: int
    flip_x: bool = False

    def point(self, x, y):
        cosine, sine = rotation(self.angle)
        rotated_x = x * cosine - y * sine
        return self.x + (-rotated_x if self.flip_x else rotated_x), self.y + x * sine + y * cosine

    def local_point(self, x, y):
        cosine, sine = rotation(self.angle)
        delta_x, delta_y = x - self.x, y - self.y
        if self.flip_x:
            delta_x = -delta_x
        return delta_x * cosine + delta_y * sine, -delta_x * sine + delta_y * cosine

    def bounds(self, x, y, width, height):
        # Each pixel is a unit square centered on an integer coordinate.
        corners = [
            self.point(px, py)
            for px in (x - 0.5, x + width - 0.5)
            for py in (y - 0.5, y + height - 0.5)
        ]
        xs, ys = zip(*corners)
        left, top = math.ceil(min(xs)), math.ceil(min(ys))
        right, bottom = math.floor(max(xs)), math.floor(max(ys))
        return [left, top, right - left + 1, bottom - top + 1]


def fields(value, context, required, optional=()):
    if not isinstance(value, dict):
        raise RecipeError(f"{context} must be an object")
    missing = set(required) - value.keys()
    unexpected = value.keys() - set(required) - set(optional)
    if missing:
        raise RecipeError(f"{context} is missing {', '.join(sorted(missing))}")
    if unexpected:
        raise RecipeError(f"{context} has unknown fields: {', '.join(sorted(unexpected))}")


def entries(value, context, maximum, allow_empty=False):
    if not isinstance(value, dict) or (not value and not allow_empty) or len(value) > maximum:
        raise RecipeError(f"{context} must be an object with {0 if allow_empty else 1}–{maximum} entries")
    for name in value:
        identifier(name, context)
    return value.items()


def identifier(value, context):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z][A-Za-z0-9_-]{0,63}", value):
        raise RecipeError(f"{context} must use IDs that start with a letter")


def reference(value, choices, context):
    if not isinstance(value, str) or value not in choices:
        raise RecipeError(f"{context} has unknown ID {value!r}")


def integer(value, minimum, maximum, context):
    if type(value) is not int or not minimum <= value <= maximum:
        raise RecipeError(f"{context} must be an integer from {minimum} to {maximum}")


def pair(value, context, minimum=-512, maximum=512):
    if not isinstance(value, list) or len(value) != 2:
        raise RecipeError(f"{context} must be a pair of integers")
    for item in value:
        integer(item, minimum, maximum, context)


def sequence(value, context, minimum, maximum):
    if not isinstance(value, list) or not minimum <= len(value) <= maximum:
        raise RecipeError(f"{context} must contain {minimum}–{maximum} items")
    return value


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise RecipeError(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def read_recipe(path):
    # A recipe is working source, so misspelled fields and duplicate keys are errors.
    if path.stat().st_size > 4 * 1024 * 1024:
        raise RecipeError("recipe exceeds 4 MiB")
    with path.open(encoding="utf-8") as source:
        recipe = json.load(source, object_pairs_hook=unique_object)
    fields(recipe, "recipe", ("version", "canvas", "palette", "parts", "layers", "clips"))
    integer(recipe["version"], 1, 1, "version")
    canvas = recipe["canvas"]
    fields(canvas, "canvas", ("width", "height", "pivot"))
    for dimension in ("width", "height"):
        integer(canvas[dimension], 1, 128, f"canvas.{dimension}")
    pair(canvas["pivot"], "canvas.pivot", 0, 127)
    if not (canvas["pivot"][0] < canvas["width"] and canvas["pivot"][1] < canvas["height"]):
        raise RecipeError("canvas.pivot must be inside the canvas")
    for name, color in entries(recipe["palette"], "palette", 256):
        if not isinstance(color, str) or not re.fullmatch(r"#[0-9a-fA-F]{6}", color):
            raise RecipeError(f"palette.{name} must be an RGB color such as #627292")

    parts = recipe["parts"]
    for name, part in entries(parts, "parts", 64):
        context = f"parts.{name}"
        fields(part, context, ("offset",), ("parent",))
        pair(part["offset"], f"{context}.offset")
        if "parent" in part:
            reference(part["parent"], parts, f"{context}.parent")
    part_poses(parts, {}, {})

    names = set()
    views = set()
    for index, layer in enumerate(sequence(recipe["layers"], "layers", 1, 256)):
        context = f"layers[{index}]"
        fields(layer, context, ("name", "stage", "part", "draw"), ("skin", "view"))
        name = layer["name"]
        if not isinstance(name, str) or not name.strip() or len(name) > 100 or name in names:
            raise RecipeError(f"{context}.name must be nonempty and unique (up to 100 characters)")
        names.add(name)
        reference(layer["stage"], ("greybox", "flat", "finish"), f"{context}.stage")
        reference(layer["part"], parts, f"{context}.part")
        if "view" in layer:
            identifier(layer["view"], f"{context}.view")
            views.add(layer["view"])
        if "skin" in layer:
            identifier(layer["skin"], f"{context}.skin")
            if layer["stage"] == "greybox":
                raise RecipeError(f"{context}.skin is available on flat and finish layers")
        for draw_index, operation in enumerate(sequence(layer["draw"], f"{context}.draw", 1, 512)):
            validate_draw(operation, recipe["palette"], f"{context}.draw[{draw_index}]")

    frame_count = 0
    for clip, frames in entries(recipe["clips"], "clips", 64):
        frame_count += len(sequence(frames, f"clips.{clip}", 1, 512))
        for index, frame in enumerate(frames):
            validate_frame(frame, parts, views, f"clips.{clip}[{index}]")
    if frame_count > 512:
        raise RecipeError("a recipe can contain up to 512 animation frames")
    return recipe


def validate_draw(operation, palette, context):
    if not isinstance(operation, dict):
        raise RecipeError(f"{context} must be a drawing operation")
    op = operation.get("op")
    reference(op, DRAW_FIELDS, f"{context}.op")
    fields(operation, context, ("op", "color", *DRAW_FIELDS[op]))
    reference(operation["color"], palette, f"{context}.color")
    for operand in DRAW_FIELDS[op]:
        value = operation[operand]
        if operand == "points":
            for point in sequence(value, f"{context}.points", 3, 64):
                pair(point, f"{context}.points")
        elif operand == "size":
            pair(value, f"{context}.size", 1)
        else:
            pair(value, f"{context}.{operand}")


def validate_frame(frame, parts, views, context):
    fields(frame, context, ("duration_ms",), ("offsets", "rotations", "view", "flip_x", "hitboxes"))
    integer(frame["duration_ms"], 1, 60_000, f"{context}.duration_ms")
    if "view" in frame:
        reference(frame["view"], views, f"{context}.view")
    if "flip_x" in frame and not isinstance(frame["flip_x"], bool):
        raise RecipeError(f"{context}.flip_x must be a boolean")
    for part, delta in entries(frame.get("offsets", {}), f"{context}.offsets", 64, True):
        reference(part, parts, f"{context}.offsets")
        pair(delta, f"{context}.offsets.{part}")
    for part, angle in entries(frame.get("rotations", {}), f"{context}.rotations", 64, True):
        reference(part, parts, f"{context}.rotations")
        integer(angle, -180, 180, f"{context}.rotations.{part}")
    for name, box in entries(frame.get("hitboxes", {}), f"{context}.hitboxes", 64, True):
        box_context = f"{context}.hitboxes.{name}"
        fields(box, box_context, ("part", "at", "size"))
        reference(box["part"], parts, f"{box_context}.part")
        pair(box["at"], f"{box_context}.at")
        pair(box["size"], f"{box_context}.size", 1)


def part_poses(parts, offsets, rotations, mirror_axis=None):
    poses = {}
    visiting = set()

    def resolve(name):
        if name in poses:
            return poses[name]
        if name in visiting:
            raise RecipeError(f"part parent cycle at {name!r}")
        visiting.add(name)
        part = parts[name]
        parent = resolve(part["parent"]) if "parent" in part else PartPose(0, 0, 0)
        x, y = part["offset"]
        dx, dy = offsets.get(name, (0, 0))
        world_x, world_y = parent.point(x + dx, y + dy)
        poses[name] = PartPose(world_x, world_y, (parent.angle + rotations.get(name, 0)) % 360)
        visiting.remove(name)
        return poses[name]

    for name in parts:
        resolve(name)
    return {
        name: PartPose(
            round(pose.x) if mirror_axis is None else 2 * mirror_axis - round(pose.x),
            round(pose.y),
            pose.angle,
            mirror_axis is not None,
        )
        for name, pose in poses.items()
    }


def visible_layers(recipe, stage, skin, view):
    if skin is not None and not any(layer.get("skin") == skin for layer in recipe["layers"]):
        raise RecipeError(f"unknown skin {skin!r}")
    stages = ("flat", "finish") if stage == "final" else (stage,)
    layers = [
        layer for layer in recipe["layers"]
        if layer["stage"] in stages and layer.get("skin") in (None, skin) and layer.get("view") == view
    ]
    required_stage = "flat" if stage == "final" else stage
    if not any(layer["stage"] == required_stage for layer in layers):
        raise RecipeError(f"add a visible {required_stage} layer for {view or 'the default view'}")
    return layers
