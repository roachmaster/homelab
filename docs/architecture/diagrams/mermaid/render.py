#!/usr/bin/env python3
"""Render Mermaid locally with pinned mmdr; --check detects stale tracked SVGs."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parent
EXPECTED_RENDERER = "mmdr 0.3.1"
SOURCES = [ROOT / f"{name}.mmd" for name in (
    "m01-contracts", "m02-bootstrap", "m03-disk-request", "m04-kernel-phases",
    "m05-foundation-modules", "m06-foundation-interfaces", "m07-job-lifecycle", "m08-remote-admission",
    "m09-template-architecture", "m10-template-render", "m11-ide-inspection",
)]
MANIFEST = ROOT / "render-manifest.json"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def validate_svg(path):
    tree = ET.parse(path)
    if tree.getroot().tag != "{http://www.w3.org/2000/svg}svg":
        raise SystemExit(f"Not SVG: {path.name}")
    for element in tree.iter():
        if element.tag.split("}")[-1] in {"script", "foreignObject"}:
            raise SystemExit(f"Unexpected active/HTML content in {path.name}")
        for key, value in element.attrib.items():
            if key.lower().startswith("on"):
                raise SystemExit(f"Unexpected event handler in {path.name}")
            if key.split("}")[-1] == "href" and not value.startswith("#"):
                raise SystemExit(f"Unexpected external reference in {path.name}")
    if "file:///" in path.read_text():
        raise SystemExit(f"Local filesystem URL leaked into {path.name}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="Render to temporary files and compare, without changing tracked files")
    parser.add_argument("--renderer", default=os.environ.get("MMDR", "mmdr"), help="Existing mmdr executable; never installed by this script")
    parser.add_argument("--png-dir", type=Path, help="Optional PNG previews, not committed beside SVGs")
    args = parser.parse_args()
    executable = shutil.which(args.renderer)
    if not executable:
        raise SystemExit("Install mermaid-rs-renderer 0.3.1 with --locked; see README.md")
    version = subprocess.check_output([executable, "--version"], text=True).strip()
    if version != EXPECTED_RENDERER:
        raise SystemExit(f"Expected {EXPECTED_RENDERER}, found {version}")
    subprocess.run(["node", str(ROOT / "validate.mjs")], check=True)
    # Native parsing/layout/rasterization: no browser, shell command, or remote renderer.
    with tempfile.TemporaryDirectory(prefix="forge-mermaid-") as directory:
        temp = Path(directory)
        env = dict(os.environ, XDG_CACHE_HOME=str(temp / "cache"))
        built = []
        for source in SOURCES:
            svg = temp / f"{source.stem}.svg"
            command = [executable, "-i", str(source), "-o", str(svg), "-e", "svg",
                       "-c", str(ROOT / "mermaid-config.json")]
            subprocess.run(command, check=True, env=env)
            validate_svg(svg)
            if args.png_dir:
                args.png_dir.mkdir(parents=True, exist_ok=True)
                png_command = command.copy()
                png_command[png_command.index("-o") + 1] = str(args.png_dir / f"{source.stem}.png")
                png_command[png_command.index("-e") + 1] = "png"
                subprocess.run(png_command, check=True, env=env)
            built.append((source, svg))
        manifest = {
            "renderer": {"name": "mermaid-rs-renderer", "version": "0.3.1",
                         "installation": "crates.io package with its published Cargo.lock (--locked)",
                         "font": "DejaVu Sans (installed locally, not embedded)",
                         "syntax_validator": "official mermaid 12.2.0 with jsdom 30.1.3"},
            "inputs": {name: digest(ROOT / name) for name in
                       ("package.json", "package-lock.json", "mermaid-config.json", "render.py", "validate.mjs")},
            "diagrams": [{"source": source.name, "source_sha256": digest(source),
                          "svg": f"images/{svg.name}", "svg_sha256": digest(svg)}
                         for source, svg in built],
        }
        if args.check:
            for _, svg in built:
                tracked = ROOT / "images" / svg.name
                if not tracked.is_file() or tracked.read_bytes() != svg.read_bytes():
                    raise SystemExit(f"Stale or environment-different SVG: {tracked.name}; render, inspect and commit source/image together")
            if not MANIFEST.is_file() or json.loads(MANIFEST.read_text()) != manifest:
                raise SystemExit("Render inputs or manifest differ; review and regenerate")
            print(f"PASS: {len(SOURCES)} SVGs and their source/config/tool hash manifest match")
        else:
            (ROOT / "images").mkdir(exist_ok=True)
            for _, svg in built:
                shutil.copyfile(svg, ROOT / "images" / svg.name)
            MANIFEST.write_text(json.dumps(manifest, indent=2) + "\n")
            print(f"Rendered {len(SOURCES)} local SVGs and hash manifest; visual review still required")


if __name__ == "__main__":
    main()
