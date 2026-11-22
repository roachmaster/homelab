#!/usr/bin/env python3
"""Pure-Python Rust source parser for dependency analysis.

Reads .rs files directly (no cargo, no compiled helper binary) and extracts
the dependency-relevant structure: module declarations, `use` / `pub use`
import trees, item definitions with visibility, and `#[cfg]` / `#[path]`
attributes. Builds a module dependency graph over a crate or workspace.

Scope is deliberately dependency-shaped, not a full Rust grammar: function
bodies, impl bodies and trait bodies are skipped by balanced-delimiter
counting (scanning them only for scoped `use` statements). Anything the
parser does not recognise is skipped to the next `;` or balanced block and
recorded as a parse note, so one odd file never aborts a workspace run.

Why pure Python: the imports check runs before compile in the CI pipeline,
so it cannot depend on a compiled helper (ast_graph.py shells out to
`forge-ide ast-graph`, which needs a build). This module is the input that
check consumes.

Usage:
  python3 rust_ast.py <file.rs>            parse one file, print JSON
  python3 rust_ast.py <rust-root>          build the workspace graph, print JSON
  python3 rust_ast.py <path> --quiet       print only errors (exit 1 on any)
"""

import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
RUST_ROOT = SCRIPT_DIR.parent

# ---------------------------------------------------------------------------
# Tokenizer
# ---------------------------------------------------------------------------

IDENT_START = re.compile(r"[A-Za-z_]")
IDENT_CONT = re.compile(r"[A-Za-z0-9_]")
MULTI_PUNCT = ("::", "->", "=>", "..=", "...", "..", "&&", "||", "<<", ">>",
               "<=", ">=", "==", "!=", "+=", "-=", "*=", "/=", "%=", "&=",
               "|=", "^=")


@dataclass
class Token:
    kind: str  # IDENT, LIFETIME, STRING, NUMBER, PUNCT
    text: str
    line: int


def _is_ident_start(ch: str) -> bool:
    return ch == "_" or ch.isalpha()


def _is_ident_cont(ch: str) -> bool:
    return ch == "_" or ch.isalnum()


def _char_end(source: str, i: int) -> int:
    """End index (exclusive) of a char literal starting at source[i] == "'".

    Returns -1 when the quote does not open a char literal (a lifetime).
    Handles escapes: '\\n', '\\'', '\\u{...}'.
    """
    n = len(source)
    j = i + 1
    if j >= n:
        return -1
    if source[j] == "\\":
        k = j + 1
        if k + 1 < n and source[k] == "u" and source[k + 1] == "{":
            close = source.find("}", k + 2)
            if close == -1:
                return -1
            k = close + 1
        elif k < n:
            k += 1
        else:
            return -1
        return k + 1 if k < n and source[k] == "'" else -1
    if _is_ident_start(source[j]):
        k = j + 1
        while k < n and _is_ident_cont(source[k]):
            k += 1
        if k < n and source[k] == "'" and k - j == 1:
            return k + 1
        return -1
    # Any other single character, e.g. '}', ' '.
    if j + 1 < n and source[j + 1] == "'":
        return j + 2
    return -1


