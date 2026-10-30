# forge-task

`Task` describes one synchronous capability operation through associated `Input`,
`Output`, `Error` and `run(&self, input)`. It has **no normal dependencies**, including
no I/O-client, bridge or provider dependency. A pure task therefore needs neither
an I/O library nor an ingress envelope. This is not a scheduler or durable job.

A client-using task implementation in a consumer capability imports the narrow
client contract it needs and receives clients by constructor injection. It owns
business request assembly, calculations, sequencing and any failure policy;
concrete clients own I/O execution and necessary boundary encoding. Generic bounds
or trait objects with fixed associated types allow replacement without changing
the task. Do not inject a service registry or put transport-specific calls inside
its policy. Pure computation is an implementation property, not enforced by the
trait or a ban on all client-using tasks.

The test-only dependencies demonstrate pure tasks, fetch-then-send orchestration,
client substitution, exact prepared payloads, original typed error causes and
existing factory/provider injection. They do not imply real I/O conformance.
Sequential calls are not an atomic transaction; errors/panics may follow earlier
effects. No async runtime, automatic retry, cancellation or worker lifecycle is
provided. A composition-owned bridge binding can extract a validated enum variant
and call `run`; the task itself need not know the header or bridge.

From **`src/rust/`**, using the [workspace toolchain](../README.md):

```bash
cargo fmt --all -- --check
cargo test -p forge-task --offline --locked
cargo clippy -p forge-task --all-targets --offline --locked -- -D warnings
cargo run -p forge-bridge --example task_clients --offline --locked
```

The two tiny contract crates are separate to preserve independent consumers:
client adapters need no task model, and pure tasks need no I/O model. This is an
original interface, not a compatibility facade over older Forge task executors.
