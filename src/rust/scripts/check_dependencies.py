#!/usr/bin/env python3
"""Read-only stdlib guard for this deliberately small, local-only workspace DAG."""
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
ALLOWED = {
    "forge-ingress-contract": set(),
    "forge-provider": set(),
    "forge-io-client": set(),
    "forge-task": {("forge-io-client", "dev"), ("forge-provider", "dev")},
    "forge-bridge": {("forge-ingress-contract", "normal"), ("forge-provider", "dev"), ("forge-io-client", "dev"), ("forge-task", "dev")},
}

def main():
    result = subprocess.run(
        ["cargo", "metadata", "--manifest-path", str(ROOT / "Cargo.toml"),
         "--format-version", "1", "--no-deps", "--offline", "--locked"],
        check=True, capture_output=True, text=True,
    )
    metadata = json.loads(result.stdout)
    packages = {p["name"]: p for p in metadata["packages"]}
    if set(packages) != set(ALLOWED):
        raise SystemExit("Workspace membership changed; review the architecture guard explicitly.")
    edges = {name: set() for name in packages}
    for name, package in packages.items():
        for dependency in package["dependencies"]:
            target = dependency["name"]
            kind = dependency["kind"] or "normal"
            if (target, kind) not in ALLOWED[name]:
                raise SystemExit(f"Forbidden dependency: {name} -> {target} ({kind})")
            expected_path = (ROOT / target).resolve()
            if not dependency.get("path") or Path(dependency["path"]).resolve() != expected_path:
                raise SystemExit(f"Dependency is not the intended local package: {name} -> {target}")
            edges[name].add(target)
    visiting, visited = set(), set()
    def visit(name):
        if name in visiting:
            raise SystemExit(f"Dependency cycle at {name}")
        if name in visited:
            return
        visiting.add(name)
        for target in edges[name]:
            visit(target)
        visiting.remove(name)
        visited.add(name)
    for name in packages:
        visit(name)
    print("PASS: five local-only crates; allowed normal/dev edges; acyclic graph")

if __name__ == "__main__":
    main()
