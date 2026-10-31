# forge-kernel — optional hosted system clients

The first module, **`disk`**, provides native filesystem inspection and a
client-owned working directory. The name groups related low-level system clients;
it is **not a bootable operating-system kernel**, `no_std` runtime or Raspberry Pi
image. It uses hosted Rust `std::fs`, `std::path` and metadata APIs—not Bash,
shell commands, subprocesses or command-output parsing. No other system-client
placeholder modules are added.

This optional implementation crate depends only on `forge-bootstrap`,
`forge-provider` and `forge-io-client`. None of the six core packages imports it.
Tasks remain consumer-owned; a client executes the caller's already prepared
`DiskRequest` rather than assembling an automation workflow.

## Implemented operations

| Request | Result / behavior |
| --- | --- |
| `Pwd` | `WorkingDirectory(PathBuf)`: last successfully committed physical location |
| `List { path }` | `Listed(DirectoryListing)`: nonrecursive, sorted native names, absolute entry paths, type, native metadata length and permissions |
| `ChangeDirectory { path }` | `DirectoryChanged(PathBuf)`: canonicalizes/validates the directory before committing this client's cwd |
| `Permissions { path, follow_symlinks }` | `Permissions(PermissionInspection)`: file kind, native readonly indicator, and Unix permission/special bits where supported |

There is no create, delete, read-content, write-content, chmod, recursive walk or
shell execution API. Creating/removing files in tests and the runnable example
is fixture setup/cleanup, not client functionality.

## Configuration, registration and injection

`DiskConfig::Native { initial_directory }` requires an explicit absolute path.
The application chooses it; neither registrar nor factory reads environment
variables or the process cwd. Requiring an absolute startup value removes ambient
working-directory dependence; it does **not** restrict subsequent navigation.

`disk::register(config)` validates syntax and returns a
`PreparedGroup<DiskFactories>` with binding `kernel.disk.client`. It performs no
filesystem access. After application registration/freeze,
`DiskFactories::client.create()` checks the current filesystem, resolves symlinks
and requires an existing directory. Missing paths and non-directories are typed
construction errors. Registration can therefore succeed before the directory
exists; construction cannot silently fall back elsewhere.

Each factory call makes a fresh `DiskClient` session. `Singleton` retains that
session; `Transient` constructs one on acquisition, so each acquisition starts
again at the configured location. Choose the scope deliberately for interactive
navigation. Sharing one client intentionally shares its cwd; independently
constructed clients never share it. Existing providers remain responsible for
lifetime, not command routing.

The [optional consumer](../examples/forge-bootstrap-demo/src/disk.rs) stages the
registrar, freezes the group, constructs a provider and injects a ready typed
client into a task and checked bridge bindings. Its list-current task prepares
`List { path: "." }`; a fake client can substitute without changing that task.
No envelope/bridge/task dependency is added to this implementation crate.

## Path, symlink and concurrency contract

- Relative requests resolve against this client's cwd; absolute paths and `..`
  are allowed. Empty paths are errors. Ambiguous Windows drive-relative and
  root-relative forms are rejected rather than consulting ambient drive state.
- Initial cwd, successful cd and list targets use canonical physical directories.
  The OS resolves symlinks and `..`; there is no lexical collapse that changes
  traversal meaning. Failed cd leaves cwd unchanged.
- List follows a symlink used as its target directory, but reports each final
  entry with `symlink_metadata`, so dangling links remain visible. Permission
  inspection explicitly chooses whether to follow the final component;
  intermediate components still resolve normally. A trailing separator may
  impose native directory traversal semantics; this is not a no-follow security
  primitive. [Rust symlink metadata](https://doc.rust-lang.org/std/fs/fn.symlink_metadata.html)
- Paths/names stay `PathBuf`/`OsString` without lossy conversion. Entries sort by
  native `OsString` ordering, not locale or cross-platform collation. All iterator
  and metadata failures propagate; no partial listing is returned. Listings are
  not atomic snapshots. [Rust directory iteration](https://doc.rust-lang.org/std/fs/fn.read_dir.html)
- A per-client mutex serializes operations, not all filesystem activity. The
  client never calls `set_current_dir`. Stored pwd is not an open directory handle:
  external rename/removal can make it stale; later operations may fail. An
  absolute cd can recover to another existing directory.

## Permission and safety limits

**The initial directory is not a sandbox root.** Navigation can leave it via
absolute paths, parents or symlinks. Operations run with the host process's
permissions. Canonicalization plus metadata checks do not prevent symlink races
or enforce authorization. A remote/multi-user host needs its own security design
before exposing this client; demo admission is not production authentication.

`readonly` reports native metadata, **not effective access**. Unix mode is
`mode & 0o7777`; other platforms report `None`. ACLs, group membership,
capabilities and mount policy are not evaluated. Directory existence/type checks
also do not guarantee later listing/search access; the OS decides each call.
The client never changes permissions. [Rust permission semantics](https://doc.rust-lang.org/std/fs/struct.Permissions.html)

`DiskError::Io` retains operation, native path and the original `std::io::Error`
(including kind/OS code); missing versus denied access is not flattened into an
empty directory. Non-directory, invalid-path and poisoned-state cases are
explicit. Path diagnostics can be sensitive: embedding hosts decide what to
expose. Calls block and can fail after the filesystem changes; there are no
hidden retries, deadlines, cancellation or transactional guarantees. Client state
is protected by Rust ownership/mutexes, not a promise to catch arbitrary panics.

## Check and run

From `src/rust/`:

```bash
cargo test -p forge-kernel --offline --locked
cargo test -p forge-bootstrap-demo --no-default-features --features disk --offline --locked
cargo run -p forge-bootstrap-demo --example disk_access --no-default-features --features disk --offline --locked
python3 scripts/check_dependencies.py
```

The example operates only in its uniquely created temporary fixture and cleans
that up. It does not take a user directory argument or modify process cwd.
Native tests cover registration-before-I/O, directory validation, ordering,
state isolation, failed navigation, provider lifetimes, typed errors and metadata.
Unix tests additionally cover symlinks and non-UTF8 names. Tests run on Linux;
Windows-specific behavior and Raspberry Pi deployment are not certified here.
