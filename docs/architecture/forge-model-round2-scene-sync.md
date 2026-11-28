# Round 2 `forge-model` Corpus Sync: `forge-foundation` + `forge-scene`

**Status:** Handoff spec for `forge-gpu` (requires local `cargo` toolchain to execute the `cargo test -D warnings` gate).  
**Context:** The 2026-11-27 Google Drive `data-scene/train.jsonl` snapshot (204 rows) predates `feature/muse` (`0435481`), where `forge-foundation` and `forge-scene` became real workspace crates. It has **0** occurrences of `forge_foundation`, `ProviderRef`, `EnvBuilder`, `Slot::`, or `OnceLock`, and **249** occurrences of the superseded `Via<...>` hand-rolled wrapper.

## 1. Regeneration Steps on `forge-gpu`

1. **Pin generator input to `src/rust/forge-scene/`:**
   - Replace the standalone `Via<Provider, Value>` templates in `~/forge-model/distill/scene/templates/` with the workspace's canonical `src/rust/forge-scene/templates/scene.rs.forge` and `src/rust/forge-scene/schemas/scene.schema.json`.
   - Render each of the 30 training domain YAMLs (and 7 held-out eval YAMLs: `sensors`, `tickets`, `weather`, `shipping`, `calendars`, `lab_instruments`, `drones`) using `cargo run --manifest-path src/rust/Cargo.toml -p forge-scene -- render <scene.yaml> <out.rs>`.
2. **Gate every rendered domain against real `forge-core` + `forge-foundation`:**
   - Compile and run `cargo test -D warnings` on each rendered domain against path dependencies `forge-core` and `forge-foundation` (matching `src/rust/forge-scene/generated/dog.rs`).
   - Require `30/30` pass before emitting rows into `data-scene-v2/`.
3. **Close Round 2 preflight items from `round-2-data-audit.md` during regeneration:**
   - **Token budget (`max_tokens`):** Run the exact `Qwen3.5` tokenizer + chat-template + loss-mask preflight on CPU; split `oo-scene-tests` into independently labeled lifecycle / optional-default / test-double rows if full rendered rows exceed the verified B50 sequence cap.
   - **Write validation coverage:** Hold out one write-enabled domain into `val.jsonl` so `oo-scene-write` is represented in both train and val.
   - **Prompt diversity:** Rotate 4–6 natural user phrasings across rows (replacing the single fixed `"Scene-driven Forge codegen..."` preamble) without multiplying row weights.
   - **Matched OO supervised tokens:** Hold total OO supervised tokens constant between the clean `data-yaml` arm and the new `data-scene-v2` arm.
   - **Eval source exclusion:** Filter the 71 `data-teacher/train.jsonl` rows sourced from `eval/oo_eval.jsonl` cited files across the entire mix before resampling.

## 2. Expanded Blind Behavioral Eval Matrix (15–20 Items)

Expand `eval/oo_eval_scene.jsonl` from 5 items to **18 blind compile-and-test items** across the 7 held-out domains (`sensors`, `tickets`, `weather`, `shipping`, `calendars`, `lab_instruments`, `drones`), reusing `forge-scene`'s BDD harness (`forge_scene::bdd::verify`) for voice-checked behavioral assertions:

| Invariant Tested | Target Count | Example Held-Out Domains |
| --- | ---: | --- |
| Trait-facing client (no concrete factory in client code) | 3 | `sensors`, `weather`, `drones` |
| Lifecycle matrix (`SingletonProvider`, `LazySingletonProvider`, `TransientProvider`, `AssistedProvider`) | 4 | `tickets`, `shipping`, `calendars`, `lab_instruments` |
| Missing required dependency rejected at `EnvBuilder::freeze` | 3 | `sensors`, `tickets`, `drones` |
| Unbound optional dependency falls back to `Slot::defaulted` Null Object (no `Option` in business code) | 3 | `weather`, `shipping`, `calendars` |
| Test-double substitution in test env without modifying wiring DSL | 3 | `sensors`, `lab_instruments`, `drones` |
| Write paths + failure propagation (`Result` preserved, no silent default) | 2 | `tickets`, `shipping` |
