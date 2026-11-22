#!/usr/bin/env python3
"""Pre-compile imports / `pub use` check for the Rust workspace.

Runs on the pure-Python AST (rust_ast.py), so it needs no build and can
run before compile in the CI pipeline, right after fmt.

Rules:
  R1 unresolved-import   An import names a module or item that does not
                         exist in the workspace (external crates and std
                         are not verifiable here and are skipped).
  R2 visibility          An import crosses a visibility boundary: a
                         private, pub(crate), pub(super) or pub(in path)
                         item or module imported from outside its scope.
  R3 lib-shape           A lib.rs holds item definitions. The formalized
                         shape is mod declarations and use/pub use only.
  R4 unreachable-pub     (warn) A pub item is not reachable from its
                         crate root through public modules or pub use
                         re-exports: public in name, crate-private in
                         fact. Warnings do not fail the gate unless
                         --strict is given.
  R5 name-collision      Two bindings with the same name in the same
                         namespace in one module (item vs item, import
                         vs import, import vs item). Glob imports are
                         exempt: they lose to explicit bindings.
  R6 re-export-privacy   A pub use widens visibility: re-exporting a
                         non-pub item as pub, or a private module as pub.
  R7 missing-module      A `mod` declaration has no file behind it (warn
                         when the declaration is cfg-gated).
  R8 orphan-file         (warn) A .rs file no crate root reaches through
                         `mod` declarations: not compiled, dead code.

Usage:
  python3 check_imports.py [rust-root] [--format text|json] [--strict]
  Exit 0 when no error-level findings; exit 1 otherwise.
"""

import json
import sys
from dataclasses import dataclass, field
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

import rust_ast  # noqa: E402

RUST_ROOT = SCRIPT_DIR.parent

# Namespace of each item kind, for R5 collision detection.
TYPE_KINDS = {"struct", "enum", "trait", "type", "union", "mod", "extern_crate"}
VALUE_KINDS = {"fn", "const", "static"}
MACRO_KINDS = {"macro"}

# Visibility rank for R6: a re-export must not outrank its item.
_VIS_RANK = {"": 0, "pub(super)": 1, "pub(crate)": 2, "pub": 3}


def _vis_rank(visibility: str) -> int:
    if visibility in _VIS_RANK:
        return _VIS_RANK[visibility]
    if visibility.startswith("pub(in "):
        return 1
    return 0


def _namespace(kind: str) -> str:
    if kind in TYPE_KINDS:
        return "type"
    if kind in VALUE_KINDS:
        return "value"
    if kind in MACRO_KINDS:
        return "macro"
    return "type"


@dataclass
class Finding:
    rule: str
    level: str  # error | warn
    file: str
    line: int
    module: str
    message: str

    def to_dict(self) -> dict:
        return {"rule": self.rule, "level": self.level, "file": self.file,
                "line": self.line, "module": self.module,
                "message": self.message}


def _is_descendant(module: str, ancestor: str) -> bool:
    return module == ancestor or module.startswith(ancestor + "::")


def _parent(module: str) -> str:
    return module.rsplit("::", 1)[0] if "::" in module else module


