#!/usr/bin/env python3
"""Small Make helpers: safe argv parsing and exactly-one-target assembly output."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "Cargo.toml"


def boolean(name):
    value = os.environ.get(name, "0")
    if value not in ("0", "1"):
        raise ValueError(f"{name} must be 0 or 1")
    return value == "1"


def feature_args():
    all_features = boolean("ALL_FEATURES")
    no_default = boolean("NO_DEFAULT_FEATURES")
    features = os.environ.get("FEATURES", "")
    if all_features and (no_default or features):
        raise ValueError("ALL_FEATURES is exclusive with FEATURES/NO_DEFAULT_FEATURES")
    args = ["--all-features"] if all_features else []
    if no_default:
        args.append("--no-default-features")
    if features:
        args.extend(["--features", features])
    return args


def cargo(command, *args):
    return ["cargo", command, "--manifest-path", str(MANIFEST),
            "--offline", "--locked", *args]


def execute(command, *, diagnostic_stream=None):
    print("+ " + shlex.join(command), flush=True, file=diagnostic_stream)
    subprocess.run(command, check=True)


def run(session):
    # shlex tokenization only: no shell evaluation, expansion, pipes or substitution.
    args = shlex.split(os.environ.get("ARGS", "--help"))
    if session:
        # ARGS contains startup options here; the terminal validates their grammar.
        args.append("--session")
    execute(cargo("run", "--package", "forge-terminal", "--bin", "forge",
                  *feature_args(), "--", *args))


def template():
    # Fixed opt-in binary, never a shell-evaluated command or library default.
    args = shlex.split(os.environ.get("ARGS", "--help"))
    execute(cargo("run", "--package", "forge-template", "--bin", "forge-template",
                  "--no-default-features", "--features", "cli", "--", *args),
            diagnostic_stream=sys.stderr)


def assembly():
    package = os.environ.get("PACKAGE") or "forge-kernel"
    selection = os.environ.get("ASM_TARGET", "lib")
    features = feature_args()
    native = boolean("NATIVE")
    metadata = json.loads(subprocess.check_output(
        cargo("metadata", "--no-deps", "--format-version", "1"), text=True))
    packages = [p for p in metadata["packages"] if p["name"] == package]
    if len(packages) != 1:
        raise ValueError(f"PACKAGE must name one workspace package, got {package!r}")
    if selection == "lib":
        targets = [t for t in packages[0]["targets"] if "lib" in t["kind"]]
        target_args = ["--lib"]
    elif selection.startswith("bin:") and selection[4:]:
        name = selection[4:]
        targets = [t for t in packages[0]["targets"]
                   if "bin" in t["kind"] and t["name"] == name]
        target_args = ["--bin", name]
    else:
        raise ValueError("ASM_TARGET must be lib or bin:<exact binary name>")
    if len(targets) != 1:
        raise ValueError(f"{package} has no unique {selection!r} target")
    # A fresh ignored invocation directory prevents stale or other-feature assembly
    # being reported as this run's result. No cleaning/deleting of existing outputs.
    base = ROOT / "target" / "asm"
    base.mkdir(parents=True, exist_ok=True)
    output_dir = Path(tempfile.mkdtemp(prefix=f"{package}-{targets[0]['name']}-", dir=base))
    output = output_dir / f"{targets[0]['name']}.s"
    codegen = [f"--emit=asm={output}", "-C", "codegen-units=1"]
    if native:
        codegen.extend(["-C", "target-cpu=native"])
    execute(cargo("rustc", "--release", "--package", package, *target_args,
                  *features, "--target-dir", str(output_dir / "build"), "--", *codegen))
    if not output.is_file() or output.stat().st_size == 0:
        raise ValueError("rustc finished but the selected assembly output is absent/empty")
    print(f"Assembly: {output}")
    print("Selected target only; generic functions may be emitted only by a consuming crate.")


def main():
    if len(sys.argv) != 2 or sys.argv[1] not in ("run", "session", "template", "asm"):
        raise ValueError("expected run, session, template or asm")
    if sys.argv[1] == "asm":
        assembly()
    elif sys.argv[1] == "template":
        template()
    else:
        run(sys.argv[1] == "session")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        sys.exit(2)
    except subprocess.CalledProcessError as error:
        sys.exit(error.returncode if error.returncode > 0 else 1)
