# Forge template

A **std-only, optional rendering capability**: parse once, validate typed
context/schema, and render to a `String`. It does not read templates from disk,
write generated files, invoke commands, discover modules or install source code.
Default builtins are pure; explicitly supplied custom methods are trusted callbacks.
No existing core contract, local command or terminal dependency changes.

## Use

```rust
use std::collections::BTreeMap;
use forge_template::{Field, Schema, Template, Value};

let schema = Schema::Object {
    fields: BTreeMap::from([("name".into(), Field {
        schema: Schema::Text, required: true,
    })]),
    allow_unknown: false,
};
let input = Value::Object(BTreeMap::from([
    ("name".into(), Value::Text("Ada <3".into())),
]));
let template = Template::parse("Hello, {{name}}!")?;
assert_eq!(template.render(&input, &schema)?, "Hello, Ada &lt;3!");
# Ok::<(), forge_template::TemplateError>(())
```

Run from the repository root:

```bash
make test PACKAGE=forge-template
make lint PACKAGE=forge-template
cargo run --manifest-path src/rust/Cargo.toml --offline --locked \
  -p forge-template --example registered_render
make ci
```

The [runnable composition example](examples/registered_render.rs) uses a typed
`RenderInput`, a pure `Task`, independent `DescribeCommand` metadata, a typed
config enum and registrar, `Factory`, `Singleton` and the shared header bridge.
Registration stores recipes; freeze precedes parse/construction. Admission and
variant checks precede acquisition/execution. Help is a separately admitted
metadata binding. Integration tests exercise that exact example, including
failure before publication and preserved renderer error causes.

The default [`greeting.forge`](examples/greeting.forge) is ordinary UTF-8 template
text embedded with Rust `include_str!`; the extension is a convention, not a
build-script/compiler requirement. The renderer never opens the file at runtime.

Those contracts are **dev dependencies only**. The renderer's normal dependency
graph contains only `forge-template`; the local application, terminal and pure
consumer do not acquire it. Rendering is computation, so no `IoClient` is invented.
An application task may prepare template values; a separately chosen client may
later persist the completed string. That I/O/installation policy is not here.

## Method extensions—not just Mustache

The renderer now includes **all 38 historical builtin method names**, chaining,
literal/path arguments and an explicit custom `MethodRegistry`. Examples:
`{{name.trim().pascal_case()}}`, `{{items.join(", ")}}`,
`{{price.multiply(1.2).round()}}` and application-defined `{{name.initials()}}`.
See the [complete method inventory, grammar and trust contract](METHODS.md).

```bash
cargo run --manifest-path src/rust/Cargo.toml --offline --locked \
  -p forge-template --example method_extensions
```

## Supported syntax and semantics

| Form | Meaning |
| --- | --- |
| `{{name}}`, `{{user.name}}` | Declared scalar, HTML escaped |
| `{{{name}}}`, `{{& name}}` | Explicit unescaped scalar |
| `{{#items}}…{{/items}}` | Iterate a list; otherwise enter one truthy value |
| `{{^items}}…{{/items}}` | Render in the existing scope when absent/falsey |
| `{{.}}` | Current scalar context |
| `{{! comment }}` | Ignore tag; surrounding whitespace is retained |

Tags trim surrounding whitespace. Names use ASCII identifiers (letter/underscore,
then letters/digits/underscores), separated by dots. Unicode **text values and
literal template text** are preserved. Unicode identifiers and unsupported expressions
return typed syntax errors, not panics. Method-call syntax is documented separately. No standalone-line stripping is performed.

Falsey means null, false, empty text or empty list. Integer/float zero and an empty object
are truthy. Scalars render as text, decimal `i64`/finite `f64`, `true`/`false`, or empty for null.
Lists/objects must use sections; they are not stringified implicitly.

