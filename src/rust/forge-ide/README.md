# Forge IDE — first project-inspection capability

**Implemented, synchronous and read-only.** This is the start of Forge IDE, not an
editor/LSP, compiler service, watcher or remote IDE. It produces a typed directory
and file tree, text line counts, and **hierarchical Rust declaration summaries
from a real syntax parser**. YAML is an optional output boundary, never an internal
request/value-map fallback.

```bash
# Repository root; fetch the locked dependencies once, then use offline targets.
cargo fetch --manifest-path src/rust/Cargo.toml --locked
make ide ARGS='inspect src/rust/forge-task'
make ide ARGS='--help'

# Equivalent Cargo command:
cargo run --manifest-path src/rust/Cargo.toml --offline --locked \
  -p forge-ide --features cli --bin forge-ide -- inspect src/rust/forge-task
```

`inspect ROOT` accepts exactly one native path argument; quote spaces at your shell
or Make boundary. No shell evaluation happens inside Forge. Prefix a path beginning
with `--` using `./`. No root/config flags, output-file write, stdin protocol or
source mutation operations are provided. Source loading uses native Rust APIs.

## Output: a tree and an outline, not just counts

The report is `model::ProjectReport`, with a root `TreeNode` named `.`. Directories
contain ordered children; files contain metadata length and typed content status.
Each successful Rust file has source-order `Declaration` nodes with kinds, names,
start/end spans and nested children, plus per-kind counts. Traits own their methods,
associated types/constants; impl blocks own their members and identify the written
trait/target type. Modules, structs, enums, unions, variants, fields, free functions,
type aliases, constants, statics and unexpanded macro names are represented.

- `impl_blocks = trait_impls + inherent_impls`; trait and impl methods do **not**
  inflate `free_functions`. Counts include only emitted outline nodes; `total`
  includes fields/variants.
- Inline modules contain declarations. `mod other;` is explicitly out-of-line;
  the parser does not load or resolve it. The directory walk may independently
  inspect its source file, without pretending it resolved a module graph.
- Spans are one-based lines and zero-based Unicode-character columns, with an
  exclusive end. They are not byte offsets or tab-expanded screen coordinates.
- Lines use Rust `str::lines().count()`: empty file = 0; `a\n` = 1; `a\n\n` = 2;
  final newline does not create a phantom line. CRLF is accepted.
- UTF-8 text of other languages gets line counts and metadata, no guessed AST.
  NUL-containing or invalid UTF-8 data is `binary`, without fabricated line counts.
  A binary `.rs` file makes the report incomplete.
- Invalid Rust or a syntax budget failure is `rust_unavailable` with a typed error
  and location where available, **not** an empty successful AST.

Function/method bodies are parsed for syntax but **not exported**, including local
functions inside bodies. Expressions, imports, extern blocks, attributes, generic
parameters and trait aliases are not separate outline nodes. There is no name/type
resolution, macro expansion, cfg evaluation, dependency fetching, build-script
execution or Cargo project compilation. Cfg-disabled source is still syntactically
seen. This is an AST-derived structural outline, not the full compiler AST.

Reports are not automatically safe to publish: identifiers, path names, normalized
impl type spelling (including possible const-generic literals), and diagnostics can
expose project information. Bodies are omitted, but that is not secret redaction.

## Modules and actual public integration

| Module | Responsibility |
| --- | --- |
| `model` | Typed tree/content/report; lossless native name representation |
| `filesystem` | Rooted native `IoClient` and typed prepared List/Read requests |
| `syntax` | Pure syn-based parser and bounded declaration extraction |
| `task` | `InspectTask<C>` traverses, prepares requests and analyzes source |
| `assembly` | Public `IdeConfig`, `InspectFactory`, `register_ide` |
| `yaml` (feature) | Bounded YAML serialization of the typed report |
| `cli` (feature) | Descriptor-derived help, typed ingress and single-shot composition |

```rust
use forge_ide::{assembly::{register_ide, IdeConfig}, task::InspectRequest};
use forge_bootstrap::Registry;
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_task::Task;

# fn inspect(root: std::path::PathBuf) -> Result<(), Box<dyn std::error::Error>> {
let group = register_ide("project.inspector", IdeConfig::project(root))?;
let pending = Registry::new().register(group).map_err(|e| e.reason().clone())?;
let frozen = pending.freeze().map_err(|e| e.reason().clone())?;
let ((), factory) = frozen.into_factories();
let provider = Singleton::new(factory.create()?);
let report = provider.acquire()?.run(InspectRequest::Project)?;
assert_eq!(report.format_version, 1);
# Ok(()) }
```

Registration checks path/config syntax and stages the factory without filesystem
I/O. Root existence/access validation and opening occur on explicit `create` after
freeze. Each factory creation owns a new root handle and task; a singleton borrows
one ready task, while a transient creates on acquisition. No registry lookup occurs
inside the task. To substitute I/O, construct the **same** `InspectTask<C>` with an
`IoClient<Request=FsRequest, Response=FsResponse, Error=FsError>` implementation.
Fake-client tests prove orchestration substitution, not native filesystem security.
The separate [consumer package](tests/consumer/Cargo.toml) imports these actual
exports, exercises failure/duplicate/provider phases, and omits the complete IDE
parser/capability dependency tree when its feature is disabled.

### CLI composition and effects

