"""Cook a verified, atomically published directory of Forge game assets."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
from tempfile import TemporaryDirectory
import zlib

from level import CookError, asset_id, read_json, read_level


ROOT = Path(__file__).resolve().parent
PIXEL_ART = ROOT.parent / "pixel-art"
SCOUT_RECIPE = PIXEL_ART / "examples/room-one-scout.json"
SCOUT_SKIN = PIXEL_ART / "examples/room-one-scout-illustrated.json"
ATLAS_ID = "scout-atlas"
IMAGE_ID = "scout-atlas-image"
ATLAS_PATH = "atlas/manifest.json"
IMAGE_PATH = "atlas/sheet.png"
ATLAS_SCALE = 4
ATLAS_OUTPUTS = {ATLAS_ID: ATLAS_PATH, IMAGE_ID: IMAGE_PATH}
MOTION_FRAMES = {"idle": 1, "walk": 4, "jump": 12, "crouch": 4}
PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


def digest(value):
    return hashlib.sha256(value).hexdigest()


def json_bytes(value):
    return (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=True) + "\n").encode()


def published_directory(output, generations):
    if output.is_symlink():
        target = Path(os.readlink(output))
        if target.parent != Path(generations.name) or not re.fullmatch("[a-f0-9]{64}", target.name):
            raise CookError(f"{output}: expected a cooker-owned output symlink")
        directory = output.parent / target
        # Keep using the resolved generation even if a parallel cook publishes.
        if directory.is_symlink() or not directory.is_dir():
            raise CookError(f"{output}: published generation is missing")
        return directory
    if output.exists() and (not output.is_dir() or any(output.iterdir())):
        raise CookError(f"{output}: expected an empty directory or a cooker-owned symlink")
    return None


def prepare_atlas(args, workspace):
    recipe = read_json(args.recipe, limit=4 * 1024 * 1024)
    skin = read_json(args.skin)
    if not isinstance(recipe, dict) or not isinstance(recipe.get("clips"), dict):
        raise CookError("Scout recipe: expected clips")
    views = skin.get("views") if isinstance(skin, dict) else None
    if not isinstance(views, dict) or not views:
        raise CookError("Scout skin: expected named SVG views")
    authored_recipe = workspace / "authored-recipe.json"
    authored_recipe.write_bytes(json_bytes(recipe))
    run_exporter("check", str(authored_recipe))
    timing = motion_timing(recipe["clips"])
    sources = {
        "recipe": digest(json_bytes(recipe)),
        "skin": digest(json_bytes(skin)),
        "python": ".".join(str(part) for part in sys.version_info[:3]),
    }
    for script in ("pixel_art.py", "illustrated.py", "recipe.py"):
        sources[script] = digest((PIXEL_ART / script).read_bytes())
    for tool in ("gdk-pixbuf-thumbnailer", "magick" if shutil.which("magick") else "convert"):
        executable = shutil.which(tool)
        if executable is None:
            raise CookError(f"illustrated atlas export requires {tool}")
        sources[tool] = digest(Path(executable).read_bytes())
    for name, path in sorted(views.items()):
        # The existing skin format allows identifier-like view names.
        if not re.fullmatch(r"[A-Za-z][A-Za-z0-9_-]{0,63}", name) or not isinstance(path, str):
            raise CookError("Scout skin: invalid view name or path")
        image = (args.skin.parent / path).read_bytes()
        if len(image) > 1024 * 1024:
            raise CookError(f"Scout skin view {name}: exceeds 1 MiB")
        sources[f"view:{name}"] = digest(image)
        (workspace / f"{name}.svg").write_bytes(image)
        views[name] = f"{name}.svg"
    normalize_jump(recipe)
    (workspace / "recipe.json").write_bytes(json_bytes(recipe))
    (workspace / "skin.json").write_bytes(json_bytes(skin))
    settings = {
        "inputs": sources,
        "scale": ATLAS_SCALE,
        "jump_root": "physics",
        "runtime_recipe": digest(json_bytes(recipe)),
        "cooker": digest(Path(__file__).read_bytes()),
    }
    return settings, timing


def motion_timing(clips):
    timing = {}
    for name, count in MOTION_FRAMES.items():
        frames = clips.get(name)
        if not isinstance(frames, list) or len(frames) != count:
            raise CookError(f"Scout recipe: expected {count} frames in {name!r}")
        timing[name] = []
        for index, frame in enumerate(frames):
            duration = frame.get("duration_ms") if isinstance(frame, dict) else None
            if type(duration) is not int or not 1 <= duration <= 60000:
                raise CookError(f"Scout recipe: invalid duration in {name}[{index}]")
            timing[name].append(duration)
    return timing


def body_collider(atlas):
    """Map approved idle body geometry to foot-relative Q16 tile offsets."""
    x, y, width, height = atlas["clips"]["idle"][0]["hitboxes"]["body"]
    pivot_x, pivot_y = atlas["pivot"]
    scale = 65_536 // 32  # The Scout is authored at 32 logical pixels per tile.
    left, top = (x - pivot_x) * scale, (y - pivot_y) * scale
    right, bottom = left + width * scale, top + height * scale
    if not (-65_536 <= left < 0 < right <= 65_536 and -65_536 <= top < 0 and bottom == 0):
        raise CookError("Scout idle body box must surround the foot pivot within one tile")
    if right - left > 65_536:
        raise CookError("Scout idle body box exceeds one tile in width")
    return dict(left=left, top=top, right=right, bottom=bottom)


def normalize_jump(recipe):
    jump = recipe["clips"].get("jump")
    if not isinstance(jump, list) or not jump:
        raise CookError("Scout recipe: expected a jump clip")
    for frame in jump:
        if not isinstance(frame, dict) or not isinstance(frame.get("offsets", {}), dict):
            raise CookError("Scout recipe: jump frame must have valid offsets")
        offsets = frame.get("offsets", {})
        if "root" in offsets:
            delta = offsets["root"]
            if not isinstance(delta, list) or len(delta) != 2 or any(type(x) is not int for x in delta):
                raise CookError("Scout recipe: jump root must be an integer pair")
            offsets["root"] = [0, 0]


def canonical_png(value):
    # PNG encoders may write timestamps and comment fields. Preserve image
    # data and color information while removing that incidental metadata.
    if len(value) > 8 * 1024 * 1024 or not value.startswith(PNG_MAGIC):
        raise CookError("Scout image: expected a PNG no larger than 8 MiB")
    result = bytearray(PNG_MAGIC)
    position = len(PNG_MAGIC)
    seen_header = seen_data = False
    while position + 12 <= len(value):
        size, kind = struct.unpack_from(">I4s", value, position)
        end = position + size + 12
        if end > len(value):
            break
        fragment = value[position:end]
        stored_crc, = struct.unpack_from(">I", fragment, len(fragment) - 4)
        if zlib.crc32(fragment[4:-4]) != stored_crc:
            raise CookError("Scout image: invalid PNG chunk integrity")
        if position == len(PNG_MAGIC):
            if kind != b"IHDR" or size != 13 or fragment[16:18] != b"\x08\x06":
                raise CookError("Scout image: expected an 8-bit RGBA PNG")
            seen_header = True
        if kind in (b"IHDR", b"IDAT", b"IEND", b"cHRM", b"gAMA", b"iCCP", b"sRGB"):
            result.extend(fragment)
        seen_data |= kind == b"IDAT"
        if kind == b"IEND" and end == len(value) and seen_header and seen_data:
            return bytes(result)
        position = end
    raise CookError("Scout image: incomplete PNG")


def atlas_cache(previous, settings):
    if previous is None:
        return None
    try:
        state = read_json(previous / "cook-state.json")
        manifest = read_json(previous / "manifest.json")
        if state["atlas"] == settings:
            values = {asset_id: (previous / name).read_bytes() for asset_id, name in ATLAS_OUTPUTS.items()}
            if all(digest(value) == manifest["entries"][asset_id]["sha256"] for asset_id, value in values.items()):
                return values
    except (OSError, CookError, KeyError, TypeError):
        pass
    return None


def cook_atlas(workspace, previous, settings):
    if (cached := atlas_cache(previous, settings)) is not None:
        return cached
    output = workspace / "export"
    run_exporter(
        "export", str(workspace / "recipe.json"), "--illustrated", str(workspace / "skin.json"),
        "--scale", str(ATLAS_SCALE), "--out", str(output),
    )
    atlas = read_json(output / "manifest.json")
    if atlas["version"] != 2 or atlas["image"] != "sheet.png" or atlas["atlas_scale"] != ATLAS_SCALE:
        raise CookError("Scout atlas export did not produce the scaled v2 manifest")
    return {
        ATLAS_ID: json_bytes(atlas),
        IMAGE_ID: canonical_png((output / "sheet.png").read_bytes()),
    }


def run_exporter(*arguments):
    process = subprocess.run(
        [sys.executable, "-B", str(PIXEL_ART / "pixel_art.py"), *arguments],
        capture_output=True, text=True, env={**os.environ, "LC_ALL": "C", "TZ": "UTC"},
    )
    if process.returncode:
        raise CookError(f"Scout {arguments[0]} failed: {process.stderr.strip()}")


def entry(kind, path, data, dependencies=(), optional=()):
    return {
        "kind": kind,
        "path": path,
        "sha256": digest(data),
        "dependencies": list(dependencies),
        "optional_dependencies": list(optional),
    }


def publish(output, generations, workspace, artifacts):
    generation_id = digest(json_bytes({path: digest(data) for path, data in artifacts.items()}))
    bundle = workspace / "bundle"
    for name, data in artifacts.items():
        path = bundle / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    destination = generations / generation_id
    try:
        bundle.rename(destination)
    except OSError as error:
        if destination.is_symlink() or not destination.is_dir():
            raise
        if any((destination / name).read_bytes() != data for name, data in artifacts.items()):
            raise CookError(f"{destination}: published generation bytes differ") from error
    pointer = workspace / "pointer"
    pointer.symlink_to(Path(generations.name) / generation_id, target_is_directory=True)
    if output.is_dir() and not output.is_symlink():
        # The first publication can claim an explicitly allocated empty dir.
        # rmdir fails safely if a file appeared since preflight.
        try:
            output.rmdir()
        except FileNotFoundError:
            pass
        except NotADirectoryError:
            published_directory(output, generations)
    os.replace(pointer, output)


def cook(args):
    output = args.out.absolute()
    generations = output.parent / f".{output.name}-generations"
    if generations.is_symlink():
        raise CookError(f"{generations}: expected a real generation directory")
    previous = published_directory(output, generations)
    levels = {}
    if not args.levels.is_dir():
        raise CookError(f"{args.levels}: expected a level source directory")
    sources = sorted(args.levels.glob("*.json"))
    if not 1 <= len(sources) <= 64:
        raise CookError("expected 1..64 authored levels")
    for source in sources:
        level = read_level(source)
        level_id = level["id"]
        if level["atlas"] != ATLAS_ID or level_id in ATLAS_OUTPUTS:
            raise CookError(f"{source}: invalid level ID or unresolved atlas dependency")
        levels[level_id] = level
    generations.mkdir(parents=True, exist_ok=True)
    with TemporaryDirectory(prefix=".cook-", dir=generations) as temporary:
        workspace = Path(temporary)
        settings, timing = prepare_atlas(args, workspace)
        atlas = cook_atlas(workspace, None if args.clean else previous, settings)
        collider = body_collider(json.loads(atlas[ATLAS_ID]))
        entries = {
            ATLAS_ID: entry("atlas", ATLAS_PATH, atlas[ATLAS_ID], dependencies=(IMAGE_ID,)),
            IMAGE_ID: entry("image", IMAGE_PATH, atlas[IMAGE_ID]),
        }
        artifacts = {path: atlas[asset] for asset, path in ATLAS_OUTPUTS.items()}
        level_sources = {}
        for level_id, level in levels.items():
            level_sources[level_id] = digest(json_bytes(level))
            data = json_bytes({**level, "timing": timing, "collider": collider})
            path = f"levels/{level_id}.json"
            entries[level_id] = entry("level", path, data, optional=(ATLAS_ID,))
            artifacts[path] = data
        artifacts["manifest.json"] = json_bytes({"version": 1, "entries": entries})
        artifacts["cook-state.json"] = json_bytes({"version": 1, "levels": level_sources, "atlas": settings})
        publish(output, generations, workspace, artifacts)
    return len(entries)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True, help="directory or symlink for cooked assets")
    parser.add_argument("--levels", type=Path, default=ROOT / "examples/levels", help="authored level directory")
    parser.add_argument("--recipe", type=Path, default=SCOUT_RECIPE)
    parser.add_argument("--skin", type=Path, default=SCOUT_SKIN)
    parser.add_argument("--clean", action="store_true", help="render fresh instead of reusing an intact atlas")
    args = parser.parse_args()
    try:
        count = cook(args)
    except (CookError, OSError) as error:
        parser.exit(1, f"cook: {error}\n")
    print(f"Cooked {count} entries at {args.out}")


if __name__ == "__main__":
    main()
