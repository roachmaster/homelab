# Python source sets

Put each Python capability or tool under `src/python/<purpose>/`.
No Python projects exist yet.

- For a real packaged project, keep `pyproject.toml` at that project's root and
  importable code under `<purpose>/src/<import_name>/`; use underscores for Python
  import names even when the project directory uses hyphens. This nested `src/`
  is the project's import layout, distinct from the repository's language grouping.
- Keep cohesive internals as modules in the owning import package. Create another
  project only for a meaningful dependency or consumer boundary; do not make a
  distribution for every module or assume that repository siblings are importable.
- A small standalone script can stay simple. Introduce packaging and dependency
  management when its actual execution or reuse needs require them, not as an
  empty repository-wide Python project.
- Keep tests in `<purpose>/tests/`. Document how tests import/install the project,
  the supported Python version, and the exact run, format, lint, and test commands.
- Choose and document project-local environment and dependency management when
  sources exist. Isolate dependencies; do not rely on global installs or commit
  virtual environments, caches, or private configuration.

See the [source-set guide](../README.md) for cross-language tests and boundaries.