def tokenize(source: str) -> list[Token]:
    """Split Rust source into tokens, dropping whitespace and comments.

    Handles nested block comments, raw strings (any hash count), byte
    strings, char literals vs lifetimes, and raw identifiers (r#type).
    """
    tokens: list[Token] = []
    i, n, line = 0, len(source), 1
    while i < n:
        ch = source[i]
        if ch == "\n":
            line += 1
            i += 1
            continue
        if ch in " \t\r":
            i += 1
            continue
        # Comments: // line, /* nested block */ (doc comments are comments too).
        if ch == "/" and i + 1 < n and source[i + 1] == "/":
            while i < n and source[i] != "\n":
                i += 1
            continue
        if ch == "/" and i + 1 < n and source[i + 1] == "*":
            depth, i = 1, i + 2
            while i < n and depth:
                if source[i] == "\n":
                    line += 1
                elif source[i] == "/" and i + 1 < n and source[i + 1] == "*":
                    depth += 1
                    i += 1
                elif source[i] == "*" and i + 1 < n and source[i + 1] == "/":
                    depth -= 1
                    i += 1
                i += 1
            continue
        # Byte char literal b'x' (including b'"' and b'\''): the quote inside
        # must not open a string.
        if ch == "b" and i + 1 < n and source[i + 1] == "'":
            end = _char_end(source, i + 1)
            if end != -1:
                tokens.append(Token("STRING", source[i:end], line))
                i = end
                continue
        # Raw byte string br#"..."# / byte string b"..." / raw string r#"..."#
        # / C string c"..." / raw C string cr#"..."#.
        if ch in ("r", "b", "c") and i + 1 < n:
            j = i + 1
            if ch in ("b", "c") and j < n and source[j] == "r":
                j += 1
            hashes = 0
            while j < n and source[j] == "#":
                hashes += 1
                j += 1
            if j < n and source[j] == '"' and (
                    hashes or ch in ("b", "c") or source[i] == "r"):
                # Raw string: closing quote + same hash count. Plain byte/C
                # strings (no hashes) still honour backslash escapes.
                start_line = line
                if hashes:
                    end = '"' + "#" * hashes
                    k = source.find(end, j + 1)
                    if k == -1:
                        k = n - len(end)
                    line += source.count("\n", i, k + len(end))
                    tokens.append(Token("STRING", source[i:k + len(end)], start_line))
                    i = k + len(end)
                else:
                    k = j + 1
                    while k < n:
                        if source[k] == "\\":
                            k += 2
                            continue
                        if source[k] == '"':
                            k += 1
                            break
                        if source[k] == "\n":
                            line += 1
                        k += 1
                    tokens.append(Token("STRING", source[i:k], start_line))
                    i = k
                continue
        # Plain string.
        if ch == '"':
            start_line = line
            j = i + 1
            while j < n:
                if source[j] == "\\":
                    j += 2
                    continue
                if source[j] == '"':
                    j += 1
                    break
                if source[j] == "\n":
                    line += 1
                j += 1
            tokens.append(Token("STRING", source[i:j], start_line))
            i = j
            continue
        # Char literal vs lifetime: 'a' / '\n' / '\u{...}' / '}' are chars;
        # 'a / 'static / '_ are lifetimes.
        if ch == "'":
            end = _char_end(source, i)
            if end != -1:
                tokens.append(Token("STRING", source[i:end], line))
                i = end
                continue
            j = i + 1
            if j < n and _is_ident_start(source[j]):
                k = j + 1
                while k < n and _is_ident_cont(source[k]):
                    k += 1
                tokens.append(Token("LIFETIME", source[i:k], line))
                i = k
                continue
            tokens.append(Token("PUNCT", "'", line))
            i += 1
            continue
        # Number: digits, hex/octal/binary, floats, suffixes.
        if ch.isdigit():
            j = i
            while j < n and (_is_ident_cont(source[j]) or
                             (source[j] == "." and j + 1 < n and source[j + 1].isdigit())):
                j += 1
            tokens.append(Token("NUMBER", source[i:j], line))
            i = j
            continue
        # Identifier / raw identifier r#type.
        if _is_ident_start(ch):
            j = i
            while j < n and _is_ident_cont(source[j]):
                j += 1
            if source[i:j] == "r" and j < n and source[j] == "#":
                k = j + 1
                while k < n and _is_ident_cont(source[k]):
                    k += 1
                tokens.append(Token("IDENT", source[j + 1:k], line))
                i = k
                continue
            tokens.append(Token("IDENT", source[i:j], line))
            i = j
            continue
        # Punctuation, longest match first.
        matched = False
        for punct in MULTI_PUNCT:
            if source.startswith(punct, i):
                tokens.append(Token("PUNCT", punct, line))
                i += len(punct)
                matched = True
                break
        if not matched:
            tokens.append(Token("PUNCT", ch, line))
            i += 1
    tokens.append(Token("PUNCT", "<eof>", line))
    return tokens


# ---------------------------------------------------------------------------
# Parser: items, use trees, modules
# ---------------------------------------------------------------------------

@dataclass
class Import:
    """One leaf of a `use` tree: a single imported name."""
    path: list[str]          # full path segments as written, e.g. [crate, task, Task]
    alias: str | None = None
    glob: bool = False       # `path::*`
    is_self: bool = False    # `path::self` imports the module itself
    visibility: str = ""     # "", "pub", "pub(crate)", "pub(super)", "pub(in ...)"
    cfg: str | None = None   # raw cfg expression, if #[cfg(...)] applied
    line: int = 0
    local: bool = False      # inside a function/impl body, not module scope
    scope: str = ""          # enclosing item name when local
    module: list[str] = field(default_factory=list)  # inline mods enclosing it

    def to_dict(self) -> dict:
        return {"path": self.path, "alias": self.alias, "glob": self.glob,
                "self": self.is_self, "visibility": self.visibility,
                "cfg": self.cfg, "line": self.line, "local": self.local,
                "scope": self.scope, "module": self.module}


