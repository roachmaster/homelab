# Forge local application

A runnable **synchronous local application**, separate from the execution/description
contracts and from its terminal adapter. It composes `forge-kernel::node` and an
optional `forge-kernel::disk` through typed configuration and package registrars.
The application owns its finite `Command`/`Output` enums; core has no business enum.

## Bounded local jobs (optional)

The [jobs guide](JOBS.md) documents the implemented counter lifecycle, typed
registrar/factory, explicit stepping, owner-scoped receipts, finite limits and feature
omission. `bootstrap_with_jobs` opts in; existing bootstrap remains node/disk-only.
No background progress, production authentication or durable acceptance is claimed.

## Assembly and requests

1. Prepare `LocalConfig` at the environment boundary. The library reads no env vars.
2. Call the selected node registrar and, only when selected, the disk registrar.
3. Register complete typed groups, validate/freeze, then create clients explicitly.
4. Own each ready client in a per-application singleton and inject handles into tasks.
5. Register bindings together with their command descriptions, freeze and publish.

No partially assembled application escapes. A later disk construction failure drops
the earlier node client. Drop is ordinary synchronous ownership cleanup, not an
async shutdown protocol or rollback. Selecting a replacement node registrar skips
the native registrar entirely; `bootstrap_with_node_registrar` takes the same typed
`NodeConfig` and a compatible `PreparedGroup<NodeFactories<F>>`. Existing `NodeTask`
logic is unchanged. Registrars/factories remain trusted Rust code, not an effect sandbox.

`Application::dispatch` accepts an explicitly constructed typed envelope.
`invoke` is a convenience that creates a bounded-counter local request ID, then
uses that same checked dispatch. The header selects a registered destination/version;
variant mismatch is rejected before task/client calls. IDs are not durable or
idempotency keys. Local admission allows this process's calls: it provides **no
production authentication**, remote authorization or filesystem sandbox.

`system.commands` is itself an admitted route. Each descriptor is appended only
when its binding registers successfully. A duplicate adds neither a route nor a
menu entry. There is no independent list of advertised capabilities or public
unchecked handler lookup. Terminal help obtains this same catalog. `CommandInfo` carries a reference to
its authoritative `CommandDescriptor`, not a second handwritten usage/help copy.
Node/disk tasks opt into `DescribeCommand`, with one descriptor per enum operation;
`DiscoverCommands` uses the same trait without implementing `Task`. Registration
selects an exact declared name, validates the whole description set and that
operation's argument names/types, then derives the route from the descriptor.
Missing/ambiguous/invalid metadata is a typed bootstrap error; duplicate routes
preserve the first binding and catalog entry. Static descriptors remain trusted,
effect-free declarations, not Rust reflection or a permission grant.

Required/optional/default declarations describe the command-facing input contract.
For example, terminal omission of `disk.list`'s optional `path` becomes a concrete
`PathBuf(".")` in its typed task input. `follow_symlinks` is a Boolean defaulting
to true; the terminal maps its `--no-follow` flag to false. The Rust enum fields
remain strongly typed and the adapter mappings are checked by tests, not derived
by a universal deserializer. Help options live in terminal presentation metadata,
not those business input enums.

## Selected commands

| Destination @ version 1 | Meaning |
| --- | --- |
| `system.commands` | Actual selected registered commands and usage |
| `node.info` | Configured label and compile-target OS/architecture |
| `node.status` | This node client responded; elapsed client-instance uptime |
| `disk.pwd` | Current client-local directory |
| `disk.list` | Native directory entries, sorted by native name |
| `disk.cd` | Change this application's client-local directory |
| `disk.permissions` | Native permission metadata, not effective access |

The last four exist only with the `disk` Cargo feature **and** selected startup
configuration. A no-default-features build compiles the kernel's `node` module but
not its `disk` module. Both share one cohesive optional kernel crate; omission is
a module/feature proof, not a claim that the entire kernel package vanishes.
The older pure demo still excludes the entire kernel and I/O/provider packages.

Node status is not machine boot uptime, CPU/memory telemetry, disk readiness or
whole-host health. Configured node IDs are labels, not authenticated identities.
Disk navigation does not change process cwd; failed or rejected cd preserves the
client state. The starting directory is not a security root.

## Run

See [terminal usage](../forge-terminal/README.md). From `src/rust`:

```bash
cargo run -p forge-terminal -- --help
cargo run -p forge-terminal -- --node-id workstation node.info
cargo run -p forge-terminal -- --session
cargo run -p forge-terminal --no-default-features -- node.status
```

Tests cover real native disk fixtures, independent sessions, header/variant checks,
truthful injected status/factory failures, retained error causes, construction
cleanup, selected route coverage and replaceable node observations. Fake provider
checks are not a remote-adapter certification. No remote-node protocol,
concurrent bridge, destructive disk operations or OS boot support is implemented.
