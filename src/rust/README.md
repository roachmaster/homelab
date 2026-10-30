# Rust source sets

Put each Rust capability or tool under `src/rust/<purpose>/`. No crates exist yet.

- Start an actual Cargo package with its own `Cargo.toml` and `src/lib.rs`,
  `src/main.rs`, or both as needed. A Cargo package contains one or more crate
  targets; ordinary internal organization uses modules, not another manifest.
- Keep cohesive internals in private modules. Extract a separate package/crate
  when it earns a dependency boundary or serves real consumers, not per type.
- Add a workspace at `src/rust/Cargo.toml` when actual member packages benefit
  from shared Cargo coordination. List real members; a workspace alone does not
  enforce architectural dependency direction. Do not add an empty root manifest.
- Keep unit tests beside the relevant Rust code and package integration tests in
  `<purpose>/tests/`. Keep package-specific examples in `<purpose>/examples/`.
- Document package-local format, lint, build, and test commands in its README,
  including required features and toolchain. Decide lockfile and generated-output
  handling when the first package exists rather than inventing a build today.

See the [source-set guide](../README.md) for cross-language tests and boundaries.