class Index:
    """Resolution index over a rust_ast workspace graph."""

    def __init__(self, graph: dict):
        self.graph = graph
        self.modules: dict[str, dict] = graph["modules"]
        self.crates: set[str] = set(graph["crates"])
        # module -> {name: item}
        self.items: dict[str, dict[str, dict]] = {
            name: {it["name"]: it for it in mod["items"]}
            for name, mod in self.modules.items()
        }
        # #[macro_export] macros live at the crate root, not in the file
        # that defines them, and are public there.
        for module, items in list(self.items.items()):
            for item in items.values():
                if item.get("macro_export"):
                    root = self.root_of(module)
                    copy = dict(item)
                    copy["visibility"] = "pub"
                    self.items.setdefault(root, {})[item["name"]] = copy
        # module -> {child name: child module path}
        self.children: dict[str, dict[str, str]] = {}
        for name in self.modules:
            if "::" in name:
                parent, _, child = name.rpartition("::")
                self.children.setdefault(parent, {})[child] = name
        # module -> its declaration {visibility, cfg} (from the parent's mods)
        self.mod_decl: dict[str, dict] = {}
        for name, mod in self.modules.items():
            for decl in mod.get("mods", []):
                self.mod_decl[decl["declared"]] = decl

    # -- module roots ------------------------------------------------------
    def root_of(self, module: str) -> str:
        """The crate root a module belongs to (lib, bin, build, test, ...)."""
        mod = self.modules[module]
        if mod.get("root"):
            return mod["root"]
        # Fallback for graphs without the root field: derive it from the
        # file. The crate dir is the path segment matching the crate name
        # (crates may nest under examples/ or tests/consumer/).
        crate_dashed = self.crate_of(module).replace("_", "-")
        parts = mod["file"].split("/")
        try:
            cut = parts.index(crate_dashed) + 1
        except ValueError:
            return self.crate_of(module)
        crate = self.crate_of(module)
        rel = parts[cut:]
        if rel == ["build.rs"]:
            return f"{crate}::build"
        if rel == ["src", "main.rs"]:
            return f"{crate}::bin"
        if len(rel) >= 3 and rel[0] == "src" and rel[1] == "bin":
            return f"{crate}::bin::{Path(rel[2]).stem}"
        if len(rel) >= 2 and rel[0] in ("tests", "examples", "benches"):
            return f"{crate}::{rel[0]}::{Path(rel[1]).stem}"
        return crate

    def crate_of(self, module: str) -> str:
        return module.split("::", 1)[0]

    def is_lib_module(self, module: str) -> bool:
        """True when the module belongs to a library crate root."""
        root = self.root_of(module)
        return root == self.crate_of(module)

    def is_conditional(self, module: str) -> bool:
        """True when any mod declaration on the module's path has #[cfg]."""
        segments = module.split("::")
        for cut in range(2, len(segments) + 1):
            decl = self.mod_decl.get("::".join(segments[:cut]))
            if decl and decl.get("cfg"):
                return True
        return False

    # -- visibility ----------------------------------------------------------
    def is_visible(self, visibility: str, container: str, importer: str) -> bool:
        """Is a name with this visibility, declared in `container`,
        visible from `importer`?"""
        if visibility == "pub":
            return True
        if visibility == "":
            return _is_descendant(importer, container)
        if visibility == "pub(crate)":
            return self.crate_of(importer) == self.crate_of(container)
        if visibility == "pub(super)":
            return _is_descendant(importer, _parent(container))
        if visibility.startswith("pub(in ") and visibility.endswith(")"):
            scope = visibility[len("pub(in "):-1]
            if scope.startswith("crate::"):
                scope = self.crate_of(container) + scope[len("crate"):]
            elif scope == "crate":
                scope = self.crate_of(container)
            elif not scope.startswith(self.crate_of(container)):
                scope = container + "::" + scope
            return _is_descendant(importer, scope)
        return True  # unknown form: do not flag

    def is_module_visible(self, module: str, importer: str) -> bool:
        """Is module visible from importer, through its declaration chain?"""
        if module == importer or _is_descendant(importer, module):
            return True
        segments = module.split("::")
        for cut in range(2, len(segments) + 1):
            prefix = "::".join(segments[:cut])
            decl = self.mod_decl.get(prefix)
            if decl is None:
                continue
            container = "::".join(segments[:cut - 1])
            if not self.is_visible(decl.get("visibility", ""), container, importer):
                return False
        return True

    # -- name resolution -----------------------------------------------------
    def resolve_in_module(self, module: str, name: str, importer: str,
                          seen: frozenset | None = None) -> tuple | None:
        """Resolve one name inside a module.

        Returns ("item", def_module, item) | ("module", path, None) | None.
        Follows re-exports: pub use from anywhere, private use only when
        the importer IS the module.
        """
        seen = seen or frozenset()
        if (module, name) in seen:
            return None
        seen = seen | {(module, name)}
        mod = self.modules.get(module)
        if mod is None:
            return None
        if name in self.items.get(module, {}):
            return ("item", module, self.items[module][name])
        if name in self.children.get(module, {}):
            return ("module", self.children[module][name], None)
        # A defining macro whose first argument is this name (e.g.
        # html_names!(Tag { ... })) plausibly defines it. The parser
        # cannot expand macros, so this resolves the name and skips the
        # visibility check; the compiler still catches a real error.
        for macro in mod.get("macros", []):
            if macro.get("first_ident") == name:
                return ("macro", module, None)
        for imp in mod.get("imports", []):
            vis = imp.get("visibility", "")
            if not vis.startswith("pub") and importer != module:
                continue
            if imp.get("glob"):
                target = imp.get("target", "")
                if not target or target not in self.modules:
                    continue
                found = self.resolve_in_module(target, name, importer, seen)
                if found and found[0] == "item":
                    if found[2].get("visibility") == "pub":
                        return found
                    if (found[2].get("visibility") == "pub(crate)"
                            and self.crate_of(importer) == self.crate_of(found[1])):
                        return found
                elif found:
                    return found
                continue
            if imp.get("self"):
                bound = imp["path"][-2] if len(imp["path"]) >= 2 else None
            else:
                bound = imp.get("alias") or (
                    imp["path"][-1] if imp.get("path") else None)
            if bound != name or not imp.get("path"):
                continue
            resolved = self.resolve_path(imp["path"], module)
            if resolved is not None:
                return resolved
        return None

    def resolve_path(self, path: list[str], from_module: str) -> tuple | None:
        """Resolve a full import path from a module.

        Returns ("item", def_module, item) | ("module", path, None) | None.
        External crates and std resolve to ("external", crate, None).
        """
        if not path:
            return None
        root = self.root_of(from_module)
        head, rest = path[0], path[1:]
        if head == "":
            current, rest = root, rest
        elif head == "crate":
            current = root
        elif head == "self":
            current = from_module
        elif head == "super":
            ups = 0
            while ups < len(path) and path[ups] == "super":
                ups += 1
            rest = path[ups:]
            segments = from_module.split("::")
            root_len = len(root.split("::"))
            if len(segments) - ups < root_len:
                return None
            current = "::".join(segments[:len(segments) - ups])
        elif head in self.crates or head.replace("-", "_") in self.crates:
            current = head.replace("-", "_")
        elif head in rust_ast.STD_ROOTS:
            return ("external", head, None)
        elif head in self.children.get(from_module, {}):
            current = self.children[from_module][head]
        else:
            # Unknown first segment: a third-party crate (unverifiable).
            return ("external", head, None)
        if current not in self.modules:
            return None
        # Walk intermediate segments as modules.
        for segment in rest[:-1]:
            found = self.resolve_in_module(current, segment, from_module)
            if found is None or found[0] != "module":
                return None
            current = found[1]
        if not rest:
            return ("module", current, None)
        leaf = rest[-1]
        if leaf == "self":
            return ("module", current, None)
        if leaf == "*":
            return ("module", current, None)
        return self.resolve_in_module(current, leaf, from_module)


