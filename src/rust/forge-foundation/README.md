# forge-foundation

Provider lifecycles and typed per-environment wiring on top of `forge-core`.
It depends on `forge-core` only and adds no runtime registry, reflection or
string lookup.

| Piece | Responsibility |
| --- | --- |
| `Provide<T>` / `ProviderRef<T>` | The object-safe shape a dependent holds: "give me my collaborator". Every provider also implements `forge_core::provider::InstanceProvider` with an `Arc<T>` lease. |
| `SingletonProvider` | Runs its factory once, when built, and shares the instance. |
| `LazySingletonProvider` | Holds the factory plus a `OnceLock<Arc<T>>`; builds on the first acquire. A failure is not cached. |
| `TransientProvider` | Runs the factory on every acquire. |
| `AssistedProvider` + `AssistedFactory` | Runtime arguments at get time give variants of one invariant port. |
| `Slot<T>` | What an environment bound to a port: `Bound`, `Defaulted` (null-object empty implementation, so business code has no `Option`), or `Unbound`. |
| `Decorated` + `Decorator` | Decoration happens inside creation, so a decorated singleton is still one instance. `CallLog` is a shared counter a decorator can record into. |
| `EnvBuilder` / `FrozenEnv` | Stages one `forge_core::bootstrap` group per bound port and freezes. A required collaborator nobody binds fails as `MissingRequirement` before anything is constructed. |
| `Assemble` | Every environment of a container (prod, dev, test, ...) runs the same stage → freeze → construct step. |

Rules it encodes: trait first; factories only create; providers own
lifecycles; a dependent takes its dependency's provider and acquires it when
used; test doubles are just another implementation bound in the test
environment, so the wiring DSL is unchanged.

`forge-scene` renders complete scenes against this crate; see
[`../forge-scene/README.md`](../forge-scene/README.md).

## Why a new crate and not forge-core

`forge-core`'s `provider` module is deliberately minimal (`Factory`,
`InstanceProvider`, `Singleton`, `Transient`) and its module DAG is guarded by
`scripts/check_core_layers.py`. These additions combine `provider` and
`bootstrap` (an edge core forbids), need `Arc`/`OnceLock` sharing semantics
core does not promise, and have not yet earned a second independent consumer.
Keeping them here leaves core unchanged. `LazySingletonProvider` is the
smallest candidate to move into `forge_core::provider` later if a host needs it.

```bash
cargo test --manifest-path src/rust/Cargo.toml -p forge-foundation
```
