#!/usr/bin/env python3
"""Deterministic YAML serialization and safe loading with strict stdlib fallback."""
import json
import re
from typing import Any

try:
    import yaml as _PYYAML  # type: ignore
except ImportError:
    _PYYAML = None

_RESERVED_SCALARS = {
    "",
    "null",
    "Null",
    "NULL",
    "~",
    "true",
    "True",
    "TRUE",
    "false",
    "False",
    "FALSE",
    "yes",
    "Yes",
    "YES",
    "no",
    "No",
    "NO",
    "on",
    "On",
    "ON",
    "off",
    "Off",
    "OFF",
    ".inf",
    "-.inf",
    "+.inf",
    ".nan",
}
_SAFE_PLAIN_RE = re.compile(r"^[A-Za-z0-9_./+@-][A-Za-z0-9_./+@ -]*$")
_NUMBER_RE = re.compile(
    r"^[+-]?(?:0[xX][0-9a-fA-F]+|0[oO][0-7]+|\d+(?:\.\d*)?(?:[eE][+-]?\d+)?)$"
)
_BLOCK_HEADER_RE = re.compile(r"^([|>])([+-]?[1-9]?|[1-9]?[+-]?)$")


def has_pyyaml() -> bool:
    return _PYYAML is not None


def _format_scalar(value: Any) -> str:
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, str):
        if (
            value not in _RESERVED_SCALARS
            and not _NUMBER_RE.match(value)
            and _SAFE_PLAIN_RE.match(value)
            and not value.endswith(" ")
            and ": " not in value
            and " #" not in value
            and not value.startswith(("-", "?", ":"))
        ):
            return value
        return json.dumps(value, ensure_ascii=False)
    raise TypeError(f"unsupported YAML scalar type: {type(value).__name__}")


def _emit(value: Any, indent: int, lines: list[str]) -> None:
    prefix = " " * indent
    if isinstance(value, dict):
        if not value:
            lines.append(f"{prefix}{{}}")
            return
        for key, item in value.items():
            key_str = _format_scalar(str(key))
            if isinstance(item, dict):
                if not item:
                    lines.append(f"{prefix}{key_str}: {{}}")
                else:
                    lines.append(f"{prefix}{key_str}:")
                    _emit(item, indent + 2, lines)
            elif isinstance(item, list):
                if not item:
                    lines.append(f"{prefix}{key_str}: []")
                else:
                    lines.append(f"{prefix}{key_str}:")
                    _emit(item, indent + 2, lines)
            else:
                lines.append(f"{prefix}{key_str}: {_format_scalar(item)}")
        return
    if isinstance(value, list):
        if not value:
            lines.append(f"{prefix}[]")
            return
        for item in value:
            if isinstance(item, dict):
                if not item:
                    lines.append(f"{prefix}- {{}}")
                    continue
                entries = list(item.items())
                first_k, first_v = entries[0]
                first_key = _format_scalar(str(first_k))
                if isinstance(first_v, dict):
                    if not first_v:
                        lines.append(f"{prefix}- {first_key}: {{}}")
                    else:
                        lines.append(f"{prefix}- {first_key}:")
                        _emit(first_v, indent + 4, lines)
                elif isinstance(first_v, list):
                    if not first_v:
                        lines.append(f"{prefix}- {first_key}: []")
                    else:
                        lines.append(f"{prefix}- {first_key}:")
                        _emit(first_v, indent + 4, lines)
                else:
                    lines.append(f"{prefix}- {first_key}: {_format_scalar(first_v)}")
                for key, val in entries[1:]:
                    key_str = _format_scalar(str(key))
                    if isinstance(val, dict):
                        if not val:
                            lines.append(f"{prefix}  {key_str}: {{}}")
                        else:
                            lines.append(f"{prefix}  {key_str}:")
                            _emit(val, indent + 4, lines)
                    elif isinstance(val, list):
                        if not val:
                            lines.append(f"{prefix}  {key_str}: []")
                        else:
                            lines.append(f"{prefix}  {key_str}:")
                            _emit(val, indent + 4, lines)
                    else:
                        lines.append(f"{prefix}  {key_str}: {_format_scalar(val)}")
            elif isinstance(item, list):
                if not item:
                    lines.append(f"{prefix}- []")
                else:
                    lines.append(f"{prefix}-")
                    _emit(item, indent + 2, lines)
            else:
                lines.append(f"{prefix}- {_format_scalar(item)}")
        return
    lines.append(f"{prefix}{_format_scalar(value)}")


