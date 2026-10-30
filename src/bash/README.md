# Bash source sets

Put each shell tool or cohesive tool family under `src/bash/<purpose>/`.
No shell tools exist yet.

- Put executable entry points in `<purpose>/bin/`, with a Bash shebang and the
  executable bit. Entry points own argument parsing, user output, and exit status.
- Add `<purpose>/lib/` only for reusable code that is actually sourced. Library
  files should define functions without running the tool on import or changing
  the caller's working directory, shell options, or traps as an import side effect.
- Resolve a tool's own files relative to its script location, not an assumed
  working directory. Quote expansions and make dependencies and error handling
  explicit. Keep shell helpers within their capability rather than a global pile.
- Keep behavior tests and safe fixtures in `<purpose>/tests/`; use temporary
  directories and isolate commands that could modify machines or infrastructure.
- Document supported Bash/platform versions, required commands, configuration,
  usage, exit codes, and exact syntax/lint/test commands in the tool's README.
  Introduce a test framework only when real tests need it.

See the [source-set guide](../README.md) for cross-language tests and boundaries.