@dataclass
class Item:
    name: str
    kind: str                # fn, struct, enum, trait, type, const, static,
                             # union, macro, mod, extern_crate
    visibility: str = ""
    cfg: str | None = None
    line: int = 0
    module: list[str] = field(default_factory=list)  # inline mods enclosing it

    def to_dict(self) -> dict:
        return {"name": self.name, "kind": self.kind,
                "visibility": self.visibility, "cfg": self.cfg,
                "line": self.line, "module": self.module}


@dataclass
class ModDecl:
    name: str
    inline: bool             # `mod x { ... }` vs `mod x;`
    path_attr: str | None = None  # #[path = "..."] override
    cfg: str | None = None
    line: int = 0
    module: list[str] = field(default_factory=list)  # inline mods enclosing it

    def to_dict(self) -> dict:
        return {"name": self.name, "inline": self.inline,
                "path": self.path_attr, "cfg": self.cfg, "line": self.line,
                "module": self.module}


@dataclass
class ParseNote:
    line: int
    message: str

    def to_dict(self) -> dict:
        return {"line": self.line, "message": self.message}


@dataclass
class ParsedFile:
    path: str
    items: list[Item] = field(default_factory=list)
    imports: list[Import] = field(default_factory=list)
    mods: list[ModDecl] = field(default_factory=list)
    notes: list[ParseNote] = field(default_factory=list)
    inner_attrs: list[str] = field(default_factory=list)  # #![...] at file top

    def to_dict(self) -> dict:
        return {"path": self.path,
                "items": [i.to_dict() for i in self.items],
                "imports": [i.to_dict() for i in self.imports],
                "mods": [m.to_dict() for m in self.mods],
                "notes": [n.to_dict() for n in self.notes],
                "inner_attrs": self.inner_attrs}


# Item keywords the parser recognises at module scope.
_ITEM_KINDS = {"fn": "fn", "struct": "struct", "enum": "enum",
               "trait": "trait", "type": "type", "const": "const",
               "static": "static", "union": "union", "macro": "macro"}
_MODIFIERS = {"unsafe", "async", "default", "const", "extern", "auto"}

OPEN_TO_CLOSE = {"{": "}", "(": ")", "[": "]"}


