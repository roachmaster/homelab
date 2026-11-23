#!/usr/bin/env python3
"""Check forge-core's DAG with the existing Rust module graph and tokenizer.

Resolve imports, aliases and re-exports; scan qualified paths in bodies too.
This source guard does not expand macros or replace the compiler. Ambiguous
aliases, root globs and incomplete core source graphs fail closed.
"""
from dataclasses import dataclass, field
from pathlib import Path
import sys

import rust_ast

ROOT = Path(__file__).resolve().parents[1]
CRATE = "forge_core"
LAYERS = {
    "provider": set(), "io_client": set(), "command": set(), "ingress": set(),
    "bootstrap": set(), "task": {"io_client", "provider"},
    "bridge": {"ingress", "io_client", "provider", "task"}, "access": {"ingress"},
}


@dataclass(frozen=True)
class Finding:
    file: str
    line: int
    message: str


@dataclass(frozen=True)
class Target:
    module: str
    is_module: bool = True
    origins: frozenset = frozenset()


@dataclass
class Scope:
    module: str
    parent: object = None
    imports: list = field(default_factory=list)


def owner(module):
    parts = module.split("::")
    return parts[1] if len(parts) > 1 and parts[0] == CRATE else None


def bound_name(imp):
    path = imp["path"]
    return imp.get("alias") or (path[-2] if path[-1] == "self" else path[-1])


