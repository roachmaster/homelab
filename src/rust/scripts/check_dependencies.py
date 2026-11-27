#!/usr/bin/env python3
"""Read-only stdlib check of local edges, DAG and optional consumer profiles."""
import copy
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
ALLOWED = {
    "forge-jenkins": {("forge-core", "normal")},
    "forge-ide-ui": set(),
    "forge-ide-web": {(name,"normal") for name in ("forge-ide-ui","forge-core","forge-template","forge-game-runtime")},
    "forge-ide-web-host": {(name,"normal") for name in ("forge-core","forge-ide","forge-ide-ui","forge-remote","forge-remote-https")} | {("forge-ide-ui", "build")},
    "forge-game-runtime": set(),
    "forge-jobs": {("forge-core", "normal")},
    "forge-core": set(),
    "forge-agent": {(name,"normal") for name in ("forge-core","forge-system","forge-ide","forge-remote","forge-remote-https")},
    "forge-remote": {("forge-core", "normal")},
    "forge-remote-https": {(name, "normal") for name in ("forge-remote", "forge-core", "forge-ide")} | {("forge-system", "dev"), ("forge-core", "dev")},
    "forge-ide": {(name, "normal") for name in ("forge-core", "forge-remote")},
    "forge-template": {("forge-core", "dev"), ("forge-core", "normal")},
    "forge-system": {("forge-core", "normal")},
    "forge-local": {("forge-jenkins", "normal")} | {(name, "normal") for name in
                    ("forge-core", "forge-system")},
    "forge-terminal": {("forge-jenkins", "normal")} | {(name, "normal") for name in
                       ("forge-local", "forge-core", "forge-jobs")},
    "forge-mcp": set(),
    "forge-mcp-host": {("forge-mcp", "normal")},
    "forge-postgres": {("forge-core", "normal"), ("forge-work", "normal"), ("forge-work-worker", "normal")},
    "forge-work": {("forge-core", "normal")},
    "forge-work-worker": {("forge-core", "normal")},
}
EXTERNAL_DEV = {"forge-ide-ui":{"serde_json"},"forge-ide-web-host":{"rcgen"},"forge-agent": {"rcgen", "nix", "rustls"},"forge-remote-https": {"rcgen"}}
EXTERNAL_BUILD = {"forge-ide-web-host": {"serde", "serde_json", "sha2"}}
EXTERNAL = {
    "forge-jenkins": {"serde", "serde_json", "url", "base64", "ureq"},
    "forge-ide-ui": {"serde"},
    "forge-terminal": {"ctrlc", "nix", "serde", "serde_json"},
    "forge-jobs": {"cap-std", "serde", "serde_json"},
    "forge-ide-web": {"serde", "serde_json", "serde_yaml_ng", "sha2"},
    "forge-ide-web-host": {"serde", "serde_json", "httparse", "getrandom", "subtle", "sha2", "ctrlc",
                           "hyper", "hyper-util", "http-body-util", "tokio"},
    "forge-agent": {"serde", "serde_json", "ctrlc"},
    "forge-remote-https": {"hyper", "hyper-util", "http-body-util", "tokio", "tokio-rustls", "rustls",
                           "serde", "serde_json"},
    "forge-ide": {"cap-std", "cap-fs-ext", "syn", "proc-macro2", "quote", "serde", "serde_yaml_ng", "sha2",
                  "nix", "serde_json"},
    "forge-mcp": {"serde", "serde_json"},
    "forge-mcp-host": {"tokio", "rmcp", "serde", "serde_json", "reqwest", "tokio-postgres", "uuid", "axum"},
    "forge-postgres": {"chrono", "nix", "serde_json", "sha2", "tokio", "tokio-postgres", "uuid"},
    "forge-work": {"serde_json", "sha2", "uuid"},
    "forge-work-worker": {"serde_json", "sha2", "uuid", "nix"},
}

WASM_PLAYTEST_PROFILE = {
    "forge-game-runtime", "sha2", "cfg-if", "digest", "block-buffer", "generic-array",
    "typenum", "crypto-common",
}


