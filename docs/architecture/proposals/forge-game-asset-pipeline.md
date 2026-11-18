# Asset Pipeline for the Forge Game Runtime

**Implementation reference — reviewed 18 November 2026.** The cooker, manifest,
verified two-level loading, illustrated atlas and replay archive path now exist.
The original proposal is retained in Git history; arbitrary asset streaming and
a general registry remain future work.

## Implemented path

| Stage | Source and responsibility |
| --- | --- |
| Author | Versioned Room One and Traversal Yard JSON plus the approved Scout recipe/skin. |
| Cook | `src/python/game-assets/cook.py` validates source, materializes defaults, exports atlas/PNG and emits canonical level JSON. |
| Publish | Verify a complete generation, then atomically replace the relative output symlink. |
| Serve | Native `assets.rs` resolves one cooked generation at startup, verifies approved paths/digests and serves fixed same-origin routes. |
| Read | Generated playtest I/O worker returns bounded buffers or a correlated failure. |
| Admit and activate | Rust `Assets` verifies bytes/dependencies and `Loader` constructs a bounded candidate and activates only the newest request at a tick boundary. |

Source: [cooker guide](../../../src/python/game-assets/README.md),
[cook implementation](../../../src/python/game-assets/cook.py),
[host asset source](../../../src/rust/forge-ide-web-host/src/assets.rs),
[Rust asset admission](../../../src/rust/forge-ide-web/src/playtest_asset.rs),
[level loader](../../../src/rust/forge-ide-web/src/playtest_level.rs).

## Cooking and identity

The version-1 manifest maps supported stable asset IDs to kind, relative path,
SHA-256, required dependencies and optional dependencies. The two level IDs,
Scout atlas metadata and atlas PNG are the supported slice; this is not arbitrary
path resolution from game input. Required data failure prevents activation;
missing optional art leaves primitive drawing available.

`cook-state.json` is authoring-only fingerprint state. Incremental reuse requires
matching source/tool/conversion fingerprints and matching recorded output hashes.
`--clean` rerenders. Clean/incremental byte equality assumes the same local
rendering toolchain; it is not a cross-platform image determinism guarantee.

A cook prepares a sibling generation, checks it and atomically replaces the
relative `--out` symlink. Existing readers can retain the old resolved generation.
`make ui-build` copies one resolved generation alongside the generated five-file
browser bundle. A reader must not re-resolve the symlink between files.

[Cook sequence](../diagrams/mermaid/images/game-asset-pipeline-cook.svg) ·
[Cooker tests](../../../src/python/game-assets/tests/test_cook.py)

## Level and atlas contract

Each canonical chunk records version, ID, tile/platform mode, dimensions, solid
rows, stable spawns and optional guard patrol, named surface, atlas identity,
idle/walk/jump/crouch timing and foot-relative Q16 collider edges. Rust repeats
schema, bound, spawn, surface and collider validation before constructing `Room`.
The current level upload bound is 16 KiB.

The Scout illustrated atlas is 4x artwork at the same logical footprint. The
cooker zeroes authored jump root translations in a temporary recipe so runtime
physics owns travel. Timing and collider data stay available even if cosmetic
loading fails. Atlas metadata requires its PNG, while levels treat the art as
optional.

Source: [authored levels](../../../src/python/game-assets/examples/levels),
[level decoder](../../../src/rust/forge-ide-web/src/playtest_level.rs),
[logical geometry validation](../../../src/rust/forge-game-runtime/src/level.rs).

## Bytes, readiness and lifetime

Native startup verifies the primary cooked directory. An optional configured
alternate can fill approved missing paths; it cannot override a valid primary
file with unrelated bytes. The generated worker reads fixed routes and enforces
byte/deadline bounds. It transfers response buffers to the page adapter, which
uploads them into bounded producer-owned WASM buffers.

Rust `Assets` verifies the manifest binding and digest. `Loader` correlates the
request generation, decodes a candidate outside the fixed tick and retains the
newest prepared `Room`. A stale completion or required-data failure cannot
replace the active room. At a permitted boundary, the loader records outgoing
activation metadata and starts the incoming room at tick zero with a new epoch.

The active room, verified atlas, staged candidate and retained replay levels have
bounded ownership in the page's Rust state. There is no implemented arbitrary
streaming arena, universal asset-handle provider or memory compaction system.
The browser I/O worker is owned and terminated by its page adapter.

[Load sequence](../diagrams/mermaid/images/game-asset-pipeline-load.svg) ·
[Identity and ownership](../diagrams/mermaid/images/game-asset-pipeline-contract.svg)

## Replay retention

The Rust session recorder captures effective inputs, room epochs, manifest
digests and activation/restart boundaries. It is independent of the 512-record
debug ring. A complete session is bounded to 36,000 ticks, 128 segments and
2 MiB; overflow marks an export incomplete rather than silently claiming a full
replay.

`--archive` on the cooker, or `FORGE_COOKED_REPLAY_ARCHIVE` for `make ui-build`,
retains exact manifests and level chunks by manifest digest. A host configured
with `cooked_replay_archive` serves only verified historical data under fixed
archive routes. Import prepares typed missing-generation requests, obtains those
bytes through the I/O worker, validates them in Rust, then publishes a separate
replay view. An absent archive cannot guarantee playback of an older release.

[Replay state and archive port](../../../src/rust/forge-ide-web/src/playtest_replay.rs) ·
[Browser replay adapter](../../../src/rust/forge-ide-web-host/src/design/playtest_replay_browser.rs)

## Verification and remaining work

Source tests cover clean/incremental cook equivalence, input invalidation,
required/optional dependency handling, host hash/path admission, stale delivery,
level activation and two-room replay. The actual-WASM tests also compare cooked
Room One with the built-in fixture. Current QA reports carry execution evidence;
this page does not certify every deployment's alternate/archive configuration.

General asset kinds, large-world spatial streaming, package archives, audio,
3D import, a save-game format and asset reclamation policy remain future work.
Use ordinary Rust construction for pure loader state and the native host's
factory/provider assembly for effectful byte-source dependencies.

[Game engine](forge-game-engine.md) · [Physics](forge-game-physics.md) ·
[Worker ownership](forge-worker-ownership.md)