def dump_yaml(data: Any) -> str:
    """Serialize Python structures to deterministic, canonical YAML with trailing LF."""
    lines: list[str] = []
    _emit(data, 0, lines)
    return "\n".join(lines) + "\n"


def _strip_inline_comment(text: str) -> str:
    in_single = False
    in_double = False
    escaped = False
    for idx, ch in enumerate(text):
        if in_double:
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                in_double = False
            continue
        if in_single:
            if ch == "'":
                in_single = False
            continue
        if ch == '"':
            in_double = True
        elif ch == "'":
            in_single = True
        elif ch == "#" and (idx == 0 or text[idx - 1] in (" ", "\t")):
            return text[:idx].rstrip()
    return text.rstrip()


def _parse_single_quoted(raw: str, lineno: int) -> str:
    if len(raw) < 2 or not (raw.startswith("'") and raw.endswith("'")):
        raise ValueError(f"line {lineno}: malformed single-quoted scalar")
    inner = raw[1:-1]
    out: list[str] = []
    i = 0
    while i < len(inner):
        if inner[i] == "'":
            if i + 1 < len(inner) and inner[i + 1] == "'":
                out.append("'")
                i += 2
                continue
            raise ValueError(f"line {lineno}: unescaped single quote inside single-quoted scalar")
        out.append(inner[i])
        i += 1
    return "".join(out)


def _is_complete_single_quoted(raw: str) -> bool:
    if not raw.startswith("'") or len(raw) < 2:
        return False
    i = 1
    while i < len(raw):
        if raw[i] == "'":
            if i + 1 < len(raw) and raw[i + 1] == "'":
                i += 2
                continue
            return i == len(raw) - 1
        i += 1
    return False


def _is_complete_double_quoted(raw: str) -> bool:
    if not raw.startswith('"') or len(raw) < 2:
        return False
    escaped = False
    for i in range(1, len(raw)):
        ch = raw[i]
        if escaped:
            escaped = False
            continue
        if ch == "\\":
            escaped = True
            continue
        if ch == '"':
            return i == len(raw) - 1
    return False


def _unique_mapping_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    mapping: dict[str, Any] = {}
    for key, value in pairs:
        if key in mapping:
            raise ValueError(f"duplicate YAML mapping key {key!r}")
        mapping[key] = value
    return mapping


