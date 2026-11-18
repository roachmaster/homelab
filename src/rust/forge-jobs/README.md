# Forge durable jobs

`authenticated_bridge` accepts a context issued by `forge-access` after a
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

`Worker::start` begins the owned worker for that store. The first job sums
the integers from one through a bounded input. Claiming assigns an attempt
and a lease. An expired attempt can be claimed again within the configured
retry budget. A cancelled or expired attempt cannot publish a result.
`Worker::shutdown` joins the worker and reports its outcome.

The store accepts a configured maximum of 4,096 records. Receipts remain
available over restarts. The first implementation does not perform
automatic retention or expiration of completed receipts.

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