class Layers:
    def __init__(self, crate):
        self.crate = Path(crate)
        self.graph = rust_ast.build_graph(self.crate)
        self.modules = {name: mod for name, mod in self.graph["modules"].items()
                        if mod["root"] == CRATE}
        self.findings = set()
        self.scans = []
        self.self_aliases = set()

    def report(self, file, line, message):
        self.findings.add(Finding(file, line, message))

    def target(self, module):
        layer = owner(module)
        return Target(module, True, frozenset((layer,)) if layer else frozenset())

    def imports(self, module, scope):
        groups = []
        while scope is not None and scope.module == module:
            groups.append(scope.imports)
            scope = scope.parent
        groups.append([imp for imp in self.modules.get(module, {}).get("imports", [])
                       if not imp.get("local")])
        return groups

    def name(self, module, name, scope, seen):
        key = (module, name, id(scope))
        if key in seen:
            raise ValueError("cyclic or ambiguous import alias: " + name)
        seen = seen | {key}
        groups = self.imports(module, scope)
        for group in groups:
            named = [imp for imp in group if not imp.get("glob")
                     and bound_name(imp) == name]
            if named:
                targets = {self.resolve(imp["path"], module, scope, seen) for imp in named}
                if len(targets) != 1:
                    raise ValueError("ambiguous import alias: " + name)
                return next(iter(targets))
        child = module + "::" + name
        if child in self.modules:
            return self.target(child)
        if name in self.self_aliases:
            return self.target(CRATE)
        if any(item["name"] == name for item in self.modules.get(module, {}).get("items", [])):
            target = self.target(module)
            return Target(module, False, target.origins)
        if name in rust_ast.STD_ROOTS:
            return None  # external roots are not names imported by their own glob
        found = set()
        for group in groups:
            for imp in group:
                if not imp.get("glob"):
                    continue
                target = self.resolve(imp["path"], module, scope, seen)
                if target is not None and target.is_module:
                    item = self.name(target.module, name, None, seen)
                    if item is not None:
                        found.add(Target(item.module, item.is_module, item.origins | target.origins))
        if len(found) > 1:
            raise ValueError("ambiguous glob import: " + name)
        return next(iter(found), None)

    def resolve(self, path, module, scope=None, seen=frozenset()):
        if not path:
            return None
        head, *rest = path
        if head in ("crate", CRATE, ""):
            target = self.target(CRATE)
        elif head == "self":
            target = self.target(module)
        elif head == "super":
            parts = module.split("::")
            while head == "super":
                if len(parts) == 1:
                    raise ValueError("super path escapes the core crate")
                parts.pop()
                if not rest or rest[0] != "super":
                    break
                head, *rest = rest
            target = self.target("::".join(parts))
        else:
            target = self.name(module, head, scope, seen)
        for segment in rest:
            if target is None or not target.is_module:
                break  # retain known module provenance across associated items
            if segment in ("self", "*"):
                continue
            found = self.name(target.module, segment, None, seen)
            if found is None:
                return Target(target.module, False, target.origins)
            target = Target(found.module, found.is_module, target.origins | found.origins)
        return target

    def edge(self, module, path, scope, file, line, glob=False):
        source = owner(module)
        if source not in LAYERS:
            return
        try:
            target = self.resolve(path, module, scope)
            if target is None:
                return  # std/external name or a local type parameter
            if glob and target.module == CRATE:
                raise ValueError("root glob hides layer dependencies; import named core modules")
            for destination in sorted(target.origins - {source}):
                if destination not in LAYERS[source]:
                    self.report(file, line, f"forbidden core layer edge: {source} -> {destination}")
        except ValueError as error:
            self.report(file, line, str(error))

    def scan(self, file, base):
        tokens = rust_ast.tokenize((self.crate / file).read_text())
        scopes, stack, uses = [], [], []
        active = Scope(base)
        pending_modules, skip = {}, set()
        for i, token in enumerate(tokens):
            scopes.append(active)
            if i in skip:
                continue
            if token.text == "use":
                end = i + 1
                while end < len(tokens) and tokens[end].text != ";":
                    end += 1
                parsed = rust_ast.parse_source(" ".join(t.text for t in tokens[i:end + 1]), file)
                if parsed.notes:
                    self.report(file, token.line, "cannot analyze use statement")
                for imp in parsed.imports:
                    value = imp.to_dict()
                    active.imports.append(value)
                    uses.append((active, value, token.line))
                skip.update(range(i, end + 1))
            elif ([t.text for t in tokens[i:i + 4]] == ["extern", "crate", "self", "as"]
                  and i + 4 < len(tokens)):
                alias = tokens[i + 4].text
                active.imports.append({"path": ["crate"], "alias": alias})
                if active.module == CRATE and active.parent is None:
                    self.self_aliases.add(alias)
            elif token.text == "include" and i + 1 < len(tokens) and tokens[i + 1].text == "!":
                self.report(file, token.line, "include! source has no module owner; declare a module instead")
            elif (token.text == "mod" and i + 2 < len(tokens)
                  and tokens[i + 1].kind == "IDENT" and tokens[i + 2].text == "{"):
                module = active.module + "::" + tokens[i + 1].text
                if module not in self.modules:
                    self.report(file, token.line, "inline module missing from source graph: " + module)
                pending_modules[i + 2] = module
            elif token.text == "{":
                stack.append(active)
                active = Scope(pending_modules.get(i, active.module), active)
            elif token.text == "}" and stack:
                active = stack.pop()
        self.scans.append((file, tokens, scopes, skip, uses))

    def analyze(self, file, tokens, scopes, skip, uses):
        # Rust uses bind throughout their block, including before declaration.
        for scope, imp, line in uses:
            self.edge(scope.module, imp["path"], scope, file, line, imp.get("glob", False))
        for i, token in enumerate(tokens):
            if (i in skip or token.kind != "IDENT"
                    or (i >= 2 and tokens[i - 1].text == "::" and tokens[i - 2].kind == "IDENT")):
                continue
            if i + 1 >= len(tokens) or tokens[i + 1].text != "::":
                continue
            path, cursor = [token.text], i + 1
            while (cursor + 1 < len(tokens) and tokens[cursor].text == "::"
                   and tokens[cursor + 1].kind == "IDENT"):
                path.append(tokens[cursor + 1].text)
                cursor += 2
            self.edge(scopes[i].module, path, scopes[i], file, token.line)

    def check(self):
        actual = {decl["name"] for decl in self.modules.get(CRATE, {}).get("mods", [])}
        if actual != set(LAYERS):
            self.report("src/lib.rs", 1, "core layer inventory changed; review LAYERS explicitly")
        files = {mod["file"] for mod in self.modules.values()}
        for problem in self.graph["errors"]:
            if problem["file"] in files:
                self.report(problem["file"], problem["line"], "incomplete Rust source graph: " + problem["message"])
        for problem in self.graph["missing_modules"]:
            if problem["module"] in self.modules:
                self.report(problem["file"], problem["line"], "missing core module: " + problem["name"])
        for file in self.graph["orphans"]:
            if file.startswith("src/"):
                self.report(file, 1, "core source file is outside the module graph")
        for module, mod in self.modules.items():
            declarations = [decl["name"] for decl in mod.get("mods", [])]
            if len(declarations) != len(set(declarations)):
                self.report(mod["file"], 1, "ambiguous repeated module declaration: " + module)
            if owner(module) not in LAYERS and module != CRATE:
                self.report(mod["file"], 1, "unknown core layer: " + module)
                continue
            parent = module.rpartition("::")[0]
            if parent in self.modules and self.modules[parent]["file"] == mod["file"]:
                continue  # inline modules get their scope during their file scan
            self.scan(mod["file"], module)
        for scan in self.scans:
            self.analyze(*scan)
        return sorted(self.findings, key=lambda f: (f.file, f.line, f.message))


def check_core_layers(crate):
    return Layers(crate).check()


def main(argv=None):
    args = sys.argv[1:] if argv is None else argv
    root = Path(args[0]) if args else ROOT
    crate = root if root.name == "forge-core" else root / "forge-core"
    findings = check_core_layers(crate)
    for finding in findings:
        print(f"{crate / finding.file}:{finding.line}: error: {finding.message}")
    print(f"Core layer check: {len(findings)} error(s)")
    return bool(findings)


if __name__ == "__main__":
    sys.exit(main())
