"""Bake part-local SVG artwork against an existing pixel recipe's animation."""

from copy import deepcopy
import json
from pathlib import Path
import re
import shutil
import struct
import subprocess
from tempfile import TemporaryDirectory
from xml.etree import ElementTree as ET

from recipe import (
    RecipeError, entries, fields, frame_hitboxes, frame_poses, integer, reference,
    sequence, unique_object,
)


SVG = "http://www.w3.org/2000/svg"
XLINK = "http://www.w3.org/1999/xlink"
LOCAL_PAINT = re.compile(r"url\(\s*['\"]?#([-\w.]+)['\"]?\s*\)")
STATIC_TAGS = {
    f"{{{SVG}}}{name}" for name in (
        "g", "path", "rect", "circle", "ellipse", "line", "polygon", "polyline", "use",
        "linearGradient", "radialGradient", "stop", "clipPath", "mask",
    )
}
ET.register_namespace("", SVG)
ET.register_namespace("xlink", XLINK)


def svg_tag(name):
    return f"{{{SVG}}}{name}"


def read_frame_parts(skin, recipe, field):
    selections = {}
    for clip, frames in entries(skin.get(field, {}), field, 64, allow_empty=True):
        reference(clip, recipe["clips"], field)
        if not isinstance(frames, dict):
            raise RecipeError(f"{field}.{clip} must be an object keyed by frame index")
        for key, parts in frames.items():
            context = f"{field}.{clip}.{key}"
            if not re.fullmatch(r"0|[1-9][0-9]{0,2}", key) or int(key) >= len(recipe["clips"][clip]):
                raise RecipeError(f"{context} does not name an existing frame")
            for part in sequence(parts, context, 1, len(recipe["parts"])):
                reference(part, recipe["parts"], context)
            if len(set(parts)) != len(parts):
                raise RecipeError(f"{context} repeats a part")
            selections[clip, int(key)] = set(parts)
    return selections


def read_views(path, recipe, definitions):
    with path.open(encoding="utf-8") as source:
        skin = json.load(source, object_pairs_hook=unique_object)
    fields(skin, "illustrated skin", ("version", "views"), ("grips", "hidden"))
    integer(skin["version"], 1, 1, "illustrated skin version")
    needed = {frame.get("view", "side") for frames in recipe["clips"].values() for frame in frames}
    files = dict(entries(skin["views"], "illustrated views", 64))
    if missing := needed - files.keys():
        raise RecipeError(f"illustrated skin is missing views: {', '.join(sorted(missing))}")
    views = {}
    for view, source in files.items():
        if not isinstance(source, str) or not source:
            raise RecipeError(f"illustrated view {view!r} must name an SVG file")
        views[view] = load_art(path.parent / source, view, recipe, definitions)
    return views, read_frame_parts(skin, recipe, "grips"), read_frame_parts(skin, recipe, "hidden")