def check_workspace(rust_root: str | Path) -> list[Finding]:
    graph = rust_ast.build_graph(rust_root)
    index = Index(graph)
    findings: list[Finding] = []
    findings.extend(_check_imports(index))
    findings.extend(_check_lib_shape(index))
    findings.extend(_check_collisions(index))
    findings.extend(_check_reexport_privacy(index))
    findings.extend(_check_unreachable_pub(index))
    for m in graph.get("missing_modules", []):
        findings.append(Finding(
            "R7", "warn" if m.get("cfg") else "error",
            m["file"], m["line"], m["module"],
            f"declared module `{m['name']}` has no file"))
    for rel in graph.get("orphans", []):
        findings.append(Finding(
            "R8", "warn", rel, 1, "",
            "file is not in any module tree (no root declares it)"))
    findings.sort(key=lambda f: (f.file, f.line, f.rule))
    return findings


def _check_imports(index: Index) -> list[Finding]:
    """R1 unresolved imports, R2 visibility violations."""
    findings: list[Finding] = []
    for module, mod in sorted(index.modules.items()):
        for imp in mod.get("imports", []):
            if imp.get("kind") in ("std", "external"):
                continue
            resolved = index.resolve_path(imp["path"], module)
            label = "::".join(imp["path"]) + ("::*" if imp.get("glob") else "")
            if resolved is None:
                findings.append(Finding(
                    "R1", "error", mod["file"], imp["line"], module,
                    f"unresolved import `{label}`: no such module or item"))
                continue
            if resolved[0] == "external":
                continue
            if resolved[0] == "macro":
                continue  # macro-generated: visibility is unknowable here
            if imp.get("glob"):
                if not index.is_module_visible(resolved[1], module):
                    findings.append(Finding(
                        "R2", "error", mod["file"], imp["line"], module,
                        f"glob import `{label}`: module `{resolved[1]}` "
                        f"is not visible here"))
                continue
            if resolved[0] == "module":
                if not index.is_module_visible(resolved[1], module):
                    findings.append(Finding(
                        "R2", "error", mod["file"], imp["line"], module,
                        f"import `{label}`: module `{resolved[1]}` "
                        f"is not visible here"))
                continue
            _, def_module, item = resolved
            if not index.is_visible(item.get("visibility", ""), def_module, module):
                findings.append(Finding(
                    "R2", "error", mod["file"], imp["line"], module,
                    f"import `{label}`: `{item['name']}` is "
                    f"{item.get('visibility') or 'private'} in "
                    f"`{def_module}` and not visible here"))
    return findings


