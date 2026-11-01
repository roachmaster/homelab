# Forge template CLI

A single-shot binary **inside `forge-template`**, enabled only with the `cli`
feature. It renders text to stdout or lists/help-describes its operations; it is
not a project generator, shell, daemon or new `forge-terminal` command.

## Run

From the repository root:

```bash
make template                         # help
make template ARGS='methods'           # the 38 builtins, sorted
make template ARGS='render --template "Hello, {{name.trim().pascal_case()}}!" --text "name=ada lovelace"'

# Direct Cargo equivalent:
cargo run --manifest-path src/rust/Cargo.toml --offline --locked \
  -p forge-template --features cli --bin forge-template -- \
  render --template 'Total: {{price.multiply(2).round()}}' --float price=12.75

# Load an existing UTF-8 template without modifying it:
cargo run --manifest-path src/rust/Cargo.toml --offline --locked \
  -p forge-template --features cli --bin forge-template -- \
  render --template-file greeting.forge --text name=Forge
```

The first render prints `Hello, AdaLovelace!`; the numeric example prints
`Total: 26`. Template output is exact: **no newline is added**. `make template`
prints its command diagnostic on stderr, leaving stdout for the result. Your
invoking shell performs its own quoting; the program never evaluates shell syntax.

Build once with `cargo build --manifest-path src/rust/Cargo.toml -p forge-template
--features cli --bin forge-template --offline --locked`, then invoke
`src/rust/target/debug/forge-template`. The binary's main only passes OS arguments
to `forge_template::cli::run` and maps the returned exit code.

## Inputs and discovery

| Input | Meaning |
| --- | --- |
| `render --template TEXT` | Inline source; choose exactly one source option |
| `render --template-file PATH` | Native regular-file source; symlinks followed |
| `--text NAME=VALUE` | Named text, including an empty value |
| `--int NAME=VALUE` | Signed i64; overflow is an error |
| `--float NAME=VALUE` | Finite f64; NaN/infinity rejected |
| `--bool NAME=true` or `NAME=false` | Exact lowercase boolean spelling |
| `--null NAME` | An explicit null field; no `=VALUE` operand |
| `methods` | Registered default builtin names, sorted, one per line |
| `--help`, `help`, `help render`, `render --help` | Admitted metadata; no template read/render |

`help methods` and `methods --help` are also supported. Binding options repeat,
but a name may occur only once across all types. Names are flat ASCII identifiers
(letter/underscore, then letters/digits/underscores); dotted/nested assignments
are not accepted. Exactly one source is required for rendering, but not for help.
Duplicate source/help flags, unknown flags/commands, malformed values and trailing
arguments fail rather than being silently ignored. Help does not hide malformed
supplied flags or values. Option operands are literal: `--text name=--help` is data.

The task creates an explicit object schema from the selected types; each supplied
field is required. No type inference, JSON input, schema-file format, nested/list
binding syntax, stdin source or dynamic custom-method loading is implemented in
this first CLI. **The library still supports lists, nested objects, sections and
custom Rust methods**; the CLI input surface does not remove those capabilities.
See [method semantics](METHODS.md) for precision, truthiness and escaping rules.

## Execution and ownership

```text
OS argv -> bounded parser -> typed Command + explicit header route
        -> shared Bridge: route/version -> local admission -> payload kind
        -> render binding acquires the host's singleton RenderTask
        -> task validates bindings, prepares native read request if needed
        -> injected Source IoClient reads bounded UTF-8 text
        -> typed RendererFactory port receives RenderConfig
        -> composition's RegisteredRendererFactory calls public registrar
        -> stage / validate / freeze -> RenderFactory::create (exactly once)
        -> invocation-owned Renderer validates/renders -> complete String
        -> console IoClient writes stdout
```

The application selects and constructs the workflow task through a typed factory
group before publishing route bindings. Its task owns preparation/orchestration,
not native I/O. A small **typed construction port**, not an opaque callback or
service locator, lets composition own the public registrar/factory protocol.
Source loading is admitted work, so renderer configuration becomes available only
then. Consequently the configured renderer is constructed per command, after
source loading; it is not a startup renderer cache or global container. The host
owns the task provider; the task owns and drops the invocation's renderer. Errors
stop the sequence, retain typed causes internally, and are not retried.

Command descriptions are the shared source for registered operation names and
help metadata. Help is its own admitted typed command and performs no source
client or render-task call. The local admission flag is trusted host policy for
this local executable, **not authentication or filesystem authorization**. The
synchronous bridge/clients do not add concurrency, cancellation or timeouts.
Construction substitution tests do not claim arbitrary rendering-service replacement.

## Bounds and failures

- At most 256 argv operands, 128 bindings and 2 MiB total native argument bytes.
  The OS can impose lower argv limits before this process starts.
- Default renderer budgets apply: 1 MiB source, 1 MiB schema/data bytes, 1 MiB
  final output, bounded nesting/nodes/work. CLI budget overrides are not exposed.
- Native reading uses fixed chunks and at most one sentinel byte beyond the source
  limit; it does not allocate from an untrusted file-size hint. UTF-8 is required.
  File paths preserve native non-UTF-8 bytes; all other arguments require UTF-8.
- Regular-file checks reject ordinary directory/device/FIFO paths. Symlinks are
  followed, relative paths use the process cwd, and the process cwd is never changed.
  Metadata/open races are **not** sandbox protection. Native/network filesystems
  can block; byte limits are not wall-clock deadlines or exact heap-size guarantees.
- No create/delete/chmod/output-file API, shell subprocess or source installation
  exists. Test fixtures create and remove only their own temporary directories.

Exit codes: **0** success/help/methods, **2** usage or typed-argument errors,
**1** file/schema/template/construction or console failure. A closed downstream
stdout pipe (`BrokenPipe`) is quiet success, as for an intentionally stopped
pipeline. A failed stderr diagnostic is still failure. Rendering must finish
before stdout writing starts: render errors emit no partial document. An OS
output error can nevertheless follow partial byte delivery; there is no rollback
of stdout. No automatic newline, hidden retry or output-path overwrite policy exists.

## Verify and keep library consumers CLI-free

```bash
make test PACKAGE=forge-template FEATURES=cli
make lint PACKAGE=forge-template FEATURES=cli
make test PACKAGE=forge-template NO_DEFAULT_FEATURES=1
make ci
```

Default library consumers retain only `forge-bootstrap` and `forge-provider`.
The `cli` feature additionally selects Task, command description, I/O-client,
bridge and ingress contracts; it does not pull in `forge-local`, `forge-terminal`
or kernel clients. The binary has `required-features = ["cli"]`. Existing separate
consumer tests still prove public assembly imports and whole-capability omission.
The dependency guard checks both profiles, and rejects accidental default CLI or
mandatory CLI-only dependencies. Tests execute the real binary, native bounded
file reads and closed stdout pipes, alongside injected-client/factory and bridge
rejection tests. No remote service or user filesystem mutation is part of those tests.
