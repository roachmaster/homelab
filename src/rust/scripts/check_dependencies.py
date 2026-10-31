#!/usr/bin/env python3
"""Read-only stdlib check of local edges, DAG and optional consumer profiles."""
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
ALLOWED = {
    "forge-ingress-contract": set(),
    "forge-provider": set(),
    "forge-io-client": set(),
    "forge-task": {("forge-io-client", "dev"), ("forge-provider", "dev")},
    "forge-bridge": {("forge-ingress-contract", "normal"), ("forge-provider", "dev"),
                     ("forge-io-client", "dev"), ("forge-task", "dev")},
    "forge-bootstrap": set(),
    "forge-demo-messages": {(name, "normal") for name in
                            ("forge-bootstrap", "forge-provider", "forge-io-client", "forge-task")},
    "forge-demo-memory-client": {(name, "normal") for name in
                                 ("forge-bootstrap", "forge-provider", "forge-io-client", "forge-demo-messages")},
    "forge-bootstrap-demo": {(name, "normal") for name in
                             ("forge-bootstrap", "forge-bridge", "forge-task", "forge-ingress-contract",
                              "forge-provider", "forge-demo-messages", "forge-demo-memory-client")},
}
DEMO_PACKAGES = {"forge-demo-messages", "forge-demo-memory-client", "forge-bootstrap-demo"}
PURE_PROFILE = {"forge-bootstrap-demo", "forge-bootstrap", "forge-bridge", "forge-task", "forge-ingress-contract"}
OPTIONAL = {"forge-demo-messages", "forge-demo-memory-client", "forge-provider"}


def cargo(*args):
    return subprocess.run(
        ["cargo", *args, "--manifest-path", str(ROOT / "Cargo.toml"), "--offline", "--locked"],
        check=True, capture_output=True, text=True,
    ).stdout


def consumer_tree(*feature_args):
    output = cargo("tree", "-p", "forge-bootstrap-demo", "--edges", "normal", "--prefix", "none",
                   "--format", "{p}", "--no-default-features", *feature_args)
    return {line.split()[0] for line in output.splitlines() if line.strip()}


def main():
    metadata = json.loads(cargo("metadata", "--format-version", "1", "--no-deps"))
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
            expected_path = (ROOT / ("examples" if target in DEMO_PACKAGES else "") / target).resolve()
            if not dependency.get("path") or Path(dependency["path"]).resolve() != expected_path:
                raise SystemExit(f"Dependency is not the intended local package: {name} -> {target}")
            if name == "forge-bootstrap-demo" and target in OPTIONAL and not dependency["optional"]:
                raise SystemExit(f"Demo capability dependency must be optional: {target}")
            edges[name].add(target)
    if packages["forge-bootstrap-demo"]["features"].get("default", []) != []:
        raise SystemExit("Demo default must remain pure; do not enable the optional capability by default.")
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
    pure = consumer_tree()
    enabled = consumer_tree("--features", "messages")
    if pure != PURE_PROFILE:
        raise SystemExit(f"Pure consumer dependency mismatch: {sorted(pure)}")
    if enabled != set(ALLOWED):
        raise SystemExit(f"Enabled consumer dependency mismatch: {sorted(enabled)}")
    print("PASS: six core + three demo packages; local-only allowed edges; acyclic graph")
    print("PASS: pure consumer excludes message/client/provider packages; messages feature includes selected packages")


if __name__ == "__main__":
    main()
