# Forge Module Stitch Guide & Crate-Spreading Matrix

**Status:** Generalized convention & scaffold proposal on `feature/gemini` (derived from Homelab revision `0435481`).  
**Companions:** [Module authoring](module-authoring.md) · [Bootstrap registrars](bootstrap-registrars.md) · [Crate dependency tree](crate-dependency-tree.md) · [Starter scaffold](templates/forge-module-scaffold/README.md)

## 1. The Five Forge Stitches

A crate conforms to Forge by implementing **only the stitches its role earns**—never by inheriting a universal mega-trait or adding empty registrars to pure utility/contract crates.

| Stitch | Core Contract (`forge-core` / `forge-foundation`) | Responsibility | Must NOT Do |
| --- | --- | --- | --- |
| **S1. Port & Client** | `forge_core::io_client::IoClient` or capability-owned port trait | Typed request/response/error contract for one cohesive capability or I/O family | Expose `serde_json::Value`, `Any`, vendor DTOs, or business routing |
| **S2. Bootstrap Registrar** | `forge_core::bootstrap::PreparedGroup<F>`, `forge_core::provider::Factory<T>` (optional 2-stage `Recipe::bind`) | Validate typed startup config and stage factories into a `PreparedGroup` | Perform I/O, spawn threads, parse templates, or construct instances at registration |
| **S3. Provider & Lifecycle** | `forge_core::provider::{InstanceProvider, Singleton, Transient}` or `forge_foundation::{SingletonProvider, LazySingletonProvider, TransientProvider, AssistedProvider, Slot, EnvBuilder, Assemble}` | Own instance scope, sharing, lazy init, Null Object optional defaults, and per-env test-double swaps | Leak concrete factories or `Option<Dep>` into business logic; act as a service locator |
| **S4. Task & Discovery** | `forge_core::task::Task`, `forge_core::command::DescribeCommand` | Pure/orchestrating capability work over injected ports, plus inspectable command metadata | Capture concrete transport adapters or bypass admission |
| **S5. Ingress Bridge** | `forge_core::ingress::{RouteKey, Header, Payload, Envelope, AdmissionPolicy}`, `forge_core::bridge::{RegistryBuilder, Bridge}` | Header-selected allowlisted dispatch, admission gate, and header/payload-variant agreement check | Infer routes from payload body, hold domain business logic, or accept untyped JSON |

## 2. How Stitches Spread Across the 30 Workspace Crates

Every crate in `src/rust/` belongs to one of five layers enforced by [`src/rust/scripts/check_dependencies.py`](../../src/rust/scripts/check_dependencies.py):

| Archetype | Expected Stitches | Current Workspace Crates | Allowed Local Deps |
| --- | --- | --- | --- |
| **L0. Framework Kernel** | Defines S1–S5 | `forge-core` (minimal kernel), `forge-foundation` (lifecycles/slots/env on top of `forge-core`) | `forge-core`: none; `forge-foundation`: `forge-core` only |
| **L1. Pure Contract / Utility / Build Tool** | None (intentionally stitch-free) | `forge-mcp`, `forge-ide-ui`, `forge-game-runtime`, `forge-agent-driver`, `forge-services`, `forge-portal-release-actor`, `forge-studio`, `forge-mcp-db`, `forge-usage`, `forge-scene` (build/BDD tool) | None (or narrow contract dep such as `forge-template` for `forge-scene`) |
| **L2. Leaf Capability / Driven Adapter** | **S1 + S2** (optional **S4** for task/descriptors) | `forge-system` (disk), `forge-template` (renderer), `forge-jenkins`, `forge-jobs` (2-stage `WorkerRecipe::bind`), `forge-remote-https`, `forge-postgres` | `forge-core` (plus narrow domain contract crates where required, e.g. `forge-postgres` → `forge-work`) |
| **L3. Domain Service / Protocol Bridge** | **S1 + S2 + S5** (or **S5** over request-context ports) | `forge-work` (`WorkBridge` with 15 routes + `WorkAdmissionPolicy`), `forge-remote`, `forge-ide`, `forge-work-publication`, `forge-work-worker` | `forge-core` + narrow sibling contracts |
| **L4. Composition Root / Host** | **S2 (consumer) + S3 + S5 (host wiring)** | `forge-local`, `forge-terminal`, `forge-agent`, `forge-ide-web`, `forge-ide-web-host`, `forge-host`, `forge-mcp-host` | Selected L2/L3 capabilities (optional capabilities feature-gated per `check_dependencies.py`) |

### Observed Gaps & Standardization Targets

1. **Startup binding vs. command route separation:** `forge-system::disk` + `forge-local::disk` splits S2 (crate-owned `PreparedGroup`) from S5 (host-owned `Bindings`), while `forge-work` owns S5 (`WorkBridge`) and takes ports via per-request `WorkContext`. Both are valid—keep S2 and S5 as separate modules (`assembly.rs` vs. `bridge.rs`) so capability crates never force an ingress bridge on callers that only need S1 + S2.
2. **`forge-foundation` adoption boundary:** New multi-collaborator domains and generated scenes should target `forge-foundation` (`ProviderRef<T>`, `Slot<T>`, `EnvBuilder`, `Assemble`, `LazySingletonProvider`) instead of hand-rolling `Via<...>` or custom `LazySingleton` wrappers. Existing hosts (`forge-local`, `forge-agent`, `forge-host`) do not need to be rewritten until they actually need lazy/optional/assisted lifecycles.
3. **Host-side `forge-mcp-host` alignment:** `forge-mcp-host` currently depends only on `forge-mcp`; if it grows shared admission or pluggable backends, add S2 (`PreparedGroup`) and S5 (`Bridge`) rather than ad-hoc wiring.

## 3. Adding a New Module (Standard Workflow)

1. **Pick the smallest archetype** from Section 2 (L1 if pure utility/contract, L2 if reusable capability/adapter, L3 if it owns inbound commands, L4 only for a new binary host).
2. **Copy the starter files** from [`templates/forge-module-scaffold/`](templates/forge-module-scaffold/README.md) into `src/rust/forge-<name>/` and keep only the files your archetype needs (`bridge.rs` is omitted for L2-only capabilities).
3. **Register dependencies explicitly:** Add the crate to `src/rust/Cargo.toml` and update `ALLOWED` (and `EXTERNAL` if needed) in `src/rust/scripts/check_dependencies.py`. Keep optional transport/CLI dependencies behind non-default Cargo features.
4. **Prove public exports from an independent consumer:** Follow [`tests/consumer_smoke.rs`](templates/forge-module-scaffold/tests/consumer_smoke.rs) (or a separate consumer crate like `src/rust/forge-template/tests/consumer/Cargo.toml`) to verify `register -> freeze -> create/acquire -> use`, duplicate rejection, missing-requirement rejection, and (for L3/L4) header/variant mismatch + denied admission with zero side effects.
5. **Run workspace guards:**
   - `cargo test --manifest-path src/rust/Cargo.toml -p forge-<name>`
   - `python3 src/rust/scripts/check_core_layers.py`
   - `python3 src/rust/scripts/check_dependencies.py`
