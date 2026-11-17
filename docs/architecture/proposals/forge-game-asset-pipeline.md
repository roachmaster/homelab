# Asset Pipeline for the Forge Game Runtime

Give authored content a reproducible path from source files to
runtime objects — so a level, a character atlas, and their
dependencies load by identity, validate before they activate,
and unload together — while keeping the fixed tick, determinism,
and the snapshot interface exactly as they are.

**Planning baseline** — GitHub `main` at `947b124` on 2026-11-17.
This design applies Gregory chapter 6 (§§6.1–6.2), chapter 15
(§§15.3–15.5), and the synthesis's streaming-door example, and
it follows the Forge package-boundary conventions (see "How this
integrates with the Forge philosophy" below).

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
| Lifetime | Resident (session) vs level-owned (arena) | A defined release point per asset |
| Publication | The tick-boundary commit | A ready generation the simulation may use |

A stage's output is the next stage's input, and no stage skips
the one before it: the runtime never reads a source file, and
the game never uses an asset that has not passed through the
registry.

## Stable asset IDs

Every runtime asset has one identity: an authored string ID
plus a content hash.

- **The ID** names the asset. It follows the recipe's
  identifier rule (`[A-Za-z][A-Za-z0-9_-]{0,63}`), is unique
  across the whole registry (one namespace — `scout-atlas`
  names one thing), and is stable across rebuilds: recooking
  the same source produces the same ID. The ID is what a level
  references, what the loader fetches by, and what an error
  message names.
- **The hash** (SHA-256 of the cooked bytes, recorded in the
  cooked manifest) verifies the asset. It answers "is this
  the asset I think it is?" at load time and "is the cooked
  output stale?" at cook time. It is not the identity — two
  builds of the same source have the same ID and the same hash,
  and a changed source has the same ID and a new hash.

The registry record for an asset is typed:

```rust
struct AssetRecord {
    id: AssetId,            // authored string, unique
    kind: AssetKind,        // Atlas | Level (v1)
    format_version: u16,    // the cooked format's version
    content_hash: [u8; 32], // SHA-256 of the cooked bytes
    dependencies: Vec<AssetId>, // at most 16, resolved first
    state: AssetState,      // Unloaded | Loading | Verifying
                            // | Resolving | Ready | Failed
}
```

Asset kinds are a typed enum, not a string: `Atlas` (a sprite
sheet + its manifest) and `Level` (a chunk) for v1. The recipe
is a source, not a runtime asset — it never appears in the
registry. New kinds (audio, tilesets) extend the enum; they do
not appear as ad-hoc strings.

## The runtime loader

The loader fetches by ID and publishes only when the asset is
fully ready. Ch6's rule: an asset is usable only after the
loader "verifies the I/O result and completes the format-specific
steps, including any reference resolution and post-load setup
required by that resource" — not when the bytes arrive.

The load states, in order:

| State | What happens | On failure |
| --- | --- | --- |
| Unloaded | Nothing; the asset is known only by ID | — |
| Loading | Fetch the cooked bytes (async in the browser) | Fetch error → Failed, with the ID and the reason |
| Verifying | Check size and content hash; parse; validate against the schema (deny unknown fields, bounded counts, errors naming their context) | Hash mismatch or validation error → Failed, with the ID, the step, and the offending field |
| Resolving | Load each dependency (recursively, dependencies first); a missing dependency fails the load | Missing or failed dependency → Failed, with the dependency's ID named |
| Ready | Post-load setup (decode the PNG, build the collision shapes, construct the room) completes; the asset is published to the registry | Setup error → Failed, with the ID and the step |

A failed load is a counted error, not a panic. The game falls
back to a placeholder: for an atlas, the primitive-shape
rendering the Room One renderer already does today — the
current renderer IS the placeholder, which is why it stays
until the atlas loader lands. A level that fails to load does
not activate: the previous level (or the loading screen) stays
up, and the error names the asset and the step.

The loader is async in the browser (`fetch` is async), but the
game sees a load request with a completion point, not a
callback maze: request the asset by ID, and the registry
reports Ready or Failed. Request priority (a preload vs an
about-to-be-visible asset, §6.1) is deferred — v1 loads one
level at a time, and the queue has one request in flight.

## The cooking step

The cook turns sources into cooked outputs and records what it
did. It is a command (a Python tool beside the pixel-art
package, `src/python/game-assets/cook.py`) that:

1. Reads the source assets (recipes, level JSON, skins).
2. Runs the exporters and validators (the pixel-art CLI for
   atlases; the level validator for chunks).
3. Computes content hashes and writes the cooked outputs.
4. Writes a build manifest — the asset database (ch6): one
   entry per asset with its ID, source path, source hash,
   dependencies, cooked path, and format version.

**Invalidation.** A cooked output is stale if its source hash,
any dependency hash, or the format version changed. The cook
rebuilds only stale outputs: "The build system needs enough
information to rebuild correct outputs without converting the
entire library on every edit" (ch6). A clean cook from scratch
and an incremental cook produce identical bytes — the cook is
deterministic, so a hash mismatch after a clean rebuild is a
bug, not a rebuild artifact.

**CI.** The cook runs in CI, and the pixel-art tests run with
it. This closes the gap flagged in the 7ba1235 review: the
Python tests currently run by hand only. A recipe edit that
breaks an export fails the gate; a stale atlas never ships.

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
the world-streaming skill's territory and arrives with the
first game that needs it.

The load/unload state machine:

```
Unloaded → Loading → Ready → Unloading → Unloaded
```

The transition is unload-then-load: the old level releases
before the new one loads. Retaining both concurrently would
reduce the visible loading gap but raise the peak memory
requirement (ch6); v1 pays the gap instead of the memory, and
the gap is masked by a loading screen — §15.4's air lock, in
its simplest form. The simulation commit happens at a tick
boundary: the loader provides a ready generation, gameplay
checks that the generation is still current, and only then
does the new level publish. The synthesis's door example is
the template: "At a tick boundary the loader provides a ready
generation and gameplay checks that the door target still
refers to it. Only then can the door open."

The generation tag is the level's load count. A save file is
the chunk ID + the format version + the dynamic state (entity
positions, status, inventory) — the entity design's save seam
("serialize descriptors + dynamic state, not runtime layout"),
with the chunk supplying the descriptors. A save remains
meaningful after the memory layout changes because it
references the level by ID, not by address (§15.4: "Keep
logical state and resource discovery separate").

## Dependency resolution

A composite asset is usable once its required parts and their
references are ready (ch6). The resolution order is
dependencies first, then the asset: the level depends on the
atlas; the atlas depends on nothing; the collision world is
built from the loaded chunk (the tile grid) plus the colliders
(the recipe hitboxes, reached through the atlas). Ch6's worked
example — a collision-shape object depending on the shared
surface-properties table through that table's ID — is the
model: the collision world needs its shapes from assets, so
the asset pipeline delivers them by ID.

Rules:

- A missing dependency fails the load, with the dependency's
  ID named. There is no silent partial load.
- A circular dependency is rejected at cook time, with the
  cycle named. The runtime never sees one.
- A dependency's version is recorded in the cooked manifest.
  Loading a level whose atlas has a newer format version than
  the runtime understands is a load failure, not a silent
  downgrade.

## Packaging: deferred, by design

Ch6: "Scene code should use the resource identity so the
packager can make those decisions independently." For v1 the
packaging decision is: loose files. The loader fetches by ID,
and it does not care whether the bytes came from a loose file
or a package — the fetch is by ID, the packaging is behind
that call. A bundle format (grouping commonly-loaded objects,
simplifying distribution) arrives when load times or update
granularity demand it, and the proposal's immutable
content-hash artifacts are the Studio-side packaging for build
outputs, not the runtime's. Nothing in the game changes when
the packager does.

## How this integrates with the Forge philosophy

- **Typed contracts.** Asset IDs, kinds, and load states are
  typed values, never strings passed around as payloads. The
  chunk format is validated JSON with deny-unknown-fields,
  the same discipline as the recipe format. No untyped
  payloads anywhere in the pipeline.
- **Bounded everything.** Registry entries, dependencies,
  level size, chunk size, atlas size, and concurrent loads
  are capped in the table below; the loader allocates nothing
  per tick (loading happens between ticks, never inside one).
- **Package boundaries.** The cook is a Python tool beside
  the pixel-art package, not a Rust crate: it runs at build
  time, and the runtime never imports it. The loader lives in
  the host/adapter layer (it fetches bytes and decodes PNGs),
  not in `forge-game-runtime`, which stays zero-dependency
  and receives only constructed objects. The runtime does not
  know where its assets came from — a file, a package, or a
  test fixture.
- **Determinism.** The cook is deterministic (same inputs,
  same cooked bytes, same hashes). The runtime load order is
  deterministic (dependencies first, then by ID). The
  simulation never sees a half-loaded asset: publication
  happens at a tick boundary, or not at all.
- **Verification is the deliverable.** The cook's output is
  verified by hash on every load; the chunk is validated
  before it constructs anything; the journey asserts the
  loaded level (the Scout stands on the floor it loaded from
  a file, not from a const).

## Bounds

| Bound | Value | Reason |
| --- | --- | --- |
| Registry entries | ≤ 256 | One namespace; the same scale as the entity table |
| Dependencies per asset | ≤ 16 | A level references a handful of atlases, not hundreds |
| Level dimensions | ≤ 64×64 tiles | Matches the adapter's `max_dimension` cap |
| Chunk file | ≤ 1 MiB | A level description, not a texture |
| Atlas | ≤ 4,096 px per side | The illustrated Scout sheet is 1,024×2,288; the bound has headroom without inviting an unbounded sheet |
| Concurrent loads | ≤ 4 | One level at a time, plus its dependencies |

## Caveats and things to consider

- **An ID is forever.** Renaming an asset breaks every level
  that references it. Rename by adding the new ID, updating
  the references, and removing the old one — never by editing
  the ID in place and hoping the references follow.
- **A failed load is not a retry loop.** The loader reports
  Failed once, with the ID and the step. Retrying is a
  gameplay decision (the player presses "retry"), not a
  loader behavior — an automatic retry on a hash mismatch
  would fetch the same wrong bytes forever.
- **The cook is deterministic or it is broken.** A clean cook
  and an incremental cook must produce identical bytes. A
  hash mismatch after a clean rebuild is a bug in the cook,
  not a rebuild artifact — investigate it, do not re-cook
  around it.
- **Publication is a tick boundary, never mid-tick.** An asset
  that becomes ready mid-tick waits for the next tick. A
  mid-tick publication would put a new level's geometry under
  a step that started with the old one's.
- **Defaults live in the cooked chunk.** A default change
  affects only newly authored levels. An old chunk cooks to
  the same room it always did — if it does not, the cooker's
  default handling is wrong.
- **The placeholder is the current renderer.** The
  primitive-shape rendering stays until the atlas loader
  lands, because it is the fallback a failed atlas load
  degrades to. Removing it before the loader exists removes
  the fallback.

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
  conventions applied throughout, summarized in "How this
  integrates with the Forge philosophy" above.
