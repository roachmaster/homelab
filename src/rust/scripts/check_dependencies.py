#!/usr/bin/env python3
"""Read-only stdlib check of local edges, DAG and optional consumer profiles."""
import copy
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
ALLOWED = {
    "forge-command": set(),
    "forge-ingress-contract": set(),
    "forge-provider": set(),
    "forge-io-client": set(),
    "forge-task": {("forge-io-client", "dev"), ("forge-provider", "dev")},
    "forge-bridge": {("forge-ingress-contract", "normal"), ("forge-provider", "dev"),
                     ("forge-io-client", "dev"), ("forge-task", "dev")},
    "forge-bootstrap": set(),
    "forge-kernel": {(name, "normal") for name in
                     ("forge-bootstrap", "forge-provider", "forge-io-client", "forge-task", "forge-command")},
    "forge-local": {(name, "normal") for name in
                    ("forge-bootstrap", "forge-bridge", "forge-ingress-contract", "forge-provider",
                     "forge-io-client", "forge-task", "forge-kernel", "forge-command")},
    "forge-terminal": {("forge-local", "normal"), ("forge-io-client", "normal"), ("forge-bootstrap", "dev")},
    "forge-demo-messages": {(name, "normal") for name in
                            ("forge-bootstrap", "forge-provider", "forge-io-client", "forge-task")},
    "forge-demo-memory-client": {(name, "normal") for name in
                                 ("forge-bootstrap", "forge-provider", "forge-io-client", "forge-demo-messages")},
    "forge-bootstrap-demo": {(name, "normal") for name in
                             ("forge-bootstrap", "forge-bridge", "forge-task", "forge-ingress-contract",
                              "forge-provider", "forge-io-client", "forge-kernel", "forge-demo-messages", "forge-demo-memory-client")},
}
DEMO_PACKAGES = {"forge-demo-messages", "forge-demo-memory-client", "forge-bootstrap-demo"}
PURE_PROFILE = {"forge-bootstrap-demo", "forge-bootstrap", "forge-bridge", "forge-task", "forge-ingress-contract"}
OPTIONAL = {"forge-demo-messages", "forge-demo-memory-client", "forge-provider", "forge-kernel", "forge-io-client"}
MESSAGES_PROFILE = PURE_PROFILE | {"forge-demo-messages", "forge-demo-memory-client", "forge-provider", "forge-io-client"}
DISK_PROFILE = PURE_PROFILE | {"forge-kernel", "forge-provider", "forge-io-client"}


def cargo(*args):
    return subprocess.run(
        ["cargo", *args, "--manifest-path", str(ROOT / "Cargo.toml"), "--offline", "--locked"],
        check=True, capture_output=True, text=True,
    ).stdout


def consumer_tree(*feature_args, package="forge-bootstrap-demo"):
    output = cargo("tree", "-p", package, "--edges", "normal", "--prefix", "none",
                   "--format", "{p}", "--no-default-features", *feature_args)
    return {line.split()[0] for line in output.splitlines() if line.strip()}


def validate_packages(packages):
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


def negative_fixtures(packages):
    fixtures = []
    bad = copy.deepcopy(packages)
    bad["forge-task"]["dependencies"].append({"name": "forge-kernel", "kind": None,
        "path": str(ROOT / "forge-kernel"), "optional": False})
    fixtures.append(("core-to-implementation edge", bad))
    bad = copy.deepcopy(packages)
    bad["forge-bootstrap-demo"]["features"]["default"] = ["disk"]
    fixtures.append(("impure default", bad))
    bad = copy.deepcopy(packages)
    bad["forge-terminal"]["dependencies"][0]["path"] = str(ROOT / "wrong-package")
    fixtures.append(("wrong dependency origin", bad))
    bad = copy.deepcopy(packages)
    bad["unreviewed-package"] = {}
    fixtures.append(("unreviewed membership", bad))
    for label, fixture in fixtures:
        try:
            validate_packages(fixture)
        except SystemExit:
            continue
        raise SystemExit(f"Negative fixture unexpectedly accepted: {label}")
    print("PASS: four in-memory negative architecture fixtures rejected")


def main():
    metadata = json.loads(cargo("metadata", "--format-version", "1", "--no-deps"))
    packages = {p["name"]: p for p in metadata["packages"]}
    validate_packages(packages)
    negative_fixtures(packages)
    pure = consumer_tree()
    messages = consumer_tree("--features", "messages")
    disk = consumer_tree("--features", "disk")
    combined = consumer_tree("--all-features")
    if pure != PURE_PROFILE:
        raise SystemExit(f"Pure consumer dependency mismatch: {sorted(pure)}")
    for name, actual, expected in [("messages", messages, MESSAGES_PROFILE),
                                   ("disk", disk, DISK_PROFILE),
                                   ("combined", combined, MESSAGES_PROFILE | DISK_PROFILE)]:
        if actual != expected:
            raise SystemExit(f"{name} consumer dependency mismatch: {sorted(actual)}")
    local_expected = (set(ALLOWED) - DEMO_PACKAGES)
    for args in [(), ("--features", "disk")]:
        if consumer_tree(*args, package="forge-terminal") != local_expected:
            raise SystemExit("Local terminal consumer has unexpected package dependencies")
    features = cargo("tree", "-p", "forge-terminal", "--no-default-features", "--edges", "features", "--prefix", "none")
    if 'forge-kernel feature "disk"' in features or 'forge-kernel feature "default"' in features:
        raise SystemExit("Node-only terminal unexpectedly includes native disk module")
    if 'forge-kernel feature "node"' not in features:
        raise SystemExit("Node-only terminal must select its explicit node capability")
    print("PASS: six execution core + independent command description + optional kernel + local application/terminal + three demos; local allowed DAG")
    print("PASS: node-only terminal omits disk Cargo feature and all demonstration implementations")
    print("PASS: pure/messages/disk/combined profiles include only selected implementation packages")


if __name__ == "__main__":
    main()
