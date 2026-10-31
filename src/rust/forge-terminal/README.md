# Forge terminal

The first runnable, **local-only** Forge front door. This std-only adapter depends on the application facade and I/O-client contract, not on concrete kernel clients or the bridge implementation. Its native console implements typed `IoClient`; injected streams/fakes exercise the same parser and presentation code. Tasks remain in the application/capability layer and prepare requests before clients execute them.

From `src/rust`:

```text
cargo run -p forge-terminal --bin forge -- --help
cargo run -p forge-terminal --bin forge -- --node-id home-node node.info
cargo run -p forge-terminal --bin forge -- node.status
cargo run -p forge-terminal --bin forge -- system.commands
cargo run -p forge-terminal --bin forge -- --cwd /some/existing/directory --session
```

Replace the example directory with a real directory you want to inspect. No create/delete/permission-change operation is exposed. The default disk start is the current process directory; an explicit relative `--cwd` is resolved once at the terminal boundary. It is **not a sandbox root**: native absolute paths, `..` and symlinks remain available under the process's OS credentials. The application never changes process-global cwd.

Example session:

```text
node.info
node.status
disk.pwd
disk.list
disk.cd child directory
disk.pwd
disk.permissions --no-follow some link
help
exit
```

A session shares one application's disk provider, so successful cd persists until exit. Failed cd leaves that client's prior directory intact. Each new invocation starts a fresh application; a one-shot cd does not change your shell or subsequent invocations.

## Omit disk

```text
# Selected at startup: no disk registrar, client or routes in this assembly.
cargo run -p forge-terminal --bin forge -- --no-disk --help
# Selected at compile time: disk modules are absent from this binary's feature graph.
cargo run -p forge-terminal --no-default-features --bin forge -- --help
```

`--no-disk` is also accepted by a node-only build; `--cwd` is rejected there. `--cwd` and `--no-disk` conflict. All startup options precede the command, and `--session`/`--help` end the options. The application owns the chosen capability implementations, factories/providers and typed startup configuration; the terminal does not resolve services.

## Parsing, output and failures

- Help and discovery come from the selected registered metadata through the admitted `system.commands` application command. The adapter selects that route descriptor first, decodes its expected typed command kind, then invokes the same checked bridge. It never calls a task/client directly or guesses a destination from a payload.
- One-shot argv preserves native `OsString` paths, including non-UTF-8 paths where the host permits them. The shell invoking the binary may perform its own quoting/expansion; Forge does not.
- Sessions accept UTF-8 lines up to **4096 bytes** excluding LF/CRLF. A command is followed by its literal remaining path, with surrounding whitespace trimmed. Spaces inside paths are retained. Quotes, backslashes, `$HOME`, `~`, semicolons and pipes are **not shell syntax**. A path with leading/trailing whitespace or non-UTF-8 bytes needs argv. `disk.permissions` alone recognizes the leading `--no-follow` option; use an explicit `./` prefix to disambiguate a similarly named literal session path.
- `disk.list` accepts zero or one path (default `.`). `disk.cd` requires one path. `disk.permissions [--no-follow] PATH` requires one path. Node and discovery commands accept no arguments. `help` is an adapter shortcut to admitted discovery; `exit`/`quit` only end a session.
- Native names/paths are quoted encoded-byte representations: controls and non-ASCII bytes appear as `\xNN`, rather than being lossy-decoded or written as terminal escape sequences. Listings keep the native client's deterministic ordering, which is not cross-platform collation. This is text output, not a promised machine wire format.
- Status reports only that the configured node client answered and elapsed time since its construction. Target OS/architecture are compile-target constants, not hardware probes. This is not global health/readiness, OS uptime or authenticated machine identity. Permission metadata is not effective-access authorization.
- Exit codes: **0** success, **2** malformed input/invalid configuration/unavailable command, **1** construction/application/console I/O failure. Sessions continue after command or rejected-line errors and return the highest command-error code seen; a console transport failure stops immediately with 1. EOF and `exit` otherwise finish successfully. Console transport failures stop execution; writes may have partially completed. No hidden retry, timeout, cancellation or rollback is provided.

The local admission policy is not production authentication or a security boundary. This milestone has no jobs, process launcher, network/remote transport, async runtime or bootable OS behavior. See the [application assembly](../forge-local/README.md) and [kernel plan](../../../docs/architecture/forge-kernel-plan.md).

## Check

```text
cargo test -p forge-terminal
cargo test -p forge-terminal --no-default-features
cargo clippy -p forge-terminal --all-targets --all-features -- -D warnings
```

Tests cover real binary invocation and a stateful binary session, typed console substitution, bounded/rejected input, malformed commands, discovered omission, native filesystem fixtures, failed-cd preservation, literal paths and terminal-control escaping. Only test-owned temporary directories are created/removed.
