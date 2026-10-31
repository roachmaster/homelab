# Demo in-memory client implementation

Optional, original, std-only **fake client**, not a transport adapter or durable
store. `MemoryClient` implements `IoClient` with capability-owned prepared
request/result/error types from `forge-demo-messages`. It records the exact
owned request it receives; it never creates business content or changes its
destination. `MemoryMode::Record` returns an instance-local receipt sequence;
`MemoryMode::Reject` records the attempted request and returns a typed rejection.
Sequence exhaustion is explicit, never a wraparound. No retry occurs.

This implementation package owns `MemoryConfig`, its supported mode enum,
`MemoryClientFactory`, `MemoryFactories`, and the reusable `register` function.
Registration validates configuration and stages the complete factory group;
client construction is deferred until application composition has frozen the
selected registry. `ClientConstruction::Fail` deliberately fails before a
client is created. It is a demo failure switch, not a real transport lifecycle.

The application explicitly chooses provider lifetime:

- `MemoryClientHandle::Shared(Rc<Singleton<MemoryClient>>)` reuses one client per
  assembled host. All handle clones refer to that host-owned instance.
- `MemoryClientHandle::Transient(Rc<Transient<MemoryClientFactory>>)` retains the
  configured factory with **zero eager create calls**. Each `execute` acquires a
  fresh client and drops it afterward, including execution errors. Factory
  failure remains `MemoryAccessError::Factory`; execution failure remains
  `MemoryAccessError::Execution`.

The ready owned handle is injected as an `IoClient`; the task does not perform
service lookup or know this concrete package. Per-call acquisition stays in the
adapter. `MemoryProbe` counts creates/drops and records requests and instance
identities. A shared in-memory trace may demonstrate cross-package destruction
order. Sharing a probe shares observations, not client receipt state.

## Check

From `src/rust/`:

```bash
cargo test -p forge-demo-memory-client --offline --locked
cargo clippy -p forge-demo-memory-client --all-targets --offline --locked -- -D warnings
```

Tests cover unchanged prepared requests, both modes, typed configuration/factory
failures, singleton reuse, lazy transient construction/cleanup, independent
instance state, and sequence exhaustion. Passing these fake tests is **not**
evidence about an external service or real network client's contract.

## Limits

No network, disk, environment reads, serialization, authentication, pools,
workers, durability, retry, async cleanup or transaction guarantee. The
`Rc`/`RefCell` implementation is deliberately single-threaded and non-`Send`.
`Drop` releases only local owned values and cannot roll back committed effects.
