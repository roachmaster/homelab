# Forge durable jobs

`authenticated_bridge` accepts a context issued by `forge_core::access` after a
configured verifier authenticates the caller. Configure exact route grants
with `RoutePolicy`. The store derives the owner from the verified subject and
checks ownership on status and cancellation.

`forge-jobs` exposes typed `jobs.submit`, `jobs.status`, and `jobs.cancel`
through the existing Forge bridge. The host supplies its admission policy
and a trusted owner context. Submission returns a stable receipt after a
durable commit. Identical owner, intent key and work return the same receipt.

Use `register_store` to stage a store factory in a host's bootstrap registry.
After freezing the registry, create a store from that factory and wire it
into `bridge`. Give the factory an existing private local directory.
The file store serializes independent processes with a file lock. Each
commit syncs a fresh snapshot, renames it into place, and syncs the
directory before acknowledging the request. Reopening that directory
recovers receipts, work and committed results.

`register_worker` stages a worker recipe that requires the store binding.
After freeze, bind the acquired store to that recipe and create the owned
worker. The first job sums
the integers from one through a bounded input. Claiming assigns an attempt
and a lease. An expired attempt can be claimed again within the configured
retry budget. A cancelled or expired attempt cannot publish a result.
`Worker::shutdown` joins the worker and reports its outcome.

The store accepts a configured maximum of 4,096 records. Receipts remain
available over restarts. The first implementation does not perform
automatic retention or expiration of completed receipts.

The Unix `forge-jobs` command is available through `forge-terminal`'s
optional `durable-jobs` feature. Use an existing private directory owned by
the current effective Unix user. The operator verifier derives the subject
from that account, and the bridge checks the subject on every command. The
store validates privacy on the directory handle it uses for job files.
Run the foreground worker under the same account and point it at the same
directory. A contested store returns a busy error to CLI requests after a
short wait. The worker retries contention and joins on `SIGINT` or `SIGTERM`.

```sh
cargo build --manifest-path src/rust/Cargo.toml -p forge-terminal --features durable-jobs --bin forge-jobs
src/rust/target/debug/forge-jobs JOB_DIRECTORY submit first 10
src/rust/target/debug/forge-jobs JOB_DIRECTORY worker
# From another terminal while the worker is running
src/rust/target/debug/forge-jobs JOB_DIRECTORY status 1
```

Run the integration tests with a Rust toolchain that supports the
standard library file locks.

```sh
make test PACKAGE=forge-jobs
```

The runnable example keeps the same receipt and result across separate
processes. Pass an existing private directory in place of `JOB_DIRECTORY`.

```sh
cargo run --manifest-path src/rust/Cargo.toml -p forge-jobs --example sum -- JOB_DIRECTORY submit first 10
cargo run --manifest-path src/rust/Cargo.toml -p forge-jobs --example sum -- JOB_DIRECTORY execute 1
cargo run --manifest-path src/rust/Cargo.toml -p forge-jobs --example sum -- JOB_DIRECTORY status 1
```