def cargo(*args):
    return subprocess.run(
        ["cargo", *args, "--manifest-path", str(ROOT / "Cargo.toml"), "--offline", "--locked"],
        check=True, capture_output=True, text=True,
    ).stdout


def consumer_tree(*feature_args, package):
    output = cargo("tree", "-p", package, "--edges", "normal", "--prefix", "none",
                   "--format", "{p}", "--no-default-features", *feature_args)
    return {line.split()[0] for line in output.splitlines() if line.strip()}


def validate_packages(packages):
    if set(packages) != set(ALLOWED):
        raise SystemExit("Workspace membership changed; review the architecture guard explicitly.")
    edges = {name: set() for name in packages}
    external = {"normal": EXTERNAL, "dev": EXTERNAL_DEV, "build": EXTERNAL_BUILD}
    for name, package in packages.items():
        for dependency in package["dependencies"]:
            target = dependency["name"]
            kind = dependency["kind"] or "normal"
            if target in external.get(kind, {}).get(name, set()):
                if dependency.get("path") or not (dependency.get("source") or "").startswith("registry+"):
                    raise SystemExit(f"Unexpected external dependency origin: {target}")
                continue
            if (target, kind) not in ALLOWED[name]:
                raise SystemExit(f"Forbidden dependency: {name} -> {target} ({kind})")
            expected_path = (ROOT / target).resolve()
            if not dependency.get("path") or Path(dependency["path"]).resolve() != expected_path:
                raise SystemExit(f"Dependency is not the intended local package: {name} -> {target}")
            edges[name].add(target)
    jenkins = packages["forge-jenkins"]
    if jenkins["features"].get("default", []) != []:
        raise SystemExit("Jenkins native HTTP must remain opt-in")
    if not next(d for d in jenkins["dependencies"] if d["name"] == "ureq")["optional"]:
        raise SystemExit("Jenkins HTTP transport must remain optional")
    for name in ("forge-local", "forge-terminal"):
        selected = packages[name]
        if "jenkins" in selected["features"].get("default", []):
            raise SystemExit("Jenkins must not expand existing CLI defaults")
        if not next(d for d in selected["dependencies"] if d["name"] == "forge-jenkins")["optional"]:
            raise SystemExit("Jenkins capability dependency must remain optional")
    web = packages["forge-ide-web"]
    if web["features"].get("default", []) != []:
        raise SystemExit("WASM facade must not enable native generation by default")
    for dep in web["dependencies"]:
        if dep["name"] == "sha2" and dep["target"] == 'cfg(target_arch = "wasm32")':
            continue  # Playtest runtime verifies cooked bytes on this target.
        if dep["name"] in {"forge-core","forge-template","serde_yaml_ng","sha2"} and not dep["optional"]:
            raise SystemExit("Asset build dependency leaked into WASM default")
        if dep["name"] == "forge-game-runtime" and dep["target"] != 'cfg(target_arch = "wasm32")':
            raise SystemExit("Game runtime must remain a WASM-only dependency of forge-ide-web")
    ide = packages["forge-ide"]
    if ide["features"].get("default", []) != []:
        raise SystemExit("IDE YAML/CLI/remote must remain opt-in")
    for dep in ide["dependencies"]:
        # Bridge/command/ingress were optional deps here before the merge;
        # they are forge-core modules now, always compiled, and their use is
        # gated by the cli/remote features in code instead.
        if dep["name"] in {"serde", "serde_yaml_ng", "forge-remote", "nix", "serde_json"} and not dep["optional"]:
            raise SystemExit("IDE integration dependencies must remain optional")
    https = packages["forge-remote-https"]
    if https["features"].get("default", []) != []:
        raise SystemExit("HTTPS IDE codec must remain opt-in")
    if not next(d for d in https["dependencies"] if d["name"] == "forge-ide")["optional"]:
        raise SystemExit("HTTPS default must not require IDE")
    ide_bins = [t for t in ide["targets"] if "bin" in t["kind"]]
    if len(ide_bins) != 1 or ide_bins[0].get("required-features") != ["cli"]:
        raise SystemExit("IDE binary must require cli")
    template = packages["forge-template"]
    if template["features"].get("default", []) != []:
        raise SystemExit("Template CLI must not be enabled by default")
    if template["features"].get("cli", []) != []:
        raise SystemExit("Template CLI must enable no dependency; its contracts are forge-core modules")
    for dep in template["dependencies"]:
        if (dep["kind"] or "normal") == "normal" and dep["name"] not in {"forge-core"} and not dep["optional"]:
            raise SystemExit("Template CLI contracts must remain optional")
    binaries = [t for t in template["targets"] if "bin" in t["kind"]]
    if len(binaries) != 1 or binaries[0].get("required-features") != ["cli"]:
        raise SystemExit("Template binary must require its explicit cli feature")
    if packages["forge-local"]["features"].get("jobs") != []:
        raise SystemExit("Jobs must remain an optional application module.")
    if packages["forge-terminal"]["features"].get("jobs") != ["forge-local/jobs"]:
        raise SystemExit("Terminal jobs must explicitly forward the application feature.")
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
    bad["forge-core"]["dependencies"].append({"name": "forge-system", "kind": None,
        "path": str(ROOT / "forge-system"), "optional": False})
    fixtures.append(("core-to-implementation edge", bad))
    bad = copy.deepcopy(packages)
    bad["forge-terminal"]["dependencies"][0]["path"] = str(ROOT / "wrong-package")
    fixtures.append(("wrong dependency origin", bad))
    bad = copy.deepcopy(packages)
    bad["unreviewed-package"] = {}
    fixtures.append(("unreviewed membership", bad))
    bad = copy.deepcopy(packages)
    bad["forge-terminal"]["features"]["jobs"] = ["forge-system/disk"]
    fixtures.append(("incorrect jobs forwarding", bad))
    bad = copy.deepcopy(packages)
    bad["forge-template"]["dependencies"].append({"name": "forge-system", "kind": None,
        "path": str(ROOT / "forge-system"), "optional": False})
    fixtures.append(("renderer runtime dependency", bad))
    bad = copy.deepcopy(packages)
    bad["forge-template"]["features"]["default"] = ["cli"]
    fixtures.append(("template CLI enabled by default", bad))
    bad = copy.deepcopy(packages)
    bad["forge-template"]["features"]["cli"] = ["dep:serde_json"]
    fixtures.append(("CLI feature enabling a dependency", bad))
    bad = copy.deepcopy(packages)
    bad["forge-ide"]["features"]["default"] = ["cli"]
    fixtures.append(("IDE CLI enabled by default", bad))
    bad = copy.deepcopy(packages)
    next(d for d in bad["forge-ide"]["dependencies"] if d["name"] == "serde_yaml_ng")["optional"] = False
    fixtures.append(("mandatory YAML dependency", bad))
    bad = copy.deepcopy(packages)
    bad["forge-core"]["dependencies"].append({"name": "syn", "kind": None, "source": "registry+https://github.com/rust-lang/crates.io-index", "optional": False})
    fixtures.append(("parser in core", bad))
    for source, target in [("forge-remote", "serde_json"), ("forge-core", "rustls"), ("forge-local", "hyper")]:
        bad = copy.deepcopy(packages)
        bad[source]["dependencies"].append({"name": target, "kind": None, "source": "registry+https://github.com/rust-lang/crates.io-index", "optional": False})
        fixtures.append((f"network/codec leakage into {source}", bad))
    for package, dependency in [("forge-ide", "forge-remote"), ("forge-remote-https", "forge-ide")]:
        bad = copy.deepcopy(packages)
        next(d for d in bad[package]["dependencies"] if d["name"] == dependency)["optional"] = False
        fixtures.append(("mandatory optional capability edge", bad))
    bad = copy.deepcopy(packages)
    bad["forge-ide"]["dependencies"].append({"name":"forge-agent","kind":None,"path":str(ROOT/"forge-agent"),"optional":True})
    fixtures.append(("capability-to-host cycle",bad))
    for source,target in [("forge-ide-ui","forge-ide"),("forge-ide-web","forge-ide-web-host")]:
        bad=copy.deepcopy(packages)
        bad[source]["dependencies"].append({"name":target,"kind":None,"path":str(ROOT/target),"optional":False})
        fixtures.append(("native dependency in portable UI",bad))
    bad=copy.deepcopy(packages)
    next(d for d in bad["forge-ide-web"]["dependencies"] if d["name"]=="forge-template")["optional"]=False
    fixtures.append(("generator in WASM facade",bad))
    bad = copy.deepcopy(packages)
    next(d for d in bad["forge-ide-web"]["dependencies"] if d["name"] == "forge-game-runtime")["target"] = None
    fixtures.append(("game runtime in native UI tooling", bad))
    bad = copy.deepcopy(packages)
    next(d for d in bad["forge-ide-web"]["dependencies"] if d["name"] == "sha2" and not d["optional"])["target"] = None
    fixtures.append(("cooked asset digest in default native UI tooling", bad))
    for label, fixture in fixtures:
        try:
            validate_packages(fixture)
        except SystemExit:
            continue
        raise SystemExit(f"Negative fixture unexpectedly accepted: {label}")
    print(f"PASS: {len(fixtures)} in-memory negative architecture fixtures rejected")