def _check_lib_shape(index: Index) -> list[Finding]:
    """R3: lib.rs holds only mod declarations and use statements."""
    findings: list[Finding] = []
    for module, mod in sorted(index.modules.items()):
        if not mod["file"].endswith("/src/lib.rs"):
            continue
        for item in mod.get("items", []):
            if item.get("module"):
                continue  # inside an inline mod: the mod is the item
            findings.append(Finding(
                "R3", "error", mod["file"], item["line"], module,
                f"lib.rs defines `{item['name']}` ({item['kind']}); "
                f"move it to a module file and re-export it"))
    return findings


def _cfgs_collide(a: str | None, b: str | None) -> bool:
    """Do two cfg conditions allow both bindings to exist at once?

    Unconditional bindings always collide; identical conditions collide;
    anything else (notably cfg(X) vs cfg(not(X)) alternatives) is treated
    as possibly disjoint and not flagged — the compiler catches a real
    collision at compile time, so the check stays quiet rather than
    crying wolf.
    """
    if a is None or b is None:
        return True
    return a.replace(" ", "") == b.replace(" ", "")


def _check_collisions(index: Index) -> list[Finding]:
    """R5: two bindings, same name, same namespace, one module."""
    findings: list[Finding] = []
    for module, mod in sorted(index.modules.items()):
        bound: dict[tuple[str, str], tuple[str, int, str | None]] = {}

        def bind(name: str, namespace: str, what: str, line: int,
                 cfg: str | None) -> None:
            key = (name, namespace)
            if key in bound:
                prev_what, prev_line, prev_cfg = bound[key]
                if _cfgs_collide(prev_cfg, cfg):
                    findings.append(Finding(
                        "R5", "error", mod["file"], line, module,
                        f"`{name}` ({namespace}) is bound twice: "
                        f"{prev_what} (line {prev_line}) and {what}"))
            else:
                bound[key] = (what, line, cfg)

        for item in mod.get("items", []):
            bind(item["name"], _namespace(item["kind"]),
                 f"{item['kind']} `{item['name']}`", item["line"],
                 item.get("cfg"))
        for imp in mod.get("imports", []):
            if imp.get("local"):
                continue  # fn-body use: scoped to its function, not the module
            if imp.get("glob"):
                continue  # globs lose to explicit bindings
            if imp.get("self"):
                name = imp["path"][-2] if len(imp["path"]) >= 2 else None
                namespace = "type"
            else:
                name = imp.get("alias") or imp["path"][-1]
                resolved = index.resolve_path(imp["path"], module)
                if resolved is None or resolved[0] in ("external", "macro"):
                    continue  # R1 already reports it, or unknowable
                namespace = ("type" if resolved[0] == "module"
                             else _namespace(resolved[2]["kind"]))
            if name:
                bind(name, namespace, f"import `{ '::'.join(imp['path']) }`",
                     imp["line"], imp.get("cfg"))
    return findings


