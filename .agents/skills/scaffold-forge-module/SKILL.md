---
name: scaffold-forge-module
description: Scaffold, classify, or review a new Forge Rust module using the five Forge stitches (IoClient/port, PreparedGroup/Factory registrar, forge-foundation lifecycles, Task/DescribeCommand, and header-routed Bridge). Use when creating a new Forge crate or module, deciding which Forge traits a crate should implement, wiring Cargo.toml and check_dependencies.py, or auditing crate conformance across the Forge workspace.
---
# Scaffold Forge Module

Guide for choosing the right Forge crate archetype, implementing only the stitches that archetype earns, and proving the public contract through workspace guards and an independent consumer test.

Start with [Wire Forge Providers](../wire-forge-providers/SKILL.md) and the assigned feature branch. This skill does not authorize publication, installation or live configuration changes. The [stitch/archetype guide](../../../docs/architecture/forge-module-stitch-scaffold.md) is a design convention/scaffold proposal; inspect actual Cargo/API/checker source before applying its dated crate matrix. Use the [Code Specification Sheet](../../../docs/architecture/code-specification-sheet.md) as a source/behavior index, not current deployment evidence.

## When to Use

- Creating a new Rust crate or module in the Forge workspace
- Deciding whether a crate should stay a stitch-free utility or adopt `PreparedGroup`, `IoClient`, `forge-foundation`, or `Bridge`
- Reviewing an existing crate's public exports, dependency edges, and bootstrap wiring
- Updating `src/rust/Cargo.toml` and `src/rust/scripts/check_dependencies.py` for a new module

## Steps

### 1. Pick the Smallest Crate Archetype

Do not force every crate into every trait. Choose the smallest layer that fits:

- **L0 Framework Kernel:** `forge-core` (minimal traits, no `Arc`/`OnceLock`) and `forge-foundation` (`ProviderRef`, `SingletonProvider`, `LazySingletonProvider`, `TransientProvider`, `AssistedProvider`, `Slot` Null Objects, `EnvBuilder`, `Assemble`).
- **L1 Pure Contract, Utility, or Build Tool:** Intentionally stitch-free. No empty registrars or dummy bridges.
- **L2 Leaf Capability or Driven Adapter:** Implements **S1** (`IoClient` or domain port trait) + **S2** (`Factory` + effect-free `PreparedGroup` registrar). Omits `Bridge` so callers are never forced into an ingress router.
- **L3 Domain Service or Protocol Bridge:** Implements **S1 + S2 + S5** (`Payload`, `RegistryBuilder`, `AdmissionPolicy`, `Bridge`). Keep `assembly.rs` and `bridge.rs` as separate modules.
- **L4 Composition Root or Host:** Consumes **S2** registrars, owns **S3** provider lifecycles (`Singleton`/`Transient` or `forge-foundation`), and binds **S5** host routes. Keep binary `main.rs` thin over a public `lib.rs`.

### 2. Lay Out the Module Files

- `src/lib.rs`: Declarations and explicit public re-exports; keep implementation bodies in owned modules (the current R3 rule). Never hide the only integration path inside `examples/`, tests, or `main.rs`.
- `src/port.rs` (S1): Capability-owned trait plus typed request, response, and error enums. Keep `serde_json::Value`, `Any`, and vendor DTOs out of internal contracts.
- `src/assembly.rs` (S2): Typed startup `Config`, `Factory<T>`, and `register(config) -> Result<PreparedGroup<Factories>, Error>`. Validate config and stage recipes only. Perform zero I/O, thread spawns, or instance construction during registration. If an upstream binding is required, declare it in `requires` and use a 2-stage `Recipe::bind(dep)` after freeze.
- `src/bridge.rs` (S5, L3/L4 only): Implement `Payload` with one distinct `Kind` per variant, supply a host-trusted `AdmissionPolicy`, and register routes on `RegistryBuilder` -> `freeze()` -> `Bridge::new(...)`.
- For multi-collaborator domains or scenes, use `forge-foundation` (`ProviderRef<T>`, `Slot::defaulted` Null Objects, `EnvBuilder`, `Assemble`) rather than hand-rolling custom provider wrappers or `Option<Dep>` in business logic.

### 3. Register Dependencies and Verify

1. Add the crate to `src/rust/Cargo.toml` and record its exact local and external edges in `src/rust/scripts/check_dependencies.py`. Gate optional transports or CLI deps behind non-default Cargo features.
2. Add an independent consumer test exercising `register -> freeze -> create/acquire -> execute` strictly through public exports, plus duplicate-binding rejection, missing-requirement freeze rejection, denied admission with zero side effects, and header/payload-variant mismatch rejection.
3. Run workspace verification:

```bash
cargo test --manifest-path src/rust/Cargo.toml -p forge-<name>
python3 src/rust/scripts/check_core_layers.py
python3 src/rust/scripts/check_process_exec.py
python3 src/rust/scripts/check_dependencies.py
```

## Gotchas

- Changing workspace membership without updating `ALLOWED` in `check_dependencies.py` fails the architecture guard immediately.
- Putting `pub` helpers in a binary-only `main.rs` blocks other crates (including `forge-scene` BDD suites) from importing them. Always expose a thin `lib.rs`.
- Never mix startup dependency binding (`PreparedGroup`) with inbound command routing (`Bridge`). They are separate registrations.

## Current References

Use the [starter scaffold](../../../docs/architecture/templates/forge-module-scaffold/README.md), [bootstrap implementation](../../../src/rust/forge-core/src/bootstrap.rs), [provider contracts](../../../src/rust/forge-core/src/provider.rs), and [module-authoring guide](../../../docs/architecture/module-authoring.md) for exact signatures. S4 (`Task` plus separate `DescribeCommand`) is optional when the capability earns execution/discovery. Run `make check-imports` as well as the guards above. `check_process_exec.py` is an available manual validator; it is not currently wired into the fixed CI plan.
