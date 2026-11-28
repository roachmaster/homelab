---
name: write-forge-bdd-tests
description: Author voice-checked Gherkin .feature files and typed Rust step bindings using forge-scene's BDD harness. Use when adding BDD test coverage for any Forge crate, writing scenarios that pass forge_scene::bdd::check_voice, wiring fake IoClient or stub ports into BDD worlds, or adding dev-dependencies to forge-scene and check_dependencies.py.
---
# Write Forge BDD Tests

Guide for writing behavioral `.feature` files and typed `forge_scene::bdd` test suites that verify real Forge crates over in-memory doubles while obeying `forge-scene`'s voice and dependency rules.

Use [Wire Forge Providers](../wire-forge-providers/SKILL.md) for the capability boundary and the assigned feature branch. Test work does not authorize live SQL/services, credentials or model calls; use owned fixtures and honor the declared database/browser scope.

## When to Use

- Adding BDD coverage for a new or existing Forge crate under `src/rust/forge-scene/`
- Fixing `forge_scene::bdd::check_voice` failures in `.feature` files
- Wiring a new crate into `forge-scene` as a dev-dependency without breaking `check_dependencies.py`
- Turning crate-level unit tests into readable behavioral specifications

## Steps

### 1. Author the `.feature` File Following Voice Rules

Place the feature at `src/rust/forge-scene/features/<name>.feature`. `forge_scene::bdd::check_voice` mechanically enforces three rules:

- **Every `Given` must state a capability:** Include one of ` can `, ` may `, ` needs `, or ` need ` (case-sensitive with surrounding spaces).
- **No routing words in `Then`:** Never use `answers through`, `through its`, `delegates`, `routes`, `calls into`, or `forwards`. Describe observable outcomes instead.
- **No restating `When` in `Then`:** The last two alphanumeric words of a `Then` line must not match the last two alphanumeric words of any `When` line in the same scenario.

Example of a passing scenario:

```gherkin
Scenario: Closed admission rejects work without touching ports
  Given the work bridge can deny traffic when admission is closed
  When a list envelope arrives while admission is closed
  Then dispatch is denied and no port is called
```

### 2. Write Typed Step Bindings in `forge-scene/tests/`

Create `src/rust/forge-scene/tests/bdd_<name>.rs`:

- Define a `#[derive(Default)] struct World` holding scenario state, captured outputs, errors, and RAII temp directories (cleaned up in `Drop`).
- Drive the real crate types (`Bridge`, `Registry`, `ProviderCooldown`, `SystemdSupervisor`, `McpServer`, `Application`) over deterministic in-memory fakes (counting ports, `ScriptedExec` `IoClient`, injected clocks) so tests have zero network or ambient environment flakiness.
- Register every step phrase on `Steps::new().given(...).when(...).then(...)` and assert outcomes with `forge_scene::bdd::ensure`.
- Run the feature in a `#[test]` via `forge_scene::bdd::verify(include_str!("../features/<name>.feature"), &steps())` and assert `report.scenarios == <expected_count>`.
- For async traits (such as `MissionControlPort`), drive futures with a local `tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(...)` helper rather than changing `forge_scene::bdd::verify` to async.

### 3. Wire Dev-Dependencies Cleanly (Only When Needed)

If the target crate is not yet a dev-dependency of `forge-scene`:

1. Ensure the target crate has a library root (`src/lib.rs`); binary-only crates cannot be dev-depended on.
2. Add the path dependency under `[dev-dependencies]` in `src/rust/forge-scene/Cargo.toml`.
3. Add the dev edge to `"forge-scene"` in `ALLOWED` (and any new external test crate to `EXTERNAL_DEV`) in `src/rust/scripts/check_dependencies.py`.
4. If the manifest change requires it, update `src/rust/Cargo.lock` intentionally under the authorized task using `--manifest-path src/rust/Cargo.toml`; do not refresh unrelated pins or mutate a trusted CI cache. Verify the resulting tests with `--locked`.

### 4. Verify

```bash
cargo test --manifest-path src/rust/Cargo.toml -p forge-scene --test bdd_<name>
python3 src/rust/scripts/check_dependencies.py
```

## Gotchas

- Tail-word collisions between `When` and `Then` (for example both ending in `correlation token`) fail `check_voice`. Vary the closing phrasing of `Then` to name the observed result.
- Unbound or ambiguous step phrases fail before any scenario executes; keep step wording identical between `.feature` and `Steps::new()`.
- Never add normal (non-dev) dependencies to `forge-scene` for crates under test. `forge-scene` is a build tool and test harness, so tested crates stay in `[dev-dependencies]`.

## Current References

Read the [forge-scene guide](../../../src/rust/forge-scene/README.md), [BDD API](../../../src/rust/forge-scene/src/bdd/mod.rs), [voice checker](../../../src/rust/forge-scene/src/bdd/voice.rs), and [step registry](../../../src/rust/forge-scene/src/bdd/steps.rs). Pattern `{}` placeholders match double-quoted values. The existing [MCP host binding](../../../src/rust/forge-scene/tests/bdd_forge_mcp_host.rs) exercises public crate APIs. A mock proves its selected boundary, not production I/O or real browser acceptance; use [Test Forge UI](../test-forge-ui/SKILL.md) for browser work.
