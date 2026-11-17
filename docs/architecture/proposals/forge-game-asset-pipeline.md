# Asset Pipeline for the Forge Game Runtime

Give authored content a reproducible path from source files to
runtime objects — so a level, a character atlas, and their
dependencies load by identity, validate before they activate,
and unload together. The proposal keeps loading outside the fixed tick
and preserves the existing v1 snapshot layout.

**Planning baseline** — GitHub `main` at `947b124` on 2026-11-17.
This design applies Gregory chapter 6 (§§6.1–6.2), chapter 15
(§§15.3–15.5), and the synthesis's streaming-door example, and
it follows the Forge package-boundary conventions (see "How this
integrates with the Forge philosophy" below).

The [editable diagrams](../diagrams/mermaid/README.md#forge-game-asset-pipeline-proposal)
show [cook and publication](../diagrams/mermaid/images/game-asset-pipeline-cook.svg),
[lookup and activation](../diagrams/mermaid/images/game-asset-pipeline-load.svg),
and [asset identity and ownership](../diagrams/mermaid/images/game-asset-pipeline-contract.svg).

## The problem

The authoring half of a pipeline exists and is good: validated
pixel-art recipes, exporters, versioned manifests. Everything
downstream of it is missing. Nothing names an asset — the
Scout's manifest has no ID, so a level cannot reference its
atlas. Nothing loads an asset at runtime — the Room One
renderer has no atlas loader (its README says so), and the WASM
fetch is the only load in the system. Nothing cooks: exports
are hand-run CLI commands with no invalidation, no dependency
graph, and no CI wiring, so a stale atlas is invisible until
someone notices. And there is no level asset at all: Room
One's map is a Rust const, so "invalid level data never
activates" is vacuous — there is no level data to validate.
The game cannot load a second level, cannot unload the first,
and cannot save, because a save has no level identity to
reference.

## The pipeline stages (ch6)

Ch6 separates the jobs: "The file system retrieves bytes. The
resource manager gives those bytes an engine identity, assembles
dependencies, and controls when the resulting objects are ready
to use and safe to release." The stages, mapped to Forge:

| Stage | Forge form | Produces |
| --- | --- | --- |
| Source | Editable files under version control: recipe JSON, level JSON, SVG skins | The author's intent, human-editable |
| Export | The pixel-art CLI (`check` / `export` / `steps`) and the level validator | Validated source, errors naming their context |
| Conditioning (cook) | The cook command (see below) | Cooked outputs + a build manifest (the asset database) |
| Runtime representation | Atlas PNG + manifest JSON; level chunk JSON | Bytes the runtime can load without authoring tools |
| Registry | The in-memory asset registry, keyed by asset ID | Deduplicated loaded objects |
| Lifetime | Resident (session) vs level-owned (arena) | A defined release point |

No stage skips the one before it. The runtime never reads a
source file. A shortcut that loads a recipe directly bypasses
validation, hashing, and dependency resolution — it is a bug,
not a convenience.

## Asset identity

Every loadable object has a stable asset ID. The ID is the only
name that appears in level chunks, save files, traces, and the
registry. It is not a path and not a pointer. Changing an ID is
a deliberate rename that updates every reference; the cook
rejects a dangling ID.

## The cook

The cook is deterministic: same inputs produce the same bytes.
It records conversion settings and dependencies in a build
manifest so a change to either invalidates the correct outputs.
A clean cook and an incremental cook must produce identical
bytes. A hash mismatch after a clean rebuild is a bug in the
cook — investigate it, do not re-cook around it.

**CI.** The cook runs in CI, and the pixel-art tests run with
it. A recipe edit that breaks an export fails the gate; a stale
atlas never ships.

## The level chunk format (§15.3)

A level is a JSON file, validated like a recipe: deny unknown
fields, bounded counts, every error naming its context. §15.3:
"A serialized chunk must name object identities and types,
store attributes and encode relationships that survive
relocation." Relationships survive relocation because they are
references by stable ID, not pointers or offsets — the chunk
is a description that constructs objects, not a binary image
of them.

A chunk contains:

| Part | Contents |
| --- | --- |
| Identity | Chunk ID (the asset ID), format version |
| Tile grid | Width, height, and the rows of tile codes (the terrain the collision world is built from) |
| Spawn descriptors | The entity design's descriptors, serialized: stable ID, kind, tile position, patrol (for guards) — the spawn definition, not the live instance |
| Asset references | Atlas IDs by entity kind (the player uses `scout-atlas`); surface properties by ID when they exist |
| Metadata | Level name, the starting status, the win condition's target |

The chunk's defaults are recorded in the cooked chunk, so a
default change affects only newly authored levels — §15.3's
rule: "Version the cooked schema and decide whether a default
change affects existing content or only newly authored
instances." The decision is: only newly authored instances.
An old chunk cooks to the same room it always did.

Room One becomes the first chunk: its 13×9 grid, the key/exit/
guard spawn descriptors, and the `scout-atlas` reference move
out of the Rust const and into `room-one.json`. The const is
deleted when the loader lands — the game loads its first level
from a file, and the "invalid level data never activates" rule
finally has something to apply to.

## Lifetime (§15.4)

§15.4 offers three models: one-level-at-a-time, air locks, and
continuous streaming. Forge takes one-level-at-a-time for v1:
simple ownership and bulk release. The level owns its assets;
unloading the level releases them together (an arena — one
release point for the whole group). Resident assets (shared
atlases, UI art) stay for the session. Continuous streaming is
the world-streaming skill's territory and a later milestone.

## Address, identity, and readiness (synthesis)

The synthesis and §6.2 separate three facts that must not be
collapsed:

| Concept | Form in Forge | Lifetime | Purpose |
| --- | --- | --- | --- |
| Identity | Stable typed asset ID | Stable across loads and saves | Names the resource in chunks, saves, traces, and the registry |
| Locator | Manifest-relative path or archive entry | Bound to a cooked build | Tells the loader where to read the bytes |
| Address | Runtime pointer / slice into a loaded buffer | Valid only while the buffer is retained | Used by the loader and the registry; never stored in a save or a chunk |
| Readiness | Published flag and generation at a tick boundary | From successful post-load init until unload | Gameplay and physics may observe the asset only after publication |

Registering a resolved entry early can help fix up cross-references.
Publishing for general use waits until every required dependency is ready
and post-load initialization has finished. Runtime handles check the owner
generation before dereferencing a loaded address.

Treat the level chunk, terrain, and collision inputs as required.
If any fails, keep the active level or loading screen and report the
asset ID and failed stage. The Scout atlas is an optional presentation
asset for v1. Use the primitive renderer if it cannot load, with
gameplay geometry supplied by the validated level and collision data.

## Search path and resolution order (§6.1)

When the same relative name exists in more than one location,
resolution order is observable behavior. For v1:

1. The cooked output directory for the active build (authoritative).
2. An optional, explicitly configured alternate directory.
3. No further fallbacks. A missing asset is an error that names
   the ID and the step, not a silent search into source trees.

Verify bytes from every location against the active manifest's digest.
An alternate directory can supply the same cooked bytes when the active
directory lacks them. For local experiments with changed bytes, cook a
separate development manifest. Record the search order in the loader
configuration. The runtime reads only cooked artifacts.

## Async completion and buffer lifetime

An asynchronous read (or a fetch) keeps its destination buffer
valid until the completion handler finishes and the loader has
either published the asset or recorded a failure. The buffer is
not recycled while a pending request still points at it. A
completion that arrives after the requesting level has been
unloaded is dropped; it must not publish into a foreign arena.

## Tick-boundary activation and replay

I/O and post-load initialization stage a candidate outside the simulation
step. A ready candidate can replace the active level only at a tick
boundary and only if its request generation is still current. Record the
manifest identity and activation tick for replay. Replay preloads the
recorded cooked inputs before applying that activation so asynchronous
completion timing cannot move the transition to another tick.

## Caveats and things to consider

- **No stage skips the one before it.** The runtime never
  reads a source file. A shortcut that loads a recipe
  directly bypasses validation, hashing, and dependency
  resolution — it is a bug, not a convenience.
- **The cook is deterministic or it is broken.** A clean cook
  and an incremental cook must produce identical bytes.
  Investigate a mismatch; do not re-cook around it.
- **Defaults live in the cooked chunk.** A default change
  affects only newly authored levels. If an old chunk cooks
  to a different room, the cooker's default handling is
  wrong.
- **Publication is a tick boundary, never mid-tick.** An
  asset that becomes ready mid-tick waits for a boundary. A stale
  request cannot replace the active level.
- **The placeholder is the current renderer.** The
  primitive-shape rendering stays until the atlas loader
  lands, because it is the fallback a failed atlas load
  degrades to. Removing it before the loader exists removes
  the fallback.
- **Packaging is behind the fetch.** The loader fetches by
  ID and does not care whether the bytes came from a loose
  file or a package. Do not let packaging decisions leak
  into game code.
- **Search path order is recorded.** A clean build and a
  developer machine using the same manifest must resolve an
  asset ID to the same bytes.
- **Identity is not an address.** Never store a runtime
  pointer in a save, a chunk, or a trace. Use the asset ID.
- **Buffers outlive their requests.** A pending async
  completion keeps its buffer valid; a completion after unload
  is dropped, not published.

## Open decisions

- **Packaging — deferred.** Loose files for v1; a bundle
  format arrives when load times or update granularity
  demand it.
- **Continuous streaming — deferred.** One-level-at-a-time
  for v1; streaming arrives with the first game that needs
  it, under the world-streaming skill.
- **Request priority — deferred.** V1 loads one level at a
  time with one request in flight; priority matters when a
  preload and a visible load compete for the same device.
- **Audio assets — not in v1.** The kind enum extends when
  the first sound arrives; the pipeline does not change.

## References

- Gregory, *Game Engine Architecture*, 2nd ed., chapter 6
  (§§6.1–6.2: file system, resource manager), chapter 15
  (§§15.3–15.5: world chunk formats, loading and streaming,
  object references and world queries), and chapter 12
  (§12.3: the collision shapes assets deliver). Study notes
  in `roachmaster/study-notes` under
  `studies/gregory-game-engine-architecture-2e-2015/`.
- [Extending Homelab with a Rust Game Engine](forge-game-engine.md)
  (Studio build flow, content-hash artifacts), the
  [entity model](forge-game-entity-model.md) (spawn
  descriptors, save seam, generational handles), the
  [physics design](forge-game-physics.md) (collider mapping,
  collision world), and the
  [pixel-art authoring README](../../../src/python/pixel-art/README.md)
  (recipe format, exporters, manifest versions).
- The `design-package-boundaries` skill (`.agents/skills/`) for
  the typed-contract, boundedness, and dependency-direction
  conventions applied throughout.