def _parse_flow_collection(raw: str, lineno: int) -> Any:
    try:
        return json.loads(raw, object_pairs_hook=_unique_mapping_pairs)
    except json.JSONDecodeError:
        pass
    # Minimal strict flow parser for YAML flow sequences/mappings with single-quoted or plain scalars.
    s = raw.strip()
    n = len(s)

    def skip_ws(pos: int) -> int:
        while pos < n and s[pos] in " \t":
            pos += 1
        return pos

    def parse_val(pos: int) -> tuple[Any, int]:
        pos = skip_ws(pos)
        if pos >= n:
            raise ValueError(f"line {lineno}: unexpected end of flow collection")
        ch = s[pos]
        if ch == "[":
            pos += 1
            items: list[Any] = []
            pos = skip_ws(pos)
            if pos < n and s[pos] == "]":
                return items, pos + 1
            while True:
                item, pos = parse_val(pos)
                items.append(item)
                pos = skip_ws(pos)
                if pos < n and s[pos] == ",":
                    pos += 1
                    pos = skip_ws(pos)
                    if pos < n and s[pos] == "]":
                        return items, pos + 1
                    continue
                if pos < n and s[pos] == "]":
                    return items, pos + 1
                raise ValueError(f"line {lineno}: expected ',' or ']' in flow sequence")
        if ch == "{":
            pos += 1
            mapping: dict[str, Any] = {}
            pos = skip_ws(pos)
            if pos < n and s[pos] == "}":
                return mapping, pos + 1
            while True:
                key, pos = parse_val(pos)
                pos = skip_ws(pos)
                if pos >= n or s[pos] != ":":
                    raise ValueError(f"line {lineno}: expected ':' in flow mapping")
                pos += 1
                val, pos = parse_val(pos)
                key = str(key)
                if key in mapping:
                    raise ValueError(f"line {lineno}: duplicate YAML mapping key {key!r}")
                mapping[key] = val
                pos = skip_ws(pos)
                if pos < n and s[pos] == ",":
                    pos += 1
                    pos = skip_ws(pos)
                    if pos < n and s[pos] == "}":
                        return mapping, pos + 1
                    continue
                if pos < n and s[pos] == "}":
                    return mapping, pos + 1
                raise ValueError(f"line {lineno}: expected ',' or '}}' in flow mapping")
        if ch == '"':
            end = pos + 1
            escaped = False
            while end < n:
                c = s[end]
                if escaped:
                    escaped = False
                elif c == "\\":
                    escaped = True
                elif c == '"':
                    try:
                        return json.loads(s[pos : end + 1]), end + 1
                    except json.JSONDecodeError as exc:
                        raise ValueError(f"line {lineno}: invalid double-quoted string: {exc}") from exc
                end += 1
            raise ValueError(f"line {lineno}: unterminated double-quoted string in flow collection")
        if ch == "'":
            end = pos + 1
            while end < n:
                if s[end] == "'":
                    if end + 1 < n and s[end + 1] == "'":
                        end += 2
                        continue
                    return _parse_single_quoted(s[pos : end + 1], lineno), end + 1
                end += 1
            raise ValueError(f"line {lineno}: unterminated single-quoted string in flow collection")
        start = pos
        while pos < n and s[pos] not in ",]}:":
            pos += 1
        token = s[start:pos].strip()
        if not token:
            raise ValueError(f"line {lineno}: empty token in flow collection")
        return _parse_scalar(token, lineno), pos

    val, end_pos = parse_val(0)
    end_pos = skip_ws(end_pos)
    if end_pos != n:
        raise ValueError(f"line {lineno}: trailing content in flow collection: {s[end_pos:]!r}")
    return val


def _check_unsupported_token(raw: str, lineno: int) -> None:
    if not raw:
        return
    if raw[0] in ("&", "*", "!", "%", "@", "`"):
        raise ValueError(f"line {lineno}: unsupported YAML syntax without PyYAML: {raw!r}")
    if raw == "?" or raw.startswith("? "):
        raise ValueError(f"line {lineno}: unsupported complex YAML key without PyYAML: {raw!r}")


def _parse_scalar(raw: str, lineno: int = 1) -> Any:
    raw = raw.strip()
    if raw == "[]":
        return []
    if raw == "{}":
        return {}
    if raw.startswith("[") or raw.startswith("{"):
        return _parse_flow_collection(raw, lineno)
    if raw in ("null", "Null", "NULL", "~", ""):
        return None
    if raw in ("true", "True", "TRUE"):
        return True
    if raw in ("false", "False", "FALSE"):
        return False
    if raw.startswith('"'):
        if not _is_complete_double_quoted(raw):
            raise ValueError(f"line {lineno}: unterminated or trailing double-quoted scalar")
        try:
            return json.loads(raw)
        except json.JSONDecodeError as exc:
            raise ValueError(f"line {lineno}: invalid double-quoted scalar: {exc}") from exc
    if raw.startswith("'"):
        if not _is_complete_single_quoted(raw):
            raise ValueError(f"line {lineno}: unterminated or trailing single-quoted scalar")
        return _parse_single_quoted(raw, lineno)
    _check_unsupported_token(raw, lineno)
    if re.fullmatch(r"[+-]?\d+", raw):
        return int(raw)
    if re.fullmatch(r"[+-]?(?:\d+\.\d*|\.\d+)(?:[eE][+-]?\d+)?", raw) or re.fullmatch(
        r"[+-]?\d+[eE][+-]?\d+", raw
    ):
        return float(raw)
    return raw


