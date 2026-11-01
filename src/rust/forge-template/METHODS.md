# Template methods and built-in functions

Methods are the Forge extension to ordinary Mustache-like interpolation. The
**38 historical built-in names**, receiver/argument behavior and useful edge cases
are implemented here, alongside explicit application-defined methods. This is
original code, not a copied private implementation. The [renderer README](README.md)
defines the remaining template/schema differences and resource boundaries.

## Chains, literals and paths

```text
{{name.trim().pascal_case()}}
{{name.trim().replace("old", replacement).to_uppercase()}}
{{items.join(", ")}}
{{price.multiply(1.2).round()}}
{{#items.first()}}{{.to_uppercase()}}{{/items.first()}}
{{^items.is_empty()}}Items are present{{/items.is_empty()}}
```

A receiver is a dotted context path or the current value (`.trim()`). Calls chain
left-to-right; each output becomes the next receiver. Arguments are quoted strings,
`null`, Boolean, signed integer, finite float literals, or paths resolved from the
current/parent context—not from the previous method result. Single/double quoted
strings support `\n`, `\r`, `\t`, `\\`, `\'` and `\"`. Commas, parentheses and tag
closing delimiters inside a quoted argument remain text. Nested method calls in
arguments, arithmetic expressions and standalone function calls are rejected.

Sections close with the same parsed expression. An absent optional receiver skips
its entire chain; `.default(...)` handles an existing falsey/null value, **not** an
absent receiver. Missing argument paths error. No implicit argument coercion is
performed except the numeric operations/conversions documented below.

All transformations happen **before one final HTML-escaping pass** for `{{...}}`.
Raw `{{{...}}}` / `{{& ...}}` bypass that pass. Neither is a source-language encoder.

## Complete built-in inventory

`MethodRegistry::new()` / `default()` contain precisely these 38 names.
`names()` lists registered names in deterministic sorted order.

| Names | Arguments | Receivers and result |
| --- | --- | --- |
| `snake_case`, `pascal_case`, `camel_case`, `kebab_case`, `upper_snake_case`, `to_SCREAMING_SNAKE_CASE` | none | Text → text. Unicode alphanumeric words, punctuation separators, lowercase/uppercase and acronym boundaries. Last two names are equivalent. |
| `to_uppercase`, `to_lowercase`, `trim` | none | Text → Unicode uppercase/lowercase or Unicode-whitespace-trimmed text. |
| `length` | none | Text/list/object/null → integer. Text counts Unicode scalar characters, not bytes/graphemes; null gives zero. |
| `is_empty` | none | Any → bool. True for null/empty text/list/object; false for other scalar values. |
| `is_null` | none | Any → bool. True only for null. |
| `to_string` | none | Any → text, using the formatting below. |
| `type` | none | Any → `null`, `boolean`, `integer`, `number`, `string`, `array`, or `object`. |
| `default` | one value | Returns receiver when truthy, otherwise argument. Zero and empty objects are truthy; this is not identical to `is_empty`. |
| `contains` | one value | Text substring (text arg), list membership (structural typed equality), or object-key membership (text arg) → bool. |
| `starts_with`, `ends_with` | one text | Text → bool. |
| `replace` | two texts | Text → text; replaces every non-overlapping match. Empty search text matches UTF-8 character boundaries including both ends. |
| `join` | one text separator | List → text, formatting each item as below. |
| `first`, `last` | none | List → selected item, or null for an empty list. |
| `at` | one integer index | List → item or null when out of range. Negative indices and non-integer indices error. |
| `get`, `has` | one text key | Object → selected value or null (`get`), or membership bool (`has`). |
| `keys`, `values` | none | Object → list in sorted key order. Keys are text; values preserve their types. |
| `not` | none | Bool → negated bool. |
| `abs`, `round`, `floor`, `ceil` | none | Integer/float → same numeric variant; integers are unchanged except `abs`. `i64::MIN.abs()` errors. Float round uses Rust's halfway-away-from-zero rule. |
| `add`, `subtract`, `multiply` | one number | Integer+integer keeps integer with checked overflow. Otherwise converts integers to f64 and returns a finite float. |
| `divide` | one number | Integer/float → finite float, including integer/integer division. Zero divisor errors. |
| `to_int` | none | Integer unchanged; finite float truncates toward zero and saturates at i64 bounds (historical behavior); text uses strict Rust i64 parsing. |
| `to_float` | none | Integer converts to f64; float unchanged; text uses Rust f64 parsing. Non-finite results error. |