class Parser:
    """Recursive-descent parser over the token stream."""

    def __init__(self, tokens: list[Token], path: str):
        self.toks = tokens
        self.i = 0
        self.result = ParsedFile(path=path)
        self._module_stack: list[list[str]] = [[]]

    # -- cursor helpers ---------------------------------------------------
    def peek(self, ahead: int = 0) -> Token:
        j = min(self.i + ahead, len(self.toks) - 1)
        return self.toks[j]

    def next(self) -> Token:
        tok = self.toks[self.i]
        if self.i < len(self.toks) - 1:
            self.i += 1
        return tok

    def at(self, text: str) -> bool:
        return self.peek().text == text and self.peek().kind != "<eof>"

    def at_eof(self) -> bool:
        return self.peek().text == "<eof>"

    def eat(self, text: str) -> bool:
        if self.at(text):
            self.next()
            return True
        return False

    def note(self, line: int, message: str) -> None:
        self.result.notes.append(ParseNote(line, message))

    # -- balanced skipping --------------------------------------------------
    def skip_balanced(self, open_tok: str, scan_uses: bool = False,
                      scope: str = "") -> None:
        """Skip a balanced delimiter pair, starting AT the opener.

        With scan_uses, any `use` statement found inside is parsed as a
        local import (function/impl bodies may contain scoped imports).
        """
        close_tok = OPEN_TO_CLOSE[open_tok]
        self.next()  # consume opener
        depth = 1
        while not self.at_eof() and depth:
            tok = self.peek()
            if tok.text in OPEN_TO_CLOSE:
                self.skip_balanced(tok.text, scan_uses, scope)
                continue
            if tok.text == close_tok:
                depth -= 1
                self.next()
                continue
            if tok.text in ("}", ")", "]"):
                # Mismatched closer: bail out rather than looping forever.
                self.note(tok.line, f"unexpected {tok.text!r} while skipping block")
                return
            if scan_uses and tok.text == "use" and tok.kind == "IDENT":
                self._parse_use(visibility="", cfg=None, local=True, scope=scope)
                continue
            self.next()

    def skip_to_item_end(self) -> None:
        """Skip to the next `;` or balanced `{...}` block at this level."""
        while not self.at_eof():
            tok = self.peek()
            if tok.text == ";":
                self.next()
                return
            if tok.text == "{":
                self.skip_balanced("{")
                return
            if tok.text == "}":
                return
            if tok.text in OPEN_TO_CLOSE:
                self.skip_balanced(tok.text)
                continue
            self.next()

    # -- attributes and visibility ------------------------------------------
    def parse_attrs(self) -> list[str]:
        """Parse leading `#...` attributes; returns their raw inner text.

        Also records the #[path] of the last attribute group on
        `self._last_path` for the immediately following `mod` declaration.
        """
        attrs: list[str] = []
        while self.at("#"):
            start_line = self.peek().line
            self.next()  # '#'
            self.eat("!")
            if not self.eat("["):
                self.note(start_line, "expected '[' after '#'")
                return attrs
            depth, parts = 1, []
            while not self.at_eof() and depth:
                tok = self.next()
                if tok.text == "[":
                    depth += 1
                elif tok.text == "]":
                    depth -= 1
                    if depth == 0:
                        break
                parts.append(tok.text)
            attrs.append(" ".join(parts))
        self._last_path = self._path_of(attrs)
        return attrs

    @staticmethod
    def _cfg_of(attrs: list[str]) -> str | None:
        for attr in attrs:
            if attr.startswith("cfg"):
                rest = attr[3:].strip()
                if rest.startswith("(") and rest.endswith(")"):
                    return rest[1:-1].strip()
                return rest or "cfg"
        return None

    @staticmethod
    def _path_of(attrs: list[str]) -> str | None:
        for attr in attrs:
            if attr.startswith("path"):
                match = re.search(r'"([^"]*)"', attr)
                if match:
                    return match.group(1)
        return None

    def parse_visibility(self) -> str:
        if not self.at("pub"):
            return ""
        self.next()
        if self.at("("):
            parts = ["pub("]
            self.next()
            while not self.at_eof() and not self.at(")"):
                parts.append(self.next().text)
            self.eat(")")
            return "".join(parts) + ")"
        return "pub"

    # -- use trees ------------------------------------------------------------
    def _parse_use_tree(self, prefix: list[str], visibility: str,
                        cfg: str | None, line: int, local: bool,
                        scope: str) -> None:
        """Parse one use tree after `use`, expanding nested groups to leaves."""
        segments = list(prefix)
        while True:
            tok = self.peek()
            if tok.kind == "IDENT" or (tok.kind == "PUNCT" and tok.text == "*"):
                if tok.text == "*":
                    self.next()
                    self.result.imports.append(Import(
                        path=segments, glob=True, visibility=visibility,
                        cfg=cfg, line=line, local=local, scope=scope,
                        module=list(self._module_stack[-1])))
                    return
                segments.append(self.next().text)
                if self.at("::"):
                    self.next()
                    if self.at("{"):
                        self._parse_use_group(segments, visibility, cfg, line,
                                              local, scope)
                        return
                    continue
                # Leaf: optional `as alias`.
                alias = None
                if self.at("as"):
                    self.next()
                    if self.peek().kind == "IDENT":
                        alias = self.next().text
                    elif self.at("_"):
                        self.next()
                        alias = "_"
                self.result.imports.append(Import(
                    path=segments, alias=alias,
                    is_self=(segments[-1] == "self") if segments else False,
                    visibility=visibility, cfg=cfg, line=line,
                    local=local, scope=scope,
                    module=list(self._module_stack[-1])))
                return
            if tok.text == "{":
                self._parse_use_group(segments, visibility, cfg, line, local, scope)
                return
            self.note(tok.line, f"unexpected {tok.text!r} in use tree")
            return

    def _parse_use_group(self, prefix: list[str], visibility: str,
                         cfg: str | None, line: int, local: bool,
                         scope: str) -> None:
        self.next()  # '{'
        while not self.at_eof() and not self.at("}"):
            self._parse_use_tree(prefix, visibility, cfg, line, local, scope)
            if not self.eat(","):
                break
        if not self.eat("}"):
            self.note(self.peek().line, "expected '}' in use group")

    def _parse_use(self, visibility: str, cfg: str | None, local: bool = False,
                   scope: str = "") -> None:
        line = self.peek().line
        self.next()  # 'use'
        prefix: list[str] = []
        if self.at("::"):  # 2015-style absolute path
            self.next()
            prefix.append("")
        self._parse_use_tree(prefix, visibility, cfg, line, local, scope)
        if not self.eat(";"):
            self.note(self.peek().line, "expected ';' after use statement")
            self.skip_to_item_end()

    # -- items ------------------------------------------------------------------
    def parse_module_body(self, module: list[str]) -> None:
        """Parse items until EOF or a closing `}` (not consumed)."""
        self._module_stack.append(list(module))
        try:
            self._parse_module_items(module)
        finally:
            self._module_stack.pop()

    def _parse_module_items(self, module: list[str]) -> None:
        while not self.at_eof() and not self.at("}"):
            attrs = self.parse_attrs()
            if self.at_eof() or self.at("}"):
                break
            cfg = self._cfg_of(attrs)
            if self.at("#"):
                continue  # stray attribute line; loop re-parses
            start = self.peek()
            if start.text == "!" and start.kind == "PUNCT":
                # Inner attribute #![...] was consumed by parse_attrs as
                # `#` + `!` + `[...]`; a lone `!` here is unexpected.
                self.note(start.line, "unexpected '!' at item position")
                self.next()
                continue
            visibility = self.parse_visibility()
            # Skip modifiers (unsafe/async/const-fn/extern/default/auto).
            # `const` is a modifier only before `fn`; before a name it is a
            # const item, so it must reach the item-kind check below.
            while self.peek().text in _MODIFIERS and self.peek().kind == "IDENT":
                if self.peek().text == "const" and self.peek(1).text != "fn":
                    break
                if self.peek().text == "extern" and self.peek(1).kind == "STRING":
                    self.next()  # extern
                    self.next()  # "C"
                    continue
                if self.peek().text == "extern" and self.peek(1).text == "crate":
                    break
                self.next()
            tok = self.peek()
            if tok.text == "use" and tok.kind == "IDENT":
                self._parse_use(visibility, cfg)
                continue
            if tok.text == "mod" and tok.kind == "IDENT":
                self._parse_mod(module, cfg)
                continue
            if tok.text == "extern" and self.peek(1).text == "crate":
                self._parse_extern_crate(visibility, cfg)
                continue
            if tok.text == "impl" and tok.kind == "IDENT":
                self._skip_impl(module)
                continue
            if tok.text in _ITEM_KINDS and tok.kind == "IDENT":
                self._parse_item(tok.text, visibility, cfg, module)
                continue
            if tok.text == "macro_rules" and self.peek(1).text == "!":
                # Checked before generic macro invocation: macro_rules is
                # an item definition, not a call.
                self.next()
                self.next()
                if self.peek().kind == "IDENT":
                    name_tok = self.next()
                    self.result.items.append(Item(
                        name=name_tok.text, kind="macro",
                        visibility=visibility, cfg=cfg, line=name_tok.line,
                        module=list(self._module_stack[-1])))
                if self.peek().text in OPEN_TO_CLOSE:
                    self.skip_balanced(self.peek().text)
                elif self.at(";"):
                    self.next()
                continue
            if tok.kind == "IDENT" and self.peek(1).text == "!":
                # Macro invocation at item position: name!(...); or name!{...}
                self._skip_macro_invocation()
                continue
            # Unknown item: skip it, record a note, keep going.
            self.note(tok.line, f"unrecognised item starting with {tok.text!r}")
            self.skip_to_item_end()

    def _parse_mod(self, module: list[str], cfg: str | None) -> None:
        line = self.peek().line
        self.next()  # 'mod'
        if self.peek().kind != "IDENT":
            self.note(self.peek().line, "expected module name after 'mod'")
            self.skip_to_item_end()
            return
        name = self.next().text
        path_attr = getattr(self, "_last_path", None)
        self._last_path = None
        if self.eat(";"):
            self.result.mods.append(ModDecl(name, inline=False,
                                            path_attr=path_attr, cfg=cfg,
                                            line=line,
                                            module=list(self._module_stack[-1])))
            return
        if self.at("{"):
            self.result.mods.append(ModDecl(name, inline=True,
                                            path_attr=path_attr, cfg=cfg,
                                            line=line,
                                            module=list(self._module_stack[-1])))
            self.next()  # '{'
            self.parse_module_body(module + [name])
            if not self.eat("}"):
                self.note(self.peek().line, f"expected '}}' closing mod {name}")
            return
        self.note(self.peek().line, f"expected ';' or '{{' after mod {name}")
        self.skip_to_item_end()

    def _parse_extern_crate(self, visibility: str, cfg: str | None) -> None:
        line = self.peek().line
        self.next()  # 'extern'
        self.next()  # 'crate'
        if self.peek().kind == "IDENT":
            name = self.next().text
            if self.at("as"):
                self.next()
                if self.peek().kind == "IDENT":
                    name = self.next().text
            self.result.items.append(Item(name=name, kind="extern_crate",
                                          visibility=visibility, cfg=cfg,
                                          line=line,
                                          module=list(self._module_stack[-1])))
        self.eat(";")

    def _parse_item(self, keyword: str, visibility: str, cfg: str | None,
                    module: list[str]) -> None:
        self.next()  # keyword
        if keyword == "static":
            self.eat("mut")
        if self.peek().kind != "IDENT":
            # e.g. `const _: T = ...;` (unnamed const) or `fn` without name
            self.skip_to_item_end()
            return
        name_tok = self.next()
        self.result.items.append(Item(name=name_tok.text, kind=_ITEM_KINDS[keyword],
                                      visibility=visibility, cfg=cfg,
                                      line=name_tok.line,
                                      module=list(self._module_stack[-1])))
        # Skip generics / signature / where clause, then the body or `;`.
        # Bodies are scanned for scoped `use` statements. const/static/type
        # initializers may hold `{...}` literals before the trailing `;`,
        # so those keep looping after a block; other items end at the block.
        while not self.at_eof():
            tok = self.peek()
            if tok.text == ";":
                self.next()
                return
            if tok.text == "{":
                self.skip_balanced("{", scan_uses=True, scope=name_tok.text)
                if keyword in ("const", "static", "type"):
                    continue
                return
            if tok.text == "}":
                return
            if tok.text in OPEN_TO_CLOSE:
                self.skip_balanced(tok.text)
                continue
            self.next()

    def _skip_impl(self, module: list[str]) -> None:
        self.next()  # 'impl'
        while not self.at_eof():
            tok = self.peek()
            if tok.text == "{":
                self.skip_balanced("{", scan_uses=True, scope="impl")
                return
            if tok.text in OPEN_TO_CLOSE:
                self.skip_balanced(tok.text)
                continue
            self.next()

    def _skip_macro_invocation(self) -> None:
        # name ! [delimited] [;] — also handles path::name!(...)
        while not self.at_eof() and self.peek().text != "!":
            self.next()
        self.eat("!")
        if self.peek().text in OPEN_TO_CLOSE:
            self.skip_balanced(self.peek().text)
        self.eat(";")


