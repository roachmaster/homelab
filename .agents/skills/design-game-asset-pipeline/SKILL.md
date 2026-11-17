---
name: design-game-asset-pipeline
description: Turn authored content into reproducible runtime data through stable identities, a cooking step, and a validating loader. Use for asset IDs, level chunks, cooked outputs, dependency resolution, and rebuild or reload checks.
---

# Design a game asset pipeline

Start from the [asset pipeline design](../../../docs/architecture/proposals/forge-game-asset-pipeline.md).
The pipeline has one rule: **no stage skips the one before
it.** The runtime never reads a source file, and the game
never uses an asset that has not passed through the registry.
Gregory ch6 separates the jobs — the file system retrieves
bytes, the resource manager gives those bytes an identity,
assembles dependencies, and controls when the resulting objects
are ready and safe to release — and this skill applies that
separation to each new asset.

## Map source, cooked, and runtime identity

Every asset has three identities, and the map between them is
explicit:

| Identity | Form | Example |
| --- | --- | --- |
| Source | A file under version control | `src/python/pixel-art/examples/room-one-scout.json` |
| Cooked | A build output + a build-manifest entry | `scout-atlas` sheet PNG + manifest JSON, hash `9f2c…` |
| Runtime | A registry record, keyed by asset ID | `scout-atlas`, kind `Atlas`, state `Ready` |

Define the map before wiring anything. The source is the
author's intent, human-editable. The cooked output is what
the runtime loads — bytes it can use without authoring tools.
The runtime record is what the game holds: an ID, a kind, a
format version, a content hash, dependencies, and a load
state. A change to any one of the three is a change to the
map, and the cook rebuilds the cooked output when the source,
a dependency, or the format version changes.

## Name the asset

Give the asset a stable ID first. It follows the recipe's
identifier rule (`[A-Za-z][A-Za-z0-9_-]{0,63}`), is unique
across the whole registry (one namespace — `scout-atlas`
names one thing), and is stable across rebuilds: recooking
the same source produces the same ID. The ID is what a level
references, what the loader fetches by, and what an error
message names.

The content hash (SHA-256 of the cooked bytes) is the
verification, not the identity. It answers "is this the asset
I think it is?" at load time and "is the cooked output stale?"
at cook time. Two builds of the same source have the same ID
and the same hash; a changed source has the same ID and a new
hash. Never use the hash as the ID — a human cannot read it,
and a level cannot reference it.

An ID is forever. Renaming an asset breaks every level that
references it. Rename by adding the new ID, updating the
references, and removing the old one — never by editing the
ID in place and hoping the references follow.

## Define the cooked format and its version

The cooked format is a contract between the cook and the
loader. For each asset kind, record:

- The cooked files (an atlas is a PNG + a manifest JSON; a
  level is a chunk JSON).
- The format version (a `u16` in the cooked manifest). A
  loader that meets a newer format version than it understands
  fails the load — never a silent downgrade.
- The defaults. Defaults are recorded in the cooked chunk, so
  a default change affects only newly authored content. An old
  chunk cooks to the same room it always did.

The chunk format is validated JSON with deny-unknown-fields,
bounded counts, and errors that name their context — the same
discipline as the recipe format. A chunk carries its identity
(the asset ID, the format version), the tile grid, serialized
spawn descriptors (the entity design's descriptors: stable ID,
kind, tile position, patrol), and asset references by stable
ID. Relationships survive relocation because they are
references by ID, not pointers or offsets.

## Declare versioned dependencies

A composite asset is usable once its required parts and their
references are ready. Declare the dependencies in the cooked
manifest, with each dependency's ID and format version:

- Resolution order is dependencies first, then the asset. The
  level depends on the atlas; the atlas depends on nothing;
  the collision world is built from the loaded chunk plus the
  colliders reached through the atlas.
- A missing dependency fails the load, with the dependency's
  ID named. There is no silent partial load.
