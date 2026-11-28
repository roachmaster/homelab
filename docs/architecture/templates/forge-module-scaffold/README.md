# Forge Module Starter Scaffold

Copy-pasteable starter layout for a new Forge Rust module conforming to `forge-core` (and optionally `forge-foundation`). This directory is intentionally **outside** `src/rust/Cargo.toml` so it does not trip `src/rust/scripts/check_dependencies.py` until you stamp out a real crate.

## Files Included

| File | Stitch | Keep When |
| --- | --- | --- |
| `src/lib.rs` | Public export boundary | Always |
| `src/port.rs` | **S1** — Trait-first port, typed request/response/error enums | Always |
| `src/assembly.rs` | **S2** — Effect-free `Config`, `Factory`, `PreparedGroup` registrar (plus 2-stage `Recipe::bind` option) | L2 capabilities, L3 services, driven adapters |
| `src/bridge.rs` | **S5** — `Payload`, `RegistryBuilder`, `AdmissionPolicy`, `Bridge` binder | L3 domain services or L4 hosts with inbound commands |
| `tests/consumer_smoke.rs` | Independent public-API proof | Always |

## Stamp-Out Checklist

1. Copy this directory to `src/rust/forge-<name>/` and add a `Cargo.toml` depending on `forge-core = { path = "../forge-core" }` (add `forge-foundation` only if you need `LazySingletonProvider`, `AssistedProvider`, `Slot`, or `EnvBuilder`).
2. Delete `src/bridge.rs` (and its re-export in `src/lib.rs`) if this is an L2 capability-only crate (like `forge-template` or `forge-system`).
3. Replace `Example*` names and `"example.*"` binding/route IDs with your domain's canonical names.
4. Register `forge-<name>` in `src/rust/Cargo.toml` and `src/rust/scripts/check_dependencies.py` (`ALLOWED` and `EXTERNAL`).
5. Run:
   ```bash
   cargo test --manifest-path src/rust/Cargo.toml -p forge-<name>
   python3 src/rust/scripts/check_core_layers.py
   python3 src/rust/scripts/check_dependencies.py
   ```