def main():
    metadata = json.loads(cargo("metadata", "--format-version", "1", "--no-deps"))
    packages = {p["name"]: p for p in metadata["packages"]}
    validate_packages(packages)
    negative_fixtures(packages)
    if consumer_tree(package="forge-template") != {"forge-template", "forge-core"}:
        raise SystemExit("Renderer public assembly must depend only on the forge-core contracts")
    if consumer_tree("--features", "cli", package="forge-template") != {"forge-template", "forge-core"}:
        raise SystemExit("Template CLI must add no crate beyond the core contracts")
    ide_default = consumer_tree(package="forge-ide")
    ide_cli = consumer_tree("--features", "cli", package="forge-ide")
    if {"serde", "serde_yaml_ng"} & ide_default:
        raise SystemExit("IDE default library includes presentation")
    if not {"syn", "cap-std", "cap-fs-ext"} <= ide_default:
        raise SystemExit("IDE lacks real parser/rooted native boundary")
    if not {"serde_yaml_ng"} <= ide_cli:
        raise SystemExit("IDE CLI lacks required YAML contract")
    if {name for name in ide_default if name.startswith("forge-")} != {"forge-ide", "forge-core"}:
        raise SystemExit("IDE default has unexpected Forge dependencies")
    remote = consumer_tree(package="forge-remote")
    if remote != {"forge-remote", "forge-core"}:
        raise SystemExit("Remote conversation port includes a transport/codec or implementation")
    https = consumer_tree(package="forge-remote-https")
    if not {"rustls", "hyper", "tokio", "serde_json"} <= https or "rcgen" in https or {"forge-system", "forge-ide", "forge-agent"} & https:
        raise SystemExit("HTTPS normal profile must own TLS/HTTP only; identity issuance/system clients stay dev-only")
    ide_remote = consumer_tree("--features", "remote", package="forge-ide")
    if not {"forge-remote"} <= ide_remote or {"serde_json", "rustls", "hyper", "forge-agent", "serde_yaml_ng"} & ide_remote:
        raise SystemExit("IDE remote contract must remain transport and presentation free")
    if not {"forge-ide", "forge-remote-https", "ctrlc"} <= consumer_tree(package="forge-agent"):
        raise SystemExit("Agent must explicitly select its capabilities")
    portable=consumer_tree(package="forge-ide-ui")
    web_native = consumer_tree(package="forge-ide-web")
    wasm = consumer_tree("--target", "wasm32-unknown-unknown", package="forge-ide-web")
    if {"forge-game-runtime", "sha2"} & web_native or wasm != web_native | WASM_PLAYTEST_PROFILE:
        raise SystemExit("Only the playtest runtime and digest verifier may expand the default WASM profile")
    if any(name.startswith("forge-") and name not in {"forge-ide-ui","forge-ide-web"} for name in portable | web_native) or {"cap-std","tokio","rustls","serde_yaml_ng"} & (portable | wasm):
        raise SystemExit("Portable UI/WASM includes native/parser/generator dependencies")
    local_expected = (set(ALLOWED) - {"forge-jenkins", "forge-template", "forge-ide", "forge-remote", "forge-remote-https", "forge-agent", "forge-ide-ui", "forge-ide-web", "forge-ide-web-host", "forge-game-runtime", "forge-jobs", "forge-mcp", "forge-mcp-host", "forge-postgres", "forge-work", "forge-work-worker"})
    for args in [(), ("--features", "disk"), ("--features", "jobs")]:
        if consumer_tree(*args, package="forge-terminal") != local_expected:
            raise SystemExit("Local terminal consumer has unexpected package dependencies")
    if packages["forge-terminal"]["features"].get("durable-jobs") != [
        "dep:forge-jobs", "dep:ctrlc", "dep:nix"
    ]:
        raise SystemExit("Durable jobs must remain a separate explicit terminal feature")
    for selection in [("--features", "durable-jobs"), ("--all-features",)]:
        tree = consumer_tree(*selection, package="forge-terminal")
        expected_forge = local_expected | {"forge-jobs"}
        if selection == ("--all-features",):
            expected_forge |= {"forge-jenkins"}
        if {name for name in tree if name.startswith("forge-")} != expected_forge:
            raise SystemExit("Durable jobs terminal consumer has unexpected Forge dependencies")
        if not {"ctrlc", "nix"} <= tree:
            raise SystemExit("Durable terminal must select native operator and worker shutdown adapters")
    if "ureq" in consumer_tree(package="forge-jenkins"):
        raise SystemExit("Jenkins default library unexpectedly requires native HTTP")
    jenkins_tree = consumer_tree("--features", "jenkins", package="forge-terminal")
    if not {"forge-jenkins", "ureq"} <= jenkins_tree:
        raise SystemExit("Selected Jenkins terminal must include its chosen HTTP adapter")
    if {name for name in jenkins_tree if name.startswith("forge-")} != local_expected | {"forge-jenkins"}:
        raise SystemExit("Jenkins terminal changed unrelated Forge dependency edges")
    features = cargo("tree", "-p", "forge-terminal", "--no-default-features", "--edges", "features", "--prefix", "none")
    if 'forge-system feature "disk"' in features or 'forge-system feature "default"' in features:
        raise SystemExit("Node-only terminal unexpectedly includes native disk module")
    if 'forge-system feature "node"' not in features:
        raise SystemExit("Node-only terminal must select its explicit node capability")
    # Resolved features, not just edges: forwarded root features may lack a
    # printed feature edge in cargo tree's --edges features representation.
    for selection, expected in [((), set()), (("--features", "jobs"), {"jobs"}),
                                (("--features", "disk"), {"disk"})]:
        tree = cargo("tree", "-p", "forge-terminal", "--no-default-features", *selection,
                     "--edges", "normal", "--prefix", "none", "--format", "{p}|{f}")
        local = next(line for line in tree.splitlines() if line.startswith("forge-local "))
        actual = set(filter(None, local.partition("|")[2].split(",")))
        if actual != expected:
            raise SystemExit(f"Application feature mismatch: {actual} != {expected}")
    print("PASS: remote typed port is transport-free; HTTPS is absent from existing local/IDE/template consumers")
    print("PASS: optional IDE parser/rooted native client isolated; YAML/CLI absent from library default")
    print("PASS: one contract core + optional system clients + local application/terminal; local allowed DAG")
    print("PASS: renderer default exposes only forge-core assembly; opt-in CLI adds no crate; terminal omits it")
    print("PASS: node-only terminal omits disk/jobs; jobs-only omits disk")


if __name__ == "__main__":
    main()