def parse_source(source: str, path: str) -> ParsedFile:
    """Parse Rust source text into its dependency-relevant structure."""
    parser = Parser(tokenize(source), path)
    parser._last_path = None
    # File-level inner attributes #![...] precede items.
    while parser.at("#") and parser.peek(1).text == "!":
        parser.result.inner_attrs.extend(parser.parse_attrs())
    parser.parse_module_body([])
    return parser.result


def parse_file(path: str | Path) -> ParsedFile:
    """Parse one .rs file from disk."""
    text = Path(path).read_text(encoding="utf-8")
    return parse_source(text, str(path))


# ---------------------------------------------------------------------------
# Workspace graph builder
# ---------------------------------------------------------------------------

# First path segments that are never workspace crates.
STD_ROOTS = {"std", "core", "alloc"}
# Files/dirs never treated as crates or modules.
SKIP_DIRS = {"target", ".git", "node_modules", "__pycache__"}


def _crate_name(manifest: Path) -> str | None:
    """Read the [package] name from a Cargo.toml (None for virtual manifests)."""
    text = manifest.read_text(encoding="utf-8", errors="replace")
    in_package = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[package]"
            continue
        if in_package:
            match = re.match(r'name\s*=\s*"([^"]+)"', stripped)
            if match:
                return match.group(1)
    return None


