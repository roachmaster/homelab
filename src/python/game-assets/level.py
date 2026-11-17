"""Validate a v1 authored level and materialize its cooked defaults."""

import json
import re


class CookError(ValueError):
    """Authored or cooked data cannot be used."""


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise CookError(f"duplicate JSON field {key!r}")
        result[key] = value
    return result


def read_json(path, limit=1_048_576):
    with path.open("rb") as source:
        data = source.read(limit + 1)
    if len(data) > limit:
        raise CookError(f"{path}: exceeds {limit} bytes")
    try:
        return json.loads(data, object_pairs_hook=unique_object)
    except (ValueError, UnicodeError, RecursionError) as error:
        raise CookError(f"{path}: {error}") from error


def fields(value, context, required, optional=()):
    if not isinstance(value, dict):
        raise CookError(f"{context}: expected an object")
    missing = sorted(set(required) - value.keys())
    if missing:
        raise CookError(f"{context}: missing {', '.join(missing)}")
    extra = sorted(value.keys() - set(required) - set(optional))
    if extra:
        raise CookError(f"{context}: unknown fields {', '.join(extra)}")


def integer(value, context, minimum, maximum):
    if type(value) is not int or not minimum <= value <= maximum:
        raise CookError(f"{context}: expected an integer in {minimum}..{maximum}")


def asset_id(value, context):
    if not isinstance(value, str) or not re.fullmatch(r"[a-z][a-z0-9-]{0,63}", value):
        raise CookError(f"{context}: expected a lowercase asset ID")


def bounded_array(value, context, minimum, maximum):
    if not isinstance(value, list) or not minimum <= len(value) <= maximum:
        raise CookError(f"{context}: expected {minimum}..{maximum} array items")


def validate_surfaces(level, context):
    surface = level["surface"]
    asset_id(surface, f"{context}.surface")
    table = level["surfaces"]
    bounded_array(table, f"{context}.surfaces", 1, 16)
    found = set()
    for index, item in enumerate(table):
        key = f"{context}.surfaces[{index}]"
        fields(item, key, ("id", "friction_q16", "restitution_q16"))
        asset_id(item["id"], f"{key}.id")
        if item["id"] in found:
            raise CookError(f"{key}.id: repeated surface ID")
        for attribute in ("friction_q16", "restitution_q16"):
            integer(item[attribute], f"{key}.{attribute}", 0, 65536)
        found.add(item["id"])
    if surface not in found:
        raise CookError(f"{context}.surface: unknown surface ID {surface!r}")


def validate_patrol(spawn, level, context):
    patrol = spawn.setdefault("patrol", None)
    if spawn["kind"] != "guard":
        if patrol is not None:
            raise CookError(f"{context}.patrol: only guards can patrol")
        return
    fields(patrol, f"{context}.patrol", ("first_x", "last_x", "period_ticks"))
    integer(patrol["first_x"], f"{context}.patrol.first_x", 0, level["width"] - 1)
    integer(patrol["last_x"], f"{context}.patrol.last_x", 0, level["width"] - 1)
    integer(patrol["period_ticks"], f"{context}.patrol.period_ticks", 1, 65535)
    first, last = patrol["first_x"], patrol["last_x"]
    if (
        first >= last
        or not first <= spawn["x"] <= last
        or "#" in level["tiles"][spawn["y"]][first:last + 1]
    ):
        raise CookError(f"{context}.patrol: expected an open segment containing the guard")


def validate_spawns(level, context):
    spawns = level["spawns"]
    bounded_array(spawns, f"{context}.spawns", 3, 4)
    ids, positions, kinds = set(), set(), set()
    for index, spawn in enumerate(spawns):
        key = f"{context}.spawns[{index}]"
        fields(spawn, key, ("id", "kind", "x", "y"), ("patrol",))
        integer(spawn["id"], f"{key}.id", 1, 65535)
        integer(spawn["x"], f"{key}.x", 0, level["width"] - 1)
        integer(spawn["y"], f"{key}.y", 0, level["height"] - 1)
        kind = spawn["kind"]
        if kind not in ("player", "key", "exit", "guard") or kind in kinds:
            raise CookError(f"{key}.kind: expected a distinct player, key, exit, or guard")
        if spawn["id"] in ids:
            raise CookError(f"{key}.id: repeated entity ID")
        x, y = spawn["x"], spawn["y"]
        if (x, y) in positions or level["tiles"][y][x] != ".":
            raise CookError(f"{key}: spawn must occupy a distinct open tile")
        validate_patrol(spawn, level, key)
        ids.add(spawn["id"])
        kinds.add(kind)
        positions.add((x, y))
    if not {"player", "key", "exit"} <= kinds:
        raise CookError(f"{context}.spawns: expected one player, key, and exit")


def read_level(path):
    level = read_json(path, limit=64 * 1024)
    context = path.name
    fields(
        level, context,
        ("version", "id", "mode", "width", "height", "tiles", "spawns", "atlas", "surface", "surfaces"),
    )
    integer(level["version"], f"{context}.version", 1, 1)
    asset_id(level["id"], f"{context}.id")
    if level["id"] != path.stem:
        raise CookError(f"{context}.id: must match the file name")
    if level["mode"] not in ("tile", "platform"):
        raise CookError(f"{context}.mode: expected tile or platform")
    integer(level["width"], f"{context}.width", 1, 64)
    integer(level["height"], f"{context}.height", 1, 64)
    bounded_array(level["tiles"], f"{context}.tiles", level["height"], level["height"])
    for index, row in enumerate(level["tiles"]):
        if not isinstance(row, str) or len(row) != level["width"] or row.strip(".#"):
            raise CookError(f"{context}.tiles[{index}]: expected width-matched . and # tiles")
    asset_id(level["atlas"], f"{context}.atlas")
    validate_surfaces(level, context)
    validate_spawns(level, context)
    # Stable table order also supplies the runtime's stable iteration order.
    level["spawns"].sort(key=lambda spawn: spawn["id"])
    level["surfaces"].sort(key=lambda surface: surface["id"])
    return level
