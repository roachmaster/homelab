---
name: design-game-asset-pipeline
description: Design or review the path from authored content to runtime objects — source, cook, identity, load, lifetime, and invalidation. Use when adding levels, atlases, or any loadable game resource.
---

# Design the game asset pipeline

Start from the [asset pipeline design](../../../docs/architecture/proposals/forge-game-asset-pipeline.md).
The pipeline turns editable source into runtime objects that load by identity, validate before they activate, and unload together. No stage skips the one before it.

## Stages

| Stage | What it does |
| --- | --- |
| Source | Editable files under version control (recipes, level JSON, skins) |
| Export | Pixel-art CLI and level validator; errors name their context |
| Conditioning (cook) | Deterministic cook; build manifest is the asset database |
| Runtime representation | Atlas PNG + manifest; level chunk JSON |
| Registry | In-memory, keyed by asset ID; deduplicates |
| Lifetime | Resident (session) vs level-owned (arena) |

## Identity vs address vs readiness

Keep these three facts distinct in every loader and registry path:

1. **Identity** — the asset ID. Permanent. Appears in chunks, saves, traces, and dependency lists.
2. **Address** — the runtime buffer or object pointer. Valid only while the owning arena or resident set retains it. Never serialized.
3. **Readiness** — published at a tick boundary after post-load init and dependency resolution. Gameplay and physics observe only ready assets.

Registering early for fix-ups is fine; publishing early is not. A handle that outlives its buffer must fail the generation or epoch check, not silently alias a new object.

## Search path

Resolution order is part of the contract:

1. Cooked output directory for the active build.
2. Optional, explicitly configured override directory.
3. Nothing else — especially not source trees.

Record the order. A missing asset fails with the ID and the step named. Do not open a recipe file as a runtime fallback.

## Async buffer lifetime

Keep the destination buffer valid until the completion handler has published or failed. If the requesting level has already unloaded, drop the completion; do not publish into a foreign arena. Priority between competing requests is deferred until more than one request is in flight.

## Cook determinism

A clean cook and an incremental cook must produce identical bytes. A hash mismatch after a clean rebuild is a bug in the cook — investigate it, do not re-cook around it. Defaults live in the cooked chunk; a default change affects only newly authored levels.

## Publication boundary

An asset that becomes ready mid-tick waits for the next tick boundary. Mid-tick publication would put new geometry under a step that started with the old world.

## Additional verification

- **Identity round-trip.** A save that stores only asset IDs (plus dynamic state) reloads the same room after the memory layout changes.
- **Search path.** With an override directory present, the resolved bytes match the override; without it, they match the cooked build. Source files are never opened.
- **Stale completion.** Unload a level while a fetch is in flight; the completion must not publish and must not panic.
- **Dependency invalidation.** Change a recipe that an atlas depends on; the cook rebuilds the atlas and the build manifest records the new hash. A clean cook and an incremental cook produce identical bytes.
- **Failed load.** A failed atlas load degrades to the primitive-shape renderer. A failed level load leaves the previous level (or loading screen) up, with the error naming the asset and the step.

## Caveats

- No stage skips the one before it. The runtime never reads a source file.
- The cook is deterministic or it is broken.
- Publication is a tick boundary, never mid-tick.
- The placeholder renderer stays until the atlas loader lands — it is the fallback.
- Packaging is behind the fetch; game code does not care whether bytes came from a loose file or a package.

## Cross-references

- Asset pipeline design: [forge-game-asset-pipeline.md](../../../docs/architecture/proposals/forge-game-asset-pipeline.md)
- Entity model: [forge-game-entity-model.md](../../../docs/architecture/proposals/forge-game-entity-model.md)
- Physics design: [forge-game-physics.md](../../../docs/architecture/proposals/forge-game-physics.md)
- Pixel-art authoring: [README](../../../src/python/pixel-art/README.md)
- Gregory study notes: chapter 6 §§6.1–6.2 and chapter 15 §§15.3–15.5