Lookup searches the current then parent **schema scopes** for a declared complete
path. If a scope does not declare the complete dotted path, lookup can fall back
to an outer scope declaring that complete path. The corresponding data supplies
its value. An optional field may be absent;
its interpolation is empty and its normal section is skipped. An absent declared
inner field shadows an outer field; it does not fall back to unrelated outer data.
Optional does not mean nullable. Undeclared paths fail, even with `allow_unknown`
data or inside a skipped branch/empty list.

`Schema` explicitly models text, bool, i64, finite f64, null, lists and object fields.
There is no inferred schema or arbitrary JSON Schema compatibility; numeric
conversion is explicit through the documented methods.
Object fields may be required/optional; unknown data fields are rejected unless
explicitly allowed. Unknown allowed data is still budget-checked and cannot be
referenced without a declaration. The full context and statically known expression shapes are validated before
rendering; dynamic method-result shapes are checked at runtime as described in
[METHODS.md](METHODS.md). A failed call returns **only an error**, never a
partial string; the immutable template remains reusable.

HTML escaping replaces `&`, `<`, `>`, `"` and `'`. It is **not** a contextual HTML
sanitizer or a Rust/SQL/shell/URL/string-literal encoder. Raw tags bypass escaping.
Rendering a source file safely requires an explicit source-language encoder and
validation owned by a future consumer; neither escaped nor raw output promises
safe code generation. No output is automatically executed or written.

## Resource bounds

`Template::parse_with_limits` stores explicit `Limits`; `parse` uses defaults.
Limits must be positive and no greater than the caps below.

| Budget | Default | Maximum configurable |
| --- | ---: | ---: |
| Source bytes | 1 MiB | 16 MiB |
| Nesting depth | 32 | 64 |
| Nodes, separately for syntax/schema/data | 10,000 | 100,000 |
| Key/text bytes, separately for schema/data | 1 MiB | 16 MiB |
| Schema-match + expression/method/render work units | 100,000 | 1,000,000 |
| Output bytes, after escaping | 1 MiB | 16 MiB |

Comments/closing tags consume syntax budget. Repeated schema matching, scope/path
lookups and empty-body list iterations consume work budget. Library errors identify syntax byte offsets, authored paths or the exhausted
budget without including supplied data values. Custom callbacks own their error
messages and should avoid disclosing sensitive inputs. Complete data/schema depth and size are checked even
when a template does not use them.

These are deterministic traversal/output limits, not wall-clock deadlines, an
allocation-failure recovery mechanism or an exact heap-size promise. Callers own
construction, cloning and destruction of their input/schema trees; don't build
unbounded recursive trees before invoking this API. There are no built-in I/O operations, background execution or shared mutable render
state. Custom method callbacks are trusted and non-preemptible; their effects,
allocation, panics and post-return result checks are covered in [METHODS.md](METHODS.md).

## Relationship to the earlier Forge renderer

This is an original, narrower implementation of the **observed rendering pattern**
in the reviewed Forge framework: `.forge` text, parse-once/render-many, escaped/raw
variables, sections/current-parent lookup and schema-before-output validation, method chains and builtin/custom transformations.
It is not a wholesale copy, drop-in API port or claim that every historical
`.forge` file is supported. [Review context](../../../docs/reviews/forge/forge-framework.md).

**Deliberate differences:** local `Value`/`Schema` types, bounded traversal, strict
ASCII lookup paths and schema-guided optional shadowing. `Value` belongs only to
this rendering domain; it must not replace typed runtime capability enums with a
universal value-map payload.

**Still outside this slice:** bundled Cargo/Rust/game/Metal templates, schema
inference, JSON/YAML/TOML loading, custom schema callbacks, partials, delimiter changes,
inheritance, Cargo build-script discovery or repository-source materialization.
Partials and delimiter changes were also unsupported in the earlier renderer.
Unsupported syntax rejects explicitly. Do not run historical generator scripts or
promise source-overwrite protection here: this library has no file-write API.

Tests cover escaping/raw output, scope/optional behavior, all structural failures,
Unicode rejection, all 38 builtins, custom methods, input/output/work bounds, reusable templates and existing
Forge composition. They verify this documented subset, not full Mustache conformance
or runtime compatibility with every older Forge feature.
