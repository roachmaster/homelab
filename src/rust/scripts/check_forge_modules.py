"""Report each crate's Forge wiring module and what it is missing.

Convention (one directory module per crate, never a single forge.rs):

    <crate>/src/forge/mod.rs      thin: `mod` declarations and re-exports only
    <crate>/src/forge/provider.rs production providers (port implementations)
    <crate>/src/forge/factory.rs  factories: effect-free recipes that create
    <crate>/src/forge/registrar.rs or bindings.rs  registrars / frozen bindings
    <crate>/src/forge/nulls.rs    null-object stand-ins for optional collaborators
    <crate>/src/forge/assisted.rs assisted providers (caller-supplied arguments)
    <crate>/src/forge/decorators.rs  decorators over provided services
    <crate>/src/forge/container.rs or config.rs  per-environment wiring

A crate creates only the files it needs; the report lists which exist and
which do not, and flags adapter-looking code still outside `src/forge`.
Domain traits (ports) and logic stay in their own modules.

Read-only, stdlib only. `python3 scripts/check_forge_modules.py [ROOT]`;
`--strict` exits 1 when an in-scope crate has no `src/forge/mod.rs`, a
`src/forge.rs` file, or a non-thin `mod.rs`.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# Crates with no Forge wiring of their own (framework, contracts, pure code).
EXEMPT = {
    "forge-core": "the Forge contract layer itself",
    "forge-foundation": "framework library: lifecycles and environment wiring",
    "forge-scene": "build-time code generation tooling",
    "forge-ide-ui": "portable UI state and protocol contracts",
    "forge-ide-web": "browser UI (WASM facade)",
    "forge-game-runtime": "pure fixed-step simulation",
    "forge-mcp": "MCP API contract crate",
    "forge-agent-driver": "benchmark fixture",
}

PIECES = [
    ("provider", ("provider.rs",)),
    ("factory", ("factory.rs",)),
    ("registrar", ("registrar.rs", "bindings.rs")),
    ("nulls", ("nulls.rs",)),
    ("assisted", ("assisted.rs",)),
    ("decorators", ("decorators.rs",)),
    ("container", ("container.rs", "config.rs")),
]

STRAY = re.compile(
    r"impl(<[^>]*>)?\s+(forge_core::provider::)?Factory<"
    r"|\bstruct\s+\w+(Factory|Registrar|Container|Collaborators)\b"
    r"|\bstruct\s+No[A-Z]\w*\s*;"
    r"|\bfn\s+production\s*\("
    r"|\.freeze\(\)"
)


def crates(root):
    for path in sorted(root.iterdir()):
        if (path / "Cargo.toml").is_file() and (path / "src").is_dir():
            yield path


def strays(src):
    found = {}
    for path in sorted(src.rglob("*.rs")):
        rel = path.relative_to(src)
        if rel.parts[0] in ("forge", "bin") or "test" in path.stem:
            continue
        count = 0
        for line in path.read_text(encoding="utf-8").splitlines():
            if line.startswith("#[cfg(test)]"):
                break
            if not line.lstrip().startswith("//") and STRAY.search(line):
                count += 1
        if count:
            found[str(rel)] = count
    return found


def report(root):
    gaps = 0
    rows = []
    for crate in crates(root):
        name = crate.name
        if name in EXEMPT:
            rows.append(f"{name}: exempt ({EXEMPT[name]})")
            continue
        src = crate / "src"
        forge = src / "forge"
        problems = []
        if (src / "forge.rs").exists():
            problems.append("uses src/forge.rs; use src/forge/mod.rs")
        if not (forge / "mod.rs").is_file():
            problems.append("missing src/forge/mod.rs")
            have, missing = [], [p for p, _ in PIECES]
        else:
            have = [p for p, files in PIECES if any((forge / f).is_file() for f in files)]
            missing = [p for p, _ in PIECES if p not in have]
            fat = [
                i for i, line in enumerate((forge / "mod.rs").read_text(encoding="utf-8").splitlines(), 1)
                if re.match(r"^\s*(pub(\([^)]*\))?\s+)?(fn|struct|enum|impl|trait|const|static|type)\b", line)
            ]
            if fat:
                problems.append(f"mod.rs not thin (items at lines {fat[:5]})")
        stray = strays(src)
        gaps += bool(problems)
        rows.append(f"{name}: {'GAP' if problems else 'ok'}")
        for problem in problems:
            rows.append(f"  ! {problem}")
        rows.append(f"  has: {', '.join(have) or '-'}")
        rows.append(f"  missing: {', '.join(missing) or '-'}")
        if stray:
            rows.append("  outside src/forge: " + ", ".join(f"{f}({n})" for f, n in stray.items()))
    return rows, gaps


def main(argv):
    args = [a for a in argv if not a.startswith("--")]
    root = Path(args[0]).resolve() if args else ROOT
    rows, gaps = report(root)
    print("\n".join(rows))
    in_scope = sum(1 for c in crates(root) if c.name not in EXEMPT)
    print(f"{in_scope - gaps}/{in_scope} in-scope crates have a forge module; {gaps} gap(s)")
    return 1 if gaps and "--strict" in argv else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
