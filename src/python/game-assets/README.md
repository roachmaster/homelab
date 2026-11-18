# Forge game asset cooker

The cooker validates versioned tile-level source files and runs the
[illustrated sprite exporter](../pixel-art/README.md) offline. It publishes
an asset-ID manifest, canonical level JSON, a 4× atlas, and its PNG sheet.
Runtime adapters can consume the cooked directory without running authoring
tools.

Run from the Homelab root with Python 3.12, `gdk-pixbuf-thumbnailer` with its
SVG loader, and ImageMagick available on `PATH`.

```bash
python3 -B src/python/game-assets/cook.py --out /tmp/forge-cooked
python3 -B src/python/game-assets/cook.py --clean --out /tmp/forge-cooked-clean
diff -r /tmp/forge-cooked /tmp/forge-cooked-clean
python3 -B -m unittest discover -s src/python/game-assets/tests -v
```

By default, the input is the [authored level directory](examples/levels) and
the Scout recipe and illustrated skin in the pixel-art source set. Use
`--levels`, `--recipe`, and `--skin` to select other explicit inputs.

## Runtime contract

- `manifest.json` version 1 maps stable IDs to a kind, relative path, SHA-256,
  required dependencies, and optional dependencies. A level can activate with
  primitive rendering when the optional Scout atlas is unavailable. The
  atlas itself requires its PNG.
- The two v1 chunks use `.` for floor space and `#` for solids. Grid rows are
  y-down. Entity IDs and spawn tiles are unique, the player, key, and exit
  are required, and an optional guard has a bounded open patrol segment.
  Solid tiles use the named surface. Its friction and restitution are Q16
  fractions in `0..65536`.
- The cooker materializes null patrols and the Scout's idle, walk, jump, and
  crouch frame durations in each level. A failed cosmetic load does not
  change simulation timing. During atlas export the authored jump's root
  translations are zeroed in a temporary copy so physics supplies travel.
- The cooked level stores foot-relative Q16 collider edges from the Scout's
  approved idle body box and logical pivot, at 32 logical pixels per tile.
  Atlas pixel scale affects drawing size only.

The root `make ui-build` command packages the active cooked generation with
the generated browser assets. Open **Room One playtest** in a host configured
with that bundle. **Traversal Yard** uses left and right plus Space or the
Jump button. The original Room One keeps its directional puzzle controls.
The playtest debugger records the level manifest digest and activation tick
with its trace.

`--out` can be a new path, an empty directory, or an existing symlink
published by this command. The cooker prepares and checks the complete
output in a sibling generation directory, then atomically replaces the
relative symlink. Resolve the symlink once when opening or copying a build
so every file belongs to the same generation. Prior generation directories
remain available to readers that already resolved them.

Pass `--archive /path/to/replay-archive` to retain the exact manifest and level
files under the manifest SHA-256. `FORGE_COOKED_REPLAY_ARCHIVE` enables the same
option for `make ui-build`. Use a shared archive directory across releases and
point the web host's `cooked_replay_archive` setting to it for browser replay.

The authoring-only `cook-state.json` records source, tool, and conversion
fingerprints. A normal cook reuses an atlas only when its fingerprint and
both recorded output digests match. `--clean` always renders it again.
The byte comparison assumes the same local rendering toolchain.
