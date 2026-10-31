# forge-command

Static, transport-neutral command descriptions with **zero normal dependencies**.
`DescribeCommand::descriptors(&self)` returns a static slice of operation names,
help text and named arguments. Each `ArgumentDescriptor` declares help, semantic
`DataType` and required/optional `Presence`, including an optional display default.
`DataType` supports booleans, text, paths, signed/unsigned 64-bit integers, 64-bit
floating-point values and named capability-owned concepts.

Description and execution are independent traits. A `Task` can implement
`DescribeCommand` when exposed as a command, but pure tasks need no command
metadata and non-task providers can describe commands. Neither crate depends on
the other. An enum-multiplexing capability returns **one descriptor per exposed
operation**, with that operation's arguments—not one flattened list of every enum
variant's fields. No framework-global business enum or metadata supertrait is
required.

Implementations return static values without running tasks, invoking clients,
acquiring resources or performing live discovery. Rust cannot enforce this purity
contract. Composition selects metadata for successfully registered bindings; this
crate provides no execution, registration, authorization, parser or serializer.

`CommandDescriptor::validate` checks nonblank names, help and custom type labels,
and duplicate argument names. `validate_descriptors` additionally rejects an
empty set or duplicate operation names. Empty argument lists are valid. Blank
means empty or entirely Unicode whitespace; duplicate identity is exact and
case-sensitive. Names are not normalized and no transport-specific grammar is
imposed. Validation is explicit, allocation-free, deterministic and quadratic
in catalog size, intended for small static declarations at assembly time.

These are **declarations, not Rust input reflection or a complete schema**.
Validation cannot prove agreement with a capability's typed input, route, parser,
resource permissions or business rules; consumer tests must do that. A default
is descriptive text, not parsed, applied or checked against `DataType`; even an
empty text default is legitimate. A `Path` is not inherently UTF-8 and does not
promise a path exists. Adapters still own conversions and presentation escaping.

Help text on each operation/argument is business metadata. An adapter's `--help`
and `-h` argument definition belongs to the adapter separately (it may reuse
`ArgumentDescriptor`), not a mandatory help field in every business input. The
metadata does not prescribe positional arguments, flags or wire formats.

From `src/rust/` with the workspace toolchain:

```bash
cargo test -p forge-command --offline --locked
cargo clippy -p forge-command --all-targets --offline --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p forge-command --no-deps --offline --locked
```

The runnable crate example and integration tests cover non-task description,
multiple operations, optional/default metadata, all type labels and validation
failures. They do not claim to exercise an executable parser or live I/O.
