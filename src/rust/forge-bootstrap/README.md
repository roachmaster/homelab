# forge-bootstrap

Implemented std-only registration helpers for **configured factory groups**, not a
service locator or automatic DI executor. No normal crate dependencies.

## Public protocol

1. A package registrar validates its typed startup configuration and returns
   `PreparedGroup<F>` containing one cohesive typed factory/recipe group and its
   metadata. No `create`, `acquire`, environment read or I/O belongs in this phase.
2. `Registry<G>::register(group)` checks every claim before committing the whole
   group. Success owns `(G, F)`, preserving heterogeneous types without erasure.
   On conflict, `Rejected::into_parts()` returns the **unchanged original
   registry**, rejected group and typed reason. Neither a late collision in a
   group nor a duplicate group ID partially inserts anything.
3. Consuming `freeze` validates that required bindings are supplied by earlier
   groups. Missing, self and forward dependencies fail; no order is invented.
   `FreezeRejected` retains the unchanged pending registry and diagnostic.
4. Only `Frozen<G>::into_factories()` releases the nested tuple. Application code
   then performs explicit typed construction, chooses providers, injects tasks
   and binds the existing bridge. Publish the application only after completion.

A registrar is an ordinary typed function. It can return several factories in one
package-owned struct. No common erased registrar/factory trait, arbitrary type
lookup, framework-global business enum or dyn-GAT provider is needed. The registry
owns both metadata and values; separately stored mutable slots cannot drift from
its index. This is the concrete refinement of the earlier slot-target proposal.

```rust
use forge_bootstrap::{PreparedGroup, Registry};

let first = PreparedGroup::new("client", &["client.port"], &[], || 7)?;
let second = PreparedGroup::new("task", &["task.run"], &["client.port"], || "ready")?;
let plan = Registry::new().register(first)?.register(second)?.freeze()?;
let (((), client_recipe), task_recipe) = plan.into_factories();
// Call typed constructors and inject ready dependencies here, not in registration.
# Ok::<(), Box<dyn std::error::Error>>(())
```

The closures above only illustrate typed storage. The [optional consumer](../examples/forge-bootstrap-demo/README.md)
and its [capability](../examples/forge-demo-messages/README.md) /
[implementation](../examples/forge-demo-memory-client/README.md) registrars demonstrate
actual existing `Factory`, `Singleton`, lazy `Transient`, `IoClient`, `Task` and
bridge composition with fake-only effects.

## Exact scope

Identifiers are nonempty, at most 256 ASCII bytes, and use letters, digits, `.`,
`_` or `-`. Each group provides at least one binding. Repeated provided/required
identifiers are rejected. Requirements name *external* bindings from earlier
groups; dependencies among factories inside one group are its typed recipe's job.
An empty overall registry can freeze. IDs are assembly diagnostics, not runtime
request destinations. Actual routes remain controlled by `forge-bridge`.

Freeze proves the declared metadata/order checks only. It cannot prove that a
factory obeys its metadata, that a caller follows the order after extraction,
that a constructor is pure, or that a client is production-ready. Registrars and
recipes are trusted Rust code; panic/allocation failure and external effects are
not rolled back. Tests use counters and typed construction to establish the
selected behavior. Factory/debug/Clone/`'static` bounds are not imposed on groups.

The registry never creates resources: normal registrars store factories/config,
not running clients. Supplied recipe values may still own external handles and
have arbitrary `Drop` behavior; callers own that contract. The application owns
construction failures and resource teardown. RAII does not reverse external effects; awaited cleanup,
pooling, hot registration, graph auto-wiring and managed host lifecycle are not
implemented. The example explicitly models singleton versus transient timing and
uses owned handles rather than self-referential provider/task fields.

## Verify

From `src/rust`:

```bash
cargo test -p forge-bootstrap --offline --locked
cargo run -p forge-bootstrap-demo --no-default-features --offline --locked
cargo run -p forge-bootstrap-demo --features messages --offline --locked
python3 scripts/check_dependencies.py
```

Twelve integration tests cover typed/non-static groups, late multi-claim conflict,
original binding preservation and recovery, group-local validation, missing/self/
forward/cyclic declared dependencies, no construction during staging, and error
causes. Four doctests include typed tuple extraction and three compile-fail
checks for premature extraction, post-freeze registration and moved-builder reuse.
