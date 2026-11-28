# forge-scene

Build-time scene code generation plus a typed BDD layer for Forge's
provider/factory pattern. This crate is a **build tool and test harness**: no
Forge host depends on it, and nothing reads YAML or templates at runtime.

## Layering

```
          ┌───────────────────────────────────────────────┐
  BDD     │ features/*.feature   (own schema: feature)     │  natural prose
          │   Given = capability · When = state · Then =   │  → typed AST
          │   observable behaviour; each phrase → 1 step   │  → fixed steps
          └───────────────────────▲───────────────────────┘
                                  │ observes, cannot invent
          ┌───────────────────────┴───────────────────────┐
  code    │ generated/<scene>.rs  (checked in)             │  compiles, tests,
          │ ports · impls · factories · providers ·        │  client example
          │ decorators · per-env container · tests · client│
          └───────────────────────▲───────────────────────┘
                                  │ forge-template, at build time
          ┌──────────────┬────────┴───────┬───────────────┐
          │ schema       │ template       │ data          │
          │ scene.schema │ scene.rs.forge │ scenes/*.yaml │
          └──────────────┴────────────────┴───────────────┘
```

1. **Schema** — `schemas/scene.schema.json` says what a scene may contain.
   `src/json_schema.rs` enforces it (a small checker for exactly the keywords
   used; any other keyword is rejected). `Scene::validate` then checks what
   JSON Schema cannot: references, defaults, every verb answered, cycles.
2. **Template** — `templates/scene.rs.forge` is the fixed structural shape. It
   uses raw `{{{ }}}` everywhere (escaped `{{ }}` would HTML-escape Rust) and
   sections only; the renderer precomputes every boolean the template needs,
   because the template language has no equality operator.
3. **Data** — `scenes/dog.yaml` names the domain. The foundation stays the
   same; only the data changes.
4. **BDD** — hand-written `.feature` files parse into a typed AST
   (`bdd::Capability`, `bdd::State`, `bdd::Behavior`) whose JSON shape is
   `schemas/feature.schema.json`. A `Steps<World>` registry binds each phrase to
   one function. Unbound or ambiguous phrases fail before any step runs. The
   outcomes come from the structural code, so a scenario can only observe what
   the wiring already does. When one fails, either the wording or the wiring is
   wrong.

## Voice rules (checked by `bdd::check_voice`)

- `Given` states a capability: the subject *can*, *may* or *needs* something.
- `Then` is an observable behaviour. It may not restate the `When`
  ("when it is crate trained, then it is crate trained") or describe routing
  ("then it answers through its owner").

```gherkin
Scenario: The shelter scans the chip
  Given a dog can have an owner
  When it has no collar but is chipped
  Then a shelter scan finds its owner
```

Collar and chip are separate optional collaborators of the dog. With neither
bound, both are empty defaults and the dog answers "stray".

## Features in this crate

| Feature | Module under test | Covers |
| --- | --- | --- |
| `dog.feature` | generated dog scene | optional defaults, singleton, transient, lazy, assisted, decorator, required-missing at freeze, test double |
| `forge_core_providers.feature` | `forge_core::provider` + `bootstrap` | singleton sharing, transient freshness, lazy first use, missing binding and order at freeze |
| `forge_jobs.feature` | `forge-jobs` store/worker registrars | shared store, lazily opened store, transient workers, worker without store, in-memory store double behind the same bridge |
| `forge_template_renderers.feature` | `forge_template::assembly` | shared renderer, parse on first use, builtin methods by default, duplicate name refused |

## Commands

```bash
# regenerate after changing the scene, schema or template
cargo run --manifest-path src/rust/Cargo.toml -p forge-scene -- \
  render src/rust/forge-scene/scenes/dog.yaml src/rust/forge-scene/generated/dog.rs
# fail if the checked-in file is stale (also covered by tests/render_golden.rs)
cargo run --manifest-path src/rust/Cargo.toml -p forge-scene -- \
  check src/rust/forge-scene/scenes/dog.yaml src/rust/forge-scene/generated/dog.rs
# the client example (container + providers only)
cargo run --manifest-path src/rust/Cargo.toml -p forge-scene --example dog_client
cargo test --manifest-path src/rust/Cargo.toml -p forge-scene
```

The generated file is excluded from rustfmt (`#[rustfmt::skip]` at its include
sites) because its layout belongs to the template; `tests/render_golden.rs`
pins it byte for byte instead.

## Relationship to the architecture decisions

`docs/architecture/decisions.md` keeps a code generator out of the *initial
kernel* until it has a demonstrated consumer and acceptance tests. This crate
stays outside the kernel: `forge-core` is unchanged and no host links it. Its
consumers are the generated scene, the BDD suites over `forge-core`,
`forge-jobs` and `forge-template`, and the training corpus for the forge-gpu
model. Its acceptance tests are the golden render, the generated structural
tests, the construction lint and the feature runs. The YAML is read only by the
`forge-scene` binary and tests, at build time.