def discover_crates(rust_root: Path) -> dict[str, Path]:
    """Map crate name -> crate dir for every Cargo.toml under rust_root."""
    crates: dict[str, Path] = {}
    for manifest in sorted(rust_root.rglob("Cargo.toml")):
        if any(part in SKIP_DIRS for part in manifest.parts):
            continue
        name = _crate_name(manifest)
        if name:
            crates[name] = manifest.parent
    return crates


@dataclass
class ModuleInfo:
    """One module in the graph: its file, items, imports and edges."""
    path: str                    # e.g. forge-core::bridge or forge-ide::model
    crate: str
    file: str                    # repo-relative path
    items: list[dict] = field(default_factory=list)
    imports: list[dict] = field(default_factory=list)
    # Resolved edges: {"to": module-or-crate path, "kind": ..., "names": [...]}
    edges: list[dict] = field(default_factory=list)


def _module_path_for_file(crate: str, crate_dir: Path, rs_file: Path) -> str | None:
    """Compute a file's module path from layout conventions.

    src/lib.rs | src/main.rs -> the crate root; src/a/b.rs -> crate::a::b;
    src/a/mod.rs -> crate::a. tests/<name>.rs, examples/<name>.rs and
    benches/<name>.rs are separate roots named by file stem.
    Returns None for files that are not under a known root.
    """
    try:
        rel = rs_file.relative_to(crate_dir)
    except ValueError:
        return None
    parts = list(rel.parts)
    if len(parts) == 1 and parts[0] == "build.rs":
        # Build scripts are their own crate root, separate from lib/bin.
        return f"{crate}::build"
    if len(parts) >= 2 and parts[0] == "src":
        stem = rs_file.stem
        if parts[-1] == "lib.rs" and len(parts) == 2:
            return crate
        if parts[-1] == "main.rs" and len(parts) == 2:
            # The default binary is a separate root from the library.
            return f"{crate}::bin"
        if parts[-1] == "mod.rs":
            mods = parts[1:-1]
        elif parts[1] == "bin" and len(parts) == 3:
            return f"{crate}::bin::{stem}"
        else:
            mods = parts[1:-1] + [stem]
        return "::".join([crate] + mods) if mods else crate
    if len(parts) >= 2 and parts[0] in ("tests", "examples", "benches"):
        stem = rs_file.stem
        if len(parts) == 2:
            return f"{crate}::{parts[0]}::{stem}"
        if parts[-1] == "mod.rs":
            mods = parts[1:-1]
        else:
            mods = parts[1:-1] + [stem]
        return "::".join([crate, parts[0]] + mods)
    return None