def _check_reexport_privacy(index: Index) -> list[Finding]:
    """R6: a pub use must not widen an item's visibility."""
    findings: list[Finding] = []
    for module, mod in sorted(index.modules.items()):
        for imp in mod.get("imports", []):
            vis = imp.get("visibility", "")
            if not vis.startswith("pub"):
                continue
            if imp.get("glob"):
                continue  # glob re-exports only carry public items
            resolved = index.resolve_path(imp["path"], module)
            if resolved is None or resolved[0] in ("external", "macro"):
                continue  # R1 already reports it, or unknowable
            label = "::".join(imp["path"])
            if resolved[0] == "module":
                decl = index.mod_decl.get(resolved[1])
                target_vis = decl.get("visibility", "") if decl else "pub"
                target_what = f"module `{resolved[1]}`"
            else:
                target_vis = resolved[2].get("visibility", "")
                target_what = f"`{resolved[2]['name']}` in `{resolved[1]}`"
            if _vis_rank(vis) > _vis_rank(target_vis):
                findings.append(Finding(
                    "R6", "error", mod["file"], imp["line"], module,
                    f"`{vis} use {label}` widens visibility: {target_what} "
                    f"is {target_vis or 'private'}"))
    return findings


def _public_surface(index: Index, root: str) -> set[tuple[str, str]]:
    """(module, item) pairs reachable from a crate root via public paths."""
    surface: set[tuple[str, str]] = set()
    queue = [root]
    seen_mods: set[str] = set()
    while queue:
        module = queue.pop(0)
        if module in seen_mods or module not in index.modules:
            continue
        seen_mods.add(module)
        mod = index.modules[module]
        for item in mod.get("items", []):
            if item.get("visibility") == "pub" and not item.get("cfg"):
                surface.add((module, item["name"]))
        for decl in mod.get("mods", []):
            if decl.get("visibility") == "pub" and not decl.get("cfg"):
                queue.append(decl["declared"])
        for imp in mod.get("imports", []):
            if not imp.get("visibility", "").startswith("pub"):
                continue
            if imp.get("cfg"):
                continue
            if imp.get("glob"):
                target = imp.get("target", "")
                if target in index.modules:
                    queue.append(target)
                continue
            resolved = index.resolve_path(imp["path"], module)
            if resolved is None or resolved[0] == "external":
                continue
            if resolved[0] == "module":
                queue.append(resolved[1])
            else:
                surface.add((resolved[1], resolved[2]["name"]))
    return surface


def _check_unreachable_pub(index: Index) -> list[Finding]:
    """R4 (warn): pub items unreachable from their crate root."""
    findings: list[Finding] = []
    roots = sorted({index.root_of(m) for m in index.modules
                    if index.is_lib_module(m)})
    surfaces = {root: _public_surface(index, root) for root in roots}
    for module, mod in sorted(index.modules.items()):
        if not index.is_lib_module(module):
            continue
        if "tests" in module.split("::"):
            continue
        if index.is_conditional(module):
            continue
        root = index.root_of(module)
        for item in mod.get("items", []):
            if item.get("visibility") != "pub" or item.get("cfg"):
                continue
            if (module, item["name"]) not in surfaces[root]:
                findings.append(Finding(
                    "R4", "warn", mod["file"], item["line"], module,
                    f"`{item['name']}` is pub but unreachable from the "
                    f"crate root (private module, no re-export)"))
    return findings


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    strict = "--strict" in args
    args = [a for a in args if a != "--strict"]
    fmt = "text"
    if "--format" in args:
        i = args.index("--format")
        fmt = args[i + 1]
        del args[i:i + 2]
    root = Path(args[0]) if args else RUST_ROOT
    findings = check_workspace(root)
    errors = [f for f in findings if f.level == "error"]
    warns = [f for f in findings if f.level == "warn"]
    if fmt == "json":
        print(json.dumps([f.to_dict() for f in findings], indent=2))
    else:
        for f in findings:
            print(f"{f.file}:{f.line}: {f.level} [{f.rule}] {f.message}")
        print(f"{len(errors)} error(s), {len(warns)} warning(s)")
    if errors or (strict and warns):
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