def load_art(path, view, recipe, definitions):
    try:
        art = ET.parse(path).getroot()
    except ET.ParseError as error:
        raise RecipeError(f"{path}: {error}") from error
    groups = art.findall(svg_tag("g"))
    if art.tag != svg_tag("svg") or len(groups) != 1:
        raise RecipeError(f"{path} must contain one SVG paint group")
    canvas = recipe["canvas"]
    expected_box = (0, 0, canvas["width"], canvas["height"])
    try:
        view_box = tuple(map(float, re.split(r"[\s,]+", art.get("viewBox", "").strip())))
    except ValueError:
        view_box = ()
    if view_box != expected_box:
        raise RecipeError(f"{path} must have viewBox=\"{' '.join(map(str, expected_box))}\"")

    paint = groups[0]
    paint_parts = {}
    for node in paint:
        if node.tag != svg_tag("g"):
            raise RecipeError(f"{path} paint group must contain only named part groups")
        part = node.get("id")
        reference(part, recipe["parts"], str(path))
        if part in paint_parts:
            raise RecipeError(f"{path} repeats part {part!r}")
        paint_parts[part] = node
    if not paint_parts:
        raise RecipeError(f"{path} needs at least one part group")

    local_definitions = [node for block in art.findall(svg_tag("defs")) for node in block]
    local_ids = [node.get("id") for item in local_definitions for node in item.iter() if "id" in node.attrib]
    renames = {name: f"paint-{view}-{name}" for name in local_ids}
    if len(local_ids) != len(renames):
        raise RecipeError(f"{path} contains duplicate SVG definition IDs")
    for node in local_definitions:
        scope_paint(node, renames, path)
        definitions.append(node)

    variants = {}
    for part, node in paint_parts.items():
        node.attrib.pop("id")
        node.attrib.pop("transform", None)
        for child in node.iter():
            if "id" in child.attrib:
                raise RecipeError(f"{path} places an ID inside part {part!r}; put reusable shapes in defs")
        scope_paint(node, renames, path)
        grip_names = {child.get("data-grip") for child in node.iter() if child.get("data-grip") is not None}
        if grip_names - {"idle", "grab"}:
            raise RecipeError(f"{path} part {part!r} uses an unknown grip")
        variants[part] = {"idle": add_art_variant(definitions, node, view, part, "idle")}
        if "grab" in grip_names:
            variants[part]["grab"] = add_art_variant(definitions, node, view, part, "grab")

    shared = ET.Element(paint.tag, {key: value for key, value in paint.attrib.items() if key != "id"})
    scope_paint(shared, renames, path)
    return shared.attrib, variants


def scope_paint(root, names, path):
    def local_reference(name):
        if name not in names:
            raise RecipeError(f"{path} references unknown local SVG paint {name!r}")
        return names[name]

    for node in root.iter():
        if node.tag not in STATIC_TAGS:
            raise RecipeError(f"{path} contains unsupported SVG element {node.tag!r}")
        for key, value in node.attrib.items():
            if key == "id":
                node.set(key, local_reference(value))
            elif key in ("href", f"{{{XLINK}}}href"):
                if not value.startswith("#"):
                    raise RecipeError(f"{path} SVG references must be local")
                node.set(key, f"#{local_reference(value[1:])}")
            elif "url(" in value:
                if "url(" in LOCAL_PAINT.sub("", value):
                    raise RecipeError(f"{path} SVG paint references must be local")
                node.set(key, LOCAL_PAINT.sub(lambda match: f"url(#{local_reference(match[1])})", value))


def add_art_variant(definitions, source, view, part, grip):
    art = deepcopy(source)
    name = f"art-{view}-{part}-{grip}"
    art.set("id", name)
    for node in art.iter():
        if state := node.attrib.pop("data-grip", None):
            if state == grip:
                node.attrib.pop("display", None)
            else:
                node.set("display", "none")
    definitions.append(art)
    return name


def paint_frame(sheet, location, size, assets, poses, grips, hidden):
    x, y = location
    width, height = size
    cell = ET.SubElement(sheet, svg_tag("svg"), {
        "x": str(x), "y": str(y), "width": str(width), "height": str(height),
        "viewBox": f"0 0 {width} {height}", "overflow": "hidden",
    })
    attributes, parts = assets
    if missing := (grips | hidden) - parts.keys():
        raise RecipeError(f"illustrated view is missing overridden parts: {', '.join(sorted(missing))}")
    paint = ET.SubElement(cell, svg_tag("g"), attributes)
    for part, variants in parts.items():
        grip = "grab" if part in grips else "idle"
        if grip not in variants:
            raise RecipeError(f"illustrated part {part!r} needs grab artwork")
        if part in hidden:
            continue
        pose = poses[part]
        translate = f"translate({pose.x:.10g} {pose.y:.10g})"
        mirror = " scale(-1 1)" if pose.flip_x else ""
        ET.SubElement(paint, svg_tag("use"), {
            f"{{{XLINK}}}href": "#" + variants[grip],
            "transform": f"{translate}{mirror} rotate({pose.angle})",
        })