The binary is a small entry point to `cli::run`. The host calls the public registrar,
freezes, constructs the native task and wraps it in `Singleton`. Root opening is a
startup effect; directory enumeration/file reads occur only after
`ide.inspect @ 1` → shared bridge admission → expected enum kind → binding → task.
The task has no envelope/CLI dependency. CLI `ide.help` follows the same bridge but
opens no root and invokes no inspection task. A console `IoClient` writes the
complete prepared YAML result. The local admission boolean is **not production
identity/authentication**, and the bridge remains synchronous/non-shared.

Exit codes: **0** success/help, **1** operational/budget/output failure (inspection/serialization failures emit no YAML),
**2** usage error, **3** a complete YAML document describing an *incomplete*
inspection (unreadable entries, invalid Rust, oversized files). A closed stdout
pipe exits quietly with 0. Output is serialized privately before writing; output
errors can still follow partial delivery by the OS, with no rollback claim.

## Filesystem policy and limits

The existing kernel navigation client is intentionally unchanged: it allows
navigation outside its start directory and does not provide bounded file reads.
The IDE's separate client holds a capability directory. Runtime paths reject
absolute paths and `..`; every component and final file is opened no-follow.
Symlinks are reported but never traversed or read (including internal links,
escapes, dangling links and loops). The configured root's final symlink is rejected;
its parent path is trusted startup authority, resolved once. Configured roots containing
`..` are rejected too; use an explicit absolute root instead. Root handles survive
renames. This is **not** an atomic snapshot, hard-link/mount provenance policy,
OS security sandbox, permission elevation or deadline mechanism.

Names remain native: valid UTF-8 is text; other Unix names carry exact bytes,
Windows names carry UTF-16 units. Other hosted targets use opaque native-encoding
bytes. No lossy replacement-character aliases. Entries are sorted by native name
order, deterministic for an unchanged tree on that platform, not universal collation.
`.git` and `target` names are excluded at every depth (still shown as excluded);
there is no `.gitignore` parser. Other dotfiles are **not** automatically excluded.

| Budget | Default | Public config hard ceiling |
| --- | --- | --- |
| Entries, root and excluded nodes included | 10,000 | 100,000 |
| Directory depth (root = 0) | 32 | 64 |
| Per-file bytes | 1 MiB | 4 MiB |
| Total read budget | 32 MiB | 256 MiB |
| Declaration nodes per Rust file | 20,000 | 100,000 |
| Syntax outline / token-group depth | 32 | 64 |
| Real source tokens per Rust file | 65,536 | fixed |
| YAML output (CLI) | 16 MiB | serializer accepts up to 64 MiB |
| CLI arguments | 64 KiB / small fixed grammar | fixed |

Directory/aggregate budget exhaustion rejects the invocation, rather than returning
an unexplained truncated tree. Oversized files are recorded without reading.
File reads use fixed chunks and at most one over-limit sentinel byte per attempt;
failed reads conservatively charge their entire requested budget because the
failure may follow reading. `bytes_read` reports successful bytes only. No hidden
business retry. Files can change between metadata/read and between directories;
reported size and read length may differ. Reads/native calls can block.

The real token lexer has iterative token/group preflight before syn and the outline
has node/depth budgets. These are defense in depth, **not** a universal stack/CPU
limit for syn's recursive parser: adversarial non-delimited expression/type chains
can still be problematic. Use this initial tool on trusted local projects. No
parser process isolation, cancellation or timeout is implemented. Serializer byte
bounds likewise do not promise all internal allocations are limited to output size.

## Features, sources and verification

Default features are empty. The capability imports only Task/I/O/bootstrap/provider
contracts plus the version-locked parser/rooted-filesystem libraries. `yaml` adds
Serde + serde_yaml_ng; `cli` adds YAML and description/bridge/ingress contracts.
No core or existing terminal/demo depends on Forge IDE. The YAML serializer supports
YAML 1.1; no JSON intermediate representation is used.

Primary library documentation: [syn 2.0.118](https://docs.rs/syn/2.0.118/syn/)
explains syntax-tree parsing; [proc-macro2 span locations](https://docs.rs/proc-macro2/1.0.107/proc_macro2/struct.Span.html)
document source locations outside procedural macros;
[cap-std directory capabilities](https://docs.rs/cap-std/4.0.3/cap_std/fs/struct.Dir.html)
and [cap-fs-ext no-follow operations](https://docs.rs/cap-fs-ext/4.0.3/cap_fs_ext/trait.DirExt.html)
provide rooted access APIs;
[serde_yaml_ng](https://docs.rs/serde_yaml_ng/0.10.0/serde_yaml_ng/)
defines the YAML boundary. The package design and policies above are Forge choices,
not claims those dependencies provide an IDE or fully sandbox arbitrary source.

Run `make ci` for full/default profiles, strict lints/docs and dependency guard;
`cargo test --manifest-path src/rust/Cargo.toml -p forge-ide --all-features` selects
this slice. Coverage includes real temporary-filesystem traversal, symlink swaps,
native names, bounds, syntax hierarchy/spans, fake-client substitution/failure,
public consumer import/provider/conflict checks, real CLI YAML and rejected ingress.
Permission-dependent checks only exercise denial where host permissions enforce it;
a special-file fixture explicitly reports a skip when socket creation is denied.
Linux is the tested host, not evidence of a Raspberry Pi image or Windows conformance.

[Implemented flow diagram](../../../docs/architecture/diagrams/mermaid/README.md#implemented-forge-ide)
