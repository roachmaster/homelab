# Demo messages capability

Original, std-only demonstration policy; **not a production messaging service**.
The generic `GreetingTask<C>` validates a name/destination, prepares the complete
`PreparedMessage { destination, body }`, and makes exactly one injected
`IoClient` call. The client does not choose greeting content. `GreetingError<E>`
preserves the client's typed failure without hidden retry. Input, prepared
request, receipt and delivery-error types belong to this capability; the
application owns its ingress command/result sums.

`MessagesConfig` is typed startup configuration, not a runtime command. The
`register(config, probe)` function validates it and returns one
`PreparedGroup<MessageFactories>` without creating a task or client. Its declared
client requirement must precede it in a frozen `forge-bootstrap::Registry`.
After freeze, application composition extracts `GreetingRecipe`, supplies an
owned ready client handle via `bind(client)`, and invokes the resulting existing
zero-argument `Factory::create` contract. `TaskConstruction::Fail` and `TaskProbe`
are explicit demo-only failure/observation facilities. Probes never read files,
environment variables or global state.

The capability imports no concrete client or ingress package. An optional fake
implementation lives in [`forge-demo-memory-client`](../forge-demo-memory-client),
and the consuming [`forge-bootstrap-demo`](../forge-bootstrap-demo) enables both
only with its `messages` feature. Pure task consumers do not need this crate.

## Check

From `src/rust/`:

```bash
cargo test -p forge-demo-messages --offline --locked
cargo clippy -p forge-demo-messages --all-targets --offline --locked -- -D warnings
```

Tests inject a different client type, verify exact task-prepared data, preserve
its distinct typed error, reject invalid business/startup input without client
calls, and reject a missing selected client before construction.

## Limits

Synchronous trusted code only. Registration's effect-free behavior is a protocol
verified by tests, not a Rust sandbox. No transport, serialization, workers,
async runtime, authentication, durability, idempotency, automatic retry or remote
rollback is supplied. Ordinary `Drop` records local destruction; it cannot undo
external effects. The sample receipt has only in-memory completion meaning.