def _walk_up(target: list[str], modules: set[str]) -> str:
    """Longest prefix of target that is a known module (else the full path)."""
    for cut in range(len(target), 0, -1):
        joined = "::".join(target[:cut])
        if joined in modules:
            return joined
    return "::".join(target)


def _resolve_import(path: list[str], from_module: list[str], crate: str,
                    modules: set[str], crates: set[str]) -> tuple[str, str]:
    """Resolve an import path to (target, kind).

    Kinds: local (same-crate module), crate (another workspace crate),
    std, external (third-party crate), unresolved. Targets are always
    modules: an item path walks up to its enclosing module.
    """
    if not path:
        return "", "unresolved"
    head = path[0]
    if head == "":
        # 2015 absolute path ::foo::bar -> crate root.
        return _walk_up([crate] + path[1:], modules), "local"
    if head == "crate":
        return _walk_up([crate] + path[1:], modules), "local"
    if head == "super":
        ups = 0
        rest = list(path)
        while rest and rest[0] == "super":
            ups += 1
            rest.pop(0)
        base = from_module[:max(0, len(from_module) - ups)]
        target = base + rest or [crate]
        return _walk_up(target, modules), "local"
    if head == "self":
        return _walk_up(from_module + path[1:], modules), "local"
    if head in STD_ROOTS:
        return "::".join(path), "std"
    if head in crates:
        # Workspace crate: walk up to the longest known module prefix, so
        # `forge_core::command::ArgumentDescriptor` targets the module
        # `forge_core::command`, not the item.
        return _walk_up(list(path), modules), "crate"
    # Bare first segment: a submodule of the current module, or an
    # external crate. Prefer the local module when it exists.
    candidate = "::".join(from_module + [head])
    if candidate in modules:
        return _walk_up(from_module + path, modules), "local"
    return head, "external"


