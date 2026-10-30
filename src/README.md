# Source-set guide

Organize code as `src/<language>/<purpose>/`, using `rust`, `python`, or `bash`.
A source set is a cohesive capability or tool, not necessarily a separate
process, deployment, or published package. Name it for what it does, using
lowercase hyphenated directory names; follow language rules for identifiers.
Do not create speculative capabilities or mirror every capability in all three
languages.

## Boundaries

Keep related behavior together. Use internal modules before introducing separate
packages, and split only when dependencies, consumers, ownership, or lifecycle
make the boundary useful. Avoid repository-wide `common`, `utils`, or layer-only
buckets that mix unrelated responsibilities.

For applications with inbound transports, follow
[Design Package Boundaries](../.agents/skills/design-package-boundaries/SKILL.md):
thin adapters submit a transport-neutral envelope to one logical shared ingress
bridge; explicit header metadata selects an authorized registered route, then
the admitted use case may fan out. Business rules stay with capabilities,
external mechanisms stay behind adapters, and composition wires implementations.
Do not scaffold a bridge or framework just to satisfy a directory diagram.

## Keep supporting files close

- **Tests:** keep unit and single-source-set integration tests with their owner,
  following the language guide. Add root `tests/integration/<scenario>/` only
  when a real scenario spans source sets or languages; document its prerequisites,
  isolation, cleanup, and command there. Avoid a second copy of package tests.
- **Documentation:** start with each source set's README. Add local `docs/` for
  detailed design or operation notes; reserve root `docs/` for genuinely
  repository-wide guidance.
- **Examples and configuration:** add local `examples/` or `config/` only when
  useful. Commit sanitized examples and schemas, not live host inventories,
  credentials, tokens, private keys, or machine-specific deployment values.
  Document how local configuration is supplied and ignore generated/private
  files before creating them.
- **Build metadata:** keep manifests and dependencies with actual projects.
  Introduce shared orchestration only when existing projects need it. Do not
  add empty workspace manifests, placeholder tests, or directory-only markers.

## Adding a source set

1. Choose its language and purpose; check whether an existing capability should
   own the behavior instead.
2. Create `src/<language>/<purpose>/` with real source and only the build metadata
   it needs. Read the [Rust](rust/README.md), [Python](python/README.md), or
   [Bash](bash/README.md) conventions first.
3. Add a local README stating responsibility, public entry points, dependencies,
   configuration, and exact build/run/test commands with their working directory.
4. Add meaningful tests for behavior and relevant failure paths. Document any
   external infrastructure required; never label an unrun check as passed.
5. Keep generated output and local secrets out of Git. Add targeted ignore rules
   and update the language README with a link to the new source set.

The [Rust workspace](rust/README.md) now contains the first Forge core packages,
a runnable in-process example and local check commands. Python and Bash remain
organizational guides until actual source sets are added.