- A circular dependency is rejected at cook time, with the
  cycle named. The runtime never sees one.
- At most 16 dependencies per asset. A level references a
  handful of atlases, not hundreds.

## Wire the loader

The loader fetches by ID and publishes only when the asset is
fully ready: Loading → Verifying (size, hash, parse, schema
validation) → Resolving (dependencies) → Ready (post-load
setup, then publication to the registry). An asset is usable
only after all of those steps — not when the bytes arrive.

Publication happens at a tick boundary, never mid-tick. An
asset that becomes ready mid-tick waits for the next tick. A
mid-tick publication would put a new level's geometry under a
step that started with the old one's. The generation tag is
the level's load count: gameplay checks that the generation
is still current before the new level publishes.

A failed load is a counted error, not a panic and not a retry
loop. The loader reports Failed once, with the ID and the
step. Retrying is a gameplay decision (the player presses
"retry"), not a loader behavior — an automatic retry on a hash
mismatch would fetch the same wrong bytes forever. The game
falls back to a placeholder: for an atlas, the primitive-shape
rendering the Room One renderer already does today. A level
that fails to load does not activate.

Lifetime is one-level-at-a-time: the level owns its assets,
and unloading the level releases them together. Resident
assets (shared atlases, UI art) stay for the session. The
transition is unload-then-load, masked by a loading screen.

## Verify the pipeline

- **Rebuild.** Change a source, a dependency, or the format
  version, and the cook rebuilds exactly the stale outputs.
  A clean cook and an incremental cook produce identical
  bytes — a hash mismatch after a clean rebuild is a bug in
  the cook, not a rebuild artifact.
- **Reload.** Load an asset by ID, unload it, and load it
  again: the second load returns the same bytes and the same
  hash. A reload after a cook picks up the new hash, never
  the old one.
- **Identity.** Two builds of the same source have the same
  ID and the same hash. A changed source has the same ID and
  a new hash. A level referencing an asset by ID resolves to
  the same asset after a recook.
- **Dependency.** A level whose atlas is missing fails to
  load, with the atlas ID named. A circular dependency is
  rejected at cook time, with the cycle named. A level whose
  atlas has a newer format version than the runtime
  understands fails to load.
- **Publication.** An asset that becomes ready mid-tick is
  invisible to the simulation until the next tick boundary.
  A save file (chunk ID + format version + dynamic state)
  loads the same room after the memory layout changes.
- **Fallback.** A failed atlas load degrades to the
  primitive-shape rendering. A failed level load leaves the
  previous level (or the loading screen) up, with the error
  naming the asset and the step.

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
  asset that becomes ready mid-tick waits. No exceptions.
- **The placeholder is the current renderer.** The
  primitive-shape rendering stays until the atlas loader
  lands, because it is the fallback a failed atlas load
  degrades to. Removing it before the loader exists removes
  the fallback.
- **Packaging is behind the fetch.** The loader fetches by
  ID and does not care whether the bytes came from a loose
  file or a package. Do not let packaging decisions leak
  into game code.

## Cross-references

- Asset pipeline design: [forge-game-asset-pipeline.md](../../../docs/architecture/proposals/forge-game-asset-pipeline.md)
  (stages, IDs, loader, cook, chunk format, lifetime)
- Entity model: [forge-game-entity-model.md](../../../docs/architecture/proposals/forge-game-entity-model.md)
  (spawn descriptors, save seam, generational handles)
- Physics design: [forge-game-physics.md](../../../docs/architecture/proposals/forge-game-physics.md)
  (collider mapping, collision world — the shapes assets deliver)
- Pixel-art authoring: [README](../../../src/python/pixel-art/README.md)
  (recipe format, exporters, manifest versions)
- Gregory study notes (study-notes repo): chapter 6 §§6.1–6.2
  (file system, resource manager) and chapter 15 §§15.3–15.5
  (chunk formats, loading and streaming, references)