Every builtin checks exact arity and receiver/argument types. `Schema::Float`
requires `Value::Float`; it does not accept an integer implicitly. Explicit
numeric methods accept both variants. Mixed arithmetic/int-to-float conversions
can lose integer precision; no decimal/exact-finance guarantee is made.
Non-finite input, literal and method-result floats are rejected.

`to_string` / `join` use human-readable formatting, **not JSON**: null is `null`,
strings are unquoted, lists are `[a, null]`, and objects are `{key: value}` in key
order. Direct interpolation of a null is still empty. Direct interpolation of
objects/lists still fails: choose a section, `join` or `to_string` explicitly.

## Custom methods

`MethodRegistry::empty()` starts with no builtins. Register deliberately; duplicate
names—including builtin names in a default registry—return an error without
replacing the existing implementation. An empty registry may define a custom
method with a builtin spelling; its result is treated as custom, not assigned a
false builtin return type. There is no process-global registry or dynamic plugin
loading.

```rust
use forge_template::{render_with_methods, MethodRegistry, Schema, TemplateError, Value};
let mut methods = MethodRegistry::new();
methods.register("initials", |value| {
    let Value::Text(name) = value else {
        return Err(TemplateError::Method {
            name: "initials".into(), reason: "expected text".into(),
        });
    };
    Ok(Value::Text(name.split_whitespace()
        .filter_map(|part| part.chars().next()).collect()))
})?;
assert_eq!(render_with_methods("{{.trim().initials()}}",
    &Value::Text(" Ada Lovelace ".into()), &Schema::Text, &methods)?, "AL");
# Ok::<(), TemplateError>(())
```

`register` accepts a no-argument `Fn(&Value) -> Result<Value, TemplateError>`.
`register_with_args` accepts `Fn(&Value, &[Value]) -> Result<Value, TemplateError>`;
that callback checks its own argument count/types. Both require `Send + Sync +
'static` and receive already evaluated arguments. They are ordinary explicitly
supplied Rust functions/closures—not discovered services or provider lookups.
See the runnable [method extension example](examples/method_extensions.rs) and
its [template file](examples/methods.forge).

## Validation, budgets and trust

Input/schema validation precedes all callbacks. Every statically known expression,
method name, builtin/no-argument arity and known builtin type is preflighted even in
skipped sections. Return shapes from custom methods, `get`, `default` and mixed
object values may be dynamic. Child paths/types in those scopes are validated
when the method result exists; an unexecuted dynamic branch cannot be statically
proven. Missing paths in a dynamic result follow absent-value semantics after
parent fallback (empty interpolation/skipped normal section); a missing method
argument still errors. This matches the historical dynamic-result behavior. This is not Rust reflection or compile-time type checking of templates.

Calls/arguments consume syntax budget; evaluation/path lookup, builtin searches,
value traversal/copying and produced bytes consume work budget. Builtin strings
are bounded **while being constructed**; result values must fit depth/node/byte
limits and finite-number rules. Per-call limits remain deterministic—not exact
CPU instruction counts, allocation accounting or a wall-clock scheduler.

Custom callbacks are **trusted application code**. Prefer deterministic, pure
transformations. They can allocate, block, panic or perform effects before returning;
this renderer cannot sandbox/preempt them or undo earlier callback effects. Their
results are checked *after return*. Panics propagate; errors return no partial
string and do not trigger retries. A callback can consume arbitrary resources or
return an overly deep value whose destruction is also caller-code risk. Do not
expose untrusted native extensions merely because render budgets exist.

No builtin performs filesystem, shell, network or code generation operations.