def render_sheet(source, raster, output, working_size, size):
    thumbnailer = shutil.which("gdk-pixbuf-thumbnailer")
    convert = shutil.which("magick") or shutil.which("convert")
    if not thumbnailer or not convert:
        raise RecipeError("illustrated export requires gdk-pixbuf-thumbnailer and ImageMagick")
    for command, path, expected_size in (
        ([thumbnailer, "-s", str(max(working_size)), str(source), str(raster)], raster, working_size),
        ([convert, str(raster), "-alpha", "on", "-filter", "box", "-resize",
          f"{size[0]}x{size[1]}!", f"PNG32:{output}"], output, size),
    ):
        result = subprocess.run(command, capture_output=True, text=True)
        if result.returncode:
            raise RecipeError(f"{Path(command[0]).name} failed: {result.stderr.strip()}")
        with path.open("rb") as image:
            prefix = image.read(24)
        if len(prefix) != 24 or prefix[:8] != b"\x89PNG\r\n\x1a\n":
            raise RecipeError(f"{Path(command[0]).name} did not produce PNG art")
        if struct.unpack(">II", prefix[16:24]) != tuple(expected_size):
            raise RecipeError(f"{Path(command[0]).name} produced unexpected atlas dimensions")


def export_illustrated(recipe, skin, scale, output):
    if scale is None:
        scale = 4
    integer(scale, 1, 8, "--scale")
    canvas = recipe["canvas"]
    width, height = canvas["width"], canvas["height"]
    frame_count = sum(map(len, recipe["clips"].values()))
    columns = min(8, frame_count)
    rows = (frame_count + columns - 1) // columns
    logical_size = [width * columns, height * rows]
    scaled_size = [dimension * scale for dimension in logical_size]
    working_size = [dimension * 2 for dimension in scaled_size]
    if working_size[0] * working_size[1] > 64 * 1024 * 1024:
        raise RecipeError("illustrated working atlas exceeds 64 megapixels; try a lower --scale")
    sheet = ET.Element(svg_tag("svg"), {
        "width": str(working_size[0]), "height": str(working_size[1]),
        "viewBox": f"0 0 {logical_size[0]} {logical_size[1]}",
    })
    views, grips, hidden = read_views(skin, recipe, ET.SubElement(sheet, svg_tag("defs")))

    clips = {}
    index = 0
    for clip, frames in recipe["clips"].items():
        clips[clip] = []
        for number, frame in enumerate(frames):
            row, column = divmod(index, columns)
            x, y = column * width, row * height
            view = frame.get("view", "side")
            paint_frame(
                sheet, (x, y), (width, height), views[view], frame_poses(recipe, frame, snap=False),
                grips.get((clip, number), set()), hidden.get((clip, number), set()),
            )
            clips[clip].append({
                "rect": [x * scale, y * scale, width * scale, height * scale],
                "duration_ms": frame["duration_ms"],
                "hitboxes": frame_hitboxes(frame, frame_poses(recipe, frame)),
            })
            index += 1

    metadata = {
        "version": 2,
        "image": "sheet.png",
        "atlas_scale": scale,
        "logical_frame_size": [width, height],
        "frame_size": [width * scale, height * scale],
        "pivot": canvas["pivot"],
        "atlas_pivot": [coordinate * scale for coordinate in canvas["pivot"]],
        "clips": clips,
    }
    output.mkdir(parents=True, exist_ok=True)
    for name in ("sheet.png", "manifest.json"):
        if (output / name).is_dir():
            raise RecipeError(f"output path is a directory: {output / name}")
    with TemporaryDirectory(dir=output) as directory:
        temporary = Path(directory)
        svg = temporary / "sheet.svg"
        image = temporary / "sheet.png"
        manifest = temporary / "manifest.json"
        ET.ElementTree(sheet).write(svg, encoding="utf-8")
        render_sheet(svg, temporary / "supersampled.png", image, working_size, scaled_size)
        manifest.write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
        image.replace(output / "sheet.png")
        manifest.replace(output / "manifest.json")