def build_graph(rust_root: str | Path) -> dict:
    """Parse every .rs file under rust_root and build the dependency graph."""
    root = Path(rust_root)
    crates = discover_crates(root)
    crate_names = {name.replace("-", "_") for name in crates}
    parsed: dict[str, ParsedFile] = {}
    file_module: dict[str, list[str]] = {}
    # Pass 1: parse all files; assign module paths by layout.
    for crate_name, crate_dir in sorted(crates.items()):
        crate_mod = crate_name.replace("-", "_")
        for rs_file in sorted(crate_dir.rglob("*.rs")):
            if any(part in SKIP_DIRS for part in rs_file.parts):
                continue
            mod_path = _module_path_for_file(crate_mod, crate_dir, rs_file)
            if mod_path is None:
                continue
            rel = str(rs_file.relative_to(root))
            try:
                parsed[rel] = parse_file(rs_file)
            except (OSError, UnicodeDecodeError) as exc:
                parsed[rel] = ParsedFile(path=rel)
                parsed[rel].notes.append(ParseNote(0, f"unreadable: {exc}"))
            file_module[rel] = mod_path.split("::")
    # Pass 2: apply #[path] overrides from mod declarations.
    for rel, pf in parsed.items():
        base_mod = file_module[rel]
        file_dir = Path(rel).parent
        for decl in pf.mods:
            if decl.path_attr is None or decl.inline:
                continue
            target = (file_dir / decl.path_attr).as_posix()
            # Normalise ./ and ../ segments.
            parts: list[str] = []
            for seg in target.split("/"):
                if seg in ("", "."):
                    continue
                if seg == "..":
                    parts.pop()
                else:
                    parts.append(seg)
            target = "/".join(parts)
            if target in file_module:
                file_module[target] = base_mod + decl.module + [decl.name]
    # Pass 3: assemble modules and resolve imports.
    modules: dict[str, ModuleInfo] = {}
    module_names = {"::".join(m) for m in file_module.values()}
    # Inline modules get their own entries, sharing the file.
    for rel, pf in sorted(parsed.items()):
        base = file_module[rel]
        seen_inline: set[str] = set()
        for decl in pf.mods:
            if decl.inline:
                full = "::".join(base + decl.module + [decl.name])
                seen_inline.add(full)
        for full in sorted(seen_inline):
            modules.setdefault(full, ModuleInfo(path=full, crate=base[0], file=rel))
        modules.setdefault("::".join(base),
                           ModuleInfo(path="::".join(base), crate=base[0], file=rel))
    for rel, pf in sorted(parsed.items()):
        base = file_module[rel]
        for item in pf.items:
            mod_path = "::".join(base + item.module)
            entry = modules.setdefault(mod_path, ModuleInfo(path=mod_path, crate=base[0], file=rel))
            entry.items.append(item.to_dict())
        for imp in pf.imports:
            mod_path = "::".join(base + imp.module)
            entry = modules.setdefault(mod_path, ModuleInfo(path=mod_path, crate=base[0], file=rel))
            from_mod = base + imp.module
            target, kind = _resolve_import(imp.path, from_mod, base[0],
                                           module_names, crate_names)
            record = imp.to_dict()
            record["target"] = target
            record["kind"] = kind
            entry.imports.append(record)
    # Pass 4: collapse imports into per-module edges.
    for mod_path, entry in modules.items():
        grouped: dict[tuple[str, str], dict] = {}
        for imp in entry.imports:
            key = (imp["target"], imp["kind"])
            slot = grouped.setdefault(key, {"to": imp["target"], "kind": imp["kind"],
                                            "names": [], "re_export": False})
            if imp["glob"]:
                slot["names"].append("*")
            elif imp["path"]:
                slot["names"].append(imp["alias"] or imp["path"][-1])
            if imp["visibility"].startswith("pub"):
                slot["re_export"] = True
        entry.edges = [grouped[k] for k in sorted(grouped)]
        for edge in entry.edges:
            edge["names"] = sorted(set(edge["names"]))
    errors = [{"file": rel, **n.to_dict()}
              for rel, pf in sorted(parsed.items()) for n in pf.notes]
    return {
        "crates": sorted(crate_names),
        "files": len(parsed),
        "modules": {k: {"crate": v.crate, "file": v.file, "items": v.items,
                        "imports": v.imports, "edges": v.edges}
                    for k, v in sorted(modules.items())},
        "errors": errors,
    }


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    quiet = "--quiet" in args
    args = [a for a in args if a != "--quiet"]
    if len(args) != 1:
        print("usage: rust_ast.py <file.rs | rust-root> [--quiet]", file=sys.stderr)
        return 2
    target = Path(args[0])
    if target.is_file():
        result = parse_file(target).to_dict()
        errors = result["notes"]
    elif target.is_dir():
        result = build_graph(target)
        errors = result["errors"]
    else:
        print(f"no such file or directory: {target}", file=sys.stderr)
        return 2
    if not quiet:
        print(json.dumps(result, indent=2, sort_keys=True))
    if errors:
        print(f"{len(errors)} parse note(s)", file=sys.stderr)
        for err in errors[:20]:
            print(f"  {err.get('file', target)}:{err.get('line', 0)}: {err['message']}",
                  file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