def _split_key_value(content: str, lineno: int) -> tuple[str, str] | None:
    if content.startswith(("- ", "[")) or content in ("-", "{}") or content.startswith("{"):
        return None
    in_single = False
    in_double = False
    escaped = False
    i = 0
    while i < len(content):
        ch = content[i]
        if in_double:
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                in_double = False
            i += 1
            continue
        if in_single:
            if ch == "'":
                if i + 1 < len(content) and content[i + 1] == "'":
                    i += 2
                    continue
                in_single = False
            i += 1
            continue
        if ch == '"':
            in_double = True
        elif ch == "'":
            in_single = True
        elif ch == ":" and (i + 1 == len(content) or content[i + 1] == " "):
            key_raw = content[:i].strip()
            if not key_raw:
                raise ValueError(f"line {lineno}: empty mapping key")
            if key_raw == "<<":
                raise ValueError(f"line {lineno}: YAML merge key '<<' unsupported without PyYAML")
            key = _parse_scalar(key_raw, lineno)
            rest = content[i + 1 :].strip()
            return str(key), rest
        i += 1
    return None


def _stdlib_load_yaml(text: str) -> Any:
    """Strict stdlib fallback parser for block/flow YAML (including indentless sequences & block scalars).

    Handles canonical dump_yaml, serde_yaml_ng output, and repository YAML files while
    explicitly rejecting malformed syntax, unsupported YAML features (anchors/aliases/tags),
    and trailing unconsumed input instead of silently corrupting data.
    """
    stripped_doc = text.strip()
    if not stripped_doc:
        return None
    if stripped_doc.startswith("{") or (
        stripped_doc.startswith("[") and not stripped_doc.startswith("- ")
    ):
        try:
            return json.loads(stripped_doc, object_pairs_hook=_unique_mapping_pairs)
        except json.JSONDecodeError:
            pass

    all_lines = text.splitlines()
    # Pre-validate indentation tabs on non-empty lines outside block scalars during traversal,
    # keeping raw lines indexed by 0-based position for faithful block scalar parsing.
    n_lines = len(all_lines)

    def line_info(pos: int) -> tuple[int, int, str]:
        """Return (lineno_1based, indent, stripped_without_comment) or (-1, -1, '') if blank/comment."""
        raw = all_lines[pos]
        if "\t" in raw[: len(raw) - len(raw.lstrip(" \t"))]:
            raise ValueError(f"line {pos + 1}: tab character in YAML indentation")
        indent = len(raw) - len(raw.lstrip(" "))
        content = _strip_inline_comment(raw[indent:])
        if not content:
            return pos + 1, -1, ""
        return pos + 1, indent, content

    def skip_trivia(pos: int) -> int:
        while pos < n_lines:
            _, indent, content = line_info(pos)
            if indent == -1:
                pos += 1
                continue
            break
        return pos

    def parse_block_scalar(header: str, pos: int, parent_indent: int, lineno: int) -> tuple[str, int]:
        m = _BLOCK_HEADER_RE.match(header)
        if not m:
            raise ValueError(f"line {lineno}: invalid block scalar header {header!r}")
        style = m.group(1)
        mod = m.group(2)
        chomp = "clip"
        explicit_indent: int | None = None
        for ch in mod:
            if ch == "-":
                chomp = "strip"
            elif ch == "+":
                chomp = "keep"
            elif ch.isdigit():
                explicit_indent = int(ch)

        block_indent = (parent_indent + explicit_indent) if explicit_indent is not None else None
        collected: list[str] = []
        while pos < n_lines:
            raw = all_lines[pos]
            if raw.strip() == "":
                collected.append("")
                pos += 1
                continue
            lead = len(raw) - len(raw.lstrip(" "))
            if block_indent is None:
                if lead <= parent_indent:
                    break
                block_indent = lead
            if lead < block_indent:
                break
            collected.append(raw[block_indent:])
            pos += 1

        trailing_empty = 0
        while collected and collected[-1] == "":
            collected.pop()
            trailing_empty += 1

        if style == "|":
            body = "\n".join(collected)
        else:
            # Folded scalar (>): join adjacent non-empty non-more-indented lines with spaces.
            parts: list[str] = []
            idx = 0
            while idx < len(collected):
                line = collected[idx]
                if line == "":
                    blanks = 0
                    while idx < len(collected) and collected[idx] == "":
                        blanks += 1
                        idx += 1
                    parts.append("\n" * blanks)
                    continue
                if parts and not parts[-1].endswith("\n"):
                    prev_more = parts[-1].startswith((" ", "\t"))
                    cur_more = line.startswith((" ", "\t"))
                    parts.append("\n" if (prev_more or cur_more) else " ")
                parts.append(line)
                idx += 1
            body = "".join(parts)

        if not collected and chomp != "keep":
            return "", pos
        if chomp == "strip":
            return body, pos
        if chomp == "keep":
            return body + ("\n" if collected else "") + ("\n" * trailing_empty), pos
        return (body + "\n") if collected else "", pos

    def parse_multiline_scalar(first: str, pos: int, owner_indent: int, lineno: int) -> tuple[Any, int]:
        if _BLOCK_HEADER_RE.match(first):
            return parse_block_scalar(first, pos, owner_indent, lineno)
        if first.startswith('"'):
            acc = first
            while not _is_complete_double_quoted(acc):
                if pos >= n_lines:
                    raise ValueError(f"line {lineno}: unterminated double-quoted scalar")
                raw = all_lines[pos]
                if raw.strip() == "":
                    acc += "\\n"
                    pos += 1
                    continue
                lead = len(raw) - len(raw.lstrip(" "))
                if lead <= owner_indent:
                    raise ValueError(f"line {lineno}: unterminated double-quoted scalar")
                acc += " " + _strip_inline_comment(raw[lead:])
                pos += 1
            return _parse_scalar(acc, lineno), pos
        if first.startswith("'"):
            acc = first
            while not _is_complete_single_quoted(acc):
                if pos >= n_lines:
                    raise ValueError(f"line {lineno}: unterminated single-quoted scalar")
                raw = all_lines[pos]
                if raw.strip() == "":
                    acc += "\n"
                    pos += 1
                    continue
                lead = len(raw) - len(raw.lstrip(" "))
                if lead <= owner_indent:
                    raise ValueError(f"line {lineno}: unterminated single-quoted scalar")
                acc += " " + _strip_inline_comment(raw[lead:])
                pos += 1
            return _parse_scalar(acc, lineno), pos
        if first.startswith(("[", "{")):
            return _parse_scalar(first, lineno), pos
        _check_unsupported_token(first, lineno)
        # Plain scalar: consume continuation lines indented deeper than owner_indent.
        parts = [first]
        blank_pending = 0
        while pos < n_lines:
            lno, ind, cont = line_info(pos)
            if ind == -1:
                if all_lines[pos].strip() == "":
                    blank_pending += 1
                    pos += 1
                    continue
                # Comment terminates plain scalar continuation.
                break
            if ind <= owner_indent or cont in ("---", "..."):
                break
            if _split_key_value(cont, lno) is not None or cont == "-" or cont.startswith("- "):
                raise ValueError(
                    f"line {lno}: unexpected nested mapping/sequence inside plain scalar: {cont!r}"
                )
            if blank_pending > 0:
                parts.append("\n" * blank_pending + cont)
                blank_pending = 0
            else:
                parts.append(" " + cont)
            pos += 1
        joined = "".join(parts)
        if re.search(r":(?:[ \t\r\n]|$)", joined):
            raise ValueError(f"line {lineno}: mapping value ':' not allowed in plain scalar: {joined!r}")
        return _parse_scalar(joined, lineno), pos

    def parse_mapping_value(rest: str, pos: int, key_indent: int, lineno: int) -> tuple[Any, int]:
        if rest:
            return parse_multiline_scalar(rest, pos, key_indent, lineno)
        next_pos = skip_trivia(pos)
        if next_pos >= n_lines:
            return None, next_pos
        _, next_indent, next_content = line_info(next_pos)
        if next_content in ("---", "..."):
            return None, next_pos
        if next_indent > key_indent:
            return parse_block(next_pos, next_indent)
        # Standard YAML indentless sequence directly under a mapping key (`key:\n- item`).
        if next_indent == key_indent and (next_content == "-" or next_content.startswith("- ")):
            return parse_sequence(next_pos, key_indent)
        return None, next_pos

    def parse_sequence(pos: int, seq_indent: int) -> tuple[list[Any], int]:
        items: list[Any] = []
        while True:
            pos = skip_trivia(pos)
            if pos >= n_lines:
                break
            lineno, cur_indent, cur = line_info(pos)
            if cur in ("---", "...") or cur_indent < seq_indent:
                break
            if cur_indent != seq_indent:
                raise ValueError(
                    f"line {lineno}: unexpected indentation {cur_indent} inside sequence at indent {seq_indent}"
                )
            if not (cur == "-" or cur.startswith("- ")):
                break
            after = cur[2:].lstrip(" ") if cur.startswith("- ") else ""
            item_indent = seq_indent + 2
            pos += 1
            if not after:
                next_pos = skip_trivia(pos)
                if next_pos < n_lines:
                    _, n_ind, n_cont = line_info(next_pos)
                    if n_ind > seq_indent and n_cont not in ("---", "..."):
                        val, pos = parse_block(next_pos, n_ind)
                        items.append(val)
                        continue
                items.append(None)
                continue

            if _BLOCK_HEADER_RE.match(after):
                val, pos = parse_block_scalar(after, pos, seq_indent, lineno)
                items.append(val)
                continue

            kv = _split_key_value(after, lineno)
            if kv is not None:
                key, rest = kv
                obj: dict[str, Any] = {}
                obj[key], pos = parse_mapping_value(rest, pos, item_indent, lineno)
                while True:
                    next_pos = skip_trivia(pos)
                    if next_pos >= n_lines:
                        pos = next_pos
                        break
                    sub_lno, sub_indent, sub_content = line_info(next_pos)
                    if sub_content in ("---", "...") or sub_indent < item_indent:
                        pos = next_pos
                        break
                    if sub_indent != item_indent:
                        raise ValueError(
                            f"line {sub_lno}: unexpected indentation {sub_indent} in sequence mapping item (expected {item_indent})"
                        )
                    sub_kv = _split_key_value(sub_content, sub_lno)
                    if sub_kv is None:
                        raise ValueError(
                            f"line {sub_lno}: expected 'key: value' inside mapping, got {sub_content!r}"
                        )
                    sub_k, sub_rest = sub_kv
                    if sub_k in obj:
                        raise ValueError(f"line {sub_lno}: duplicate YAML mapping key {sub_k!r}")
                    pos = next_pos + 1
                    obj[sub_k], pos = parse_mapping_value(sub_rest, pos, item_indent, sub_lno)
                items.append(obj)
            else:
                val, pos = parse_multiline_scalar(after, pos, seq_indent, lineno)
                items.append(val)
        return items, pos

    def parse_mapping(pos: int, map_indent: int) -> tuple[dict[str, Any], int]:
        mapping: dict[str, Any] = {}
        while True:
            pos = skip_trivia(pos)
            if pos >= n_lines:
                break
            lineno, cur_indent, cur = line_info(pos)
            if cur in ("---", "...") or cur_indent < map_indent:
                break
            if cur_indent != map_indent:
                raise ValueError(
                    f"line {lineno}: unexpected indentation {cur_indent} inside mapping at indent {map_indent}"
                )
            kv = _split_key_value(cur, lineno)
            if kv is None:
                raise ValueError(f"line {lineno}: expected mapping entry 'key: value', got {cur!r}")
            key, rest = kv
            if key in mapping:
                raise ValueError(f"line {lineno}: duplicate YAML mapping key {key!r}")
            pos += 1
            mapping[key], pos = parse_mapping_value(rest, pos, map_indent, lineno)
        return mapping, pos

    def parse_block(pos: int, block_indent: int) -> tuple[Any, int]:
        pos = skip_trivia(pos)
        if pos >= n_lines:
            return None, pos
        lineno, indent, content = line_info(pos)
        if indent != block_indent:
            raise ValueError(
                f"line {lineno}: unexpected indentation {indent} (expected {block_indent})"
            )
        if content == "-" or content.startswith("- "):
            return parse_sequence(pos, indent)
        if _split_key_value(content, lineno) is not None:
            return parse_mapping(pos, indent)
        return parse_multiline_scalar(content, pos + 1, indent, lineno)

    pos = skip_trivia(0)
    if pos >= n_lines:
        return None
    lno, ind, content = line_info(pos)
    if ind == 0 and content == "---":
        pos = skip_trivia(pos + 1)
        if pos >= n_lines:
            return None
        lno, ind, content = line_info(pos)
    if ind == 0 and content == "...":
        pos = skip_trivia(pos + 1)
        if pos < n_lines:
            tlno, _, tcontent = line_info(pos)
            raise ValueError(f"line {tlno}: unexpected trailing YAML input after '...': {tcontent!r}")
        return None

    result, pos = parse_block(pos, ind)
    pos = skip_trivia(pos)
    if pos < n_lines:
        tlno, tind, tcontent = line_info(pos)
        if tind == 0 and tcontent == "...":
            pos = skip_trivia(pos + 1)
    if pos < n_lines:
        tlno, _, tcontent = line_info(pos)
        raise ValueError(f"line {tlno}: unexpected trailing YAML input: {tcontent!r}")
    return result


def load_yaml(text: str) -> Any:
    """Safely load YAML using PyYAML CSafeLoader/SafeLoader when installed, else strict stdlib fallback."""
    if _PYYAML is not None:
        loader = getattr(_PYYAML, "CSafeLoader", _PYYAML.SafeLoader)

        class UniqueKeyLoader(loader):
            def flatten_mapping(self, node):
                # Check explicit entries before merge expansion; valid merge overrides remain valid.
                checked = getattr(self, "_checked_mappings", None)
                if checked is None:
                    checked = self._checked_mappings = set()
                if id(node) not in checked:
                    checked.add(id(node))
                    keys = set()
                    for key_node, _ in node.value:
                        if key_node.tag == "tag:yaml.org,2002:merge":
                            continue
                        key = (
                            key_node.value
                            if key_node.tag == "tag:yaml.org,2002:value"
                            else self.construct_object(key_node, deep=False)
                        )
                        if key in keys:
                            raise ValueError(
                                f"line {key_node.start_mark.line + 1}: duplicate YAML mapping key {key!r}"
                            )
                        keys.add(key)
                super().flatten_mapping(node)

        return _PYYAML.load(text, Loader=UniqueKeyLoader)
    return _stdlib_load_yaml(text)
